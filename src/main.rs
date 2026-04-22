mod sim;

use axum::{
    Json, Router,
    extract::State,
    response::Html,
    routing::{get, post},
};
use sim::{PhaseRequest, ResetRequest, SimRunner, StateSnapshot};
use std::{net::SocketAddr, process::Command, sync::Arc, thread, time::Duration};
use tokio::{
    net::TcpListener,
    sync::Mutex,
    time::{MissedTickBehavior, interval},
};

#[derive(Clone)]
struct AppState {
    inner: Arc<Mutex<SharedSim>>,
}

struct SharedSim {
    sim: SimRunner,
    paused: bool,
}

#[tokio::main]
async fn main() {
    free_port(3000);

    let state = AppState {
        inner: Arc::new(Mutex::new(SharedSim {
            sim: SimRunner::new(),
            paused: false,
        })),
    };

    spawn_tick_loop(state.clone());

    let app = Router::new()
        .route("/", get(index))
        .route("/api/state", get(get_state))
        .route("/api/reset", post(reset))
        .route("/api/phase", post(set_phase))
        .route("/api/pause", post(toggle_pause))
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    let listener = TcpListener::bind(addr).await.expect("bind listener");
    println!("W-TinyLFU Rust simulator listening on http://{addr}");
    axum::serve(listener, app).await.expect("serve app");
}

fn free_port(port: u16) {
    let output = match Command::new("lsof")
        .args(["-t", &format!("-iTCP:{port}"), "-sTCP:LISTEN"])
        .output()
    {
        Ok(output) => output,
        Err(error) => {
            eprintln!("warning: failed to inspect port {port}: {error}");
            return;
        }
    };

    if !output.status.success() || output.stdout.is_empty() {
        return;
    }

    let current_pid = std::process::id();
    let pids: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|pid| !pid.is_empty())
        .filter(|pid| pid.parse::<u32>().ok() != Some(current_pid))
        .map(str::to_string)
        .collect();

    if pids.is_empty() {
        return;
    }

    let _ = Command::new("kill").arg("-TERM").args(&pids).status();
    thread::sleep(Duration::from_millis(250));

    let still_listening = Command::new("lsof")
        .args(["-t", &format!("-iTCP:{port}"), "-sTCP:LISTEN"])
        .output()
        .ok()
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::trim)
                .filter(|pid| !pid.is_empty())
                .filter(|pid| pid.parse::<u32>().ok() != Some(current_pid))
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if !still_listening.is_empty() {
        let _ = Command::new("kill")
            .arg("-KILL")
            .args(&still_listening)
            .status();
        thread::sleep(Duration::from_millis(100));
    }
}

fn spawn_tick_loop(state: AppState) {
    tokio::spawn(async move {
        let mut ticker = interval(Duration::from_millis(50));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            let mut shared = state.inner.lock().await;
            if !shared.paused && shared.sim.has_configs() {
                shared.sim.run_batch();
            }
        }
    });
}

async fn index() -> Html<&'static str> {
    Html(include_str!("../static/index.html"))
}

async fn get_state(State(state): State<AppState>) -> Json<StateSnapshot> {
    let shared = state.inner.lock().await;
    Json(shared.sim.state(shared.paused))
}

async fn reset(
    State(state): State<AppState>,
    Json(request): Json<ResetRequest>,
) -> Json<StateSnapshot> {
    let mut shared = state.inner.lock().await;
    shared.sim.reset(Some(request));
    shared.paused = false;
    Json(shared.sim.state(shared.paused))
}

async fn set_phase(
    State(state): State<AppState>,
    Json(request): Json<PhaseRequest>,
) -> Json<StateSnapshot> {
    let mut shared = state.inner.lock().await;
    shared.sim.set_phase(&request.phase);
    Json(shared.sim.state(shared.paused))
}

async fn toggle_pause(State(state): State<AppState>) -> Json<StateSnapshot> {
    let mut shared = state.inner.lock().await;
    shared.paused = !shared.paused;
    Json(shared.sim.state(shared.paused))
}
