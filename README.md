# W-TinyLFU Cache Simulator

An interactive cache-policy simulator for exploring how **W-TinyLFU** behaves under mixed workloads.

This project visualizes the trade-offs behind TinyLFU-style admission with a **window**, **probation**, and **protected** layout, plus a **Count-Min Sketch** for frequency estimation. It compares three window-size strategies side by side so you can see how each one reacts during warmup and under scan pressure.

## What It Does

- Simulates **W-TinyLFU admission** with windowed caching and SLRU-style segmentation.
- Compares **1%**, **50%**, and **99%** window configurations in parallel.
- Models a **Zipf-like hot-key workload** and a **streaming scan workload**.
- Shows **hot hit rate over time**, segment composition, entry age distribution, sketch frequencies, and hot-key placement.
- Lets you switch instantly between **Warmup** and **Attack** phases to observe scan resistance.

## Why It’s Useful

W-TinyLFU is designed to protect a cache from one-time or low-value items while still adapting to recency and frequency. This simulator makes that behavior visible:

- During **Warmup**, you can watch the cache fill with frequently reused keys.
- During **Attack**, you can see whether the cache keeps serving hot items or gets polluted by scans.
- Across window sizes, you can compare how much recency versus admission filtering changes outcomes.

It’s a practical way to understand why W-TinyLFU is effective in real systems and how tuning the window size changes behavior.

## Running It

Make sure you have a recent Rust toolchain installed, then start the app:

```bash
cargo run
```

Open [http://127.0.0.1:3000](http://127.0.0.1:3000) in your browser.

The app automatically frees port `3000` on startup if an older simulator instance is still running.

## Controls

- **Cache**: total cache capacity.
- **Hot keys**: number of keys in the hot working set.
- **Scan keys**: number of keys in the scan stream.
- **Zipf s**: skew of the hot-key distribution.
- **Hot/batch**: hot-key accesses per simulation batch.
- **Scan/batch**: scan accesses per batch during attack mode.
- **Reset & Start**: rebuilds all caches with the current settings.
- **Warmup**: runs only the hot workload.
- **Attack!**: runs hot traffic plus scan traffic.
- **Pause**: freezes the simulation state.

## Implementation Notes

The simulator preserves the original browser-based dashboard while moving the cache engine into Rust for cleaner structure and stronger performance. The core model includes:

- a 4-row Count-Min Sketch with 4-bit counters,
- W-TinyLFU admission logic,
- ordered cache segments for LRU-style movement,
- a timed batch runner that advances the simulation at a steady rate.

## Use Cases

- Learning how **TinyLFU** and **W-TinyLFU** work
- Demonstrating **scan resistance**
- Comparing cache admission behavior under different window sizes
- Teaching cache-policy concepts with a visual, interactive example

## References

- [W-TinyLFU paper](https://arxiv.org/abs/1512.00727)
- [ScyllaDB](https://github.com/scylladb/scylladb)
