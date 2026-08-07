//! Replaced in this fork: the old stub handler is gone. Career creation
//! (`POST /api/game/create`) lives in [`super::saves`] together with the
//! rest of the save-slot session layer. This shim keeps the historical
//! module path alive for readers of the upstream tree.

#[allow(unused_imports)] // routes.rs imports via `crate::game::*` (saves.rs)
pub use super::saves::{CreateGameRequest, game_create_action};
