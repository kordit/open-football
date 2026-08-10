use crate::GameAppData;
use crate::snapshot::world_snapshot_action;
use axum::Router;
use axum::routing::get;

/// Added in this fork: read model for the Laravel panel.
pub fn snapshot_routes() -> Router<GameAppData> {
    Router::new().route("/api/world/snapshot", get(world_snapshot_action))
}
