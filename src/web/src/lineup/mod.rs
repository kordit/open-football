//! Added in this fork: manager-set starting XI and formation.
//!
//! Upstream has no notion of a human picking the side — every club is
//! selected by its coach. The engine does carry
//! `Player.is_force_match_selection`, a Main-team pin that gives a player
//! a large bonus in the selection scoring, and that is the hook used
//! here: the manager's eleven are pinned, everyone else at the club is
//! un-pinned, so the coach's own logic still fills the bench and handles
//! injuries and suspensions on the day.
//!
//! Only the managed club is affected. Every other club in the world keeps
//! choosing for itself.
//!
//! The bench is deliberately not forced. Pinning substitutes would fight
//! the in-match substitution logic, which reads form and the state of the
//! game; the roadmap keeps bench control for a later pass.

pub mod routes;

use crate::GameAppData;
use crate::error::{ApiError, ApiResult};
use crate::game::saves::{publish_world, write_slot};
use axum::Json;
use axum::extract::State;
use core::{MatchTacticType, SimulatorData, TacticSelectionReason, Tactics, TeamType};
use log::info;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::task::spawn_blocking;

#[derive(Deserialize)]
pub struct LineupRequest {
    /// Engine formation name — `T442`, `T4231`, … Optional: omitting it
    /// leaves the coach's own choice in place.
    pub formation: Option<String>,
    /// Player ids to pin into the starting eleven. An empty list clears
    /// the manager's selection and hands the side back to the coach.
    pub starting: Vec<u32>,
}

#[derive(Serialize)]
pub struct LineupResponse {
    pub team_id: u32,
    pub team_name: String,
    pub formation: Option<String>,
    /// Ids actually pinned — the request minus anything that turned out
    /// not to belong to the managed club.
    pub pinned: Vec<u32>,
    /// Ids from the request that were rejected, and why the caller should
    /// care: they are not at this club.
    pub rejected: Vec<u32>,
}

/// `POST /api/game/lineup` — pin the managed club's starting eleven.
pub async fn game_lineup_action(
    State(state): State<GameAppData>,
    Json(request): Json<LineupRequest>,
) -> ApiResult<Json<LineupResponse>> {
    let _guard = Arc::clone(&state.process_lock)
        .try_lock_owned()
        .map_err(|_| ApiError::BadRequest("game is busy (processing in progress)".to_string()))?;

    if request.starting.len() > 11 {
        return Err(ApiError::BadRequest(format!(
            "a starting eleven is at most 11 players, got {}",
            request.starting.len()
        )));
    }

    let formation = match request.formation.as_deref() {
        None => None,
        Some(raw) => Some(parse_formation(raw)?),
    };

    let (slug, saves_dir) = {
        let meta = state.saves.read().await;
        let slug = meta
            .current_slug
            .clone()
            .ok_or_else(|| ApiError::BadRequest("no active career".to_string()))?;
        (slug, meta.saves_dir.clone())
    };

    let world_arc = {
        let guard = state.data.read().await;
        guard
            .as_ref()
            .map(Arc::clone)
            .ok_or_else(|| ApiError::InternalError("simulator data not loaded".to_string()))?
    };

    // Resolve (and validate) on the shared snapshot before paying for the
    // deep clone — a rejected request must not cost a world copy.
    let (team_id, club_id, team_name) = {
        let manager = world_arc
            .player_manager
            .as_ref()
            .ok_or_else(|| ApiError::BadRequest("no active career".to_string()))?;

        let team = world_arc
            .team(manager.team_id)
            .ok_or_else(|| ApiError::NotFound("managed team not found".to_string()))?;

        if team.team_type != TeamType::Main {
            return Err(ApiError::BadRequest(
                "only a club's main team has a manager-set lineup".to_string(),
            ));
        }

        (team.id, team.club_id, team.name.clone())
    };

    let starting = request.starting.clone();

    let (response, world) = spawn_blocking(move || -> Result<_, ApiError> {
        let mut world = Arc::unwrap_or_clone(world_arc);

        let (pinned, rejected) = apply_lineup(&mut world, club_id, &starting);

        if let Some(tactic_type) = formation {
            if let Some(team) = world.team_mut(team_id) {
                team.tactics = Some(Tactics {
                    tactic_type,
                    selected_reason: TacticSelectionReason::CoachPreference,
                    // The manager asked for this shape; the selector must
                    // not weigh it against alternatives.
                    formation_strength: 1.0,
                });
            }
        }

        // The lineup is part of the world, so it has to survive a restart
        // exactly like a transfer does — persist into the active slot.
        write_slot(&saves_dir, &slug, &world)?;

        Ok((
            LineupResponse {
                team_id,
                team_name,
                formation: formation.map(formation_name).map(str::to_string),
                pinned,
                rejected,
            },
            world,
        ))
    })
    .await
    .map_err(|e| ApiError::InternalError(format!("lineup task failed: {e}")))??;

    publish_world(&state, world).await;

    info!(
        "lineup: pinned {} players for team {}",
        response.pinned.len(),
        response.team_id,
    );

    Ok(Json(response))
}

/// Clears the pin across every team of the club, then re-applies it to the
/// requested ids. Clearing first is what makes the endpoint idempotent:
/// yesterday's eleven never leaks into today's.
fn apply_lineup(world: &mut SimulatorData, club_id: u32, starting: &[u32]) -> (Vec<u32>, Vec<u32>) {
    let mut pinned = Vec::new();

    let Some(club) = world.club_mut(club_id) else {
        return (pinned, starting.to_vec());
    };

    for team in club.teams.teams.iter_mut() {
        for player in team.players.players.iter_mut() {
            player.is_force_match_selection = false;

            if starting.contains(&player.id) {
                player.is_force_match_selection = true;
                pinned.push(player.id);
            }
        }
    }

    let rejected = starting
        .iter()
        .copied()
        .filter(|id| !pinned.contains(id))
        .collect();

    (pinned, rejected)
}

fn parse_formation(raw: &str) -> ApiResult<MatchTacticType> {
    match raw {
        "T442" => Ok(MatchTacticType::T442),
        "T433" => Ok(MatchTacticType::T433),
        "T451" => Ok(MatchTacticType::T451),
        "T4231" => Ok(MatchTacticType::T4231),
        "T352" => Ok(MatchTacticType::T352),
        "T442Diamond" => Ok(MatchTacticType::T442Diamond),
        "T442DiamondWide" => Ok(MatchTacticType::T442DiamondWide),
        "T442Narrow" => Ok(MatchTacticType::T442Narrow),
        "T4141" => Ok(MatchTacticType::T4141),
        "T4411" => Ok(MatchTacticType::T4411),
        "T343" => Ok(MatchTacticType::T343),
        "T1333" => Ok(MatchTacticType::T1333),
        "T4312" => Ok(MatchTacticType::T4312),
        "T4222" => Ok(MatchTacticType::T4222),
        other => Err(ApiError::BadRequest(format!(
            "unknown formation \"{other}\""
        ))),
    }
}

fn formation_name(tactic: MatchTacticType) -> &'static str {
    match tactic {
        MatchTacticType::T442 => "T442",
        MatchTacticType::T433 => "T433",
        MatchTacticType::T451 => "T451",
        MatchTacticType::T4231 => "T4231",
        MatchTacticType::T352 => "T352",
        MatchTacticType::T442Diamond => "T442Diamond",
        MatchTacticType::T442DiamondWide => "T442DiamondWide",
        MatchTacticType::T442Narrow => "T442Narrow",
        MatchTacticType::T4141 => "T4141",
        MatchTacticType::T4411 => "T4411",
        MatchTacticType::T343 => "T343",
        MatchTacticType::T1333 => "T1333",
        MatchTacticType::T4312 => "T4312",
        MatchTacticType::T4222 => "T4222",
    }
}
