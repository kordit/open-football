//! Added by the fork: the gate on the live-match work.
//!
//! A match a human watches is driven from outside the engine — one tick, then
//! back to the caller, then the next tick. A match the league simulates is
//! driven by a `while` loop that never lets go. If those two produce different
//! football, every table in the game becomes a lie the moment somebody watches
//! one of their own fixtures.
//!
//! So the two drivers are compared here on the whole result, not on the
//! scoreline: goals with their minutes, every substitution, every field of
//! every player's stat line, the physical snapshots the condition drop is
//! computed from, stoppage time, the tactics both sides finished in, and the
//! serialised replay recording.
//!
//! The external driver deliberately *stops between ticks* — that is the thing
//! being tested. Anything the loop keeps on its stack across a yield would
//! show up here as a divergence.

use super::stepper::{PeriodLoop, TickOutcome};
use super::*;
use crate::club::player::builder::PlayerBuilder;
use crate::r#match::PlayMatchStateResult;
use crate::r#match::engine::context::MatchEngineConfig;
use crate::shared::fullname::FullName;
use crate::utils::random::engine::RandomEngine;
use crate::{
    MatchTacticType, PersonAttributes, PlayerAttributes, PlayerPosition, PlayerPositionType,
    PlayerPositions, PlayerSkills, Tactics,
};
use chrono::NaiveDate;
use std::fmt::Write as _;

type Engine = FootballEngine<840, 545>;

// ── fixture ────────────────────────────────────────────────────────────────

const T442: [PlayerPositionType; 11] = [
    PlayerPositionType::Goalkeeper,
    PlayerPositionType::DefenderLeft,
    PlayerPositionType::DefenderCenterLeft,
    PlayerPositionType::DefenderCenterRight,
    PlayerPositionType::DefenderRight,
    PlayerPositionType::MidfielderLeft,
    PlayerPositionType::MidfielderCenterLeft,
    PlayerPositionType::MidfielderCenterRight,
    PlayerPositionType::MidfielderRight,
    PlayerPositionType::ForwardLeft,
    PlayerPositionType::ForwardRight,
];

const BENCH: [PlayerPositionType; 5] = [
    PlayerPositionType::Goalkeeper,
    PlayerPositionType::DefenderCenter,
    PlayerPositionType::MidfielderCenter,
    PlayerPositionType::MidfielderCenter,
    PlayerPositionType::ForwardCenter,
];

fn build_player(id: u32, team_id: u32, pos: PlayerPositionType, level: f32) -> MatchPlayer {
    let mut attrs = PlayerAttributes::default();
    attrs.condition = 9_000;
    attrs.jadedness = 0;

    let mut skills = PlayerSkills::default();
    skills.technical.finishing = level;
    skills.technical.passing = level;
    skills.technical.first_touch = level;
    skills.technical.technique = level;
    skills.technical.dribbling = level;
    skills.technical.tackling = level;
    skills.technical.marking = level;
    skills.technical.heading = level;
    skills.technical.crossing = level;
    skills.technical.long_shots = level;
    skills.mental.composure = level;
    skills.mental.decisions = level;
    skills.mental.vision = level;
    skills.mental.anticipation = level;
    skills.mental.concentration = level;
    skills.mental.positioning = level;
    skills.mental.off_the_ball = level;
    skills.mental.work_rate = level;
    skills.mental.determination = level;
    skills.mental.teamwork = level;
    skills.mental.bravery = level;
    skills.mental.flair = level;
    skills.mental.aggression = 10.0;
    skills.physical.pace = level;
    skills.physical.acceleration = level;
    skills.physical.agility = level;
    skills.physical.balance = level;
    skills.physical.strength = level;
    skills.physical.stamina = level;
    skills.physical.jumping = level;
    skills.physical.natural_fitness = level;
    skills.physical.match_readiness = level;
    skills.goalkeeping.reflexes = level;
    skills.goalkeeping.handling = level;

    let player = PlayerBuilder::new()
        .id(id)
        .full_name(FullName::new("T".to_string(), format!("P{id}")))
        .birth_date(NaiveDate::from_ymd_opt(1998, 3, 14).unwrap())
        .country_id(1)
        .attributes(PersonAttributes::default())
        .skills(skills)
        .positions(PlayerPositions {
            positions: vec![PlayerPosition {
                position: pos,
                level: 18,
            }],
        })
        .player_attributes(attrs)
        .build()
        .unwrap();

    MatchPlayer::from_player(team_id, &player, pos, false, None)
}

/// Two sides of slightly different quality, with a bench.
///
/// The bench is not decoration: substitutions are the one part of the tick
/// body that mutates the roster under the loop, so a driver that keeps a stale
/// view of the squad diverges here and nowhere else.
pub(super) fn squad(team_id: u32, level: f32) -> MatchSquad {
    let base = team_id * 1_000;

    MatchSquad {
        team_id,
        team_name: format!("Team {team_id}"),
        tactics: Tactics::new(MatchTacticType::T442),
        main_squad: T442
            .iter()
            .enumerate()
            .map(|(i, &pos)| build_player(base + i as u32, team_id, pos, level))
            .collect(),
        substitutes: BENCH
            .iter()
            .enumerate()
            .map(|(i, &pos)| build_player(base + 100 + i as u32, team_id, pos, level - 1.0))
            .collect(),
        captain_id: None,
        vice_captain_id: None,
        penalty_taker_id: None,
        free_kick_taker_id: None,
        selection_omissions: Vec::new(),
        coach_snapshot: None,
    }
}

/// Everything pinned. `MatchEngineConfig::default()` reads the wall clock for
/// `today`, which would make two runs of the same seed differ across midnight
/// — the youth-protection substitution branch compares against it.
pub(super) fn config(seed: u64) -> MatchEngineConfig {
    MatchEngineConfig {
        seed: Some(seed),
        today: NaiveDate::from_ymd_opt(2025, 10, 4).unwrap(),
        match_recordings: true,
        ..Default::default()
    }
}

// ── the external driver ────────────────────────────────────────────────────

/// How many ticks the external driver takes before handing control back.
///
/// Chosen to be co-prime with none of the engine's cadences in particular —
/// the point is that it does NOT line up with the coach (500), tactical (25/10)
/// or recording (3) cycles, so yields land in the middle of every one of them.
const YIELD_EVERY: u32 = 137;

/// Play a match the way a live session would: hold the four pieces of state
/// yourself, take a handful of ticks, come back, take a few more.
fn play_stepwise(left: MatchSquad, right: MatchSquad, cfg: MatchEngineConfig) -> MatchResultRaw {
    let (mut field, mut context, mut data, mut states) = Engine::setup(left, right, &cfg);

    while let Some(state) = states.next(&context.score, context.is_knockout) {
        context.state.set(state);

        match state {
            MatchState::PenaltyShootout => {
                Engine::run_penalty_shootout(&mut field, &mut context);
            }
            _ => {
                let mut period = PeriodLoop::enter(&field, &context, &data);
                let mut finished = false;

                while !finished {
                    // One slice of ticks, then out of the loop entirely — the
                    // caller could render a frame, answer HTTP, or sleep here.
                    for _ in 0..YIELD_EVERY {
                        if matches!(
                            Engine::step_period_tick(
                                &mut field,
                                &mut context,
                                &mut data,
                                &mut period
                            ),
                            TickOutcome::PeriodFinished
                        ) {
                            finished = true;
                            break;
                        }
                    }
                }
            }
        }

        StateManager::handle_state_finish(
            &mut context,
            &mut field,
            PlayMatchStateResult::default(),
        );
    }

    Engine::build_result(field, context, data)
}

// ── fingerprint ────────────────────────────────────────────────────────────

/// Everything about a match that the rest of the game reads.
///
/// Deliberately a string rather than a field-by-field comparison: a new field
/// on `MatchResultRaw` that nobody adds here would silently stop being
/// checked, whereas `{:?}` on the whole stat line keeps up on its own.
fn fingerprint(result: &MatchResultRaw) -> String {
    let mut out = String::new();

    let score = result.score.as_ref().expect("score");
    writeln!(
        out,
        "score {}:{} ({} vs {})",
        score.home_team.get(),
        score.away_team.get(),
        score.home_team.team_id,
        score.away_team.team_id
    )
    .unwrap();

    for goal in score.detail() {
        writeln!(out, "goal {goal:?}").unwrap();
    }

    writeln!(out, "match_time_ms {}", result.match_time_ms).unwrap();
    writeln!(out, "additional_time_ms {}", result.additional_time_ms).unwrap();
    writeln!(
        out,
        "tactics {:?} {:?} -> {:?} {:?} (shape change {:?})",
        result.starting_home_tactic,
        result.starting_away_tactic,
        result.final_home_tactic,
        result.final_away_tactic,
        result.shape_change_minute
    )
    .unwrap();
    writeln!(out, "motm {:?}", result.player_of_the_match_id).unwrap();
    writeln!(out, "left {:?}", result.left_team_players).unwrap();
    writeln!(out, "right {:?}", result.right_team_players).unwrap();

    for sub in &result.substitutions {
        writeln!(out, "sub {sub:?}").unwrap();
    }
    for kick in &result.penalty_shootout {
        writeln!(out, "pen {kick:?}").unwrap();
    }

    // HashMap iteration order is not stable across runs — sort before writing
    // or the fingerprint fails for a reason that has nothing to do with football.
    let mut stat_ids: Vec<u32> = result.player_stats.keys().copied().collect();
    stat_ids.sort_unstable();
    for id in stat_ids {
        writeln!(out, "stats {id} {:?}", result.player_stats[&id]).unwrap();
    }

    let mut snapshot_ids: Vec<u32> = result.physical_snapshots.keys().copied().collect();
    snapshot_ids.sort_unstable();
    for id in snapshot_ids {
        writeln!(out, "phys {id} {:?}", result.physical_snapshots[&id]).unwrap();
    }

    // The recording serialises `HashMap`s, and two `HashMap`s in one process
    // do not agree on iteration order — `RandomState` re-keys per instance.
    // Canonicalise before comparing, or the gate fails on key order and calls
    // it a football difference.
    let recording: serde_json::Value =
        serde_json::to_value(&result.position_data).expect("position data serialises");
    writeln!(out, "recording {}", canonical(&recording)).unwrap();

    out
}

/// Serialise JSON with every object's keys in sorted order.
///
/// `serde_json::Map` is a `BTreeMap` only while the `preserve_order` feature
/// is off — which is a property of whatever else in the workspace pulls the
/// crate in, not something this test should depend on.
fn canonical(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            let body: Vec<String> = keys
                .iter()
                .map(|k| format!("{k:?}:{}", canonical(&map[*k])))
                .collect();
            format!("{{{}}}", body.join(","))
        }
        serde_json::Value::Array(items) => {
            let body: Vec<String> = items.iter().map(canonical).collect();
            format!("[{}]", body.join(","))
        }
        other => other.to_string(),
    }
}

/// Keep a failure message readable — the recording line alone is megabytes.
fn clip(line: &str) -> String {
    const MAX: usize = 240;

    if line.len() <= MAX {
        return line.to_string();
    }

    let cut = line
        .char_indices()
        .map(|(i, _)| i)
        .take_while(|i| *i <= MAX)
        .last()
        .unwrap_or(0);

    format!("{}… (+{} chars)", &line[..cut], line.len() - cut)
}

// ── pinning both random streams ────────────────────────────────────────────

/// A match draws from **two** random sources, and pinning one is not enough.
///
/// `MatchEngineConfig::seed` pins `MatchRng` — substitution timing, cards,
/// penalties. Player AI states also call `IntegerUtils::random` (defender
/// runs, keeper walks, forward jitter), which draws from the process-global
/// thread-local stream in `utils::random::engine`. That stream *carries on
/// between matches*: play the same fixture twice in one process without
/// touching it and you get two different scorelines. It is not a bug this
/// module introduced and not one it fixes; it is a fact any comparison of two
/// matches has to work around.
///
/// So each run is preceded by `set_seed`, which bumps the generation counter
/// and forces this thread's stream to rebuild from a known base.
pub(super) fn pin_streams(stream_seed: u64) {
    RandomEngine::set_seed(stream_seed);
}

/// Play the same fixture through both drivers, or report that another test
/// disturbed the shared stream while we were mid-match.
///
/// The seed is process-global, so a parallel test in this crate calling
/// `set_seed` re-seeds our thread's stream underneath a running match. That
/// would show up as a divergence caused by test scheduling rather than by the
/// engine, so it is detected and retried instead of being reported as a
/// failure. Detection is by seed value: no other test in the crate uses ours.
fn play_both_drivers(seed: u64) -> Option<(MatchResultRaw, MatchResultRaw)> {
    let stream = 0x5_7E99_0000_u64 ^ seed;

    pin_streams(stream);
    let batch = Engine::play_with_config(squad(1, 15.0), squad(2, 14.0), config(seed));
    if RandomEngine::current_seed() != stream {
        return None;
    }

    pin_streams(stream);
    let stepped = play_stepwise(squad(1, 15.0), squad(2, 14.0), config(seed));
    if RandomEngine::current_seed() != stream {
        return None;
    }

    Some((batch, stepped))
}

/// Up to a handful of attempts before giving up on a seed.
const STREAM_ATTEMPTS: usize = 8;

fn compare_drivers(seed: u64) -> bool {
    for _ in 0..STREAM_ATTEMPTS {
        let Some((batch, stepped)) = play_both_drivers(seed) else {
            continue;
        };

        let (a, b) = (fingerprint(&batch), fingerprint(&stepped));

        if a == b {
            return true;
        }

        let diff = a
            .lines()
            .zip(b.lines())
            .find(|(x, y)| x != y)
            .map(|(x, y)| format!("\n  batch:    {}\n  stepwise: {}", clip(x), clip(y)))
            .unwrap_or_else(|| "\n  (line counts differ)".to_string());

        panic!("seed {seed}: stepping the match changed its outcome{diff}");
    }

    false
}

// ── tests ──────────────────────────────────────────────────────────────────

/// The gate. Twenty seeds, batch driver against external driver, whole result.
///
/// **Run it optimised.** `MATCH_HALF_TIME_MS` is gated on `debug_assertions`,
/// so a plain `cargo test` plays five-minute halves — both halves and the
/// medical pass are crossed, but the discretionary substitution windows and
/// the stoppage-time accumulation of a real match are not:
///
/// ```text
/// cargo test --profile quick -p core stepper_identity
/// ```
#[test]
fn stepwise_driver_matches_batch() {
    let mut compared = 0;

    for seed in 1..=20u64 {
        if compare_drivers(seed) {
            compared += 1;
        }
    }

    assert!(
        compared >= 15,
        "only {compared}/20 seeds ran on an undisturbed random stream — \
         the comparison never really happened"
    );
}

/// A guard against the fingerprint quietly becoming a constant.
///
/// If seeding broke, every seed would return the same football and the gate
/// above would still pass — comparing nothing with nothing.
#[test]
fn different_seeds_produce_different_matches() {
    pin_streams(0xD1FF_0001);
    let a = fingerprint(&Engine::play_with_config(
        squad(1, 15.0),
        squad(2, 14.0),
        config(7),
    ));

    pin_streams(0xD1FF_0001);
    let b = fingerprint(&Engine::play_with_config(
        squad(1, 15.0),
        squad(2, 14.0),
        config(8),
    ));

    assert_ne!(
        a, b,
        "two seeds produced byte-identical matches — seeding is dead"
    );
}
