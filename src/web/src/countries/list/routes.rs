use crate::GameAppData;
use axum::Router;
use axum::routing::get;

pub fn routes() -> Router<GameAppData> {
    Router::new()
        // Changed in this fork: the bare language root is the player-facing
        // landing page (career hero + saves; redirects to the managed club
        // while a career is active). The countries listing keeps living
        // under /countries for world browsing.
        .route("/{lang}", get(super::country_home_action))
        .route("/{lang}/countries", get(super::country_list_action))
        // Added in this fork: saves list reachable while a career is active.
        .route("/{lang}/saves", get(super::saves_page_action))
        // Added in this fork: operator page, reachable only by direct URL.
        .route("/{lang}/admin", get(super::admin_action))
}
