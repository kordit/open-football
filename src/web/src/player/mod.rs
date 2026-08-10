//! Modified from upstream: every rendered player tab has been removed.
//! What remains is the set of manager actions the panel calls.
pub mod actions;

use crate::GameAppData;
use axum::Router;

pub fn player_routes() -> Router<GameAppData> {
    Router::new().merge(actions::routes::routes())
}
