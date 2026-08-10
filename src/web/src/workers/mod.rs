//! Modified from upstream: the rendered `/workers` operator page and its
//! row DTO are gone (this fork serves no HTML); the worker-registry JSON
//! endpoints stay, because the distributed match dispatcher is driven
//! through them.

pub mod routes;

use crate::worker::{WorkerSnapshot, WorkerStatus};
use crate::GameAppData;
use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use core::MatchRuntime;
use serde::{Deserialize, Serialize};

/// Body of the "add worker" dialog POST. `port` defaults to the worker's
/// standard listen port (18001) when the dialog leaves it untouched, but
/// it's always sent explicitly by the form.
#[derive(Deserialize)]
pub struct AddWorkerRequest {
    pub host: String,
    pub port: u16,
}

/// Dial the worker the operator typed into the dialog, run the
/// version-checked handshake, register it, and hand the outcome back as
/// JSON so the dialog can show "connected — vX, N threads" or the
/// failure reason without a page reload.
pub async fn workers_add_action(
    State(state): State<GameAppData>,
    Json(body): Json<AddWorkerRequest>,
) -> impl IntoResponse {
    let address = format!("{}:{}", body.host.trim(), body.port);
    Json(state.workers.add_worker(address).await)
}

/// Live status payload polled by the workers page. Mirrors the figures the
/// page renders server-side so the table — status badges, "last seen",
/// per-worker counters and the summary tiles — refreshes in place as the
/// health monitor pings, fences, and reconnects workers, without a full
/// page reload. New/removed rows still require a reload (workers are only
/// added by the operator, which reloads anyway).
#[derive(Serialize)]
pub struct WorkersStatusDto {
    pub ready: usize,
    pub total: usize,
    pub total_threads: usize,
    pub total_batches: u64,
    pub total_matches: u64,
    pub total_failures: u64,
    pub workers: Vec<WorkerStatusDto>,
}

#[derive(Serialize)]
pub struct WorkerStatusDto {
    pub address: String,
    pub status_label: &'static str,
    pub status_detail: String,
    pub last_seen_secs: Option<u64>,
    pub threads: usize,
    pub version: String,
    pub batches_sent: u64,
    pub matches_completed: u64,
    pub failures: u64,
    pub last_latency_ms: Option<u64>,
    pub throughput_mps: Option<String>,
    pub last_error: Option<String>,
}

impl WorkersStatusDto {
    fn from_snapshot(snapshot: Vec<WorkerSnapshot>) -> Self {
        // Match the page's summary: ready/total count only remote workers,
        // total_threads adds the local in-process pool on top.
        let total = snapshot.len();
        let mut ready = 0usize;
        let mut total_threads = MatchRuntime::engine_pool().num_threads();
        let mut total_batches = 0u64;
        let mut total_matches = 0u64;
        let mut total_failures = 0u64;
        let mut workers = Vec::with_capacity(total);
        for w in snapshot {
            if matches!(w.status, WorkerStatus::Ready) {
                ready += 1;
            }
            total_threads += w.threads;
            total_batches = total_batches.saturating_add(w.stats.batches_sent);
            total_matches = total_matches.saturating_add(w.stats.matches_completed);
            total_failures = total_failures.saturating_add(w.stats.failures);
            workers.push(WorkerStatusDto::from_snapshot(w));
        }
        WorkersStatusDto {
            ready,
            total,
            total_threads,
            total_batches,
            total_matches,
            total_failures,
            workers,
        }
    }
}

impl WorkerStatusDto {
    fn from_snapshot(w: WorkerSnapshot) -> Self {
        let throughput_mps = w.throughput_mps().map(|v| format!("{:.1}", v));
        let status_detail = match &w.status {
            WorkerStatus::VersionMismatch { worker_version } => worker_version.clone(),
            WorkerStatus::Unreachable { reason } => reason.clone(),
            _ => String::new(),
        };
        WorkerStatusDto {
            status_label: w.status.label(),
            status_detail,
            last_seen_secs: w.last_seen_secs,
            address: w.address,
            threads: w.threads,
            version: w.version,
            batches_sent: w.stats.batches_sent,
            matches_completed: w.stats.matches_completed,
            failures: w.stats.failures,
            last_latency_ms: w.stats.last_latency_ms,
            throughput_mps,
            last_error: w.stats.last_error,
        }
    }
}

/// JSON snapshot of every worker's live status, polled by the workers page.
pub async fn workers_status_action(State(state): State<GameAppData>) -> impl IntoResponse {
    let snapshot = state.workers.snapshot().await;
    Json(WorkersStatusDto::from_snapshot(snapshot))
}

/// Body of the "remove worker" button POST — the address of the worker
/// row the operator clicked.
#[derive(Deserialize)]
pub struct RemoveWorkerRequest {
    pub address: String,
}

/// Outcome of a remove request: `removed` is `false` when no worker with
/// that address was registered (e.g. a double-click after a reload).
#[derive(Serialize)]
pub struct RemoveWorkerResponse {
    pub removed: bool,
}

/// Drop a worker from the registry by address. The local in-process slot
/// has no registry entry, so it can never be removed this way.
pub async fn workers_remove_action(
    State(state): State<GameAppData>,
    Json(body): Json<RemoveWorkerRequest>,
) -> impl IntoResponse {
    let removed = state.workers.remove_worker(body.address.trim()).await;
    Json(RemoveWorkerResponse { removed })
}
