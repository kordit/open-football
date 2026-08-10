//! Added in this fork: world snapshot for the Laravel panel.
//!
//! The panel is Blade, not askama — it renders from a projection of the
//! world kept in Postgres, refreshed from this endpoint after every
//! `POST /api/game/process`. The engine stays the owner of the career
//! state (the OFSV save); Postgres is a read model that may be dropped
//! and rebuilt from a `scope=full` pull at any time.
//!
//! Two knobs keep the payload honest on a full Polish pyramid (hundreds
//! of clubs, tens of thousands of players):
//!
//! * `scope=delta` (default) — league tables and the managed club's
//!   squad. Everything the panel repaints after a tick.
//! * `scope=full` — every club and every player. The initial sync and
//!   the season-rollover resync.
//!
//! `since=YYYY-MM-DD` trims the fixture list, which is otherwise the
//! whole season for every division in the world on every single pull.

pub mod routes;

use crate::GameAppData;
use crate::error::{ApiError, ApiResult};
use async_compression::tokio::write::GzipEncoder;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::http::header::{ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use chrono::NaiveDate;
use core::league::League;
use core::{Country, Player, PlayerPositionType, PlayerStatusType, SimulatorData, Team, TeamType};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

/// Bumped whenever the payload shape changes in a way the importer on
/// the PHP side must notice. The importer refuses versions it does not
/// know rather than silently half-reading a newer snapshot.
const FORMAT_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Request
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct SnapshotQuery {
    /// Only include fixtures on or after this day (`YYYY-MM-DD`).
    pub since: Option<String>,
    /// `delta` (default) or `full`.
    pub scope: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Delta,
    Full,
}

impl Scope {
    fn parse(raw: Option<&str>) -> ApiResult<Self> {
        match raw {
            None | Some("delta") => Ok(Scope::Delta),
            Some("full") => Ok(Scope::Full),
            Some(other) => Err(ApiError::BadRequest(format!(
                "unknown scope \"{other}\" (expected \"delta\" or \"full\")"
            ))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Scope::Delta => "delta",
            Scope::Full => "full",
        }
    }
}

// ---------------------------------------------------------------------------
// Response model
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct WorldSnapshot {
    pub format_version: u32,
    pub engine_version: String,
    pub scope: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<NaiveDate>,
    pub in_game_date: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub managed: Option<ManagedSnapshot>,
    pub leagues: Vec<LeagueSnapshot>,
    pub clubs: Vec<ClubSnapshot>,
    pub matches: Vec<MatchSnapshot>,
    pub players: Vec<PlayerSnapshot>,
    pub counts: SnapshotCounts,
}

#[derive(Serialize)]
pub struct SnapshotCounts {
    pub leagues: usize,
    pub clubs: usize,
    pub teams: usize,
    pub matches: usize,
    pub players: usize,
    pub free_agents: usize,
}

#[derive(Serialize)]
pub struct ManagedSnapshot {
    pub team_id: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub club_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub league_id: Option<u32>,
    pub manager_name: String,
}

#[derive(Serialize)]
pub struct LeagueSnapshot {
    pub id: u32,
    pub name: String,
    pub slug: String,
    pub country_id: u32,
    pub tier: u8,
    pub is_cup: bool,
    pub promotion_spots: u8,
    pub relegation_spots: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promotes_to: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region_code: Option<String>,
    pub table: Vec<TableRowSnapshot>,
}

#[derive(Serialize)]
pub struct TableRowSnapshot {
    pub team_id: u32,
    pub played: u8,
    pub win: u8,
    pub draw: u8,
    pub lost: u8,
    pub goals_for: i32,
    pub goals_against: i32,
    /// Points earned, before deductions.
    pub points: u8,
    pub points_deduction: u8,
}

#[derive(Serialize)]
pub struct ClubSnapshot {
    pub id: u32,
    pub name: String,
    pub country_id: u32,
    pub city_id: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region_code: Option<String>,
    pub balance: i64,
    pub teams: Vec<TeamSnapshot>,
}

#[derive(Serialize)]
pub struct TeamSnapshot {
    pub id: u32,
    pub name: String,
    pub slug: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub league_id: Option<u32>,
    pub is_main: bool,
    pub reputation_home: u16,
    pub reputation_national: u16,
    pub reputation_world: u16,
}

#[derive(Serialize)]
pub struct MatchSnapshot {
    pub id: String,
    pub league_id: u32,
    pub league_slug: String,
    /// Matchday number within the league's schedule.
    pub round: u8,
    pub date: String,
    pub home_team_id: u32,
    pub away_team_id: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub home_goals: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub away_goals: Option<u8>,
}

#[derive(Serialize)]
pub struct PlayerSnapshot {
    pub id: u32,
    /// 0 for a free agent — he belongs to no club.
    pub club_id: u32,
    pub team_id: u32,
    /// True when the player sits in the world's free-agent pool rather than
    /// at a club. Without this the projection would simply lose him the day
    /// his contract expires, which reads as "the player was deleted".
    pub free_agent: bool,
    pub first_name: String,
    pub last_name: String,
    pub birth_date: NaiveDate,
    pub country_id: u32,
    /// Short code of the primary position ("GK", "DC", "AMC", …).
    pub position: &'static str,
    pub positions: Vec<PositionSnapshot>,
    pub current_ability: u8,
    pub potential_ability: u8,
    pub value: u32,
    pub condition: i16,
    pub fitness: i16,
    pub jadedness: i16,
    pub morale: f32,
    pub height: u8,
    pub weight: u8,
    pub preferred_foot: &'static str,
    pub is_injured: bool,
    pub is_banned: bool,
    pub transfer_listed: bool,
    pub loan_listed: bool,
    pub retiring: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contract: Option<ContractSnapshot>,
    pub skills: SkillsSnapshot,
    pub statistics: StatisticsSnapshot,
}

#[derive(Serialize)]
pub struct PositionSnapshot {
    pub position: &'static str,
    pub level: u8,
}

#[derive(Serialize)]
pub struct ContractSnapshot {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shirt_number: Option<u8>,
    pub salary: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started: Option<NaiveDate>,
    pub expiration: NaiveDate,
    pub is_transfer_listed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loan_from_club_id: Option<u32>,
}

#[derive(Serialize)]
pub struct SkillsSnapshot {
    pub technical: TechnicalSnapshot,
    pub mental: MentalSnapshot,
    pub physical: PhysicalSnapshot,
    pub goalkeeping: GoalkeepingSnapshot,
}

#[derive(Serialize)]
pub struct TechnicalSnapshot {
    pub corners: f32,
    pub crossing: f32,
    pub dribbling: f32,
    pub finishing: f32,
    pub first_touch: f32,
    pub free_kicks: f32,
    pub heading: f32,
    pub long_shots: f32,
    pub long_throws: f32,
    pub marking: f32,
    pub passing: f32,
    pub penalty_taking: f32,
    pub tackling: f32,
    pub technique: f32,
}

#[derive(Serialize)]
pub struct MentalSnapshot {
    pub aggression: f32,
    pub anticipation: f32,
    pub bravery: f32,
    pub composure: f32,
    pub concentration: f32,
    pub decisions: f32,
    pub determination: f32,
    pub flair: f32,
    pub leadership: f32,
    pub off_the_ball: f32,
    pub positioning: f32,
    pub teamwork: f32,
    pub vision: f32,
    pub work_rate: f32,
}

#[derive(Serialize)]
pub struct PhysicalSnapshot {
    pub acceleration: f32,
    pub agility: f32,
    pub balance: f32,
    pub jumping: f32,
    pub natural_fitness: f32,
    pub pace: f32,
    pub stamina: f32,
    pub strength: f32,
    pub match_readiness: f32,
}

#[derive(Serialize)]
pub struct GoalkeepingSnapshot {
    pub aerial_reach: f32,
    pub command_of_area: f32,
    pub communication: f32,
    pub eccentricity: f32,
    pub first_touch: f32,
    pub handling: f32,
    pub kicking: f32,
    pub one_on_ones: f32,
    pub passing: f32,
    pub punching: f32,
    pub reflexes: f32,
    pub rushing_out: f32,
    pub throwing: f32,
}

#[derive(Serialize)]
pub struct StatisticsSnapshot {
    pub played: u16,
    pub played_subs: u16,
    pub goals: u16,
    pub assists: u16,
    pub penalties: u16,
    pub yellow_cards: u8,
    pub red_cards: u8,
    pub player_of_the_match: u8,
    pub average_rating: f32,
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

pub async fn world_snapshot_action(
    State(state): State<GameAppData>,
    headers: HeaderMap,
    Query(query): Query<SnapshotQuery>,
) -> ApiResult<Response> {
    let scope = Scope::parse(query.scope.as_deref())?;

    let since = match query.since.as_deref() {
        None => None,
        Some(raw) => Some(NaiveDate::parse_from_str(raw, "%Y-%m-%d").map_err(|_| {
            ApiError::BadRequest(format!("since must be YYYY-MM-DD, got \"{raw}\""))
        })?),
    };

    let snapshot = {
        let guard = state.data.read().await;
        let world = guard.as_ref().ok_or_else(|| {
            ApiError::NotFound("no world loaded — create or load a save first".to_string())
        })?;

        build_snapshot(world, scope, since)
    };

    let body = serde_json::to_vec(&snapshot)
        .map_err(|e| ApiError::InternalError(format!("snapshot serialization failed: {e}")))?;

    let wants_gzip = headers
        .get(ACCEPT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("gzip"));

    if !wants_gzip {
        return Ok(([(CONTENT_TYPE, "application/json")], body).into_response());
    }

    let compressed = gzip(body)
        .await
        .map_err(|e| ApiError::InternalError(format!("snapshot compression failed: {e}")))?;

    Ok((
        [
            (CONTENT_TYPE, "application/json"),
            (CONTENT_ENCODING, "gzip"),
        ],
        compressed,
    )
        .into_response())
}

async fn gzip(bytes: Vec<u8>) -> std::io::Result<Vec<u8>> {
    let mut encoder = GzipEncoder::new(Vec::new());
    encoder.write_all(&bytes).await?;
    encoder.shutdown().await?;

    Ok(encoder.into_inner())
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

fn build_snapshot(world: &SimulatorData, scope: Scope, since: Option<NaiveDate>) -> WorldSnapshot {
    let managed = world.player_manager.as_ref().map(|manager| {
        let team = world.team(manager.team_id);

        ManagedSnapshot {
            team_id: manager.team_id,
            club_id: team.map(|t| t.club_id),
            team_name: team.map(|t| t.name.clone()),
            league_id: team.and_then(|t| t.league_id),
            manager_name: manager.name.clone(),
        }
    });

    // In `delta` scope the player payload is limited to the clubs the
    // human manages. Everything else in the world still moves, but the
    // panel has no screen that reads it before the next full resync.
    let managed_club_id = managed.as_ref().and_then(|m| m.club_id);

    let mut leagues = Vec::new();
    let mut clubs = Vec::new();
    let mut matches = Vec::new();
    let mut players = Vec::new();
    let mut team_count = 0usize;

    for continent in &world.continents {
        for country in &continent.countries {
            collect_country(
                country,
                scope,
                since,
                managed_club_id,
                &mut leagues,
                &mut clubs,
                &mut matches,
                &mut players,
                &mut team_count,
            );
        }
    }

    // Free agents live outside the club tree. Walking only the clubs would
    // drop every out-of-contract player from the projection the day his deal
    // expires — indistinguishable, from the panel's side, from the player
    // having been deleted. Matters more in this fork than upstream: with
    // synthetic players switched off nobody replaces them, so the pool is
    // the whole remaining supply of footballers.
    let free_agents = if scope == Scope::Full {
        world.free_agents.len()
    } else {
        0
    };

    if scope == Scope::Full {
        for player in &world.free_agents {
            players.push(player_snapshot(player, 0, 0));
        }
    }

    WorldSnapshot {
        format_version: FORMAT_VERSION,
        engine_version: env!("CARGO_PKG_VERSION").to_string(),
        scope: scope.as_str(),
        since,
        in_game_date: world.date.format("%Y-%m-%dT%H:%M:%S").to_string(),
        managed,
        counts: SnapshotCounts {
            leagues: leagues.len(),
            clubs: clubs.len(),
            teams: team_count,
            matches: matches.len(),
            players: players.len(),
            free_agents,
        },
        leagues,
        clubs,
        matches,
        players,
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_country(
    country: &Country,
    scope: Scope,
    since: Option<NaiveDate>,
    managed_club_id: Option<u32>,
    leagues: &mut Vec<LeagueSnapshot>,
    clubs: &mut Vec<ClubSnapshot>,
    matches: &mut Vec<MatchSnapshot>,
    players: &mut Vec<PlayerSnapshot>,
    team_count: &mut usize,
) {
    for league in &country.leagues.leagues {
        leagues.push(league_snapshot(league));
        collect_fixtures(league, since, matches);
    }

    for club in &country.clubs {
        let wants_players = scope == Scope::Full || managed_club_id == Some(club.id);

        let mut teams = Vec::with_capacity(club.teams.teams.len());

        for team in &club.teams.teams {
            *team_count += 1;
            teams.push(team_snapshot(team));

            if wants_players {
                for player in &team.players.players {
                    players.push(player_snapshot(player, club.id, team.id));
                }
            }
        }

        clubs.push(ClubSnapshot {
            id: club.id,
            name: club.name.clone(),
            country_id: country.id,
            city_id: club.location.city_id,
            region_code: club.location.region_code.clone(),
            balance: club.finance.balance.balance,
            teams,
        });
    }
}

fn league_snapshot(league: &League) -> LeagueSnapshot {
    LeagueSnapshot {
        id: league.id,
        name: league.name.clone(),
        slug: league.slug.clone(),
        country_id: league.country_id,
        tier: league.settings.tier,
        is_cup: league.is_cup,
        promotion_spots: league.settings.promotion_spots,
        relegation_spots: league.settings.relegation_spots,
        promotes_to: league.settings.promotes_to,
        region_code: league.settings.region_code.clone(),
        table: league
            .table
            .get()
            .iter()
            .map(|row| TableRowSnapshot {
                team_id: row.team_id,
                played: row.played,
                win: row.win,
                // Upstream spells the drawn column `draft`; the wire
                // format uses the word the rest of the world uses.
                draw: row.draft,
                lost: row.lost,
                goals_for: row.goal_scored,
                goals_against: row.goal_concerned,
                points: row.points,
                points_deduction: row.points_deduction,
            })
            .collect(),
    }
}

fn collect_fixtures(league: &League, since: Option<NaiveDate>, matches: &mut Vec<MatchSnapshot>) {
    for tour in &league.schedule.tours {
        for item in &tour.items {
            if let Some(since) = since {
                if item.date.date() < since {
                    continue;
                }
            }

            let (home_goals, away_goals) = match &item.result {
                Some(score) => (
                    Some(score.home_team.get()),
                    Some(score.away_team.get()),
                ),
                None => (None, None),
            };

            matches.push(MatchSnapshot {
                id: item.id.clone(),
                league_id: item.league_id,
                league_slug: item.league_slug.clone(),
                round: tour.num,
                date: item.date.format("%Y-%m-%dT%H:%M:%S").to_string(),
                home_team_id: item.home_team_id,
                away_team_id: item.away_team_id,
                home_goals,
                away_goals,
            });
        }
    }
}

fn team_snapshot(team: &Team) -> TeamSnapshot {
    TeamSnapshot {
        id: team.id,
        name: team.name.clone(),
        slug: team.slug.clone(),
        league_id: team.league_id,
        is_main: team.team_type == TeamType::Main,
        reputation_home: team.reputation.home,
        reputation_national: team.reputation.national,
        reputation_world: team.reputation.world,
    }
}

fn player_snapshot(player: &Player, club_id: u32, team_id: u32) -> PlayerSnapshot {
    let statuses = player.statuses.get();

    PlayerSnapshot {
        id: player.id,
        club_id,
        team_id,
        free_agent: club_id == 0,
        first_name: player.full_name.first_name.clone(),
        last_name: player.full_name.last_name.clone(),
        birth_date: player.birth_date,
        country_id: player.country_id,
        position: position_code(player.position()),
        positions: player
            .positions
            .positions
            .iter()
            .map(|p| PositionSnapshot {
                position: position_code(p.position),
                level: p.level,
            })
            .collect(),
        current_ability: player.player_attributes.current_ability,
        potential_ability: player.player_attributes.potential_ability,
        value: player.player_attributes.value,
        condition: player.player_attributes.condition,
        fitness: player.player_attributes.fitness,
        jadedness: player.player_attributes.jadedness,
        morale: player.happiness.morale,
        height: player.player_attributes.height,
        weight: player.player_attributes.weight,
        preferred_foot: player.preferred_foot_str(),
        is_injured: player.player_attributes.is_injured,
        is_banned: player.player_attributes.is_banned,
        transfer_listed: statuses.contains(&PlayerStatusType::Lst),
        loan_listed: statuses.contains(&PlayerStatusType::Loa),
        retiring: statuses.contains(&PlayerStatusType::Ret),
        contract: player.contract.as_ref().map(|contract| ContractSnapshot {
            shirt_number: contract.shirt_number,
            salary: contract.salary,
            started: contract.started,
            expiration: contract.expiration,
            is_transfer_listed: contract.is_transfer_listed,
            loan_from_club_id: contract.loan_from_club_id,
        }),
        skills: SkillsSnapshot {
            technical: TechnicalSnapshot {
                corners: player.skills.technical.corners,
                crossing: player.skills.technical.crossing,
                dribbling: player.skills.technical.dribbling,
                finishing: player.skills.technical.finishing,
                first_touch: player.skills.technical.first_touch,
                free_kicks: player.skills.technical.free_kicks,
                heading: player.skills.technical.heading,
                long_shots: player.skills.technical.long_shots,
                long_throws: player.skills.technical.long_throws,
                marking: player.skills.technical.marking,
                passing: player.skills.technical.passing,
                penalty_taking: player.skills.technical.penalty_taking,
                tackling: player.skills.technical.tackling,
                technique: player.skills.technical.technique,
            },
            mental: MentalSnapshot {
                aggression: player.skills.mental.aggression,
                anticipation: player.skills.mental.anticipation,
                bravery: player.skills.mental.bravery,
                composure: player.skills.mental.composure,
                concentration: player.skills.mental.concentration,
                decisions: player.skills.mental.decisions,
                determination: player.skills.mental.determination,
                flair: player.skills.mental.flair,
                leadership: player.skills.mental.leadership,
                off_the_ball: player.skills.mental.off_the_ball,
                positioning: player.skills.mental.positioning,
                teamwork: player.skills.mental.teamwork,
                vision: player.skills.mental.vision,
                work_rate: player.skills.mental.work_rate,
            },
            physical: PhysicalSnapshot {
                acceleration: player.skills.physical.acceleration,
                agility: player.skills.physical.agility,
                balance: player.skills.physical.balance,
                jumping: player.skills.physical.jumping,
                natural_fitness: player.skills.physical.natural_fitness,
                pace: player.skills.physical.pace,
                stamina: player.skills.physical.stamina,
                strength: player.skills.physical.strength,
                match_readiness: player.skills.physical.match_readiness,
            },
            goalkeeping: GoalkeepingSnapshot {
                aerial_reach: player.skills.goalkeeping.aerial_reach,
                command_of_area: player.skills.goalkeeping.command_of_area,
                communication: player.skills.goalkeeping.communication,
                eccentricity: player.skills.goalkeeping.eccentricity,
                first_touch: player.skills.goalkeeping.first_touch,
                handling: player.skills.goalkeeping.handling,
                kicking: player.skills.goalkeeping.kicking,
                one_on_ones: player.skills.goalkeeping.one_on_ones,
                passing: player.skills.goalkeeping.passing,
                punching: player.skills.goalkeeping.punching,
                reflexes: player.skills.goalkeeping.reflexes,
                rushing_out: player.skills.goalkeeping.rushing_out,
                throwing: player.skills.goalkeeping.throwing,
            },
        },
        statistics: StatisticsSnapshot {
            played: player.statistics.played,
            played_subs: player.statistics.played_subs,
            goals: player.statistics.goals,
            assists: player.statistics.assists,
            penalties: player.statistics.penalties,
            yellow_cards: player.statistics.yellow_cards,
            red_cards: player.statistics.red_cards,
            player_of_the_match: player.statistics.player_of_the_match,
            average_rating: player.statistics.average_rating,
        },
    }
}

fn position_code(position: PlayerPositionType) -> &'static str {
    position.get_short_name()
}
