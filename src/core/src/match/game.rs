use super::engine::FootballEngine;
use crate::MatchRuntime;
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

        let match_recordings = MatchRuntime::recordings_mode() && !self.is_friendly;
        // Modified from upstream: route through the seeded entry point so a
        // stamped per-fixture seed makes the match reproducible.
        let match_result = FootballEngine::<840, 545>::play_seeded(
            self.home_squad,
            self.away_squad,
            match_recordings,
            self.is_friendly,
            self.is_knockout,
            self.seed,
        );

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
