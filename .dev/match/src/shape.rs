//! Added in this fork: the shape diagnostic (`dev_match shape N [level]`).
//!
//! Kept out of `main.rs` deliberately. That file is upstream's and already
//! runs to five and a half thousand lines; a new diagnostic has no business
//! making it longer. Everything the mode needs is in here — the squad
//! builder, the run loop, and the printing.

use core::club::team::tactics::{MatchTacticType, Tactics};
use core::r#match::FootballEngine;
use core::r#match::MatchSquad;
use core::r#match::player::MatchPlayer;
use core::MatchRuntime;
use rayon::prelude::*;
use std::collections::HashMap;

use crate::{POSITIONS_442, generate_player};

/// Added in this fork: does the match look like football from above?
///
/// The 2D pitch made two complaints legible that a scoreline never could:
/// everybody sprinting at the ball, and nobody holding the touchline. Both
/// are shape properties, both are invisible in `stats`, and both are cheap
/// to read straight off the position recording.
///
/// Prints against real-football reference bands so a run is self-judging.
pub fn run_shape(n_matches: usize, level: u8) {
    // State tracking is what lets the report name the states a pile-up is
    // made of, rather than just counting bodies.
    MatchRuntime::set_events_mode(true);

    // Positions are only recorded when the match is asked to record.
    let rows: Vec<core::r#match::result::ShapeReport> = (0..n_matches)
        .into_par_iter()
        .map(|i| {
            let home = make_squad_shape(1, level, i);
            let away = make_squad_shape(2, level, i + 1000);
            let result = FootballEngine::<840, 545>::play(home, away, true, false, false);

            // Outfield only: a goalkeeper standing on his line would flatter
            // every width number in the report.
            let home_ids: Vec<u32> = (101..=110).collect();
            let away_ids: Vec<u32> = (201..=210).collect();

            result
                .position_data
                .shape_report(
                    &home_ids,
                    &away_ids,
                    &(101..=104).collect::<Vec<u32>>(),
                    &(201..=204).collect::<Vec<u32>>(),
                    840.0,
                    545.0,
                    500,
                )
        })
        .collect();

    let n = rows.iter().filter(|r| r.samples > 0).count().max(1) as f64;
    let mean_within = rows.iter().map(|r| r.mean_within_10m).sum::<f64>() / n;
    let scrum = rows.iter().map(|r| r.scrum_share as f64).sum::<f64>() / n;
    let spread = rows
        .iter()
        .map(|r| (r.lateral_spread_m[0] + r.lateral_spread_m[1]) as f64 / 2.0)
        .sum::<f64>()
        / n;
    let widest = rows
        .iter()
        .map(|r| (r.widest_player_m[0] + r.widest_player_m[1]) as f64 / 2.0)
        .sum::<f64>()
        / n;

    // Back line vs ball: only meaningful over frames where the ball was
    // actually in someone's defensive third, so weight by those frames
    // rather than by match.
    let deep: u32 = rows.iter().map(|r| r.deep_frames).sum();
    let line_ahead: u32 = rows.iter().map(|r| r.line_ahead_frames).sum();
    let defenders_ahead = if deep > 0 {
        rows.iter()
            .map(|r| r.mean_defenders_ahead * r.deep_frames as f64)
            .sum::<f64>()
            / deep as f64
    } else {
        0.0
    };

    println!("Shape report: {n_matches} match(es) at level {level}\n");
    println!(
        "  players within 10 m of the ball   {mean_within:6.2}    (real football: 2–4)"
    );
    println!(
        "  share of match with 8+ in that circle {:5.1}%    (real football: ~0% outside set pieces)",
        scrum * 100.0
    );
    println!(
        "  lateral spread (sd of y), per side {spread:6.2} m  (a 4-4-2 holding shape: 13–18 m)"
    );
    println!(
        "  widest player from centre line     {widest:6.2} m  (68 m pitch = 34 m to the touchline; \
holding width: 20 m+)"
    );

    let mut states: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    for row in &rows {
        for (name, count) in &row.scrum_states {
            *states.entry(name.clone()).or_insert(0) += count;
        }
    }
    if !states.is_empty() {
        let total: u32 = states.values().sum();
        let mut ranked: Vec<(&String, &u32)> = states.iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(a.1));
        println!("\n  what the pile-ups are made of (states inside the 10 m circle):");
        for (name, count) in ranked.iter().take(10) {
            println!(
                "    {:28} {:5.1}%",
                name,
                **count as f32 / total as f32 * 100.0
            );
        }
    }

    println!(
        "\n  ball in own third, defenders caught upfield of it   {:5.2}    (football: <1)",
        defenders_ahead
    );
    println!(
        "  share of those frames with 2+ defenders upfield     {:5.1}%   (football: ~0%)",
        if deep > 0 {
            line_ahead as f32 / deep as f32 * 100.0
        } else {
            0.0
        }
    );

    let mut verdict = Vec::new();
    if deep > 0 && line_ahead as f32 / deep as f32 > 0.10 {
        verdict.push("BACK LINE AHEAD OF THE BALL: defenders standing upfield of their own siege");
    }
    if mean_within > 5.0 {
        verdict.push("BALL SWARM: too many bodies at the ball");
    }
    if scrum > 0.05 {
        verdict.push("SCRUM: eight-plus at the ball for a visible share of the match");
    }
    if widest < 18.0 {
        verdict.push("NO WIDTH: nobody is holding the touchline");
    }
    println!();
    if verdict.is_empty() {
        println!("  verdict: shape looks like football");
    } else {
        for line in verdict {
            println!("  verdict: {line}");
        }
    }
}

/// A plain 4-4-2 with stable ids, so the shape report can name the outfield
/// ten per side without threading the squad back out of the engine.
fn make_squad_shape(team_id: u32, level: u8, seed: usize) -> MatchSquad {
    let base_id = team_id * 100;
    let cond = |k: usize| 8200 + ((seed * 7 + k * 131) % 1400) as i16;

    let main_squad: Vec<MatchPlayer> = POSITIONS_442
        .iter()
        .enumerate()
        .map(|(i, &pos)| {
            // id 100 is the keeper, 101..110 the outfield ten.
            let mut player = generate_player(base_id + i as u32, pos, level);
            player.player_attributes.condition = cond(i);
            MatchPlayer::from_player(team_id, &player, pos, false, None)
        })
        .collect();

    MatchSquad {
        team_id,
        team_name: format!("Team {team_id}"),
        tactics: Tactics::new(MatchTacticType::T442),
        main_squad,
        substitutes: Vec::new(),
        captain_id: None,
        vice_captain_id: None,
        penalty_taker_id: None,
        free_kick_taker_id: None,
        selection_omissions: Vec::new(),
        coach_snapshot: None,
    }
}

