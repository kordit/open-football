//! HTTP surface of the engine.
//!
//! In this fork the engine serves no user interface at all — the game's
//! front end is the Blade panel in the parent repository, which reads a
//! projection of the world from `/api/world/snapshot` and drives the
//! world through `/api/game/*`. Everything that used to render a page
//! (including upstream's Champions League / Europa League / Copa
//! Libertadores / national-competition sections, which this game has no
//! use for) has been removed; see FORK_CHANGES.md.

mod common;
mod date;
mod error;
mod game;
pub mod i18n;
// Added in this fork: manager-set starting XI.
mod lineup;
pub mod live;
mod r#match;
mod player;
mod routes;
pub use live::LiveRegistry;
pub mod settings;
// Added in this fork: world snapshot consumed by the Laravel panel.
mod snapshot;
pub mod worker;
mod workers;

pub use settings::{RunMode, Settings};

pub use error::{ApiError, ApiResult};
// Added in this fork: save-slot session bookkeeping.
pub use game::saves::SaveMeta;
pub use i18n::events::{EventI18n, EventI18nManager};
pub use i18n::news::{NewsI18n, NewsI18nManager};
pub use i18n::{I18n, I18nManager};
pub use worker::{
    DistributedDispatcher, WorkerRegistry, WorkerServer, WorkerSnapshot, WorkerStatus,
};

use crate::routes::ServerRoutes;
use axum::response::IntoResponse;
use core::SimulatorData;
use database::DatabaseEntity;
use log::{error, info};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, RwLock};
use tower::ServiceBuilder;
use tower_http::catch_panic::CatchPanicLayer;

pub struct FootballSimulatorServer {
    data: GameAppData,
}

impl FootballSimulatorServer {
    pub fn new(data: GameAppData) -> Self {
        FootballSimulatorServer { data }
    }

    pub async fn run(&self) {
        let app = ServerRoutes::create()
            .layer(ServiceBuilder::new()
                    // Catch panics in handlers and convert them to 500 errors
                    .layer(CatchPanicLayer::custom(|_err| {
                        (
                            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                            "Internal server error - handler panicked".to_string(),
                        )
                            .into_response()
                    })))
            .with_state(self.data.clone());

        let addr = SocketAddr::from(([0, 0, 0, 0], 18000));

        let listener = match TcpListener::bind(addr).await {
            Ok(listener) => listener,
            Err(e) => {
                error!("Failed to bind to address {}: {}", addr, e);
                panic!("Cannot start server without binding to port");
            }
        };

        info!("engine API on http://localhost:18000 (JSON only — no UI)");

        if let Err(e) = axum::serve(listener, app).await {
            error!("Server error: {}", e);
            error!("Server stopped unexpectedly, but not crashing the process");
        }
    }
}

pub struct GameAppData {
    pub database: Arc<DatabaseEntity>,
    pub data: Arc<RwLock<Option<Arc<SimulatorData>>>>,
    pub process_lock: Arc<Mutex<()>>,
    pub cancel_flag: Arc<AtomicBool>,
    pub i18n: Arc<I18nManager>,
    /// Press vocabulary, scoped away from `i18n` so the chrome bundle every
    /// page clones stays small. Only the newspaper pages resolve against it.
    pub news_i18n: Arc<NewsI18nManager>,
    /// Happiness-event vocabulary, scoped for the same reason. Only the
    /// player events page resolves against it.
    pub events_i18n: Arc<EventI18nManager>,
    /// Live registry of distributed match workers. Always present;
    /// starts empty and is populated at runtime from the /workers page.
    pub workers: WorkerRegistry,
    /// Added in this fork: save-slot session state — active slot slug,
    /// saves directory, last autosave date.
    pub saves: Arc<RwLock<SaveMeta>>,
    /// Added in this fork: the one match the manager may be playing by hand.
    /// Empty almost always; holds a session between kickoff and full time.
    pub live: crate::live::LiveRegistry,
}

impl Clone for GameAppData {
    fn clone(&self) -> Self {
        GameAppData {
            database: Arc::clone(&self.database),
            data: Arc::clone(&self.data),
            process_lock: Arc::clone(&self.process_lock),
            cancel_flag: Arc::clone(&self.cancel_flag),
            i18n: Arc::clone(&self.i18n),
            news_i18n: Arc::clone(&self.news_i18n),
            events_i18n: Arc::clone(&self.events_i18n),
            workers: self.workers.clone(),
            saves: Arc::clone(&self.saves),
            live: self.live.clone(),
        }
    }
}
