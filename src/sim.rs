use hashlink::LinkedHashMap;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Params {
    pub cache_capacity: usize,
    pub hot_keys: u32,
    pub scan_keys: u32,
    pub zipf_s: f64,
    pub window_pct: usize,
    pub hot_rate: u32,
    pub scan_rate: u32,
    pub phase: String,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            cache_capacity: 1_000,
            hot_keys: 500,
            scan_keys: 10_000,
            zipf_s: 0.8,
            window_pct: 1,
            hot_rate: 500,
            scan_rate: 1_500,
            phase: "warmup".to_string(),
        }
    }
}

#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetRequest {
    pub cache_capacity: Option<usize>,
    pub hot_keys: Option<u32>,
    pub scan_keys: Option<u32>,
    pub zipf_s: Option<f64>,
    pub hot_rate: Option<u32>,
    pub scan_rate: Option<u32>,
}

#[derive(Clone, Deserialize)]
pub struct PhaseRequest {
    pub phase: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StateSnapshot {
    pub params: Params,
    pub batch: u64,
    pub configs: BTreeMap<String, CacheSnapshot>,
    pub paused: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheSnapshot {
    pub tick: u64,
    pub capacity: usize,
    pub window_pct: usize,
    pub segments: SegmentsSnapshot,
    pub hot_hr: f64,
    pub scan_hr: f64,
    pub hot_hits: u64,
    pub hot_misses: u64,
    pub scan_hits: u64,
    pub scan_misses: u64,
    pub admissions: u64,
    pub rejections: u64,
    pub freq_hot: Vec<u8>,
    pub freq_scan: Vec<u8>,
    pub history: Vec<HistoryEntry>,
}

#[derive(Clone, Serialize)]
pub struct SegmentsSnapshot {
    pub window: SegmentSnapshot,
    pub probation: SegmentSnapshot,
    pub protected: SegmentSnapshot,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SegmentSnapshot {
    pub name: String,
    pub size: usize,
    pub hot_count: usize,
    pub scan_count: usize,
    pub hot_keys: Vec<u32>,
    pub avg_age: f64,
    pub max_age: u64,
    pub ages: Vec<u64>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub tick: u64,
    pub hot_hr: f64,
    pub scan_hr: f64,
    pub batch_hot_hr: f64,
    pub batch_scan_hr: f64,
    pub window_hot: usize,
    pub window_scan: usize,
    pub probation_hot: usize,
    pub probation_scan: usize,
    pub protected_hot: usize,
    pub protected_scan: usize,
}

#[derive(Clone)]
struct CountMinSketch {
    width: usize,
    mask: usize,
    rows: [Vec<u8>; 4],
    seeds: [u32; 4],
}

impl CountMinSketch {
    fn new(width_log2: u32) -> Self {
        let width = 1usize << width_log2;
        Self {
            width,
            mask: width - 1,
            rows: std::array::from_fn(|_| vec![0; width]),
            seeds: [0x9e37_79b9, 0x517c_c1b7, 0x6a09_e667, 0xbb67_ae85],
        }
    }

    fn hash(&self, key: u32, row: usize) -> usize {
        let mut h = key.wrapping_mul(self.seeds[row]);
        h ^= h >> 16;
        (h as usize) & self.mask
    }

    fn increment(&mut self, key: u32) {
        for row in 0..4 {
            let idx = self.hash(key, row);
            if self.rows[row][idx] < 15 {
                self.rows[row][idx] += 1;
            }
        }
    }

    fn estimate(&self, key: u32) -> u8 {
        let mut min = 15u8;
        for row in 0..4 {
            let value = self.rows[row][self.hash(key, row)];
            if value < min {
                min = value;
            }
        }
        min
    }

    fn reset(&mut self) {
        for row in &mut self.rows {
            for cell in row.iter_mut().take(self.width) {
                *cell >>= 1;
            }
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum SegmentName {
    Window,
    Probation,
    Protected,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Source {
    Hot,
    Scan,
}

#[derive(Clone)]
struct CacheEntry {
    source: Source,
    segment: SegmentName,
    insert_time: u64,
    last_access: u64,
}

pub struct WTinyLFUCache {
    capacity: usize,
    window_pct: usize,
    window_max: usize,
    protected_max: usize,
    window: LinkedHashMap<u32, CacheEntry>,
    probation: LinkedHashMap<u32, CacheEntry>,
    protected: LinkedHashMap<u32, CacheEntry>,
    lookup: HashMap<u32, SegmentName>,
    sketch: CountMinSketch,
    sample_count: usize,
    sample_threshold: usize,
    jitter_state: u32,
    hot_hits: u64,
    hot_misses: u64,
    scan_hits: u64,
    scan_misses: u64,
    admissions: u64,
    rejections: u64,
    tick: u64,
    batch_hot_hits: u64,
    batch_hot_misses: u64,
    batch_scan_hits: u64,
    batch_scan_misses: u64,
    history_len: usize,
    history: Vec<HistoryEntry>,
}

impl WTinyLFUCache {
    pub fn new(capacity: usize, window_pct: usize) -> Self {
        let safe_capacity = capacity.max(1);
        let window_max = ((safe_capacity * window_pct) / 100).max(1);
        let slru = safe_capacity.saturating_sub(window_max);
        let probation_max = ((slru * 20) / 100).max(1);
        let protected_max = slru.saturating_sub(probation_max);
        let sketch_width = ((safe_capacity * 4) as f64).log2().floor() as u32;
        Self {
            capacity: safe_capacity,
            window_pct,
            window_max,
            protected_max,
            window: LinkedHashMap::new(),
            probation: LinkedHashMap::new(),
            protected: LinkedHashMap::new(),
            lookup: HashMap::new(),
            sketch: CountMinSketch::new(sketch_width.max(8)),
            sample_count: 0,
            sample_threshold: (safe_capacity * 10).max(100),
            jitter_state: 0xDEAD_BEEF,
            hot_hits: 0,
            hot_misses: 0,
            scan_hits: 0,
            scan_misses: 0,
            admissions: 0,
            rejections: 0,
            tick: 0,
            batch_hot_hits: 0,
            batch_hot_misses: 0,
            batch_scan_hits: 0,
            batch_scan_misses: 0,
            history_len: 600,
            history: Vec::new(),
        }
    }

    fn total_len(&self) -> usize {
        self.window.len() + self.probation.len() + self.protected.len()
    }

    fn jitter(&mut self) -> u32 {
        let mut s = self.jitter_state;
        s ^= s << 13;
        s ^= s >> 17;
        s ^= s << 5;
        self.jitter_state = s;
        s
    }

    fn record_access(&mut self, key: u32) {
        self.sketch.increment(key);
        self.sample_count += 1;
        if self.sample_count >= self.sample_threshold {
            self.sketch.reset();
            self.sample_count = 0;
        }
    }

    fn rebalance_protected(&mut self) {
        while self.protected.len() > self.protected_max && !self.protected.is_empty() {
            let (key, mut entry) = self.protected.pop_front().expect("protected not empty");
            entry.segment = SegmentName::Probation;
            self.probation.insert(key, entry);
            self.lookup.insert(key, SegmentName::Probation);
        }
    }

    fn drain_window(&mut self) {
        self.rebalance_protected();
        while self.window.len() > self.window_max && !self.window.is_empty() {
            let w_key = *self.window.front().expect("window not empty").0;
            if !self.probation.is_empty() {
                let p_key = *self.probation.front().expect("probation not empty").0;
                let w_freq = self.sketch.estimate(w_key);
                let p_freq = self.sketch.estimate(p_key);

                let admit = if w_freq > p_freq {
                    true
                } else if w_freq >= 6 {
                    (self.jitter() & 127) == 0
                } else {
                    false
                };

                if admit {
                    let mut w_entry = self.window.remove(&w_key).expect("window entry exists");
                    w_entry.segment = SegmentName::Probation;
                    self.probation.insert(w_key, w_entry);
                    self.lookup.insert(w_key, SegmentName::Probation);
                    self.probation
                        .remove(&p_key)
                        .expect("probation victim exists");
                    self.lookup.remove(&p_key);
                    self.admissions += 1;
                } else {
                    self.window.remove(&w_key).expect("window entry exists");
                    self.lookup.remove(&w_key);
                    self.rejections += 1;
                }
            } else {
                let mut w_entry = self.window.remove(&w_key).expect("window entry exists");
                w_entry.segment = SegmentName::Probation;
                self.probation.insert(w_key, w_entry);
                self.lookup.insert(w_key, SegmentName::Probation);
            }
        }
    }

    fn evict_for_capacity(&mut self) {
        for segment in [&mut self.probation, &mut self.window, &mut self.protected] {
            if let Some((victim, _)) = segment.pop_front() {
                self.lookup.remove(&victim);
                return;
            }
        }
    }

    fn access(&mut self, key: u32, source: Source) -> bool {
        self.tick += 1;
        let now = self.tick;

        if let Some(segment_name) = self.lookup.get(&key).copied() {
            self.record_access(key);
            match segment_name {
                SegmentName::Window => {
                    if let Some(mut entry) = self.window.remove(&key) {
                        entry.last_access = now;
                        self.window.insert(key, entry);
                    }
                }
                SegmentName::Probation => {
                    if let Some(mut entry) = self.probation.remove(&key) {
                        entry.last_access = now;
                        entry.segment = SegmentName::Protected;
                        self.protected.insert(key, entry);
                        self.lookup.insert(key, SegmentName::Protected);
                    }
                }
                SegmentName::Protected => {
                    if let Some(mut entry) = self.protected.remove(&key) {
                        entry.last_access = now;
                        self.protected.insert(key, entry);
                    }
                }
            }

            match source {
                Source::Hot => {
                    self.hot_hits += 1;
                    self.batch_hot_hits += 1;
                }
                Source::Scan => {
                    self.scan_hits += 1;
                    self.batch_scan_hits += 1;
                }
            }
            true
        } else {
            self.record_access(key);
            let entry = CacheEntry {
                source,
                segment: SegmentName::Window,
                insert_time: now,
                last_access: now,
            };
            self.window.insert(key, entry);
            self.lookup.insert(key, SegmentName::Window);

            let mut total = self.total_len();
            while total > self.capacity {
                self.drain_window();
                total = self.total_len();
                if total > self.capacity {
                    self.evict_for_capacity();
                    total = self.total_len();
                }
            }

            match source {
                Source::Hot => {
                    self.hot_misses += 1;
                    self.batch_hot_misses += 1;
                }
                Source::Scan => {
                    self.scan_misses += 1;
                    self.batch_scan_misses += 1;
                }
            }
            false
        }
    }

    fn segment_snapshot(
        now: u64,
        name: &str,
        segment: &LinkedHashMap<u32, CacheEntry>,
    ) -> SegmentSnapshot {
        let mut hot_keys = Vec::new();
        let mut ages = Vec::new();

        for (key, entry) in segment {
            ages.push(now.saturating_sub(entry.insert_time));
            match entry.source {
                Source::Hot => hot_keys.push(*key),
                Source::Scan => {}
            }
        }

        let age_sum: u64 = ages.iter().sum();
        let avg_age = if ages.is_empty() {
            0.0
        } else {
            age_sum as f64 / ages.len() as f64
        };
        let max_age = ages.iter().copied().max().unwrap_or(0);

        SegmentSnapshot {
            name: name.to_string(),
            size: segment.len(),
            hot_count: hot_keys.len(),
            scan_count: segment.len().saturating_sub(hot_keys.len()),
            hot_keys,
            avg_age,
            max_age,
            ages,
        }
    }

    pub fn snapshot(&self) -> CacheSnapshot {
        let now = self.tick;
        let mut freq_hot = Vec::new();
        let mut freq_scan = Vec::new();

        for (key, segment_name) in &self.lookup {
            let frequency = self.sketch.estimate(*key);
            let entry = match segment_name {
                SegmentName::Window => self.window.get(key),
                SegmentName::Probation => self.probation.get(key),
                SegmentName::Protected => self.protected.get(key),
            };

            if let Some(entry) = entry {
                match entry.source {
                    Source::Hot => freq_hot.push(frequency),
                    Source::Scan => freq_scan.push(frequency),
                }
            }
        }

        let hot_total = self.hot_hits + self.hot_misses;
        let scan_total = self.scan_hits + self.scan_misses;

        CacheSnapshot {
            tick: now,
            capacity: self.capacity,
            window_pct: self.window_pct,
            segments: SegmentsSnapshot {
                window: Self::segment_snapshot(now, "window", &self.window),
                probation: Self::segment_snapshot(now, "probation", &self.probation),
                protected: Self::segment_snapshot(now, "protected", &self.protected),
            },
            hot_hr: if hot_total == 0 {
                0.0
            } else {
                self.hot_hits as f64 / hot_total as f64 * 100.0
            },
            scan_hr: if scan_total == 0 {
                0.0
            } else {
                self.scan_hits as f64 / scan_total as f64 * 100.0
            },
            hot_hits: self.hot_hits,
            hot_misses: self.hot_misses,
            scan_hits: self.scan_hits,
            scan_misses: self.scan_misses,
            admissions: self.admissions,
            rejections: self.rejections,
            freq_hot,
            freq_scan,
            history: self
                .history
                .iter()
                .rev()
                .take(300)
                .cloned()
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect(),
        }
    }

    pub fn record_history(&mut self) {
        let hot_total = self.hot_hits + self.hot_misses;
        let scan_total = self.scan_hits + self.scan_misses;
        let batch_hot = self.batch_hot_hits + self.batch_hot_misses;
        let batch_scan = self.batch_scan_hits + self.batch_scan_misses;

        self.history.push(HistoryEntry {
            tick: self.tick,
            hot_hr: if hot_total == 0 {
                0.0
            } else {
                self.hot_hits as f64 / hot_total as f64 * 100.0
            },
            scan_hr: if scan_total == 0 {
                0.0
            } else {
                self.scan_hits as f64 / scan_total as f64 * 100.0
            },
            batch_hot_hr: if batch_hot == 0 {
                0.0
            } else {
                self.batch_hot_hits as f64 / batch_hot as f64 * 100.0
            },
            batch_scan_hr: if batch_scan == 0 {
                0.0
            } else {
                self.batch_scan_hits as f64 / batch_scan as f64 * 100.0
            },
            window_hot: self
                .window
                .values()
                .filter(|entry| entry.source == Source::Hot)
                .count(),
            window_scan: self
                .window
                .values()
                .filter(|entry| entry.source == Source::Scan)
                .count(),
            probation_hot: self
                .probation
                .values()
                .filter(|entry| entry.source == Source::Hot)
                .count(),
            probation_scan: self
                .probation
                .values()
                .filter(|entry| entry.source == Source::Scan)
                .count(),
            protected_hot: self
                .protected
                .values()
                .filter(|entry| entry.source == Source::Hot)
                .count(),
            protected_scan: self
                .protected
                .values()
                .filter(|entry| entry.source == Source::Scan)
                .count(),
        });

        if self.history.len() > self.history_len {
            let keep_from = self.history.len() - self.history_len;
            self.history = self.history.split_off(keep_from);
        }

        self.batch_hot_hits = 0;
        self.batch_hot_misses = 0;
        self.batch_scan_hits = 0;
        self.batch_scan_misses = 0;
    }
}

pub struct SimRunner {
    caches: BTreeMap<usize, WTinyLFUCache>,
    configs: Vec<usize>,
    pub params: Params,
    scan_cursor: u64,
    batch_count: u64,
}

impl SimRunner {
    pub fn new() -> Self {
        let mut runner = Self {
            caches: BTreeMap::new(),
            configs: Vec::new(),
            params: Params::default(),
            scan_cursor: 0,
            batch_count: 0,
        };
        runner.reset(None);
        runner
    }

    pub fn reset(&mut self, params: Option<ResetRequest>) {
        if let Some(params) = params {
            if let Some(value) = params.cache_capacity {
                self.params.cache_capacity = value.max(1);
            }
            if let Some(value) = params.hot_keys {
                self.params.hot_keys = value.max(1);
            }
            if let Some(value) = params.scan_keys {
                self.params.scan_keys = value.max(1);
            }
            if let Some(value) = params.zipf_s {
                self.params.zipf_s = value;
            }
            if let Some(value) = params.hot_rate {
                self.params.hot_rate = value;
            }
            if let Some(value) = params.scan_rate {
                self.params.scan_rate = value;
            }
        }

        self.caches.clear();
        self.configs = vec![1, 50, 99];
        for window_pct in self.configs.iter().copied() {
            self.caches.insert(
                window_pct,
                WTinyLFUCache::new(self.params.cache_capacity, window_pct),
            );
        }
        self.scan_cursor = 0;
        self.batch_count = 0;
        self.params.phase = "warmup".to_string();
        self.params.window_pct = 1;
    }

    pub fn set_phase(&mut self, phase: &str) {
        self.params.phase = if phase == "attack" {
            "attack".to_string()
        } else {
            "warmup".to_string()
        };
    }

    fn zipf_key(&self, i: u64) -> u32 {
        let n = self.params.hot_keys.max(1);
        let s = self.params.zipf_s;
        let alpha = 1.0 / (1.0 - s);
        let mut h = (i as u32).wrapping_mul(2_654_435_761) ^ 0x12_345;
        h = (h ^ (h >> 16)).wrapping_mul(0x045d_9f3b);
        h ^= h >> 16;
        let u = ((h % 1_000_000) as f64 + 1.0) / 1_000_001.0;
        ((n as f64 * u.powf(alpha)).floor() as u32) % n
    }

    pub fn run_batch(&mut self) {
        let phase = self.params.phase.clone();
        let hot_rate = self.params.hot_rate as u64;
        let scan_rate = self.params.scan_rate as u64;
        let batch_scan_start = self.scan_cursor;
        let hot_keys = self.params.hot_keys;
        let scan_keys = self.params.scan_keys.max(1);
        let hot_batch: Vec<u32> = (0..hot_rate)
            .map(|i| self.zipf_key(self.batch_count * hot_rate + i))
            .collect();
        let scan_batch: Vec<u32> = if phase == "attack" {
            (0..scan_rate)
                .map(|i| hot_keys + ((batch_scan_start + i) as u32 % scan_keys))
                .collect()
        } else {
            Vec::new()
        };

        for window_pct in self.configs.iter().copied() {
            if let Some(cache) = self.caches.get_mut(&window_pct) {
                for key in &hot_batch {
                    cache.access(*key, Source::Hot);
                }

                if phase == "attack" {
                    for key in &scan_batch {
                        cache.access(*key, Source::Scan);
                    }
                }

                cache.record_history();
            }
        }

        self.scan_cursor = batch_scan_start + scan_rate;
        self.batch_count += 1;
    }

    pub fn state(&self, paused: bool) -> StateSnapshot {
        let mut configs = BTreeMap::new();
        for window_pct in &self.configs {
            if let Some(cache) = self.caches.get(window_pct) {
                configs.insert(window_pct.to_string(), cache.snapshot());
            }
        }

        StateSnapshot {
            params: self.params.clone(),
            batch: self.batch_count,
            configs,
            paused,
        }
    }

    pub fn has_configs(&self) -> bool {
        !self.configs.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_min_sketch_saturates_and_ages() {
        let mut sketch = CountMinSketch::new(8);
        for _ in 0..100 {
            sketch.increment(42);
        }
        assert_eq!(sketch.estimate(42), 15);
        sketch.reset();
        assert_eq!(sketch.estimate(42), 7);
    }

    #[test]
    fn cache_never_exceeds_capacity() {
        let mut cache = WTinyLFUCache::new(10, 1);
        for key in 0..200 {
            cache.access(key, Source::Hot);
            assert!(cache.total_len() <= 10);
        }
    }

    #[test]
    fn sim_runner_builds_three_comparison_caches() {
        let sim = SimRunner::new();
        let state = sim.state(false);
        assert_eq!(state.configs.len(), 3);
        assert!(state.configs.contains_key("1"));
        assert!(state.configs.contains_key("50"));
        assert!(state.configs.contains_key("99"));
    }
}
