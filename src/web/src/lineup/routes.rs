use crate::GameAppData;
use crate::lineup::game_lineup_action;
use axum::Router;
use axum::routing::post;

/// Added in this fork: manager-set starting XI for the managed club.
pub fn lineup_routes() -> Router<GameAppData> {
    Router::new().route("/api/game/lineup", post(game_lineup_action))
}
