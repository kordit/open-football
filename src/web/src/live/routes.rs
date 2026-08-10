//! Added in this fork: `/api/live/*` — the control plane of a match a manager
//! is playing.
//!
//! Five endpoints, all short. The long-running thing (the matchday) is started
//! by `start` and then runs on its own; everything after that is a question
//! answered between two ticks.

use crate::GameAppData;
use crate::error::{ApiError, ApiResult};
use crate::game::ProcessingRun;
use crate::live::{LiveInterceptor, LiveSession, StateDto, parse_instruction};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use core::r#match::engine::engine::live::MatchCommand;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::runtime::Handle;
use tokio::task::spawn_blocking;

pub fn live_routes() -> Router<GameAppData> {
    Router::new()
        .route("/api/live/start", post(live_start_action))
        .route("/api/live/state", get(live_state_action))
        .route("/api/live/advance", post(live_advance_action))
        .route("/api/live/command", post(live_command_action))
        .route("/api/live/abandon", post(live_abandon_action))
}

// ── start ──────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct StartRequest {
    /// Engine fixture id — `YYYY-MM-DD_{home_team_id}_{away_team_id}`.
    ///
    /// Supplied by the panel rather than worked out here: the projection
    /// already stores it as `game_matches.engine_match_id`, and re-deriving it
    /// would mean walking the world for a fixture the caller can already name.
    pub match_id: String,
    /// Days to simulate around it. One, unless something wants otherwise.
    pub days: Option<u32>,
}

#[derive(Serialize)]
pub struct StartResponse {
    pub session_id: String,
    pub match_id: String,
}

/// `POST /api/live/start` — take this fixture out of the day and hold it.
///
/// Returns as soon as the day is under way, not when the match is over. The
/// matchday runs on the blocking pool; when it reaches the claimed fixture the
/// simulation thread parks inside it and waits to be driven from here.
pub async fn live_start_action(
    State(state): State<GameAppData>,
    Json(request): Json<StartRequest>,
) -> ApiResult<impl IntoResponse> {
    // The managed club comes from the world, not from the request: which side
    // the human coaches is a property of the career, and letting a caller name
    // it would let them coach somebody else's team.
    let team_id = {
        let guard = state.data.read().await;
        guard
            .as_ref()
            .and_then(|world| world.player_manager.as_ref().map(|m| m.team_id))
            .ok_or_else(|| ApiError::BadRequest("no managed club in this career".to_string()))?
    };

    let session = LiveSession::new(
        format!("live-{}", request.match_id),
        request.match_id.clone(),
        team_id,
    );

    state
        .live
        .install(session.clone())
        .map_err(|_| ApiError::BadRequest("a live match is already in progress".to_string()))?;

    // The process lock is held for the whole matchday, which now includes the
    // time somebody spends watching. Anything else that wants to move the
    // world gets a clear refusal rather than a corrupted one.
    let process_guard = Arc::clone(&state.process_lock)
        .try_lock_owned()
        .map_err(|_| {
            state.live.clear();
            ApiError::BadRequest("game is busy (processing in progress)".to_string())
        })?;

    LiveInterceptor::install(session.clone());

    state.cancel_flag.store(false, Ordering::SeqCst);

    let data_arc = {
        let guard = state.data.read().await;
        Arc::clone(guard.as_ref().unwrap())
    };

    let run = ProcessingRun {
        handle: Handle::current(),
        data: Arc::clone(&state.data),
        i18n: Arc::clone(&state.i18n),
        cancel_flag: Arc::clone(&state.cancel_flag),
    };

    let days = request.days.unwrap_or(1);
    let autosave_state = state.clone();
    let finished_session = session.clone();

    // Not awaited — this is the whole reason `/api/live/*` exists separately
    // from `/api/game/process`. That endpoint holds the connection until the
    // day is done, and a day now lasts as long as a football match.
    tokio::spawn(async move {
        let joined = spawn_blocking(move || {
            let _guard = process_guard;
            run.execute(Arc::unwrap_or_clone(data_arc), days);
        })
        .await;

        if let Err(err) = joined {
            log::error!("live matchday task failed: {err}");
        }

        // The day is over either way, so the session is over. This also
        // catches the case where the fixture never turned up — a `match_id`
        // that is not on today's calendar is never claimed, and without this
        // the session would sit at `awaiting_kickoff` forever and refuse
        // every later start.
        core::r#match::MatchInterceptorRegistry::clear();
        finished_session.mark_done();
        autosave_state.live.clear();

        crate::game::saves::spawn_autosave(&autosave_state);
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(StartResponse {
            session_id: session.session_id().to_string(),
            match_id: session.match_id().to_string(),
        }),
    ))
}

// ── state ──────────────────────────────────────────────────────────────────

/// `GET /api/live/state` — clock, score, who is on the pitch.
pub async fn live_state_action(State(state): State<GameAppData>) -> ApiResult<Json<StateDto>> {
    let session = current(&state)?;

    Ok(Json(ask(move || session.state()).await?))
}

// ── advance ────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct AdvanceRequest {
    /// Where the caller believes the clock is. Guards against two tabs
    /// interleaving requests for overlapping windows.
    pub since_ms: u64,
    /// How far to play. The caller picks this from how fast it wants the
    /// match to run, which is why speed never becomes a server concern.
    pub until_ms: u64,
}

/// `POST /api/live/advance` — play a slice of match time.
pub async fn live_advance_action(
    State(state): State<GameAppData>,
    Json(request): Json<AdvanceRequest>,
) -> ApiResult<axum::response::Response> {
    let session = current(&state)?;

    let Some(outcome) = ask(move || session.advance(request.since_ms, request.until_ms)).await?
    else {
        return Err(ApiError::BadRequest(
            "the match has not kicked off yet".to_string(),
        ));
    };

    Ok(match outcome {
        Ok(dto) => Json(dto).into_response(),
        // 409, with the real cursor, so the loser of the race corrects itself
        // instead of silently replaying football the other tab already saw.
        Err(conflict) => (
            StatusCode::CONFLICT,
            Json(json!({
                "error": "cursor mismatch",
                "cursor_ms": conflict.cursor_ms,
            })),
        )
            .into_response(),
    })
}

// ── command ────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommandRequest {
    Substitution {
        out: u32,
        r#in: u32,
    },
    Instruction {
        value: String,
    },
    ReleaseInstruction,
    /// Leave half time and start the next period.
    Resume,
}

/// `POST /api/live/command` — a substitution, an instruction, or the whistle
/// for the second half.
pub async fn live_command_action(
    State(state): State<GameAppData>,
    Json(request): Json<CommandRequest>,
) -> ApiResult<Json<StateDto>> {
    let session = current(&state)?;

    let command = match request {
        CommandRequest::Resume => {
            return ask(move || session.resume())
                .await?
                .map(Json)
                .ok_or_else(|| {
                    ApiError::BadRequest("the match has not kicked off yet".to_string())
                });
        }
        CommandRequest::Substitution { out, r#in } => MatchCommand::Substitution { out, r#in },
        CommandRequest::ReleaseInstruction => MatchCommand::ReleaseInstruction,
        CommandRequest::Instruction { value } => {
            let parsed = parse_instruction(&value)
                .ok_or_else(|| ApiError::BadRequest(format!("unknown instruction: {value}")))?;
            MatchCommand::Instruction(parsed)
        }
    };

    match ask(move || session.command(command)).await? {
        None => Err(ApiError::BadRequest(
            "the match has not kicked off yet".to_string(),
        )),
        Some(Err(reason)) => Err(ApiError::BadRequest(reason)),
        Some(Ok(dto)) => Ok(Json(dto)),
    }
}

// ── abandon ────────────────────────────────────────────────────────────────

/// `POST /api/live/abandon` — hand the rest of the match to the assistant.
///
/// There is no un-playing a fixture. Abandoning means the match is finished
/// without the manager, the day folds up, and the result stands.
pub async fn live_abandon_action(
    State(state): State<GameAppData>,
) -> ApiResult<Json<serde_json::Value>> {
    let session = current(&state)?;

    let match_id = session.match_id().to_string();
    let handed_over = ask(move || session.abandon()).await?;

    Ok(Json(json!({
        "abandoned": handed_over,
        "match_id": match_id,
    })))
}

// ── shared ─────────────────────────────────────────────────────────────────

/// Talk to the simulation thread without blocking a tokio worker.
///
/// Every question here ends in a channel `recv()` that parks until the match
/// answers between two ticks. That is microseconds in the normal case and a
/// full slice of football in the worst one — either way it is a blocking wait,
/// and blocking waits do not belong on the runtime's worker threads.
async fn ask<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, ApiError> {
    spawn_blocking(f)
        .await
        .map_err(|err| ApiError::InternalError(format!("live session task failed: {err}")))
}

fn current(state: &GameAppData) -> Result<LiveSession, ApiError> {
    state
        .live
        .current()
        .ok_or_else(|| ApiError::NotFound("no live match in progress".to_string()))
}
