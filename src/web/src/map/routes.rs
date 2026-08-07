//! Added in this fork: route for the club-selection map page.

use crate::GameAppData;
use axum::Router;
use axum::routing::get;

pub fn map_routes() -> Router<GameAppData> {
    Router::new().route("/{lang}/map", get(super::map_action))
}
