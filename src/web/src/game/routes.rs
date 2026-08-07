use crate::GameAppData;
use crate::game::{
    game_cancel_action, game_create_action, game_process_action, game_processing_status_action,
    game_save_action, game_session_action, save_load_action, saves_list_action,
};
use axum::Router;
use axum::routing::{get, post};

pub fn game_routes() -> Router<GameAppData> {
    Router::new()
        // Changed in this fork: create is a real POST JSON handler now
        // (was a GET stub returning 200).
        .route("/api/game/create", post(game_create_action))
        .route("/api/game/process", post(game_process_action))
        .route("/api/game/processing", get(game_processing_status_action))
        .route("/api/game/cancel", post(game_cancel_action))
        // Added in this fork: save slots + session.
        .route("/api/game/save", post(game_save_action))
        .route("/api/game/session", get(game_session_action))
        .route("/api/saves", get(saves_list_action))
        .route("/api/saves/{slug}/load", post(save_load_action))
}
