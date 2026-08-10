//! Added by the fork: what a manager can actually do to a live match.
//!
//! The stepper's own gate proves a stopped match is the same football as an
//! unstopped one. These tests cover the other half — that stopping is *for*
//! something: the two commands land, they survive, and the match still closes
//! out into the result the league consumes.
//!
//! Gated the same way as `stepper_identity_tests` and for the same reason: a
//! full match under `debug_assertions` trips a pre-existing loose-ball
//! assertion in `player/strategies/processor.rs`.
//!
//!     cargo test --profile quick -p core live_tests

use super::live::{LiveMatch, LivePhase, MatchCommand, StopPolicy};
use super::stepper_identity_tests::{config, pin_streams, squad};
use crate::r#match::engine::flow::result::SubstitutionReason;
use crate::r#match::game::Match;
use crate::r#match::pool::MatchPlayEnginePool;
use crate::r#match::{
    CoachInstruction, MatchInterceptor, MatchInterceptorRegistry, MatchResult, MatchState,
};
use std::sync::Arc;

const HUMAN_TEAM: u32 = 1;

fn live(seed: u64) -> LiveMatch {
    pin_streams(0x11FE_0000 ^ seed);

    let cfg = config(seed);
    let mut fixture = Match::make(
        "2025-10-04_1_2".to_string(),
        7,
        "test-league",
        squad(HUMAN_TEAM, 15.0),
        squad(2, 14.0),
        false,
    );
    fixture.seed = cfg.seed;

    LiveMatch::start(fixture, HUMAN_TEAM)
}

/// Wind the clock to `minute`, stopping at half time on the way if needed.
fn play_to_minute(m: &mut LiveMatch, minute: u64) {
    let target = minute * 60_000;

    loop {
        let out = m.advance_to(target, StopPolicy::AtInterval);

        match out.phase {
            LivePhase::Interval(_) => m.resume(),
            LivePhase::Finished => break,
            _ if out.clock_ms >= target => break,
            // No progress and not at a break — nothing left to wait for.
            _ if out.ticks == 0 => break,
            _ => {}
        }
    }
}

#[test]
fn a_manager_substitution_goes_through_the_full_substitution_path() {
    let mut m = live(11);
    play_to_minute(&mut m, 50);

    let snap = m.snapshot();

    // Picked from the live snapshot rather than hard-coded: a medical
    // substitution could already have moved somebody, and a test that fails
    // for that reason tells nobody anything.
    let out = snap
        .on_pitch
        .iter()
        .filter(|p| p.team_id == HUMAN_TEAM)
        .map(|p| p.id)
        .max()
        .expect("our side is on the pitch");
    let incoming = snap
        .bench
        .iter()
        .filter(|p| p.team_id == HUMAN_TEAM)
        .map(|p| p.id)
        .max()
        .expect("our bench is not empty");

    m.apply(MatchCommand::Substitution {
        out,
        r#in: incoming,
    })
    .expect("a legal substitution in the 50th minute");

    let result = m.finish_headless();
    let raw = result.details.expect("details");

    let manual: Vec<_> = raw
        .substitutions
        .iter()
        .filter(|s| s.reason == SubstitutionReason::Manual)
        .collect();

    assert_eq!(manual.len(), 1, "exactly one manual change was made");
    assert_eq!(manual[0].player_out_id, out);
    assert_eq!(manual[0].player_in_id, incoming);
    assert_eq!(manual[0].team_id, HUMAN_TEAM);

    // The point of routing through `execute_substitution` rather than
    // `field.substitute_player`: the man coming off keeps a stat line and a
    // physical snapshot stamped at the minute he left. Without them his
    // rating and his post-match condition drop are both silently wrong.
    assert!(
        raw.player_stats.contains_key(&out),
        "the substituted player kept his stat line"
    );
    assert!(
        raw.physical_snapshots.contains_key(&out),
        "the substituted player kept his physical snapshot"
    );
    assert!(
        raw.player_stats.contains_key(&incoming),
        "the substitute who came on has a stat line"
    );
}

#[test]
fn an_illegal_substitution_is_refused_and_changes_nothing() {
    let mut m = live(12);
    play_to_minute(&mut m, 50);

    let before = m.snapshot();

    // A player who is on the pitch cannot come on.
    let on_pitch = before
        .on_pitch
        .iter()
        .filter(|p| p.team_id == HUMAN_TEAM)
        .map(|p| p.id)
        .max()
        .unwrap();

    assert!(
        m.apply(MatchCommand::Substitution {
            out: on_pitch,
            r#in: on_pitch,
        })
        .is_err(),
        "a player cannot replace himself"
    );

    // Somebody else's player cannot be taken off by us.
    let theirs = before
        .on_pitch
        .iter()
        .find(|p| p.team_id != HUMAN_TEAM)
        .map(|p| p.id)
        .unwrap();
    let ours_bench = before
        .bench
        .iter()
        .filter(|p| p.team_id == HUMAN_TEAM)
        .map(|p| p.id)
        .max()
        .unwrap();

    assert!(
        m.apply(MatchCommand::Substitution {
            out: theirs,
            r#in: ours_bench,
        })
        .is_err(),
        "we do not pick the opposition's team"
    );

    let after = m.snapshot();
    assert_eq!(before.subs_used, after.subs_used, "no quota was spent");
    assert_eq!(
        before.on_pitch.len(),
        after.on_pitch.len(),
        "nobody moved on a refused command"
    );
}

#[test]
fn a_manual_instruction_outlives_the_assistant() {
    let mut m = live(13);
    play_to_minute(&mut m, 10);

    m.apply(MatchCommand::Instruction(CoachInstruction::ParkTheBus))
        .expect("setting an instruction mid-half");

    // The evaluator runs every 500 ticks; five sim minutes is ~30 000 ticks,
    // so this crosses it sixty times over. Before `manual_instruction` the
    // manager's choice used to survive about five seconds.
    let from = m.clock_ms();
    play_to_minute(&mut m, 15);
    assert!(m.clock_ms() > from + 240_000, "the clock actually moved");

    let snap = m.snapshot();
    assert_eq!(snap.instruction, CoachInstruction::ParkTheBus);
    assert!(snap.instruction_is_manual);

    // Handing it back does exactly that — the flag drops, and the assistant
    // owns the next evaluation.
    m.apply(MatchCommand::ReleaseInstruction).unwrap();
    assert!(!m.snapshot().instruction_is_manual);
}

#[test]
fn the_match_stops_at_half_time_and_only_moves_on_when_told() {
    let mut m = live(14);

    let out = m.advance_to(u64::MAX, StopPolicy::AtInterval);
    assert_eq!(
        out.phase,
        LivePhase::Interval(MatchState::HalfTime),
        "an uninterrupted request must still stop at the break"
    );

    // A second request while the manager is still in the dressing room must
    // not sneak the second half past them.
    let again = m.advance_to(u64::MAX, StopPolicy::AtInterval);
    assert_eq!(again.ticks, 0);
    assert_eq!(again.phase, LivePhase::Interval(MatchState::HalfTime));

    m.resume();
    let rest = m.advance_to(u64::MAX, StopPolicy::AtInterval);
    assert_eq!(rest.phase, LivePhase::Finished);
}

#[test]
fn finishing_in_the_background_plays_the_whole_match() {
    let mut m = live(15);
    play_to_minute(&mut m, 20);

    let result = m.finish_headless();
    let raw = result.details.expect("details");

    // Both halves, not just the one somebody watched.
    assert!(
        raw.match_time_ms >= 89 * 60_000,
        "match ran to full time, got {} ms",
        raw.match_time_ms
    );
    assert!(raw.score.is_some());
    assert!(
        !raw.player_stats.is_empty(),
        "the league needs stat lines out of this"
    );
}

// ── the matchday seam ──────────────────────────────────────────────────────

/// An interceptor standing in for a manager: claims one fixture, plays a bit
/// of it by hand, then lets the assistant finish.
struct HumanPlaysOne {
    match_id: String,
    team_id: u32,
}

impl MatchInterceptor for HumanPlaysOne {
    fn claims(&self, match_id: &str) -> bool {
        match_id == self.match_id
    }

    fn play(&self, fixture: Match) -> MatchResult {
        let mut m = LiveMatch::start(fixture, self.team_id);
        m.advance_to(20 * 60_000, StopPolicy::RunThrough);
        let _ = m.apply(MatchCommand::Instruction(CoachInstruction::PushForward));
        m.finish_headless()
    }
}

/// Removes the interceptor whatever happens — a leaked one would silently
/// claim a fixture in every later test in the process.
struct Installed;

impl Drop for Installed {
    fn drop(&mut self) {
        MatchInterceptorRegistry::clear();
    }
}

fn fixture(id: &str, home: u32, away: u32) -> Match {
    Match::make(
        id.to_string(),
        7,
        "test-league",
        squad(home, 15.0),
        squad(away, 14.0),
        false,
    )
}

#[test]
fn the_managers_match_comes_out_of_the_day_and_goes_back_in_its_own_slot() {
    pin_streams(0x1A7E_0001);

    let mine = "2025-10-04_3_4";

    MatchInterceptorRegistry::set(Arc::new(HumanPlaysOne {
        match_id: mine.to_string(),
        team_id: 3,
    }));
    let _guard = Installed;

    let day = vec![
        fixture("2025-10-04_1_2", 1, 2),
        fixture(mine, 3, 4),
        fixture("2025-10-04_5_6", 5, 6),
    ];

    let results = MatchPlayEnginePool::new(2).play(day);

    // Order is load-bearing: `WorldMatchdayResult::process` slices results by
    // continent range, so a result in the wrong slot lands in the wrong league.
    assert_eq!(
        results.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        vec!["2025-10-04_1_2", mine, "2025-10-04_5_6"],
    );

    // Every fixture played, the intercepted one included.
    for r in &results {
        assert!(
            r.details.as_ref().is_some_and(|d| d.match_time_ms > 0),
            "fixture {} produced no match",
            r.id
        );
    }

    let ours = &results[1];
    assert_eq!(ours.home_team_id, 3);
    assert_eq!(ours.away_team_id, 4);
    assert!(
        ours.details
            .as_ref()
            .is_some_and(|d| !d.player_stats.is_empty()),
        "the league needs stat lines out of the manager's match too"
    );
}

#[test]
fn a_day_without_the_managers_fixture_is_untouched() {
    pin_streams(0x1A7E_0002);

    MatchInterceptorRegistry::set(Arc::new(HumanPlaysOne {
        match_id: "a-fixture-that-is-not-today".to_string(),
        team_id: 3,
    }));
    let _guard = Installed;

    let results = MatchPlayEnginePool::new(2).play(vec![
        fixture("2025-10-11_1_2", 1, 2),
        fixture("2025-10-11_5_6", 5, 6),
    ]);

    assert_eq!(results.len(), 2);
    assert_eq!(
        results.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        vec!["2025-10-11_1_2", "2025-10-11_5_6"],
    );
}
