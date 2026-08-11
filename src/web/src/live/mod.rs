//! Added in this fork: the match the manager plays themselves.
//!
//! Upstream has no such thing — a fixture is simulated by the league and the
//! result appears. Here one fixture per matchday can be claimed out of the
//! batch (`core::MatchInterceptor`) and driven from the panel, while the rest
//! of the day is simulated alongside it.
//!
//! ## Why this is not `POST /api/game/process`
//!
//! That endpoint holds the process lock and an open connection until the
//! whole day is simulated. A match played at real speed is ~5400 seconds; the
//! panel's HTTP client gives up at 900. So a live match cannot be *one*
//! request. It is a session plus a stream of short ones.
//!
//! ## Pull, not push
//!
//! The panel asks for the next slice of match time; the engine never pushes.
//! Three things fall out of that and all three matter:
//!
//! * **Pause is free.** Stop asking and the engine sits at zero CPU. There is
//!   no paused-state to model, because not being asked *is* the pause.
//! * **Speed is the client's business.** Watching at 8× is asking for a wider
//!   window, not asking eight times as often.
//! * **A lost response costs one repeat.** The cursor is in milliseconds and
//!   the request carries where the caller thinks it is, so a dropped reply is
//!   re-asked rather than reconciled.
//!
//! ## Who owns the match
//!
//! The `LiveMatch` lives on the simulation thread — the one blocked inside
//! `MatchPlayEnginePool::play`, waiting for this fixture's result. HTTP
//! handlers never touch it; they send it messages and wait for a reply. That
//! is what makes "apply a substitution" land between two ticks rather than
//! inside one.
//!
//! ## The watchdog is on that same thread
//!
//! Deliberately, and it is the reason `recv_timeout` is used instead of
//! `recv`. A watchdog living in its own task can die and leave the simulation
//! thread — and with it the process lock and the whole matchday — blocked
//! forever. Here, the thing that would hang is the thing that times out.

pub mod routes;

use core::r#match::engine::engine::live::{LiveMatch, LivePhase, MatchCommand, StopPolicy};
use core::r#match::{
    CoachInstruction, Match, MatchInterceptor, MatchInterceptorRegistry, MatchResult, MatchState,
    PlayerSide, PositionWindow,
};
use log::{info, warn};
use serde::Serialize;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long the engine waits to be asked for more match before deciding the
/// manager has gone.
///
/// Two minutes is chosen against the panel's own cadence (~1 Hz): it is far
/// longer than any hiccup, and short enough that a closed tab does not hold a
/// matchday open while somebody waits for the league table.
const IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// Absolute ceiling on a live session, independent of whether anyone is
/// asking. A client that polls forever without advancing the clock must not
/// be able to hold the world open indefinitely.
const SESSION_CEILING: Duration = Duration::from_secs(3 * 60 * 60);

/// The pitch the engine plays on, in its own units. `FootballEngine<840, 545>`
/// is instantiated in half a dozen places; the live DTO reports it so the
/// viewer never has to know that number independently.
const FIELD_WIDTH: u32 = 840;
const FIELD_HEIGHT: u32 = 545;

/// Default floor on the gap between position samples handed to the viewer.
///
/// The engine records every 30 ms. Ten frames of simulated football per
/// second is smooth on screen and an eighth of the bytes; the client can ask
/// for finer if it ever wants slow motion.
const DEFAULT_FRAME_STEP_MS: u64 = 100;

// ───────────────────────────────────────────────────────────────────────────
// Messages to the simulation thread
// ───────────────────────────────────────────────────────────────────────────

enum Request {
    State(Sender<StateDto>),
    Advance {
        since_ms: u64,
        until_ms: u64,
        step_ms: u64,
        reply: Sender<Result<AdvanceDto, Conflict>>,
    },
    Command {
        command: MatchCommand,
        reply: Sender<Result<StateDto, String>>,
    },
    Resume(Sender<StateDto>),
    Abandon(Sender<()>),
}

/// The caller's cursor disagreed with the engine's.
///
/// Two panel tabs on the same match produce exactly this, and answering with
/// the real cursor lets both of them correct themselves instead of silently
/// interleaving requests for overlapping windows.
pub struct Conflict {
    pub cursor_ms: u64,
}

// ───────────────────────────────────────────────────────────────────────────
// What the panel sees
// ───────────────────────────────────────────────────────────────────────────

#[derive(Serialize, Clone)]
pub struct PlayerDto {
    pub id: u32,
    pub team_id: u32,
    pub condition: i16,
    pub sent_off: bool,
    pub goals: u16,
    pub minutes: u16,
    /// `left` | `right` — the half this player defends right now, or absent
    /// on the bench. The sides swap at half time, so the 2D pitch reads this
    /// every window instead of assuming home-on-the-left.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub side: Option<&'static str>,
    /// Position played today, engine-side: `GK`, `DL`, `MC`, `ST`, …
    pub position: &'static str,
}

#[derive(Serialize, Clone)]
pub struct StateDto {
    /// `awaiting_kickoff` | `playing` | `interval` | `finished`
    pub status: &'static str,
    /// The period being played, or the one waiting behind an interval.
    pub period: Option<&'static str>,
    pub clock_ms: u64,
    pub minute: u32,
    pub home_team_id: u32,
    pub away_team_id: u32,
    pub home_goals: u8,
    pub away_goals: u8,
    pub on_pitch: Vec<PlayerDto>,
    pub bench: Vec<PlayerDto>,
    pub subs_used: usize,
    pub subs_allowed: usize,
    pub instruction: String,
    pub instruction_is_manual: bool,
    /// Pitch dimensions in engine units. Every coordinate in `frames` is on
    /// this grid, so the viewer scales against it rather than hard-coding a
    /// size that only this engine build happens to use.
    pub field_width: u32,
    pub field_height: u32,
}

#[derive(Serialize)]
pub struct AdvanceDto {
    #[serde(flatten)]
    pub state: StateDto,
    /// Where the engine's clock now is. The next request should carry this
    /// back as `since_ms`.
    pub cursor_ms: u64,
    pub ticks: u32,
    /// Everything that happened in the slice just played: where the ball and
    /// every player were, which passes were made, which events fired.
    ///
    /// This rides along with `advance` rather than living at its own endpoint
    /// because the window is exactly the one just simulated — a separate
    /// request would need the same two cursors and could only ever return the
    /// same answer, at the cost of a second round trip per frame batch.
    pub frames: PositionWindow,
    /// Goals, fouls and cards awarded inside the same window, each stamped
    /// with the minute it happened on.
    ///
    /// The score in `state` is the score *now*; these carry their own clock.
    /// The difference matters as soon as anybody watches above 1×: a window
    /// twelve seconds wide would otherwise put the goal on the scoreboard
    /// twelve seconds before the viewer sees the shot.
    pub incidents: Vec<IncidentDto>,
}

#[derive(Serialize)]
pub struct IncidentDto {
    pub at_ms: u64,
    pub minute: u32,
    pub kind: &'static str,
    pub player_id: u32,
    pub team_id: u32,
}

fn describe_phase(phase: LivePhase) -> (&'static str, Option<&'static str>) {
    use MatchState::*;

    let name = |s: MatchState| -> &'static str {
        match s {
            Initial => "initial",
            FirstHalf => "first_half",
            HalfTime => "half_time",
            SecondHalf => "second_half",
            ExtraTime => "extra_time",
            PenaltyShootout => "penalty_shootout",
            End => "end",
        }
    };

    match phase {
        LivePhase::Pending => ("awaiting_kickoff", None),
        LivePhase::Playing(s) => ("playing", Some(name(s))),
        LivePhase::Interval(s) => ("interval", Some(name(s))),
        LivePhase::Finished => ("finished", None),
    }
}

fn instruction_name(instruction: CoachInstruction) -> String {
    format!("{instruction:?}")
}

pub fn parse_instruction(raw: &str) -> Option<CoachInstruction> {
    match raw {
        "Normal" | "normal" => Some(CoachInstruction::Normal),
        "SlowDown" | "slow_down" => Some(CoachInstruction::SlowDown),
        "PushForward" | "push_forward" => Some(CoachInstruction::PushForward),
        "AllOutAttack" | "all_out_attack" => Some(CoachInstruction::AllOutAttack),
        "WasteTime" | "waste_time" => Some(CoachInstruction::WasteTime),
        "ParkTheBus" | "park_the_bus" => Some(CoachInstruction::ParkTheBus),
        _ => None,
    }
}

fn state_of(m: &LiveMatch) -> StateDto {
    let snap = m.snapshot();
    let (status, period) = describe_phase(snap.phase);

    let describe = |p: &core::r#match::engine::engine::live::LivePlayer| PlayerDto {
        id: p.id,
        team_id: p.team_id,
        condition: p.condition,
        sent_off: p.is_sent_off,
        goals: p.goals,
        minutes: p.minutes,
        side: p.side.map(|s| match s {
            PlayerSide::Left => "left",
            PlayerSide::Right => "right",
        }),
        position: p.position,
    };

    StateDto {
        status,
        period,
        clock_ms: snap.clock_ms,
        minute: snap.minute,
        home_team_id: snap.home_team_id,
        away_team_id: snap.away_team_id,
        home_goals: snap.home_goals,
        away_goals: snap.away_goals,
        on_pitch: snap.on_pitch.iter().map(describe).collect(),
        bench: snap.bench.iter().map(describe).collect(),
        subs_used: snap.subs_used,
        subs_allowed: snap.subs_allowed,
        instruction: instruction_name(snap.instruction),
        instruction_is_manual: snap.instruction_is_manual,
        field_width: FIELD_WIDTH,
        field_height: FIELD_HEIGHT,
    }
}

/// The state to report before the simulation thread has reached the fixture.
fn awaiting_kickoff() -> StateDto {
    StateDto {
        status: "awaiting_kickoff",
        period: None,
        clock_ms: 0,
        minute: 0,
        home_team_id: 0,
        away_team_id: 0,
        home_goals: 0,
        away_goals: 0,
        on_pitch: Vec::new(),
        bench: Vec::new(),
        subs_used: 0,
        subs_allowed: 0,
        instruction: instruction_name(CoachInstruction::Normal),
        instruction_is_manual: false,
        field_width: FIELD_WIDTH,
        field_height: FIELD_HEIGHT,
    }
}

// ───────────────────────────────────────────────────────────────────────────
// The session
// ───────────────────────────────────────────────────────────────────────────

struct SessionInner {
    session_id: String,
    match_id: String,
    team_id: u32,
    /// Present only while the simulation thread is inside the fixture.
    /// Absent before kickoff and after the final whistle.
    tx: Mutex<Option<Sender<Request>>>,
    /// Set once the match is over, so the panel can stop asking.
    done: Mutex<bool>,
}

/// Handle shared between the HTTP handlers and the simulation thread.
#[derive(Clone)]
pub struct LiveSession(Arc<SessionInner>);

/// Serial number handed to every session so two of them are never confused.
///
/// The caller's id describes the *fixture* — `demo-demo_A_B`, `live-2025-09-22_A_B`
/// — and the same fixture can be played twice in one process. That is not
/// hypothetical: replaying the same demo is the normal way to use that screen.
/// Without a serial, the tidy-up from the first run matches the second run's
/// id and clears a slot it does not own, and the symptom is a match that
/// starts and immediately reports that it does not exist.
static SESSION_SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl LiveSession {
    pub fn new(session_id: String, match_id: String, team_id: u32) -> Self {
        let serial = SESSION_SERIAL.fetch_add(1, Ordering::Relaxed);

        LiveSession(Arc::new(SessionInner {
            session_id: format!("{session_id}#{serial}"),
            match_id,
            team_id,
            tx: Mutex::new(None),
            done: Mutex::new(false),
        }))
    }

    pub fn session_id(&self) -> &str {
        &self.0.session_id
    }

    pub fn match_id(&self) -> &str {
        &self.0.match_id
    }

    pub fn is_done(&self) -> bool {
        *self.0.done.lock().unwrap()
    }

    /// Close the session from outside the match.
    ///
    /// Needed because the fixture may never arrive: a `match_id` that is not
    /// on today's calendar is simply never claimed, the matchday finishes
    /// normally, and without this the session would sit at `awaiting_kickoff`
    /// forever and refuse every later start.
    pub fn mark_done(&self) {
        *self.0.done.lock().unwrap() = true;
    }

    fn send<T>(&self, make: impl FnOnce(Sender<T>) -> Request) -> Option<T> {
        let (reply_tx, reply_rx) = channel();

        let tx = self.0.tx.lock().unwrap().clone()?;
        tx.send(make(reply_tx)).ok()?;

        // No timeout: the simulation thread answers a request between two
        // ticks, and a tick is ten milliseconds of simulated football. If it
        // ever stops answering, the process has bigger problems than this
        // request.
        reply_rx.recv().ok()
    }

    pub fn state(&self) -> StateDto {
        if self.is_done() {
            let mut s = awaiting_kickoff();
            s.status = "finished";
            return s;
        }

        self.send(Request::State).unwrap_or_else(awaiting_kickoff)
    }

    pub fn advance(
        &self,
        since_ms: u64,
        until_ms: u64,
        step_ms: Option<u64>,
    ) -> Option<Result<AdvanceDto, Conflict>> {
        self.send(|reply| Request::Advance {
            since_ms,
            until_ms,
            step_ms: step_ms.unwrap_or(DEFAULT_FRAME_STEP_MS),
            reply,
        })
    }

    pub fn command(&self, command: MatchCommand) -> Option<Result<StateDto, String>> {
        self.send(|reply| Request::Command { command, reply })
    }

    pub fn resume(&self) -> Option<StateDto> {
        self.send(Request::Resume)
    }

    pub fn abandon(&self) -> bool {
        self.send(Request::Abandon).is_some()
    }

    /// Close the session whatever state it is in, and say whether the match
    /// itself heard about it.
    ///
    /// `abandon` only reaches a match that already has a message loop. A demo
    /// that was started and never stepped has none — its sender is still
    /// `None` — so the request goes nowhere, `is_done` never flips, and the
    /// session sits at `awaiting_kickoff` refusing every later start. That is
    /// what `mark_done` is for; this is the one place that knows to use it.
    pub fn abandon_or_close(&self) -> bool {
        if self.abandon() {
            return true;
        }

        self.mark_done();

        false
    }
}

/// Registry of the one live session a process can have.
///
/// One, not many: the engine holds a single world per process, and a live
/// match is a fixture inside that world's matchday.
#[derive(Clone, Default)]
pub struct LiveRegistry(Arc<Mutex<Option<LiveSession>>>);

impl LiveRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Claim the slot. Fails if a session is already running — the caller
    /// turns that into a 409.
    pub fn install(&self, session: LiveSession) -> Result<(), ()> {
        let mut slot = self.0.lock().unwrap();

        if slot.as_ref().is_some_and(|s| !s.is_done()) {
            return Err(());
        }

        *slot = Some(session);
        Ok(())
    }

    pub fn current(&self) -> Option<LiveSession> {
        self.0.lock().unwrap().clone()
    }

    pub fn clear(&self) {
        *self.0.lock().unwrap() = None;
    }

    /// Clear the slot only if it still holds this session.
    ///
    /// Every match tidies up after itself when it ends, and that tidying runs
    /// *after* the session is marked done — which is exactly the moment the
    /// next match is allowed to move in. An unconditional `clear` there wipes
    /// whoever took the slot in between, and the symptom is a match that
    /// starts fine and then reports that it does not exist.
    pub fn clear_if(&self, session_id: &str) {
        let mut slot = self.0.lock().unwrap();

        if slot.as_ref().is_some_and(|s| s.session_id() == session_id) {
            *slot = None;
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// The interceptor — runs on the simulation thread
// ───────────────────────────────────────────────────────────────────────────

pub struct LiveInterceptor {
    session: LiveSession,
}

impl LiveInterceptor {
    pub fn new(session: LiveSession) -> Self {
        LiveInterceptor { session }
    }

    /// Install for one fixture and arrange for removal when it is over.
    pub fn install(session: LiveSession) {
        MatchInterceptorRegistry::set(Arc::new(LiveInterceptor::new(session)));
    }
}

impl MatchInterceptor for LiveInterceptor {
    fn claims(&self, match_id: &str) -> bool {
        match_id == self.session.match_id()
    }

    fn play(&self, fixture: Match) -> MatchResult {
        let (tx, rx) = channel::<Request>();
        *self.session.0.tx.lock().unwrap() = Some(tx);

        info!(
            "live match {} claimed by session {}",
            self.session.match_id(),
            self.session.session_id()
        );

        let mut m = LiveMatch::start(fixture, self.session.0.team_id);
        // Kick off so the first `state` call has a pitch to describe rather
        // than an empty shell.
        m.advance_to(0, StopPolicy::AtInterval);

        let result = drive(&mut m, rx);

        *self.session.0.tx.lock().unwrap() = None;
        *self.session.0.done.lock().unwrap() = true;
        MatchInterceptorRegistry::clear();

        info!(
            "live match {} finished ({})",
            self.session.match_id(),
            match result {
                Ended::ByWhistle => "final whistle",
                Ended::Abandoned => "abandoned",
                Ended::Idle => "manager stopped asking",
                Ended::Ceiling => "session ceiling",
            }
        );

        m.finish()
    }
}

/// Added in this fork: a live match that is not on anybody's calendar.
///
/// Same `LiveMatch`, same message loop, same DTOs as a real fixture — the only
/// difference is what happens to the result, which is nothing. There is no
/// matchday around it, no process lock, no interceptor, and the world is only
/// read (once, to build the two squads) and never written.
///
/// It exists because the 2D pitch is the hardest part of this screen to get
/// right and the slowest to reach: a real fixture needs a career, a calendar
/// wound to the right day, and a matchday that will not come round again if
/// the drawing was wrong. A demo is the same football on demand, so the view
/// can be checked against passes, fouls and cards in a minute rather than a
/// season — and checked against the *real* code path, which a canned replay
/// file would not be.
pub fn run_demo(session: LiveSession, fixture: Match) {
    let (tx, rx) = channel::<Request>();
    *session.0.tx.lock().unwrap() = Some(tx);

    info!(
        "demo match {} running under session {}",
        session.match_id(),
        session.session_id()
    );

    let mut m = LiveMatch::start(fixture, session.0.team_id);
    m.advance_to(0, StopPolicy::AtInterval);

    let ended = drive(&mut m, rx);

    *session.0.tx.lock().unwrap() = None;
    *session.0.done.lock().unwrap() = true;

    // Built and dropped. `finish` is still called rather than skipped: it is
    // the only thing that consumes the field, and running it here means the
    // demo exercises the same close-out path a real match takes instead of a
    // shortcut that could hide a panic in it.
    let _discarded = m.finish();

    info!(
        "demo match {} over ({})",
        session.match_id(),
        match ended {
            Ended::ByWhistle => "final whistle",
            Ended::Abandoned => "abandoned",
            Ended::Idle => "nobody watching",
            Ended::Ceiling => "session ceiling",
        }
    );
}

enum Ended {
    ByWhistle,
    Abandoned,
    Idle,
    Ceiling,
}

/// The message loop. Returns when the match is over, one way or another.
///
/// Every exit finishes the match rather than leaving it half-played: there is
/// no such thing as un-playing a fixture, and the league is waiting for a
/// result either way.
fn drive(m: &mut LiveMatch, rx: Receiver<Request>) -> Ended {
    let started = std::time::Instant::now();

    loop {
        if m.phase() == LivePhase::Finished {
            return Ended::ByWhistle;
        }

        if started.elapsed() > SESSION_CEILING {
            warn!("live match hit the session ceiling — finishing without the manager");
            m.advance_to(u64::MAX, StopPolicy::RunThrough);
            return Ended::Ceiling;
        }

        match rx.recv_timeout(IDLE_TIMEOUT) {
            Ok(Request::State(reply)) => {
                let _ = reply.send(state_of(m));
            }

            Ok(Request::Advance {
                since_ms,
                until_ms,
                step_ms,
                reply,
            }) => {
                let cursor = m.clock_ms();

                if since_ms != cursor {
                    let _ = reply.send(Err(Conflict { cursor_ms: cursor }));
                    continue;
                }

                let outcome = m.advance_to(until_ms, StopPolicy::AtInterval);

                // The window is bounded by where the clock actually got to,
                // not by what was asked for. An interval stops the match
                // early, and asking the recording for football past that
                // point would hand the viewer an empty tail it then has to
                // guess the meaning of.
                let until = outcome.clock_ms + 1;
                let frames = m.recording().window(since_ms, until, step_ms);
                let incidents = m
                    .incidents(since_ms, until)
                    .into_iter()
                    .map(|i| IncidentDto {
                        at_ms: i.at_ms,
                        minute: (i.at_ms / 60_000) as u32,
                        kind: i.kind,
                        player_id: i.player_id,
                        team_id: i.team_id,
                    })
                    .collect();

                let _ = reply.send(Ok(AdvanceDto {
                    state: state_of(m),
                    cursor_ms: outcome.clock_ms,
                    ticks: outcome.ticks,
                    frames,
                    incidents,
                }));
            }

            Ok(Request::Command { command, reply }) => {
                let answer = match m.apply(command) {
                    Ok(()) => Ok(state_of(m)),
                    Err(err) => Err(format!("{err:?}")),
                };
                let _ = reply.send(answer);
            }

            Ok(Request::Resume(reply)) => {
                m.resume();
                let _ = reply.send(state_of(m));
            }

            Ok(Request::Abandon(reply)) => {
                m.advance_to(u64::MAX, StopPolicy::RunThrough);
                let _ = reply.send(());
                return Ended::Abandoned;
            }

            // Nobody is asking any more. The tab is closed, or the network
            // went. Either way the fixture is on the calendar and the table
            // needs it, so the assistant finishes the job.
            Err(RecvTimeoutError::Timeout) => {
                warn!("no request for {IDLE_TIMEOUT:?} — finishing the match without the manager");
                m.advance_to(u64::MAX, StopPolicy::RunThrough);
                return Ended::Idle;
            }

            // Every sender dropped: the session handle is gone.
            Err(RecvTimeoutError::Disconnected) => {
                m.advance_to(u64::MAX, StopPolicy::RunThrough);
                return Ended::Idle;
            }
        }
    }
}
