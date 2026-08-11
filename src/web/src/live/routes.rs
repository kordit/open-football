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
use core::club::team::tactics::plan::{AttackingPlan, DefensivePlan, TacticalPlan};
use core::r#match::Match;
use core::r#match::engine::engine::live::MatchCommand;
use core::simulator::SimulatorData;
use core::{MatchTacticType, TacticSelectionReason, Tactics, Team, TeamType};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::runtime::Handle;
use tokio::task::spawn_blocking;

pub fn live_routes() -> Router<GameAppData> {
    Router::new()
        .route("/api/live/start", post(live_start_action))
        .route("/api/live/demo", post(live_demo_action))
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
        autosave_state.live.clear_if(finished_session.session_id());

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

// ── demo ───────────────────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
pub struct DemoRequest {
    /// Defaults to the managed club.
    pub home_team_id: Option<u32>,
    /// Defaults to the first other main team in the same league.
    pub away_team_id: Option<u32>,
    /// The plan the home side plays. Absent means the club's own coach
    /// picks, exactly as before this field existed.
    pub home_tactics: Option<TacticsRequest>,
    /// The plan the away side plays. Settable so the screen can stage a
    /// specific contest — a possession side against a high press is the
    /// whole reason to have a demo match — rather than only ever testing
    /// against whatever the opponent's coach fancied.
    pub away_tactics: Option<TacticsRequest>,
}

/// A manager's plan as it arrives over the wire.
///
/// Three ways to say the same thing, in order of increasing detail: name a
/// preset, name a preset and override some dials, or send all ten. The
/// panel uses the middle one — preset cards with sliders under them.
#[derive(Deserialize, Default, Clone)]
pub struct TacticsRequest {
    /// Formation key, e.g. `4-4-2`. Absent keeps the club's own shape.
    pub formation: Option<String>,
    /// Plan ofensywny, np. `counter`. Wypelnia piec pokretel z pilka.
    pub attack: Option<String>,
    /// Plan defensywny, np. `low_block`. Wypelnia piec pokretel bez pilki.
    pub defence: Option<String>,
    /// Per-dial overrides on top of the preset, 0.0–1.0. Anything absent
    /// keeps the preset's value.
    #[serde(default)]
    pub dials: HashMap<String, f32>,
}

impl TacticsRequest {
    /// Resolve into engine tactics, or explain what was wrong with it.
    ///
    /// `fallback` is the shape to keep when the request names no formation
    /// — the club's own, so "set me a low block" does not silently also
    /// reorganise the team into 4-4-2.
    fn resolve(&self, fallback: MatchTacticType) -> Result<Tactics, ApiError> {
        let shape = match self.formation.as_deref() {
            Some(name) => crate::lineup::parse_formation(name)?,
            None => fallback,
        };

        // Obie osie sa niezalezne: mozna podac sam atak, sama obrone albo
        // jedno i drugie. Brakujaca polowa idzie na neutralna.
        let attack = match self.attack.as_deref() {
            Some(key) => Some(
                AttackingPlan::from_key(key)
                    .ok_or_else(|| ApiError::BadRequest(format!("unknown attacking plan: {key}")))?,
            ),
            None => None,
        };

        let defence = match self.defence.as_deref() {
            Some(key) => Some(
                DefensivePlan::from_key(key)
                    .ok_or_else(|| ApiError::BadRequest(format!("unknown defensive plan: {key}")))?,
            ),
            None => None,
        };

        let preset = match (attack, defence) {
            (None, None) => None,
            (a, d) => Some(TacticalPlan::new(
                a.unwrap_or(AttackingPlan::Balanced),
                d.unwrap_or(DefensivePlan::MidBlock),
            )),
        };

        // No plan and no dials is not a plan — it is a request to leave
        // the coach alone, and it must not install a Balanced default that
        // overrides whatever the club would otherwise have done.
        if preset.is_none() && self.dials.is_empty() {
            return Ok(Tactics::with_reason(
                shape,
                TacticSelectionReason::CoachPreference,
                1.0,
            ));
        }

        let mut instructions = preset.unwrap_or_default().instructions();

        for (dial, value) in &self.dials {
            let slot = match dial.as_str() {
                "tempo" => &mut instructions.tempo,
                "directness" => &mut instructions.directness,
                "width" => &mut instructions.width,
                "risk" => &mut instructions.risk,
                "support" => &mut instructions.support,
                "press" => &mut instructions.press,
                "line_height" => &mut instructions.line_height,
                "compactness" => &mut instructions.compactness,
                "counter_press" => &mut instructions.counter_press,
                "aggression" => &mut instructions.aggression,
                other => {
                    return Err(ApiError::BadRequest(format!("unknown dial: {other}")));
                }
            };
            *slot = *value;
        }

        Ok(
            Tactics::with_reason(shape, TacticSelectionReason::CoachPreference, 1.0)
                .with_instructions(Some(instructions), preset),
        )
    }
}

#[derive(Serialize)]
pub struct DemoResponse {
    pub session_id: String,
    pub match_id: String,
    pub home_team_id: u32,
    pub away_team_id: u32,
    pub home_team_name: String,
    pub away_team_name: String,
}

/// `POST /api/live/demo` — a match to watch, played out of nothing.
///
/// Every other way into the 2D pitch runs through a career: a save, a calendar
/// on the right day, a fixture that only comes round once. That is a poor loop
/// to check a *drawing* against — you get one attempt per matchday, and a
/// mistake costs a season to reach again.
///
/// So this plays two real squads against each other with no fixture behind
/// them. Nothing is written: the world is read once to pick the elevens, and
/// the result is dropped at the final whistle. What it is not is a mock — it
/// is the same `LiveMatch`, the same recording, and the same five endpoints,
/// which is the entire point. A stub pitch fed by canned frames would confirm
/// the drawing and nothing else.
pub async fn live_demo_action(
    State(state): State<GameAppData>,
    request: Option<Json<DemoRequest>>,
) -> ApiResult<impl IntoResponse> {
    let Json(request) = request.unwrap_or_default();

    let world = {
        let guard = state.data.read().await;
        guard
            .as_ref()
            .map(Arc::clone)
            .ok_or_else(|| ApiError::InternalError("simulator data not loaded".to_string()))?
    };

    let home_id = request
        .home_team_id
        .or_else(|| world.player_manager.as_ref().map(|m| m.team_id))
        .ok_or_else(|| {
            ApiError::BadRequest("no managed club in this career — name a home_team_id".to_string())
        })?;

    let home = world
        .team(home_id)
        .ok_or_else(|| ApiError::NotFound(format!("no team {home_id}")))?;

    let away_id = match request.away_team_id {
        Some(id) => id,
        None => pick_opponent(&world, home).ok_or_else(|| {
            ApiError::BadRequest(format!(
                "found nobody for {} to play — name an away_team_id",
                home.name
            ))
        })?,
    };

    if away_id == home_id {
        return Err(ApiError::BadRequest(
            "a team cannot play itself".to_string(),
        ));
    }

    let away = world
        .team(away_id)
        .ok_or_else(|| ApiError::NotFound(format!("no team {away_id}")))?;

    // Checked here rather than left to the squad selector, which asserts its
    // way out of an unfillable eleven. In this world a thin club is normal —
    // no player is ever invented to pad one — so "that club cannot field a
    // team" is an answer the caller has to be given, not a panic.
    for team in [home, away] {
        let available = team.players.players().len();
        if available < 11 {
            return Err(ApiError::BadRequest(format!(
                "{} has {available} players — not enough for a match",
                team.name
            )));
        }
    }

    let league_id = home.league_id.or(away.league_id).unwrap_or(0);
    let league_slug = world
        .league(league_id)
        .map(|l| l.slug.clone())
        .unwrap_or_else(|| "demo".to_string());

    let (home_name, away_name) = (home.name.clone(), away.name.clone());
    let match_id = format!("demo_{home_id}_{away_id}");

    let session = LiveSession::new(format!("demo-{match_id}"), match_id.clone(), home_id);

    // A demo asked for while a demo is running replaces it. Refusing would be
    // pedantic: the match being replaced has no result, no calendar and no
    // consequences, and "start another one" is the whole point of the screen.
    //
    // A *fixture* is different — it is on the calendar, the league is waiting
    // for its result, and the matchday is parked inside it. That one is never
    // taken over, whatever it costs the person clicking.
    if let Some(running) = state.live.current() {
        if !running.is_done() {
            if !running.match_id().starts_with(DEMO_PREFIX) {
                return Err(ApiError::BadRequest(
                    "a live match is already in progress".to_string(),
                ));
            }

            take_over_from(running).await?;
        }
    }

    state
        .live
        .install(session.clone())
        .map_err(|_| ApiError::BadRequest("a live match is already in progress".to_string()))?;

    let date = world.date.date();
    let fixture_id = match_id.clone();
    let fixture_slug = league_slug.clone();

    // Squad selection walks both rosters and is not something to do on a
    // runtime worker. It also needs the world only for reading, which is why
    // no process lock is taken anywhere in this handler — a demo cannot
    // collide with a matchday because it never writes.
    // Plan managera rozstrzygamy przed wejsciem na watek roboczy, zeby blad
    // w nazwie planu wrocil jako 400, a nie zgubil sie w srodku selekcji.
    let shape_of = |team: &core::club::team::Team| {
        team.tactics
            .as_ref()
            .map(|t| t.tactic_type)
            .unwrap_or(MatchTacticType::T442)
    };

    let home_plan = match request.home_tactics.as_ref() {
        Some(req) => Some(req.resolve(shape_of(home))?),
        None => None,
    };
    let away_plan = match request.away_tactics.as_ref() {
        Some(req) => Some(req.resolve(shape_of(away))?),
        None => None,
    };

    let world_for_squads = Arc::clone(&world);
    let fixture = spawn_blocking(move || {
        let home = world_for_squads.team(home_id)?;
        let away = world_for_squads.team(away_id)?;

        let mut home_squad = home.get_rotation_match_squad_at(date);
        let mut away_squad = away.get_rotation_match_squad_at(date);

        // Plan managera zastepuje wybor trenera klubu — na obu polowach,
        // bo caly sens tego ekranu to zestawic dwa konkretne pomysly.
        if let Some(plan) = home_plan {
            home_squad.tactics = plan;
        }

        if let Some(plan) = away_plan {
            away_squad.tactics = plan;
        }

        let mut fixture = Match::make(
            fixture_id,
            league_id,
            &fixture_slug,
            home_squad,
            away_squad,
            false,
        );

        // Not a friendly, whatever it looks like: `is_friendly` switches off
        // cards and hard tackles in the engine, and a demo that cannot show a
        // foul is no use for checking that fouls are drawn.
        fixture.record = true;

        Some(fixture)
    })
    .await
    .map_err(|err| {
        state.live.clear_if(session.session_id());
        ApiError::InternalError(format!("demo squad selection failed: {err}"))
    })?
    .ok_or_else(|| {
        state.live.clear_if(session.session_id());
        ApiError::NotFound("a team went missing between two reads of the world".to_string())
    })?;

    let running = session.clone();
    let registry = state.live.clone();
    let slot = session.session_id().to_string();

    tokio::spawn(async move {
        if let Err(err) = spawn_blocking(move || crate::live::run_demo(running, fixture)).await {
            log::error!("demo match task failed: {err}");
        }

        // Only if nobody has taken the slot in the meantime — a demo replacing
        // a demo installs the newcomer as soon as the old one is done, which
        // is before this line runs.
        registry.clear_if(&slot);
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(DemoResponse {
            session_id: session.session_id().to_string(),
            match_id,
            home_team_id: home_id,
            away_team_id: away_id,
            home_team_name: home_name,
            away_team_name: away_name,
        }),
    ))
}

/// What a demo fixture's id starts with. The only marker distinguishing a
/// match nobody is waiting for from one the league needs a result from.
const DEMO_PREFIX: &str = "demo_";

/// End a running demo and wait until its slot is genuinely free.
///
/// The waiting is the point. `abandon` returns as soon as the simulation
/// thread has acknowledged it, but that thread still has to leave the message
/// loop, close the match out and mark the session done — and installing a new
/// session before it does gets refused for a match that is already over.
async fn take_over_from(running: LiveSession) -> Result<(), ApiError> {
    let watched = running.clone();

    spawn_blocking(move || {
        // A session the match never claimed cannot close itself; closing it
        // from outside is the difference between "try again in a moment" (for
        // ever) and a demo that simply starts.
        if !watched.abandon_or_close() {
            return;
        }

        // Bounded, because a wait that cannot end is worse than a refusal.
        // Closing out a match is microseconds; a hundred tries at a
        // millisecond is four orders of magnitude of headroom.
        for _ in 0..100 {
            if watched.is_done() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    })
    .await
    .map_err(|err| ApiError::InternalError(format!("could not end the running demo: {err}")))?;

    if !running.is_done() {
        return Err(ApiError::BadRequest(
            "the running demo would not stop — try again in a moment".to_string(),
        ));
    }

    Ok(())
}

/// Somebody in the same league with enough players to field an eleven.
fn pick_opponent(world: &SimulatorData, home: &Team) -> Option<u32> {
    let mut fallback = None;

    for continent in &world.continents {
        for country in &continent.countries {
            for club in &country.clubs {
                for team in &club.teams.teams {
                    if team.id == home.id
                        || team.team_type != TeamType::Main
                        || team.players.players().len() < 11
                    {
                        continue;
                    }

                    if team.league_id == home.league_id && home.league_id.is_some() {
                        return Some(team.id);
                    }

                    fallback.get_or_insert(team.id);
                }
            }
        }
    }

    fallback
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
    /// Floor on the gap between position samples in `frames`, in milliseconds
    /// of match time. Absent means ten frames a second, which is what a 2D
    /// pitch wants; a viewer doing slow motion can ask for less.
    pub step_ms: Option<u64>,
}

/// `POST /api/live/advance` — play a slice of match time.
pub async fn live_advance_action(
    State(state): State<GameAppData>,
    Json(request): Json<AdvanceRequest>,
) -> ApiResult<axum::response::Response> {
    let session = current(&state)?;

    let Some(outcome) =
        ask(move || session.advance(request.since_ms, request.until_ms, request.step_ms)).await?
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

    // Ten sam problem co przy przejmowaniu: mecz, ktory nigdy nie ruszyl, nie
    // ma jak uslyszec prosby. `abandoned: false` znaczy wtedy "zamknieta z
    // zewnatrz", a nie "nie udalo sie" — i tak czy siak slot jest wolny.
    let handed_over = ask(move || session.abandon_or_close()).await?;

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
