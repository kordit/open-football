//! Modified from upstream: the router is now a pure JSON API.
//!
//! Upstream merged one route group per rendered section (countries,
//! leagues, teams, players, cups, the four continental competitions, …)
//! and wrapped everything in language-prefix middleware plus a
//! redirect-to-home error handler, because every route returned a page.
//! Nothing here returns a page any more — the front end is the Blade
//! panel in the parent repository — so the language prefixes, the
//! sitemap and the redirect middleware went with the templates.

use crate::GameAppData;
use crate::date::current_date_routes;
use crate::game::game_routes;
use crate::lineup::routes::lineup_routes;
use crate::r#match::routes::match_routes;
use crate::player::player_routes;
use crate::snapshot::routes::snapshot_routes;
use crate::workers::routes::workers_routes;
use axum::Json;
use axum::Router;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use serde_json::json;

/// Liveness probe and self-description. Something has to answer `/`, and
/// a list of what this process speaks is more useful than a redirect to
/// a page that no longer exists.
async fn root() -> impl IntoResponse {
    Json(json!({
        "service": "polish-football-manager-engine",
        "version": env!("CARGO_PKG_VERSION"),
        "ui": "none — the front end is the Blade panel in the parent repository",
        "api": [
            "GET  /api/date",
            "GET  /api/game/session",
            "POST /api/game/create",
            "POST /api/game/process",
            "GET  /api/game/processing",
            "POST /api/game/cancel",
            "POST /api/game/save",
            "POST /api/game/takeover",
            "POST /api/game/lineup",
            "GET  /api/saves",
            "POST /api/saves/{slug}/load",
            "GET  /api/world/snapshot",
            "GET  /api/match/{id}/metadata",
            "GET  /api/match/{id}/chunk/{n}",
            "GET  /api/clubs",
        ],
    }))
}

async fn not_found() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": "no such endpoint — this process serves JSON only, see GET /" })),
    )
}

pub struct ServerRoutes;

impl ServerRoutes {
    pub fn create() -> Router<GameAppData> {
        Router::<GameAppData>::new()
            .route("/", get(root))
            .merge(game_routes())
            // Added in this fork: manager-set starting XI.
            .merge(lineup_routes())
            // Added in this fork: read model for the Laravel panel.
            .merge(snapshot_routes())
            .merge(player_routes())
            .merge(match_routes())
            .merge(current_date_routes())
            .merge(workers_routes())
            .fallback(not_found)
    }
}
