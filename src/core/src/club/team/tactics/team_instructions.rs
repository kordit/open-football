//! Added in this fork: team instructions — the dials a manager sets before
//! kickoff, and the presets that move them in recognisable groups.
//!
//! # Why this exists
//!
//! The match engine already runs on a team-wide tactical bus
//! (`TeamTacticalState`): press intensity, defensive line, compactness,
//! width, tempo, risk appetite, build-up patience and rest defence are
//! recomputed every ten ticks and read by the defender, midfielder,
//! forward and goalkeeper state machines. That bus is the reason football
//! played by two sides looks like two *plans* rather than twenty-two
//! independent agents.
//!
//! What it had no input for was a human. Every signal on the bus was
//! derived — from the formation (`tactical_style()`), the scoreline, the
//! clock, and the squad's own skill composites. A manager could pick a
//! shape and nothing else, and picking 4-4-2 forced `Balanced` whether he
//! wanted a low block or a high press out of it.
//!
//! `TeamInstructions` is the missing input. Every dial here lands on a bus
//! signal that already has consumers — nothing is decorative, and nothing
//! needed a new behaviour to be invented for it:
//!
//! | dial | bus signal | who reads it |
//! |---|---|---|
//! | `tempo` | `tempo` | pass evaluator, forward hold-before-shot |
//! | `directness` | `build_up_patience` (inverted) | GK distribution, MF recycle, pass evaluator |
//! | `width` | `team_width_target` | off-ball movement lateral target, FB overlaps |
//! | `risk` | `risk_appetite` | pass evaluator forward/backward bias, GK sweeping |
//! | `support` | `rest_defense_count` (inverted) | FB/CB overlap gate |
//! | `press` | `press_intensity` | every pressing / closing-down state |
//! | `line_height` | `defensive_line_x` | back line's shared reference |
//! | `compactness` | `compactness_target` | CB spacing, pivot positioning |
//! | `counter_press` | defensive-transition window | counter-press burst after losing the ball |
//! | `aggression` | `tackle_aggression` | tackle-vs-jockey choice, and the fouls that follow |
//!
//! # Absent means "as before"
//!
//! `Tactics::instructions` is an `Option`, and every dial is applied by
//! steering an already-computed value toward it. A side with no
//! instructions — which is every AI club in the world, on every matchday —
//! computes exactly the numbers it computed before this module existed.
//! That is deliberate: this is a manager's control panel, not a change to
//! how football is simulated.

use serde::{Deserialize, Serialize};

/// How hard a manager's dial pulls against the engine's own read.
///
/// Not 1.0. The bus signals fold in things the manager does not control
/// and should not be able to wish away — fatigue suppressing a press, a
/// weak back line refusing to hold a high line, the scoreline dragging a
/// leading side backwards. At 0.65 the instruction is clearly the loudest
/// voice in the room while those still speak; at 1.0 a manager could order
/// an exhausted side to press for ninety minutes and get it.
const STEER_WEIGHT: f32 = 0.65;

/// Pull `computed` toward `wanted`, if a manager asked for anything.
///
/// The one place the override arithmetic lives. Callers on the tactical
/// bus pass their own freshly-computed value and get back either the same
/// number (no instructions) or a steered one.
pub fn steer(computed: f32, wanted: Option<f32>) -> f32 {
    match wanted {
        Some(target) => (computed + (target.clamp(0.0, 1.0) - computed) * STEER_WEIGHT).clamp(0.0, 1.0),
        None => computed,
    }
}

/// The ten dials, all on 0.0–1.0, split by phase of play.
///
/// Naming is from the manager's side of the touchline, not the engine's:
/// `directness` rises toward the long ball, `support` rises toward bodies
/// in the box. Where a dial is the inverse of the bus signal it drives,
/// the inversion happens at the wiring point, not here.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TeamInstructions {
    // ── with the ball ──────────────────────────────────────────────────
    /// 0.0 slow it down · 1.0 play at speed.
    pub tempo: f32,
    /// 0.0 work it short · 1.0 go long and direct.
    pub directness: f32,
    /// 0.0 narrow, everything through the middle · 1.0 stretch the pitch.
    pub width: f32,
    /// 0.0 safety first · 1.0 try the pass that breaks the line.
    pub risk: f32,
    /// 0.0 keep everyone home · 1.0 get bodies forward.
    pub support: f32,

    // ── without the ball ───────────────────────────────────────────────
    /// 0.0 stand off · 1.0 hunt the ball.
    pub press: f32,
    /// 0.0 drop to the edge of the box · 1.0 squeeze onto the halfway line.
    pub line_height: f32,
    /// 0.0 hold the width · 1.0 shrink the space between every player.
    pub compactness: f32,
    /// 0.0 fall back into shape · 1.0 win it back inside five seconds.
    pub counter_press: f32,
    /// 0.0 stay on your feet · 1.0 get stuck in (and take the cards).
    pub aggression: f32,
}

impl Default for TeamInstructions {
    /// Every dial in the middle: the bus then passes each computed value
    /// through untouched, because `steer()` toward 0.5 from 0.5 is a no-op
    /// only when the caller had nothing to say. This is what a side with
    /// no plan set looks like.
    fn default() -> Self {
        TeamInstructions {
            tempo: 0.5,
            directness: 0.5,
            width: 0.5,
            risk: 0.5,
            support: 0.5,
            press: 0.5,
            line_height: 0.5,
            compactness: 0.5,
            counter_press: 0.5,
            aggression: 0.5,
        }
    }
}

impl TeamInstructions {
    /// Clamp every dial into range. Called on anything arriving over HTTP:
    /// the wire is not a trusted source of floats.
    pub fn sanitised(mut self) -> Self {
        for dial in [
            &mut self.tempo,
            &mut self.directness,
            &mut self.width,
            &mut self.risk,
            &mut self.support,
            &mut self.press,
            &mut self.line_height,
            &mut self.compactness,
            &mut self.counter_press,
            &mut self.aggression,
        ] {
            *dial = if dial.is_finite() {
                dial.clamp(0.0, 1.0)
            } else {
                0.5
            };
        }
        self
    }

    /// How much of this plan the squad can actually carry out, 0.0–1.0.
    ///
    /// A tactic is a demand on players, and a demand nobody can meet is
    /// not a tactic — it is a way of losing. This is the number that makes
    /// "possession needs passers" true rather than decorative: it reads
    /// the squad composites the engine already computes and asks whether
    /// they cover what the dials are asking for.
    ///
    /// The four composites arrive on 0.0–1.0 from
    /// `TeamSkillAggregates` — the same values the engine uses to decide
    /// whether a back line can hold a high line at all.
    pub fn execution_competence(
        &self,
        build_up_quality: f32,
        press_quality: f32,
        defensive_quality: f32,
        attacking_quality: f32,
    ) -> f32 {
        // Each demand pairs a dial with the composite that has to answer
        // it. Weight = how loudly the dial is asking; deficit = how far
        // short the squad falls of what a full ask needs.
        let demands = [
            // Playing out from the back under a slow patient build-up is
            // the passing-est thing a team can do.
            ((1.0 - self.directness) * (1.0 - self.tempo * 0.4), build_up_quality),
            // Hunting the ball and winning it back instantly both run on
            // legs and appetite for the chase.
            (self.press.max(self.counter_press), press_quality),
            // A high line is a bet on the back four reading it right.
            (self.line_height, defensive_quality),
            // Bodies forward at speed only pays if the bodies can finish.
            (self.support * self.risk.max(0.4), attacking_quality),
        ];

        let mut weighted_shortfall = 0.0;
        let mut total_weight = 0.0;
        for (weight, capability) in demands {
            let weight = weight.clamp(0.0, 1.0);
            // A demand at w needs capability ≥ w to be met cleanly.
            weighted_shortfall += weight * (weight - capability.clamp(0.0, 1.0)).max(0.0);
            total_weight += weight;
        }

        if total_weight <= f32::EPSILON {
            return 1.0;
        }

        (1.0 - weighted_shortfall / total_weight).clamp(0.0, 1.0)
    }
}

/// Uwaga: jednoosiowe presety zastapil dwuosiowy `plan::TacticalPlan` —
/// atak i obrona wybierane osobno. Ten modul zostaje wlascicielem samych
/// pokretel i arytmetyki sterowania; kto je ustawia, mieszka w `plan.rs`.


#[cfg(test)]
mod tests {
    use super::*;
    use crate::club::team::tactics::plan::{AttackingPlan, DefensivePlan, TacticalPlan};

    fn plan(attack: AttackingPlan, defence: DefensivePlan) -> TeamInstructions {
        TacticalPlan::new(attack, defence).instructions()
    }

    #[test]
    fn no_instructions_leaves_the_computed_value_alone() {
        assert_eq!(steer(0.37, None), 0.37);
    }

    #[test]
    fn a_dial_pulls_the_computed_value_most_of_the_way() {
        // Asked for 1.0 against a computed 0.0: lands at the steer weight,
        // not at 1.0 — fatigue and scoreline keep their say.
        let steered = steer(0.0, Some(1.0));
        assert!(steered > 0.6 && steered < 0.7, "{steered}");
    }

    #[test]
    fn wire_values_out_of_range_are_pulled_back_in() {
        let clean = TeamInstructions {
            tempo: 7.0,
            press: -3.0,
            line_height: f32::NAN,
            ..TeamInstructions::default()
        }
        .sanitised();

        assert_eq!(clean.tempo, 1.0);
        assert_eq!(clean.press, 0.0);
        assert_eq!(clean.line_height, 0.5);
    }

    #[test]
    fn a_squad_that_can_do_everything_executes_everything() {
        let plan = crate::club::team::tactics::plan::TacticalPlan::new(
            crate::club::team::tactics::plan::AttackingPlan::Balanced,
            crate::club::team::tactics::plan::DefensivePlan::HighPress,
        )
        .instructions();
        assert!(plan.execution_competence(1.0, 1.0, 1.0, 1.0) > 0.99);
    }

    #[test]
    fn possession_punishes_bad_passers_and_a_low_block_does_not() {
        // The same threadbare squad, two plans. Playing out from the back
        // asks the thing they cannot do; sitting deep asks nothing of it.
        let poor_on_the_ball = (0.15, 0.6, 0.6, 0.6);

        let possession = plan(AttackingPlan::Possession, DefensivePlan::HighPress)
            .execution_competence(
                poor_on_the_ball.0,
                poor_on_the_ball.1,
                poor_on_the_ball.2,
                poor_on_the_ball.3,
            );
        let low_block = plan(AttackingPlan::Direct, DefensivePlan::LowBlock).execution_competence(
            poor_on_the_ball.0,
            poor_on_the_ball.1,
            poor_on_the_ball.2,
            poor_on_the_ball.3,
        );

        assert!(
            possession < low_block - 0.1,
            "possession {possession} vs low block {low_block}"
        );
    }

    #[test]
    fn counter_punishes_a_squad_without_legs_less_than_gegenpress_does() {
        let no_engine = (0.6, 0.12, 0.6, 0.6);

        let counter = plan(AttackingPlan::Counter, DefensivePlan::LowBlock)
            .execution_competence(no_engine.0, no_engine.1, no_engine.2, no_engine.3);
        let gegenpress = plan(AttackingPlan::Possession, DefensivePlan::HighPress)
            .execution_competence(no_engine.0, no_engine.1, no_engine.2, no_engine.3);

        assert!(
            gegenpress < counter - 0.1,
            "gegenpress {gegenpress} vs counter {counter}"
        );
    }

    #[test]
    fn every_plan_round_trips_through_its_keys() {
        for attack in AttackingPlan::ALL {
            assert_eq!(AttackingPlan::from_key(attack.key()), Some(attack));
        }

        for defence in DefensivePlan::ALL {
            assert_eq!(DefensivePlan::from_key(defence.key()), Some(defence));
        }
    }
}
