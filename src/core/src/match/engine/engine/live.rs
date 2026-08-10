//! Added by the fork: a match somebody is watching.
//!
//! [`LiveMatch`] owns the four pieces of state a match is made of and walks
//! them with the stepper from `stepper.rs`. It is the same football the league
//! simulates — the outer state loop here mirrors `play_with_config`'s, and the
//! inner one is `step_period_tick`. What it adds is the ability to stop.
//!
//! Three things about it are worth knowing before changing anything:
//!
//! * **It does not survive a restart.** `MatchPlayer` and friends derive
//!   `Debug, Clone` and nothing else; `MatchRng` wraps a `RefCell<StdRng>`
//!   with no serde. Serialising a paused match would mean rewriting ~40
//!   structs. So a live match lives in memory for as long as the process
//!   does, and the manager has to be told that in plain words rather than
//!   discovering it.
//! * **Commands land between ticks, never inside one.** [`LiveMatch::apply`]
//!   runs while no tick is in progress — `advance_to` has already returned —
//!   which is what makes a substitution in the 60th minute deterministic
//!   rather than a coin flip on where in the tick body it happened to land.
//! * **Half time is a real state that costs one tick.** `increment_time`
//!   returns false for it immediately, but not before advancing the clock.
//!   The interval the manager sees therefore sits *before* the state runs,
//!   not instead of it.

use super::stepper::{PeriodLoop, TickOutcome};
use super::*;
use crate::r#match::engine::context::MatchEngineConfig;
use crate::r#match::engine::substitution::substitutions::{
    SubstitutionError, execute_manual_substitution,
};
use crate::r#match::game::Match;
use crate::r#match::{MatchResult, PlayMatchStateResult};

type Engine = FootballEngine<840, 545>;

/// Where the match currently is.
///
/// `PartialEq` only — `MatchState` and `CoachInstruction` come from upstream
/// without `Eq`, and widening their derives is a bigger change than this
/// module is entitled to make.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LivePhase {
    /// Built, not kicked off.
    Pending,
    /// Ticking through the named period.
    Playing(MatchState),
    /// Stopped at a break, holding the period that runs when play resumes.
    Interval(MatchState),
    /// Full time. Only `finish` is left.
    Finished,
}

/// What to do when the match reaches a break.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopPolicy {
    /// Hand control back at half time and before extra time / penalties.
    /// This is what a manager watching the match gets.
    AtInterval,
    /// Play straight through every break. Used when the manager walked away
    /// and the match is being finished off without them.
    RunThrough,
}

/// Something the manager did.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MatchCommand {
    Substitution {
        out: u32,
        r#in: u32,
    },
    Instruction(CoachInstruction),
    /// Give the instruction back to the assistant.
    ReleaseInstruction,
}

/// Why a command was refused.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CommandError {
    /// The match has not kicked off, or is already over.
    NotInPlay,
    /// The substitution itself was rejected.
    Substitution(SubstitutionError),
}

/// What one call to [`LiveMatch::advance_to`] did.
#[derive(Debug, Clone, Copy)]
pub struct AdvanceOutcome {
    pub phase: LivePhase,
    /// Match clock after the call, in milliseconds.
    pub clock_ms: u64,
    /// Ticks actually played. Zero is normal — it means the call arrived at
    /// a break, or the requested horizon was already behind the clock.
    pub ticks: u32,
}

/// A player as the live screen needs to show them.
#[derive(Debug, Clone)]
pub struct LivePlayer {
    pub id: u32,
    pub team_id: u32,
    /// 0..10000, the engine's own scale.
    pub condition: i16,
    pub is_sent_off: bool,
    pub goals: u16,
    pub minutes: u16,
}

/// Everything the panel draws between two steps.
#[derive(Debug, Clone)]
pub struct LiveSnapshot {
    pub phase: LivePhase,
    pub clock_ms: u64,
    pub minute: u32,
    pub home_team_id: u32,
    pub away_team_id: u32,
    pub home_goals: u8,
    pub away_goals: u8,
    /// On the pitch right now.
    pub on_pitch: Vec<LivePlayer>,
    /// Available to come on.
    pub bench: Vec<LivePlayer>,
    /// Substitutions the human side has already used, and its cap.
    pub subs_used: usize,
    pub subs_allowed: usize,
    pub instruction: CoachInstruction,
    /// Whether the instruction above is the manager's or the assistant's.
    pub instruction_is_manual: bool,
}

pub struct LiveMatch {
    field: MatchField,
    context: MatchContext,
    match_data: ResultMatchPositionData,
    states: StateManager,
    period: Option<PeriodLoop>,
    phase: LivePhase,

    human_team_id: u32,
    /// Every command with the clock reading it landed on.
    ///
    /// Nothing reads this yet. It is here because a live match cannot be
    /// serialised, so if one ever has to be reconstructed — after a crash,
    /// or to explain a result somebody disputes — the command log plus the
    /// seed is the only description of what the human contributed.
    command_log: Vec<(u64, MatchCommand)>,

    id: String,
    league_id: u32,
    league_slug: String,
    home_team_id: u32,
    away_team_id: u32,
    is_friendly: bool,
}

impl LiveMatch {
    /// Build a match ready to kick off, with `human_team_id` under the
    /// manager's control.
    ///
    /// Note on reproducibility: `Match::seed` pins `MatchRng` only. Player AI
    /// also draws from the process-global stream in `utils::random::engine`,
    /// which this does **not** touch — see the fork's known defects. It means
    /// a live match is not the match the batch simulator would have produced
    /// from the same fixture. Since a live match is taken *out* of the batch
    /// rather than run alongside it, there is no second result to disagree
    /// with; the day this changes, pin both here.
    pub fn start(m: Match, human_team_id: u32) -> Self {
        let id = m.id().to_string();
        let league_id = m.league_id();
        let league_slug = m.league_slug().to_string();
        let home_team_id = m.home_squad.team_id;
        let away_team_id = m.away_squad.team_id;
        let is_friendly = m.is_friendly;

        // Same recording rule as `Match::play`: a fixture the manager is
        // watching is exactly the fixture worth having a replay of.
        let match_recordings = (MatchRuntime::recordings_mode() || m.record) && !is_friendly;

        let config = MatchEngineConfig {
            seed: m.seed,
            match_recordings,
            is_friendly,
            is_knockout: m.is_knockout,
            ..Default::default()
        };

        let (field, context, match_data, states) =
            Engine::setup(m.home_squad, m.away_squad, &config);

        LiveMatch {
            field,
            context,
            match_data,
            states,
            period: None,
            phase: LivePhase::Pending,
            human_team_id,
            command_log: Vec::new(),
            id,
            league_id,
            league_slug,
            home_team_id,
            away_team_id,
            is_friendly,
        }
    }

    pub fn phase(&self) -> LivePhase {
        self.phase
    }

    pub fn clock_ms(&self) -> u64 {
        self.context.total_match_time
    }

    pub fn human_team_id(&self) -> u32 {
        self.human_team_id
    }

    pub fn command_log(&self) -> &[(u64, MatchCommand)] {
        &self.command_log
    }

    /// Play until the clock reaches `until_ms`, or until the match stops on
    /// its own.
    ///
    /// Returns having played zero or more ticks; both are normal. The caller
    /// decides the horizon, which is how a 90-minute match becomes a sequence
    /// of short requests instead of one that holds a connection open for the
    /// length of a simulation.
    pub fn advance_to(&mut self, until_ms: u64, stop: StopPolicy) -> AdvanceOutcome {
        let mut ticks: u32 = 0;

        loop {
            match self.phase {
                LivePhase::Finished => break,

                LivePhase::Interval(_) if stop == StopPolicy::AtInterval => break,
                LivePhase::Interval(state) => self.begin(state),

                LivePhase::Pending => self.next_period(),

                LivePhase::Playing(_) => {
                    if self.context.total_match_time >= until_ms {
                        break;
                    }

                    let Some(period) = self.period.as_mut() else {
                        // Playing without a period loop cannot happen — but
                        // spinning forever if it ever did would be worse than
                        // stopping.
                        self.phase = LivePhase::Finished;
                        break;
                    };

                    let outcome = Engine::step_period_tick(
                        &mut self.field,
                        &mut self.context,
                        &mut self.match_data,
                        period,
                    );

                    match outcome {
                        TickOutcome::PeriodFinished => self.finish_period(),
                        // A dead-ball tick still moved the clock, so it counts
                        // — otherwise a caller pacing by tick count would run
                        // the post-goal pause at unbounded speed.
                        TickOutcome::Ticked | TickOutcome::DeadBall => ticks += 1,
                    }
                }
            }
        }

        AdvanceOutcome {
            phase: self.phase,
            clock_ms: self.context.total_match_time,
            ticks,
        }
    }

    /// Leave a break and start the period behind it. No-op anywhere else.
    pub fn resume(&mut self) {
        if let LivePhase::Interval(state) = self.phase {
            self.begin(state);
        }
    }

    /// Apply a manager's decision.
    ///
    /// Safe to call because it is only reachable between ticks: `advance_to`
    /// has returned by the time the caller has a `&mut self` to pass here.
    /// That is the whole determinism argument — there is no "mid-tick", so
    /// there is nothing to get half-applied.
    pub fn apply(&mut self, command: MatchCommand) -> Result<(), CommandError> {
        if matches!(self.phase, LivePhase::Pending | LivePhase::Finished) {
            return Err(CommandError::NotInPlay);
        }

        match command {
            MatchCommand::Substitution { out, r#in } => {
                execute_manual_substitution(
                    &mut self.field,
                    &mut self.context,
                    self.human_team_id,
                    out,
                    r#in,
                )
                .map_err(CommandError::Substitution)?;

                // The roster changed under a loop that may take a *light*
                // tick next, and light ticks do not rebuild the snapshot.
                // The shot-flight goalkeeper branch reads it, and reading a
                // player who has left the pitch is a panic, not a wrong
                // number.
                if let Some(period) = self.period.as_mut() {
                    period.refresh_tick_ctx(&self.field, &self.context);
                }
            }

            MatchCommand::Instruction(instruction) => {
                self.context
                    .coach_for_team_mut(self.human_team_id)
                    .set_manual_instruction(instruction);
            }

            MatchCommand::ReleaseInstruction => {
                self.context
                    .coach_for_team_mut(self.human_team_id)
                    .release_manual_instruction();
            }
        }

        self.command_log
            .push((self.context.total_match_time, command));

        Ok(())
    }

    /// The state of play, as the screen needs it.
    pub fn snapshot(&self) -> LiveSnapshot {
        let clock_ms = self.context.total_match_time;
        let describe = |p: &MatchPlayer| LivePlayer {
            id: p.id,
            team_id: p.team_id,
            condition: p.player_attributes.condition,
            is_sent_off: p.is_sent_off,
            goals: p.statistics.goals_count(),
            minutes: p.minutes_played_at(clock_ms),
        };

        let coach = self.context.coach_for_team(self.human_team_id);

        LiveSnapshot {
            phase: self.phase,
            clock_ms,
            minute: (clock_ms / 60_000) as u32,
            home_team_id: self.context.score.home_team.team_id,
            away_team_id: self.context.score.away_team.team_id,
            home_goals: self.context.score.home_team.get(),
            away_goals: self.context.score.away_team.get(),
            on_pitch: self.field.players.iter().map(describe).collect(),
            bench: self.field.substitutes.iter().map(describe).collect(),
            subs_used: self.context.subs_used_by_team(self.human_team_id),
            subs_allowed: self.context.max_substitutions_per_team,
            instruction: coach.instruction,
            instruction_is_manual: coach.instruction_is_manual(),
        }
    }

    /// Play the rest without stopping, then close the match out.
    ///
    /// This is what happens when the manager closes the tab: the fixture is
    /// still on the calendar and the league still needs a result, so the
    /// assistant finishes it. Any instruction the manager set stays set —
    /// walking away is not the same as changing your mind.
    pub fn finish_headless(mut self) -> MatchResult {
        self.advance_to(u64::MAX, StopPolicy::RunThrough);
        self.finish()
    }

    /// Close the match out and hand back the result the league consumes.
    ///
    /// Playing on after this is impossible by construction: `build_result`
    /// takes the field by value.
    pub fn finish(self) -> MatchResult {
        let raw = Engine::build_result(self.field, self.context, self.match_data);
        let score = raw.score.as_ref().expect("no score").clone();

        MatchResult {
            id: self.id,
            league_id: self.league_id,
            league_slug: self.league_slug,
            home_team_id: self.home_team_id,
            away_team_id: self.away_team_id,
            score,
            details: Some(raw),
            friendly: self.is_friendly,
        }
    }

    // ── the outer state loop, mirroring `play_with_config` ──────────────

    /// Ask the state machine what comes next and either enter it or stop
    /// in front of it.
    fn next_period(&mut self) {
        let Some(state) = self
            .states
            .next(&self.context.score, self.context.is_knockout)
        else {
            self.phase = LivePhase::Finished;
            return;
        };

        if Self::is_break(state) {
            self.phase = LivePhase::Interval(state);
        } else {
            self.begin(state);
        }
    }

    /// A state the manager should be given a beat in front of.
    ///
    /// Half time is the obvious one. Extra time and penalties are here for
    /// the same reason: both are moments where a manager has something to
    /// say, and both arrive without warning from the previous whistle.
    fn is_break(state: MatchState) -> bool {
        matches!(
            state,
            MatchState::HalfTime | MatchState::ExtraTime | MatchState::PenaltyShootout
        )
    }

    fn begin(&mut self, state: MatchState) {
        self.context.state.set(state);

        if state == MatchState::PenaltyShootout {
            Engine::run_penalty_shootout(&mut self.field, &mut self.context);
            self.finish_period();
            return;
        }

        // Half time goes through here too. It is a zero-tick state — the first
        // `step_period_tick` finds `increment_time` false and reports the
        // period finished — but it still runs, still costs its 10 ms, and
        // still hits `handle_state_finish`, which is what swaps the squads
        // and restarts the clock for the second half.
        self.period = Some(PeriodLoop::enter(
            &self.field,
            &self.context,
            &self.match_data,
        ));
        self.phase = LivePhase::Playing(state);
    }

    fn finish_period(&mut self) {
        self.period = None;

        StateManager::handle_state_finish(
            &mut self.context,
            &mut self.field,
            PlayMatchStateResult::default(),
        );

        self.next_period();
    }
}
