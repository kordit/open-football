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
};
use log::{info, warn};
use serde::Serialize;
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

// ───────────────────────────────────────────────────────────────────────────
// Messages to the simulation thread
// ───────────────────────────────────────────────────────────────────────────

enum Request {
    State(Sender<StateDto>),
    Advance {
        since_ms: u64,
        until_ms: u64,
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
}

#[derive(Serialize)]
pub struct AdvanceDto {
    #[serde(flatten)]
    pub state: StateDto,
    /// Where the engine's clock now is. The next request should carry this
    /// back as `since_ms`.
    pub cursor_ms: u64,
    pub ticks: u32,
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

impl LiveSession {
    pub fn new(session_id: String, match_id: String, team_id: u32) -> Self {
        LiveSession(Arc::new(SessionInner {
            session_id,
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

    pub fn advance(&self, since_ms: u64, until_ms: u64) -> Option<Result<AdvanceDto, Conflict>> {
        self.send(|reply| Request::Advance {
            since_ms,
            until_ms,
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
                reply,
            }) => {
                let cursor = m.clock_ms();

                if since_ms != cursor {
                    let _ = reply.send(Err(Conflict { cursor_ms: cursor }));
                    continue;
                }

                let outcome = m.advance_to(until_ms, StopPolicy::AtInterval);

                let _ = reply.send(Ok(AdvanceDto {
                    state: state_of(m),
                    cursor_ms: outcome.clock_ms,
                    ticks: outcome.ticks,
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
