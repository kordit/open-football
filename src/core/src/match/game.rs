use super::engine::FootballEngine;
use crate::MatchRuntime;
use crate::r#match::quick::QuickMatch;
use crate::r#match::{MatchResult, MatchSquad};
use log::debug;

#[derive(Debug, Clone)]
pub struct Match {
    id: String,
    league_id: u32,
    league_slug: String,
    pub home_squad: MatchSquad,
    pub away_squad: MatchSquad,
    pub is_friendly: bool,
    /// Knockout-format match — if level after 90 min, play extra time;
    /// if still level, resolve on penalties.
    pub is_knockout: bool,
    /// Added in this fork: engine seed for this fixture. `Some(_)` pins the
    /// match engine's owned RNG so the same squads + seed replay
    /// bit-identically; `None` keeps legacy OS-entropy behaviour. Stamped
    /// by the world matchday dispatch from the pinned sim seed.
    pub seed: Option<u64>,
    /// Added in this fork: per-match recording override. When true this
    /// fixture records position data even if the GLOBAL
    /// `MatchRuntime::recordings_mode()` flag is off. Stamped by the
    /// world matchday dispatch for matches involving the managed club
    /// (career mode replays). Friendlies never record either way.
    pub record: bool,
}

impl Match {
    pub fn make(
        id: String,
        league_id: u32,
        league_slug: &str,
        home_squad: MatchSquad,
        away_squad: MatchSquad,
        is_friendly: bool,
    ) -> Self {
        Match {
            id,
            league_id,
            league_slug: String::from(league_slug),
            home_squad,
            away_squad,
            is_friendly,
            is_knockout: false,
            seed: None,
            record: false,
        }
    }

    pub fn make_knockout(
        id: String,
        league_id: u32,
        league_slug: &str,
        home_squad: MatchSquad,
        away_squad: MatchSquad,
    ) -> Self {
        Match {
            id,
            league_id,
            league_slug: String::from(league_slug),
            home_squad,
            away_squad,
            is_friendly: false,
            is_knockout: true,
            seed: None,
            record: false,
        }
    }

    /// Accessors for the private identity fields (used by the
    /// distributed worker wire layer to flatten a Match across the
    /// network). Internal mutation still flows through `make` /
    /// `make_knockout`, so keeping the fields private elsewhere is
    /// intentional.
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn league_id(&self) -> u32 {
        self.league_id
    }

    pub fn league_slug(&self) -> &str {
        &self.league_slug
    }

    pub fn play(self) -> MatchResult {
        let home_team_id = self.home_squad.team_id;
        let home_team_name = String::from(&self.home_squad.team_name);

        let away_team_id = self.away_squad.team_id;
        let away_team_name = String::from(&self.away_squad.team_name);

        // Modified from upstream: per-match `record` flag (managed-club
        // replays) enables recording even when the global flag is off.
        let match_recordings =
            (MatchRuntime::recordings_mode() || self.record) && !self.is_friendly;

        // Added in this fork: everything the manager is not involved in can
        // be resolved statistically instead of ticked out. `record` is the
        // existing managed-club marker — the world matchday stamps it on any
        // fixture with the player's team on either side (see
        // `simulator/matchday.rs`), which is exactly the set that must keep
        // full fidelity: those are the matches with a replay to watch and a
        // result the manager is answerable for. Recording implies the real
        // engine either way, since a quick result has no positions to record.
        let match_result = if crate::settings::quick_other_matches()
            && !self.record
            && !match_recordings
        {
            QuickMatch::play(
                self.home_squad,
                self.away_squad,
                self.seed,
                self.is_knockout,
            )
        } else {
            // Modified from upstream: route through the seeded entry point so a
            // stamped per-fixture seed makes the match reproducible.
            FootballEngine::<840, 545>::play_seeded(
                self.home_squad,
                self.away_squad,
                match_recordings,
                self.is_friendly,
                self.is_knockout,
                self.seed,
            )
        };

        let score = match_result.score.as_ref().expect("no score");

        if score.had_shootout() {
            debug!(
                "match played: {} {}:{} {} ({}:{} pens)",
                home_team_name,
                score.home_team.get(),
                away_team_name,
                score.away_team.get(),
                score.home_shootout,
                score.away_shootout,
            );
        } else {
            debug!(
                "match played: {} {}:{} {}",
                home_team_name,
                score.home_team.get(),
                away_team_name,
                score.away_team.get(),
            );
        }

        MatchResult {
            id: self.id,
            league_id: self.league_id,
            league_slug: String::from(&self.league_slug),
            home_team_id,
            away_team_id,
            score: score.clone(),
            details: Some(match_result),
            friendly: self.is_friendly,
        }
    }
}

// Added in this fork: guards for the per-match recording flag defaults.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tactics;
    use crate::club::team::tactics::MatchTacticType;

    fn squad(team_id: u32) -> MatchSquad {
        MatchSquad {
            team_id,
            team_name: format!("Team{}", team_id),
            tactics: Tactics::new(MatchTacticType::T442),
            main_squad: Vec::new(),
            substitutes: Vec::new(),
            captain_id: None,
            vice_captain_id: None,
            penalty_taker_id: None,
            free_kick_taker_id: None,
            selection_omissions: vec![],
            coach_snapshot: None,
        }
    }

    #[test]
    fn make_defaults_record_to_false() {
        let m = Match::make("m1".to_string(), 1, "league", squad(10), squad(20), false);
        assert!(!m.record);
        assert!(m.seed.is_none());
    }

    #[test]
    fn make_knockout_defaults_record_to_false() {
        let m = Match::make_knockout("m2".to_string(), 1, "league", squad(10), squad(20));
        assert!(!m.record);
    }
}
