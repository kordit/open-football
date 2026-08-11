//! Added in this fork: the manager's plan on two axes.
//!
//! # Why two axes and not one preset
//!
//! The preset table this replaces mixed both halves of a plan into one
//! row: "Gegenpress" decided how the side attacked *and* how it defended,
//! so a manager who wanted to counter-attack out of a high press had
//! nothing to pick. Real plans are chosen separately — how we hurt them,
//! and how we stop them — and the interesting football is in the
//! *crossing* of the two: our attack against their defence.
//!
//! [`AttackingPlan`] owns the five with-the-ball dials, [`DefensivePlan`]
//! the five without-the-ball dials, and [`TacticalPlan`] is simply the
//! pair. Together they fill exactly the same [`TeamInstructions`] the
//! tactical bus already consumes — nothing new was invented downstream,
//! and every dial keeps the consumers listed in `team_instructions`.
//!
//! # Rock, paper, scissors
//!
//! No plan is safe. [`AttackingPlan::against`] states which defence each
//! attack takes apart and which one smothers it, in the terms anyone who
//! watches football already uses: a counter punishes a side that pushed
//! up, a long ball goes over a press, a deep block has nothing to counter
//! and nothing to run behind. The engine does not apply this as a
//! multiplier on results — it is a statement of intent that the dials
//! then have to actually produce on the pitch, and `dev_match` measures
//! whether they did.

use serde::{Deserialize, Serialize};

use crate::club::team::tactics::team_instructions::TeamInstructions;

/// How the side tries to score.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttackingPlan {
    Balanced,
    Possession,
    Direct,
    Counter,
    Wings,
}

/// How the side tries to stop the other one scoring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DefensivePlan {
    MidBlock,
    HighPress,
    LowBlock,
    ManMarking,
    OffsideTrap,
}

/// The pair a manager actually sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TacticalPlan {
    pub attack: AttackingPlan,
    pub defence: DefensivePlan,
}

impl Default for TacticalPlan {
    fn default() -> Self {
        TacticalPlan {
            attack: AttackingPlan::Balanced,
            defence: DefensivePlan::MidBlock,
        }
    }
}

impl AttackingPlan {
    pub const ALL: [AttackingPlan; 5] = [
        AttackingPlan::Balanced,
        AttackingPlan::Possession,
        AttackingPlan::Direct,
        AttackingPlan::Counter,
        AttackingPlan::Wings,
    ];

    pub fn key(&self) -> &'static str {
        match self {
            AttackingPlan::Balanced => "balanced",
            AttackingPlan::Possession => "possession",
            AttackingPlan::Direct => "direct",
            AttackingPlan::Counter => "counter",
            AttackingPlan::Wings => "wings",
        }
    }

    pub fn from_key(raw: &str) -> Option<AttackingPlan> {
        AttackingPlan::ALL.into_iter().find(|p| p.key() == raw)
    }

    /// The five with-the-ball dials: tempo, directness, width, risk, support.
    ///
    /// Read a row as a sentence. Possession: slow it down, work it short,
    /// stretch the pitch to open lanes, take few chances, send a moderate
    /// number of bodies forward.
    fn dials(&self) -> (f32, f32, f32, f32, f32) {
        match self {
            //                  tempo directness width  risk  support
            AttackingPlan::Balanced => (0.50, 0.50, 0.50, 0.50, 0.50),
            AttackingPlan::Possession => (0.35, 0.12, 0.62, 0.30, 0.55),
            AttackingPlan::Direct => (0.72, 0.92, 0.45, 0.60, 0.45),
            AttackingPlan::Counter => (0.82, 0.72, 0.42, 0.64, 0.34),
            AttackingPlan::Wings => (0.60, 0.55, 0.94, 0.50, 0.64),
        }
    }

    /// Whether this attack is built to hurt that defence.
    ///
    /// `+1` we take them apart, `-1` they smother us, `0` neither.
    pub fn against(&self, defence: DefensivePlan) -> i8 {
        use AttackingPlan::*;
        use DefensivePlan::*;

        match (self, defence) {
            // Cierpliwe rozegranie dusi sie pod pressingiem i o niski blok,
            // ale spokojnie omija pulapke ofsajdowa.
            (Possession, HighPress) => -1,
            (Possession, LowBlock) => -1,
            (Possession, OffsideTrap) => 1,

            // Dlugie podanie mija press i krycie, i wchodzi za plecy pulapki
            // — ale niski blok wybija dosrodkowania.
            (Direct, HighPress) => 1,
            (Direct, ManMarking) => 1,
            (Direct, OffsideTrap) => 1,
            (Direct, LowBlock) => -1,

            // Kontra karze wysunieta druzyne i nie ma czego kontrowac
            // przeciw blokowi; krycie i pulapka lapia wybiegajacych.
            (Counter, HighPress) => 1,
            (Counter, LowBlock) => -1,
            (Counter, ManMarking) => -1,
            (Counter, OffsideTrap) => -1,

            // Skrzydla przenosza gre ponad zamknietym srodkiem i obok linii
            // spalonego, ale press odcina boki, a obronca idzie za skrzydlowym.
            (Wings, LowBlock) => 1,
            (Wings, OffsideTrap) => 1,
            (Wings, HighPress) => -1,
            (Wings, ManMarking) => -1,

            _ => 0,
        }
    }
}

impl DefensivePlan {
    pub const ALL: [DefensivePlan; 5] = [
        DefensivePlan::MidBlock,
        DefensivePlan::HighPress,
        DefensivePlan::LowBlock,
        DefensivePlan::ManMarking,
        DefensivePlan::OffsideTrap,
    ];

    pub fn key(&self) -> &'static str {
        match self {
            DefensivePlan::MidBlock => "mid_block",
            DefensivePlan::HighPress => "high_press",
            DefensivePlan::LowBlock => "low_block",
            DefensivePlan::ManMarking => "man_marking",
            DefensivePlan::OffsideTrap => "offside_trap",
        }
    }

    pub fn from_key(raw: &str) -> Option<DefensivePlan> {
        DefensivePlan::ALL.into_iter().find(|p| p.key() == raw)
    }

    /// The five without-the-ball dials: press, line height, compactness,
    /// counter-press, tackle aggression.
    fn dials(&self) -> (f32, f32, f32, f32, f32) {
        match self {
            //                      press  line  compact c-press aggression
            DefensivePlan::MidBlock => (0.50, 0.50, 0.50, 0.50, 0.50),
            DefensivePlan::HighPress => (0.95, 0.88, 0.75, 0.95, 0.70),
            DefensivePlan::LowBlock => (0.15, 0.10, 0.92, 0.15, 0.45),
            // Krycie idzie za czlowiekiem, wiec linia rozciaga sie bardziej
            // niz w kazdym innym planie, a wejscia sa ostrzejsze.
            DefensivePlan::ManMarking => (0.62, 0.45, 0.30, 0.42, 0.80),
            // Pulapka stoi wysoko i plasko; nie presuje, tylko czeka na ruch.
            DefensivePlan::OffsideTrap => (0.52, 0.94, 0.64, 0.50, 0.32),
        }
    }
}

impl TacticalPlan {
    pub fn new(attack: AttackingPlan, defence: DefensivePlan) -> Self {
        TacticalPlan { attack, defence }
    }

    /// Fill the bus input from both halves of the plan.
    pub fn instructions(&self) -> TeamInstructions {
        let (tempo, directness, width, risk, support) = self.attack.dials();
        let (press, line_height, compactness, counter_press, aggression) = self.defence.dials();

        TeamInstructions {
            tempo,
            directness,
            width,
            risk,
            support,
            press,
            line_height,
            compactness,
            counter_press,
            aggression,
        }
    }

    /// Our attack against their defence.
    pub fn edge_against(&self, rival: &TacticalPlan) -> i8 {
        self.attack.against(rival.defence)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_attack_has_a_defence_that_beats_it_and_one_it_beats() {
        for attack in AttackingPlan::ALL {
            if attack == AttackingPlan::Balanced {
                continue;
            }

            let scores: Vec<i8> = DefensivePlan::ALL
                .into_iter()
                .map(|d| attack.against(d))
                .collect();

            assert!(scores.contains(&1), "{:?} nie bije zadnej obrony", attack);
            assert!(scores.contains(&-1), "{:?} nie ma swojej trucizny", attack);
        }
    }

    #[test]
    fn every_defence_is_beaten_by_something_and_smothers_something() {
        for defence in DefensivePlan::ALL {
            if defence == DefensivePlan::MidBlock {
                continue;
            }

            let scores: Vec<i8> = AttackingPlan::ALL
                .into_iter()
                .map(|a| a.against(defence))
                .collect();

            assert!(scores.contains(&1), "{:?} nikt nie rozbiera", defence);
            assert!(scores.contains(&-1), "{:?} nikogo nie gasi", defence);
        }
    }

    #[test]
    fn the_plan_fills_every_dial() {
        let plan = TacticalPlan::new(AttackingPlan::Counter, DefensivePlan::LowBlock);
        let dials = plan.instructions();

        // Kontra z niskiego bloku: szybko do przodu, glęboko przy swojej bramce.
        assert!(dials.tempo > 0.7);
        assert!(dials.line_height < 0.2);
        assert!(dials.compactness > 0.8);
    }
}
