//! Added by the fork: one period of a match, one tick at a time.
//!
//! `play_inner` used to own eighteen local variables and drive them from a
//! `while` loop. That shape is fine for a batch simulation and useless for a
//! match a human sits through: the caller can never get between two ticks.
//!
//! This module splits the loop in two without touching what happens inside it.
//! [`PeriodLoop`] holds exactly the locals `play_inner` held, initialised in
//! exactly the same order, and [`FootballEngine::step_period_tick`] carries the
//! former loop body verbatim. `play_inner` becomes three lines around them, so
//! a batch match and a live match walk the same code down to the RNG draw.
//!
//! Two invariants keep that promise, and both are load-bearing:
//!
//! * `GameTickContext` is built **once per period**, in [`PeriodLoop::enter`].
//!   It memoises `ProfileMemos` — including `receiver_threat`, which carries no
//!   key and is therefore valid for the whole match. Rebuilding it mid-period
//!   silently changes results in release builds.
//! * `continue` in the old loop became [`TickOutcome::DeadBall`]. It is a
//!   *skipped* tick, not a finished period: the clock moved, nothing else did.
//!   A driver that treats it as anything but "keep going" loses the post-goal
//!   celebration pause.

use super::phase_prof::PhaseProf;
use super::*;
use crate::r#match::BallZone;

/// What one tick did to the period.
///
/// `DeadBall` and `Ticked` both mean "the period is still running"; they are
/// kept apart only so a live driver can tell a frame worth rendering from the
/// frozen post-goal pause.
pub(super) enum TickOutcome {
    Ticked,
    DeadBall,
    PeriodFinished,
}

/// The state `play_inner` used to keep on its stack.
///
/// Field order mirrors the original declaration order one to one. That is not
/// cosmetic: `GameTickContext::new` and `EventCollection::with_capacity` sit in
/// the middle of it, and moving allocations relative to the RNG-touching
/// initialisers is exactly the kind of change that shifts a scoreline without
/// failing to compile.
pub(super) struct PeriodLoop {
    prof_on: bool,

    next_sub_time_ms: u64,
    sub_times_initialized: bool,
    et_bonus_granted: bool,
    next_medical_time_ms: u64,
    medical_period: Option<MatchState>,

    tick_ctx: GameTickContext,
    events: EventCollection,

    tick_parity: u32,
    coach_eval_counter: u32,
    tactical_eval_counter: u32,
    transition_window_remaining: u32,

    last_owner_id: Option<u32>,
    last_possession_team: Option<u32>,
    last_home_score: u8,
    last_away_score: u8,
    last_home_instruction: CoachInstruction,
    last_away_instruction: CoachInstruction,
    last_home_zone: BallZone,
    last_away_zone: BallZone,

    next_position_record_ms: u64,
    track_positions: bool,
}

impl PeriodLoop {
    // Tactical refresh uses an adaptive cadence: BASE during stable
    // play, TRANSITION right after possession swings / set-piece
    // restarts / goals / coach-instruction changes / ball entering
    // or leaving the attacking third. Each "transition trigger"
    // opens a TRANSITION_WINDOW_TICKS window during which the
    // cheaper TRANSITION interval is used.
    const BASE_TACTICAL_INTERVAL_TICKS: u32 = 25;
    const TRANSITION_TACTICAL_INTERVAL_TICKS: u32 = 10;
    const TRANSITION_WINDOW_TICKS: u32 = 40;

    /// Arm the loop for one period. Call once per `MatchState`, never mid-period.
    pub(super) fn enter(
        field: &MatchField,
        context: &MatchContext,
        match_data: &ResultMatchPositionData,
    ) -> Self {
        let last_owner_id = field.ball.current_owner;
        let last_possession_team = last_owner_id
            .and_then(|id| field.players.iter().find(|p| p.id == id).map(|p| p.team_id));

        // Position recording cursor — replaces the per-tick
        // `timestamp % POSITION_RECORD_INTERVAL_MS == 0` check. Round
        // the starting timestamp UP to the next multiple of the
        // recording interval so a half restart preserves the original
        // 30 ms cadence (the loop increments time *before* the body,
        // so we never see `t == 0`).
        let initial_t = context.total_match_time;
        let next_position_record_ms = (initial_t / positions::POSITION_RECORD_INTERVAL_MS + 1)
            * positions::POSITION_RECORD_INTERVAL_MS;

        Self {
            prof_on: PhaseProf::enabled(),

            next_sub_time_ms: 0,
            sub_times_initialized: false,
            et_bonus_granted: false,
            // Medical (forced-injury) pass scheduling — independent of the
            // discretionary sub timer, re-armed at the start of each period.
            next_medical_time_ms: 0,
            medical_period: None,

            tick_ctx: GameTickContext::new(field, &context.players),
            events: EventCollection::with_capacity(10),

            tick_parity: 0,
            coach_eval_counter: 0,
            tactical_eval_counter: 0,
            transition_window_remaining: Self::TRANSITION_WINDOW_TICKS,

            // Snapshots used to detect transition triggers between refresh
            // points without a per-tick walk over players.
            last_owner_id,
            last_possession_team,
            last_home_score: context.score.home_team.get(),
            last_away_score: context.score.away_team.get(),
            last_home_instruction: context.coach_home.instruction,
            last_away_instruction: context.coach_away.instruction,
            last_home_zone: context.tactical_home.ball_zone,
            last_away_zone: context.tactical_away.ball_zone,

            next_position_record_ms,
            track_positions: match_data.is_tracking_positions(),
        }
    }

    /// Rebuild the tick snapshot after the roster changed under the loop.
    ///
    /// Only a manual substitution needs this: the AI substitution passes run
    /// inside the tick body, where the next full tick refreshes `tick_ctx`
    /// anyway. A command applied between ticks can land right before a *light*
    /// tick, which does not refresh — and the shot-flight goalkeeper branch
    /// reads the stale snapshot. Losing a player id there is a panic, not a
    /// wrong number.
    ///
    /// This is deliberately NOT called on a normal tick: rebuilding
    /// `GameTickContext` mid-period drops `ProfileMemos` and changes results.
    // Unused until `LiveMatch` starts applying commands between ticks; it
    // belongs here, with the invariant it protects, not in the file that will
    // eventually call it.
    #[allow(dead_code)]
    pub(super) fn refresh_tick_ctx(&mut self, field: &MatchField, context: &MatchContext) {
        self.tick_ctx = GameTickContext::new(field, &context.players);
    }
}

impl<const W: usize, const H: usize> FootballEngine<W, H> {
    /// One iteration of the former `play_inner` loop.
    ///
    /// The body below is the original, moved without edits. Read any change to
    /// it as a change to every simulated match in the game, not just the one
    /// somebody is watching.
    pub(super) fn step_period_tick(
        field: &mut MatchField,
        context: &mut MatchContext,
        match_data: &mut ResultMatchPositionData,
        lp: &mut PeriodLoop,
    ) -> TickOutcome {
        if !context.increment_time() {
            return TickOutcome::PeriodFinished;
        }

        // Post-goal dead time: only the match clock advances while
        // the players celebrate / walk back / wait for the restart
        // whistle. No ball physics, no AI, no events, no coach
        // evals — the world is already reset and frozen in
        // formation, so skipping the tick body IS the celebration.
        // See `MatchContext::dead_ball_until_ms` for why this pause
        // is load-bearing (it consumed the post-goal hot window
        // that made goals beget goals).
        if context.total_match_time < context.dead_ball_until_ms {
            return TickOutcome::DeadBall;
        }

        lp.tick_parity += 1;
        lp.coach_eval_counter += 1;
        lp.tactical_eval_counter += 1;
        if lp.transition_window_remaining > 0 {
            lp.transition_window_remaining -= 1;
        }

        // Coach evaluates every 500 ticks (~5 seconds of match time)
        if lp.coach_eval_counter >= 500 {
            lp.coach_eval_counter = 0;
            let prof_t = lp.prof_on.then(Instant::now);
            Self::evaluate_coaches(field, context);
            // Once every coach-eval slice, also probe for situational
            // formation overrides — the manager swap to a chasing /
            // protecting shape based on score and minute. Cheap: a
            // single match arm and an equality check against the
            // current type per side.
            Self::evaluate_situational_shape(field, &mut *context);
            if let Some(t) = prof_t {
                PhaseProf::add(PhaseProf::P_COACH, t.elapsed().as_nanos() as u64);
            }
            // Condition-trajectory sampling for the dev harness —
            // average condition per position group per 15-min band.
            // Rides the coach cadence so it costs one 22-player walk
            // every 5 sim-seconds, match-logs builds only.
            #[cfg(feature = "match-logs")]
            {
                use crate::r#match::player::strategies::players::ops::forward_shot_decision::time_band_diag;
                use std::sync::atomic::Ordering;
                let band =
                    time_band_diag::band_for_minute((context.total_match_time / 60_000) as u32);
                for p in field.players.iter().filter(|p| !p.is_sent_off) {
                    let group = match p.tactical_position.current_position.position_group() {
                        crate::PlayerFieldPositionGroup::Goalkeeper => 0,
                        crate::PlayerFieldPositionGroup::Defender => 1,
                        crate::PlayerFieldPositionGroup::Midfielder => 2,
                        crate::PlayerFieldPositionGroup::Forward => 3,
                    };
                    time_band_diag::COND_SUM_BY_BAND_GROUP[band][group].fetch_add(
                        p.player_attributes.condition.max(0) as u64,
                        Ordering::Relaxed,
                    );
                    time_band_diag::COND_N_BY_BAND_GROUP[band][group]
                        .fetch_add(1, Ordering::Relaxed);
                }
            }
        }

        // Team-level tactical state (phase, possession timers, line
        // height) used a fixed 10-tick cadence. Adaptive cadence:
        // stable possession uses BASE (25 ticks), while a 40-tick
        // window after any transition trigger drops to TRANSITION
        // (10 ticks) so phase/line-height/transition windows still
        // resolve crisply when the game state actually shifts.
        //
        // Triggers (each cheap, no per-tick player walks):
        //   • possession owner team changed
        //   • score changed (goal scored — handled via reset path)
        //   • coach instruction changed for either side
        //   • ball zone moved into / out of attacking third for
        //     either side
        //
        // Set-piece restarts are covered indirectly: kickoff /
        // corner / goal kick all reassign the ball owner, which
        // flips `last_possession_team` and re-opens the window.
        //
        // Cheap fast path: most ticks have the same `current_owner`
        // as the previous tick (passes/dribbles span many ticks).
        // Only re-resolve `team_id` via a 22-element scan when the
        // raw id actually changed since the last evaluation.
        let raw_owner = field.ball.current_owner;
        let current_owner_team = if raw_owner == lp.last_owner_id {
            lp.last_possession_team
        } else {
            lp.last_owner_id = raw_owner;
            raw_owner.and_then(|id| field.players.iter().find(|p| p.id == id).map(|p| p.team_id))
        };
        let possession_changed =
            current_owner_team != lp.last_possession_team && current_owner_team.is_some();
        let home_score_now = context.score.home_team.get();
        let away_score_now = context.score.away_team.get();
        let score_changed =
            home_score_now != lp.last_home_score || away_score_now != lp.last_away_score;
        let home_instr_now = context.coach_home.instruction;
        let away_instr_now = context.coach_away.instruction;
        let instr_changed = home_instr_now != lp.last_home_instruction
            || away_instr_now != lp.last_away_instruction;
        let home_zone_now = context.tactical_home.ball_zone;
        let away_zone_now = context.tactical_away.ball_zone;
        // Attacking-third entry/exit on either side.
        let zone_changed = matches!(home_zone_now, BallZone::AttackingThird)
            != matches!(lp.last_home_zone, BallZone::AttackingThird)
            || matches!(away_zone_now, BallZone::AttackingThird)
                != matches!(lp.last_away_zone, BallZone::AttackingThird);
        if possession_changed || score_changed || instr_changed || zone_changed {
            lp.transition_window_remaining = PeriodLoop::TRANSITION_WINDOW_TICKS;
            if possession_changed {
                lp.last_possession_team = current_owner_team;
            }
            if score_changed {
                lp.last_home_score = home_score_now;
                lp.last_away_score = away_score_now;
            }
            if instr_changed {
                lp.last_home_instruction = home_instr_now;
                lp.last_away_instruction = away_instr_now;
            }
            if zone_changed {
                lp.last_home_zone = home_zone_now;
                lp.last_away_zone = away_zone_now;
            }
        }

        let tactical_interval = if lp.transition_window_remaining > 0 {
            PeriodLoop::TRANSITION_TACTICAL_INTERVAL_TICKS
        } else {
            PeriodLoop::BASE_TACTICAL_INTERVAL_TICKS
        };
        if lp.tactical_eval_counter >= tactical_interval {
            let interval = lp.tactical_eval_counter;
            lp.tactical_eval_counter = 0;
            let prof_t = lp.prof_on.then(Instant::now);
            Self::refresh_tactical_states(field, context, interval);
            if let Some(t) = prof_t {
                PhaseProf::add(PhaseProf::P_TACTICAL, t.elapsed().as_nanos() as u64);
            }
            // refresh_tactical_states may have repointed
            // ball_zone — re-snapshot to avoid spuriously
            // re-triggering the window on the next tick.
            lp.last_home_zone = context.tactical_home.ball_zone;
            lp.last_away_zone = context.tactical_away.ball_zone;
        }

        // Full tick: ball + player AI + events
        // Light tick: ball + player movement only (no AI re-evaluation)
        if lp.tick_parity & 1 == 0 {
            Self::game_tick_light(field, context, match_data, &mut lp.tick_ctx, &mut lp.events);
        } else {
            Self::game_tick_inner(field, context, match_data, &mut lp.tick_ctx, &mut lp.events);
        }

        // Replay-position recording, gated by a cursor instead of
        // a per-tick modulo. Same 30 ms cadence as before; just one
        // u64 comparison + add per tick when nothing is being
        // tracked (the dominant production case).
        if lp.track_positions && context.total_match_time >= lp.next_position_record_ms {
            Self::write_match_positions(field, context.total_match_time, match_data);
            lp.next_position_record_ms += Self::POSITION_RECORD_INTERVAL_MS;
        }

        // Forced medical substitutions run in ANY playing period —
        // real football replaces an injured player whenever it
        // happens, first half included. The pass owns the in-match
        // injury roll; first check lands 3-8 minutes into each
        // period, then every 6-14 minutes.
        let medical_enabled = matches!(
            context.state.match_state,
            MatchState::FirstHalf | MatchState::SecondHalf | MatchState::ExtraTime
        );
        if medical_enabled {
            if lp.medical_period != Some(context.state.match_state) {
                lp.medical_period = Some(context.state.match_state);
                lp.next_medical_time_ms =
                    context.time.time + context.rng.range_u64(3, 8) * 60 * 1000;
            }
            if context.time.time >= lp.next_medical_time_ms {
                Substitutions::process_medical(field, context);
                lp.next_medical_time_ms =
                    context.time.time + context.rng.range_u64(6, 14) * 60 * 1000;
            }
        }

        // Discretionary substitutions allowed from the second half
        // onwards, plus extra time when we reach it in a knockout
        // tie. First-half subs in real football are reactive
        // (injuries) — the medical pass above owns those. ET gets
        // one bonus sub on entry (FIFA rule).
        let subs_enabled = matches!(
            context.state.match_state,
            MatchState::SecondHalf | MatchState::ExtraTime
        );

        if subs_enabled {
            // Grant the ET bonus once — bumps the cap by 1 for both
            // sides — but only when the active rule set allows it.
            // Friendlies (cap = usize::MAX) skip the increment.
            if context.state.match_state == MatchState::ExtraTime
                && !lp.et_bonus_granted
                && context.allow_extra_time_extra_sub
            {
                if context.max_substitutions_per_team < usize::MAX {
                    context.max_substitutions_per_team += 1;
                }
                lp.et_bonus_granted = true;
                // Reset the next-sub timer for the new period.
                lp.sub_times_initialized = false;
            }

            if !lp.sub_times_initialized {
                lp.next_sub_time_ms = context.rng.range_u64(10, 20) * 60 * 1000;
                lp.sub_times_initialized = true;
            }

            let period_time = context.time.time;
            if period_time >= lp.next_sub_time_ms {
                // Deterministic "today" — captured at context
                // construction. Used only for the youth-protection
                // sub branch, where the comparison is age <= 17.
                let today = context.today;
                let per_pass_cap = context.max_substitutions_per_pass;
                process_substitutions(field, context, per_pass_cap, today);
                lp.next_sub_time_ms = period_time + context.rng.range_u64(5, 15) * 60 * 1000;
            }
        }

        TickOutcome::Ticked
    }
}
