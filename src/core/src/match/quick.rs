//! Statistical stand-in for the tick-based match engine.
//!
//! New file in this fork. Upstream plays every fixture in the world at
//! full fidelity: ~540 000 ticks per match, 22 agents running the state
//! machine on every second tick. Measured on a full Polish pyramid that
//! is ~98 s of wall clock for a single matchday of 400 fixtures — the
//! dominant cost of advancing one day, and 20x the cost of a quiet day.
//!
//! Polish Football Manager only ever *watches* one of those fixtures.
//! This module produces a `MatchResultRaw` for the other 399 from the
//! squads' abilities instead of from simulated play: a scoreline, the
//! players who scored and assisted, cards, minutes, substitutions and a
//! full per-player stat line. Downstream (league tables, season stats,
//! player development, morale, coach memory) cannot tell the difference,
//! because it only ever reads `MatchResultRaw`.
//!
//! What it deliberately does NOT produce is `position_data` — there are
//! no coordinates because nothing moved. That is the whole point: a
//! quick result is never recorded, never replayed and never watched.
//! `Match::play` therefore routes the managed club's fixtures to the
//! real engine regardless of this module.
//!
//! Determinism: every roll comes from a `MatchRng` seeded with the same
//! per-fixture seed the real engine gets (`Match::seed`, stamped from
//! `(world_seed, fixture id, date)`). No draw touches the process-global
//! `utils::random::engine` stream, so replacing a quick match with a real
//! one — or vice versa — cannot shift any *other* match's result.

use std::collections::HashMap;

use crate::club::PlayerFieldPositionGroup;
use crate::r#match::engine::flow::rng::MatchRng;
use crate::r#match::engine::player::statistics::MatchStatisticType;
use crate::r#match::engine::result::{
    FieldSquad, GoalDetail, PenaltyShootoutKick, PlayerMatchEndStats, Score, SubstitutionInfo,
    SubstitutionReason, TeamScore,
};
use crate::r#match::engine::rating::RatingContext;
use crate::r#match::{MatchPlayer, MatchResultRaw, MatchSquad};

/// Regulation length. Quick matches carry no stoppage time — nothing
/// happened that could stop the clock.
const FULL_TIME_MS: u64 = 90 * 60 * 1000;
const FULL_TIME_MIN: u16 = 90;

/// League-average goals per team per match. The two teams' expected
/// goals are this figure redistributed by relative strength, so the
/// mean total across a division stays put no matter how lopsided the
/// individual fixtures are.
const BASE_EXPECTED_GOALS: f32 = 1.35;

/// Home advantage as a multiplier on the home side's expected goals.
/// ~1.25 reproduces the long-run home win share in senior football.
const HOME_ADVANTAGE: f32 = 1.25;

/// Ceiling on a single side's goals. Poisson has no upper bound and
/// `TeamScore` is a `u8`; a 40-0 tail event would be both impossible
/// and unrepresentable.
const MAX_GOALS: u8 = 9;

/// How strongly relative strength is allowed to skew the split of
/// expected goals. 1.0 would let a twice-as-good side score twice as
/// often; football is noisier than that.
const STRENGTH_SKEW: f32 = 0.75;

pub struct QuickMatch;

impl QuickMatch {
    /// Play a fixture statistically. Mirrors the shape of
    /// `FootballEngine::play_with_config` — same inputs, same output
    /// type — so `Match::play` can choose between them at the call site.
    pub fn play(
        home_squad: MatchSquad,
        away_squad: MatchSquad,
        seed: Option<u64>,
        is_knockout: bool,
    ) -> MatchResultRaw {
        let rng = match seed {
            Some(seed) => MatchRng::from_seed(seed),
            None => MatchRng::from_entropy(),
        };

        let home_strength = squad_strength(&home_squad);
        let away_strength = squad_strength(&away_squad);

        let (mut home_goals, mut away_goals) =
            draw_scoreline(&rng, home_strength, away_strength);

        // Knockouts cannot end level. Extra time is modelled as a third
        // of a match at the same rates rather than simulated; if that
        // still leaves it level, it goes to penalties.
        let mut shootout = Vec::new();
        let (mut home_pens, mut away_pens) = (0u8, 0u8);

        if is_knockout && home_goals == away_goals {
            let (extra_home, extra_away) =
                draw_extra_time(&rng, home_strength, away_strength);
            home_goals = (home_goals + extra_home).min(MAX_GOALS);
            away_goals = (away_goals + extra_away).min(MAX_GOALS);

            if home_goals == away_goals {
                let (h, a, kicks) = draw_shootout(&rng, &home_squad, &away_squad);
                home_pens = h;
                away_pens = a;
                shootout = kicks;
            }
        }

        // Each side's on-pitch story: who came off, who came on, who
        // scored, who was booked, and a stat line for everyone involved.
        let home = build_team(
            &rng,
            &home_squad,
            home_goals,
            away_goals,
            home_strength,
            away_strength,
        );
        let away = build_team(
            &rng,
            &away_squad,
            away_goals,
            home_goals,
            away_strength,
            home_strength,
        );

        let mut score = Score {
            home_team: TeamScore::new_with_score(home_squad.team_id, home_goals),
            away_team: TeamScore::new_with_score(away_squad.team_id, away_goals),
            details: Vec::new(),
            home_shootout: home_pens,
            away_shootout: away_pens,
        };

        // Goal / assist / card details from both sides, interleaved in
        // chronological order the way the real engine emits them — the
        // match-events consumer walks this list front to back.
        score.details.extend(home.details.iter().cloned());
        score.details.extend(away.details.iter().cloned());
        score.details.sort_by_key(|detail| detail.time);

        let mut result = MatchResultRaw::with_match_time(FULL_TIME_MS);
        result.score = Some(score);
        result.left_team_players = home.field_squad;
        result.right_team_players = away.field_squad;
        result.starting_home_tactic = Some(home_squad.tactics.tactic_type);
        result.starting_away_tactic = Some(away_squad.tactics.tactic_type);
        result.final_home_tactic = Some(home_squad.tactics.tactic_type);
        result.final_away_tactic = Some(away_squad.tactics.tactic_type);
        result.penalty_shootout = shootout;

        result.substitutions = home.substitutions;
        result.substitutions.extend(away.substitutions);

        result.player_stats = home.stats;
        result.player_stats.extend(away.stats);

        // `physical_snapshots` is left empty on purpose:
        // `apply_post_match_physical_effects` documents a minutes-only
        // fallback for results not built by the engine, which is exactly
        // the right model here — nobody tracked energy because nobody ran.
        result.player_of_the_match_id = pick_motm(&result.player_stats);

        result
    }
}

/// One team's contribution to the result.
struct TeamOutcome {
    field_squad: FieldSquad,
    stats: HashMap<u32, PlayerMatchEndStats>,
    substitutions: Vec<SubstitutionInfo>,
    details: Vec<GoalDetail>,
}

/// Mean ability of the starting eleven, in the engine's 1..200 scale,
/// each player rated for the position he actually starts in. An empty
/// squad scores the floor rather than dividing by zero — thin clubs are
/// a documented reality in this world, not an error.
fn squad_strength(squad: &MatchSquad) -> f32 {
    if squad.main_squad.is_empty() {
        return 1.0;
    }

    let total: f32 = squad
        .main_squad
        .iter()
        .map(|player| {
            player
                .skills
                .calculate_ability_for_position(player.tactical_position.current_position)
                as f32
        })
        .sum();

    (total / squad.main_squad.len() as f32).max(1.0)
}

/// Split `2 * BASE_EXPECTED_GOALS` between the sides by relative
/// strength, apply home advantage, and draw each side's goals from its
/// own Poisson.
fn draw_scoreline(rng: &MatchRng, home_strength: f32, away_strength: f32) -> (u8, u8) {
    let (home_lambda, away_lambda) = expected_goals(home_strength, away_strength);
    (
        poisson(rng, home_lambda).min(MAX_GOALS),
        poisson(rng, away_lambda).min(MAX_GOALS),
    )
}

/// Extra time: 30 minutes at the same scoring rate, i.e. a third of a
/// match. Drawn separately so a knockout that goes the distance shows a
/// plausible 2-1 rather than a regulation-shaped scoreline.
fn draw_extra_time(rng: &MatchRng, home_strength: f32, away_strength: f32) -> (u8, u8) {
    let (home_lambda, away_lambda) = expected_goals(home_strength, away_strength);
    (
        poisson(rng, home_lambda / 3.0),
        poisson(rng, away_lambda / 3.0),
    )
}

fn expected_goals(home_strength: f32, away_strength: f32) -> (f32, f32) {
    let total = home_strength + away_strength;
    // Guarded above by `squad_strength`'s floor, but division by zero
    // here would poison every downstream draw, so be explicit.
    let share = if total > 0.0 {
        home_strength / total
    } else {
        0.5
    };

    // Pull the raw share toward 0.5 so ability tilts the fixture without
    // deciding it. A 0.75 skew turns a 2:1 strength edge into roughly a
    // 1.6:1 goal edge, which is about what real divisions show.
    let home_share = 0.5 + (share - 0.5) * STRENGTH_SKEW;
    let away_share = 1.0 - home_share;

    (
        2.0 * BASE_EXPECTED_GOALS * home_share * HOME_ADVANTAGE,
        2.0 * BASE_EXPECTED_GOALS * away_share,
    )
}

/// Knuth's product-of-uniforms Poisson sampler. Exact for the small
/// lambdas here (< 4), and it draws from `MatchRng` so the whole result
/// stays reproducible from the fixture seed.
fn poisson(rng: &MatchRng, lambda: f32) -> u8 {
    if lambda <= 0.0 {
        return 0;
    }

    let limit = (-lambda).exp();
    let mut count: u8 = 0;
    let mut product = rng.unit_f32();

    while product > limit && count < MAX_GOALS {
        count += 1;
        product *= rng.unit_f32();
    }

    count
}

/// Best-of-five then sudden death, decided by each taker's finishing
/// against the standard ~75% conversion rate.
fn draw_shootout(
    rng: &MatchRng,
    home_squad: &MatchSquad,
    away_squad: &MatchSquad,
) -> (u8, u8, Vec<PenaltyShootoutKick>) {
    let home_takers = shootout_takers(home_squad);
    let away_takers = shootout_takers(away_squad);

    if home_takers.is_empty() || away_takers.is_empty() {
        return (0, 0, Vec::new());
    }

    let mut kicks = Vec::new();
    let (mut home_scored, mut away_scored) = (0u8, 0u8);

    for round in 0..5u8 {
        for (is_home, takers) in [(true, &home_takers), (false, &away_takers)] {
            let taker = takers[round as usize % takers.len()];
            let scored = rng.bernoulli(0.75);

            if scored {
                if is_home {
                    home_scored += 1;
                } else {
                    away_scored += 1;
                }
            }

            kicks.push(PenaltyShootoutKick {
                team_id: if is_home {
                    home_squad.team_id
                } else {
                    away_squad.team_id
                },
                taker_id: taker,
                goalkeeper_id: None,
                round: round + 1,
                scored,
                sudden_death: false,
            });
        }
    }

    // Sudden death until someone blinks. Bounded so a pathological RNG
    // cannot spin here forever.
    let mut round = 5u8;
    while home_scored == away_scored && round < 20 {
        let home_hit = rng.bernoulli(0.75);
        let away_hit = rng.bernoulli(0.75);

        for (is_home, hit, takers, team_id) in [
            (true, home_hit, &home_takers, home_squad.team_id),
            (false, away_hit, &away_takers, away_squad.team_id),
        ] {
            kicks.push(PenaltyShootoutKick {
                team_id,
                taker_id: takers[round as usize % takers.len()],
                goalkeeper_id: None,
                round: round + 1,
                scored: hit,
                sudden_death: true,
            });
            if hit {
                if is_home {
                    home_scored += 1;
                } else {
                    away_scored += 1;
                }
            }
        }

        round += 1;
    }

    (home_scored, away_scored, kicks)
}

/// Outfield players, best finishers first — the order a manager would
/// actually nominate.
fn shootout_takers(squad: &MatchSquad) -> Vec<u32> {
    let mut takers: Vec<(u32, f32)> = squad
        .main_squad
        .iter()
        .filter(|player| !is_goalkeeper(player))
        .map(|player| (player.id, player.skills.technical.penalty_taking))
        .collect();

    takers.sort_by(|a, b| b.1.total_cmp(&a.1));
    takers.into_iter().map(|(id, _)| id).collect()
}

fn is_goalkeeper(player: &MatchPlayer) -> bool {
    player.tactical_position.current_position.position_group()
        == PlayerFieldPositionGroup::Goalkeeper
}

/// Build everything that happened to one team: substitutions, minutes,
/// goal and card attribution, per-player stat lines and ratings.
fn build_team(
    rng: &MatchRng,
    squad: &MatchSquad,
    goals_for: u8,
    goals_against: u8,
    strength: f32,
    opponent_strength: f32,
) -> TeamOutcome {
    let mut field_squad = FieldSquad::from_team(squad);
    let mut substitutions = Vec::new();
    let mut details = Vec::new();

    // Minutes: starters play the full match unless replaced.
    let mut minutes: HashMap<u32, u16> = squad
        .main_squad
        .iter()
        .map(|player| (player.id, FULL_TIME_MIN))
        .collect();

    // Substitutions. Real coaches make up to three; who comes off is
    // left to chance here because the reason (fatigue, tactics, injury)
    // is exactly the detail a quick match does not model.
    let planned_subs = rng.range_i32(0, 4).min(squad.substitutes.len() as i32);

    for index in 0..planned_subs {
        let Some(coming_on) = squad.substitutes.get(index as usize) else {
            break;
        };

        // Never withdraw the goalkeeper — an outfield sub for a keeper
        // is an injury story, and injuries are not modelled here.
        let candidates: Vec<&MatchPlayer> = squad
            .main_squad
            .iter()
            .filter(|player| !is_goalkeeper(player))
            .filter(|player| minutes.get(&player.id) == Some(&FULL_TIME_MIN))
            .collect();

        if candidates.is_empty() {
            break;
        }

        let going_off = candidates[rng.range_i32(0, candidates.len() as i32) as usize];
        let minute = rng.range_i32(55, 86) as u16;

        minutes.insert(going_off.id, minute);
        minutes.insert(coming_on.id, FULL_TIME_MIN - minute);
        field_squad.mark_substitute_used(coming_on.id);

        substitutions.push(SubstitutionInfo {
            team_id: squad.team_id,
            player_out_id: going_off.id,
            player_in_id: coming_on.id,
            match_time_ms: minute as u64 * 60 * 1000,
            reason: SubstitutionReason::default(),
        });
    }

    // Everyone who touched the pitch gets a stat line.
    let appeared: Vec<&MatchPlayer> = squad
        .main_squad
        .iter()
        .chain(squad.substitutes.iter())
        .filter(|player| minutes.contains_key(&player.id))
        .collect();

    let mut stats: HashMap<u32, PlayerMatchEndStats> = appeared
        .iter()
        .map(|player| {
            let played = minutes.get(&player.id).copied().unwrap_or(0);
            (
                player.id,
                baseline_stats(rng, player, played, strength, opponent_strength, goals_against),
            )
        })
        .collect();

    // Goals, then assists on the goals that had one, then cards.
    for _ in 0..goals_for {
        let minute = rng.range_i32(1, 91) as u16;

        if let Some(scorer) = weighted_pick(rng, &appeared, &minutes, scoring_weight) {
            if let Some(line) = stats.get_mut(&scorer) {
                line.goals += 1;
                line.shots_total += 1;
                line.shots_on_target += 1;
                line.xg += 0.35;
            }

            details.push(GoalDetail {
                player_id: scorer,
                stat_type: MatchStatisticType::Goal,
                is_auto_goal: false,
                time: minute as u64 * 60 * 1000,
            });

            // Roughly three in four goals are assisted, and never by the
            // scorer himself.
            if rng.bernoulli(0.72) {
                let assist_pool: Vec<&MatchPlayer> = appeared
                    .iter()
                    .copied()
                    .filter(|player| player.id != scorer)
                    .collect();

                if let Some(assister) = weighted_pick(rng, &assist_pool, &minutes, assist_weight) {
                    if let Some(line) = stats.get_mut(&assister) {
                        line.assists += 1;
                        line.key_passes += 1;
                    }

                    details.push(GoalDetail {
                        player_id: assister,
                        stat_type: MatchStatisticType::Assist,
                        is_auto_goal: false,
                        time: minute as u64 * 60 * 1000,
                    });
                }
            }
        }
    }

    // Cards. Booking rates are position-weighted the same way as
    // tackling, because that is what earns them.
    let yellows = poisson(rng, 1.7).min(5);
    for _ in 0..yellows {
        if let Some(booked) = weighted_pick(rng, &appeared, &minutes, card_weight) {
            if let Some(line) = stats.get_mut(&booked) {
                line.yellow_cards += 1;
                line.fouls += 1;
            }

            details.push(GoalDetail {
                player_id: booked,
                stat_type: MatchStatisticType::YellowCard,
                is_auto_goal: false,
                time: rng.range_i32(1, 91) as u64 * 60 * 1000,
            });
        }
    }

    if rng.bernoulli(0.045) {
        if let Some(sent_off) = weighted_pick(rng, &appeared, &minutes, card_weight) {
            if let Some(line) = stats.get_mut(&sent_off) {
                line.red_cards += 1;
            }

            details.push(GoalDetail {
                player_id: sent_off,
                stat_type: MatchStatisticType::RedCard,
                is_auto_goal: false,
                time: rng.range_i32(20, 91) as u64 * 60 * 1000,
            });
        }
    }

    // Ratings last: they read the finished stat line, exactly as the
    // real engine does, so a quick match and a played match are scored
    // by the same function.
    for line in stats.values_mut() {
        let rating = RatingContext::new(line, goals_for, goals_against).calculate();
        line.match_rating = rating;
        line.raw_match_rating = rating;
    }

    TeamOutcome {
        field_squad,
        stats,
        substitutions,
        details,
    }
}

/// A plausible stat line for a player who did his job and nothing
/// remarkable. Goals, assists and cards are layered on top afterwards.
///
/// Volumes are per-90 figures scaled by minutes played, nudged by the
/// player's own ability relative to the opposition — a good side in a
/// weak fixture completes more passes, wins more of the ball, and makes
/// fewer saves.
fn baseline_stats(
    rng: &MatchRng,
    player: &MatchPlayer,
    minutes_played: u16,
    strength: f32,
    opponent_strength: f32,
    goals_conceded: u8,
) -> PlayerMatchEndStats {
    let group = player.tactical_position.current_position.position_group();
    let share = minutes_played as f32 / FULL_TIME_MIN as f32;
    // 0.75..1.25 — how much of the game this side controlled.
    let dominance = (strength / opponent_strength.max(1.0)).clamp(0.75, 1.25);

    let scale = |per_90: f32| -> u16 { (per_90 * share * rng.range_f32(0.7, 1.3)).round() as u16 };

    let (passes, tackles, interceptions, shots) = match group {
        PlayerFieldPositionGroup::Goalkeeper => (28.0 * dominance, 0.3, 0.6, 0.0),
        PlayerFieldPositionGroup::Defender => (48.0 * dominance, 2.6, 2.4, 0.6),
        PlayerFieldPositionGroup::Midfielder => (62.0 * dominance, 2.2, 1.8, 1.3),
        PlayerFieldPositionGroup::Forward => (32.0 * dominance, 0.9, 0.7, 2.6),
    };

    let passes_attempted = scale(passes);
    // Completion climbs with the team's control of the game.
    let completion = (0.68 + (dominance - 1.0) * 0.4).clamp(0.55, 0.92);
    let shots_total = scale(shots);
    let shots_on_target = (shots_total as f32 * 0.4).round() as u16;

    // Keepers: what they faced is the other side's shooting, and what
    // they saved is that minus what went in.
    let (saves, shots_faced) = if group == PlayerFieldPositionGroup::Goalkeeper {
        let faced = scale(4.5 / dominance) + goals_conceded as u16;
        (faced.saturating_sub(goals_conceded as u16), faced)
    } else {
        (0, 0)
    };

    PlayerMatchEndStats {
        shots_on_target,
        shots_total,
        passes_attempted,
        passes_completed: (passes_attempted as f32 * completion).round() as u16,
        tackles: scale(tackles),
        interceptions: scale(interceptions),
        saves,
        shots_faced,
        goals: 0,
        assists: 0,
        match_rating: 0.0,
        raw_match_rating: 0.0,
        xg: shots_total as f32 * 0.11,
        position_group: group,
        fouls: scale(1.1),
        yellow_cards: 0,
        red_cards: 0,
        minutes_played,
        key_passes: scale(0.9),
        progressive_passes: scale(3.2),
        progressive_carries: scale(2.1),
        successful_dribbles: scale(0.8),
        attempted_dribbles: scale(1.4),
        successful_pressures: scale(4.0),
        pressures: scale(11.0),
        blocks: scale(0.7),
        clearances: scale(if group == PlayerFieldPositionGroup::Defender {
            3.4
        } else {
            0.8
        }),
        passes_into_box: scale(0.9),
        crosses_attempted: scale(1.2),
        crosses_completed: scale(0.35),
        xg_chain: shots_total as f32 * 0.08,
        xg_buildup: shots_total as f32 * 0.05,
        miscontrols: scale(1.3),
        heavy_touches: scale(0.9),
        carry_distance: (scale(120.0)) as u32,
        errors_leading_to_shot: 0,
        errors_leading_to_goal: 0,
        xg_prevented: 0.0,
        xg_faced: shots_faced as f32 * 0.11,
        offsides: scale(0.4),
        own_goals: 0,
        zone_stats: Default::default(),
    }
}

/// Draw one player from `pool`, weighted by `weight` and by how long he
/// was on the pitch. Returns `None` for an empty pool — a club with no
/// eligible players simply records no scorer, which is the honest
/// outcome in a world where thin squads are allowed to be thin.
fn weighted_pick(
    rng: &MatchRng,
    pool: &[&MatchPlayer],
    minutes: &HashMap<u32, u16>,
    weight: fn(&MatchPlayer) -> f32,
) -> Option<u32> {
    let weights: Vec<f32> = pool
        .iter()
        .map(|player| {
            let played = minutes.get(&player.id).copied().unwrap_or(0) as f32;
            weight(player) * (played / FULL_TIME_MIN as f32)
        })
        .collect();

    let total: f32 = weights.iter().sum();

    if total <= 0.0 {
        return None;
    }

    let mut roll = rng.range_f32(0.0, total);

    for (player, weight) in pool.iter().zip(weights.iter()) {
        roll -= weight;
        if roll <= 0.0 {
            return Some(player.id);
        }
    }

    pool.last().map(|player| player.id)
}

/// Who scores: forwards mostly, midfielders often, defenders from set
/// pieces, keepers effectively never. Multiplied by finishing so the
/// best striker in the division outscores the worst one.
fn scoring_weight(player: &MatchPlayer) -> f32 {
    let positional = match player.tactical_position.current_position.position_group() {
        PlayerFieldPositionGroup::Forward => 5.0,
        PlayerFieldPositionGroup::Midfielder => 2.0,
        PlayerFieldPositionGroup::Defender => 0.55,
        PlayerFieldPositionGroup::Goalkeeper => 0.01,
    };

    positional * (player.skills.technical.finishing / 10.0).max(0.1)
}

/// Who assists: the same shape shifted toward midfield, weighted by
/// passing rather than finishing.
fn assist_weight(player: &MatchPlayer) -> f32 {
    let positional = match player.tactical_position.current_position.position_group() {
        PlayerFieldPositionGroup::Forward => 2.4,
        PlayerFieldPositionGroup::Midfielder => 3.4,
        PlayerFieldPositionGroup::Defender => 1.1,
        PlayerFieldPositionGroup::Goalkeeper => 0.05,
    };

    positional * (player.skills.technical.passing / 10.0).max(0.1)
}

/// Who gets booked: whoever does the tackling. Aggression raises it,
/// composure does not lower it — the engine's own foul model behaves
/// the same way.
fn card_weight(player: &MatchPlayer) -> f32 {
    let positional = match player.tactical_position.current_position.position_group() {
        PlayerFieldPositionGroup::Forward => 1.4,
        PlayerFieldPositionGroup::Midfielder => 2.6,
        PlayerFieldPositionGroup::Defender => 2.8,
        PlayerFieldPositionGroup::Goalkeeper => 0.2,
    };

    positional * (player.skills.mental.aggression / 10.0).max(0.2)
}

/// Man of the match: best rating on the pitch. The real engine weighs
/// goals and the result too, but the rating already carries both.
fn pick_motm(stats: &HashMap<u32, PlayerMatchEndStats>) -> Option<u32> {
    stats
        .iter()
        .max_by(|left, right| left.1.match_rating.total_cmp(&right.1.match_rating))
        .map(|(id, _)| *id)
}
