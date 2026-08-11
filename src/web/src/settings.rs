// Modified from upstream: added `--database=`, `--no-international` and
// `--no-synthetic-players` flags, and the `serve` / `simulate` /
// `validate-db` subcommands.
use core::MatchRuntime;
use log::info;
use std::env;

/// What the binary was asked to do. `Serve` (the default) runs the web UI;
/// `Simulate` runs a headless world simulation; `ValidateDb` loads and
/// checks a world database file, then exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    Serve,
    Simulate,
    ValidateDb,
}

pub struct Settings {
    pub run_mode: RunMode,
    /// Explicit world database path (`--database=<path>`). When absent the
    /// loader falls back to `OF_DATABASE_PATH` and then `./polish-database.db`.
    pub database_path: Option<String>,
    /// `--no-international`: disable national teams, continental club
    /// competitions and the bundled U21 layer (single-country worlds).
    pub no_international: bool,
    /// `--no-synthetic-players`: the engine never invents a footballer.
    /// Clubs get exactly the players the world database supplies, youth
    /// squads are not generated, and academy intake produces nobody.
    pub no_synthetic_players: bool,
    /// `--quick-other-matches`: resolve every fixture the managed club is
    /// not playing in with the statistical model instead of the tick
    /// engine. Turns a ~400-fixture matchday from ~98 s into well under a
    /// second; the manager's own match is unaffected.
    pub quick_other_matches: bool,
    pub match_events: bool,
    pub match_recordings: bool,
    pub match_threads: usize,
    pub match_store_threads: usize,
    /// True when the binary was invoked with `--worker`. In that mode
    /// the process skips DB load and the HTTP web UI and listens for
    /// match-batch RPCs on `worker_port`.
    pub worker_mode: bool,
    pub worker_port: u16,
}

impl Settings {
    pub fn from_env() -> Self {
        let args: Vec<String> = env::args().collect();

        // The first non-flag argument selects the run mode; no argument
        // means `serve` so existing invocations keep working.
        let run_mode = args
            .iter()
            .skip(1)
            .find(|arg| !arg.starts_with("--"))
            .map(|cmd| match cmd.as_str() {
                "simulate" => RunMode::Simulate,
                "validate-db" => RunMode::ValidateDb,
                _ => RunMode::Serve,
            })
            .unwrap_or(RunMode::Serve);

        let database_path = args
            .iter()
            .find(|arg| arg.starts_with("--database="))
            .and_then(|arg| arg.strip_prefix("--database="))
            .map(str::to_string);

        let no_international = args.iter().any(|arg| arg == "--no-international");

        let no_synthetic_players = args.iter().any(|arg| arg == "--no-synthetic-players");

        let quick_other_matches = args.iter().any(|arg| arg == "--quick-other-matches");

        let match_events = args.iter().any(|arg| arg == "--match-events");

        let match_recordings = args.iter().any(|arg| arg == "--match-recording-enabled")
            || env::var("MATCH_RECORDING_ENABLED")
                .map(|v| v == "true")
                .unwrap_or(false);

        let match_threads = args
            .iter()
            .find(|arg| arg.starts_with("--match-threads="))
            .and_then(|arg| arg.strip_prefix("--match-threads="))
            .and_then(|v| v.parse().ok())
            .or_else(|| {
                env::var("MATCH_PLAY_POOL_MAX_THREADS")
                    .ok()
                    .and_then(|v| v.parse().ok())
            })
            .unwrap_or_else(|| {
                std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(4)
            });

        let match_store_threads = env::var("MATCH_STORE_POOL_MAX_THREADS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(4);

        let worker_mode = args.iter().any(|arg| arg == "--worker");

        let worker_port = args
            .iter()
            .find(|arg| arg.starts_with("--worker-port="))
            .and_then(|arg| arg.strip_prefix("--worker-port="))
            .and_then(|v| v.parse().ok())
            .unwrap_or(18001);

        Settings {
            run_mode,
            database_path,
            no_international,
            no_synthetic_players,
            quick_other_matches,
            match_events,
            match_recordings,
            match_threads,
            match_store_threads,
            worker_mode,
            worker_port,
        }
    }

    pub fn apply(&self) {
        if let Some(path) = &self.database_path {
            database::set_database_path(path.clone());
        }
        core::settings::set_international_enabled(!self.no_international);
        core::settings::set_synthetic_players_enabled(!self.no_synthetic_players);
        core::settings::set_quick_other_matches(self.quick_other_matches);
        MatchRuntime::set_events_mode(self.match_events);
        MatchRuntime::set_recordings_mode(self.match_recordings);
        MatchRuntime::init_engine_pool(self.match_threads);
        MatchRuntime::set_store_max_threads(self.match_store_threads);
    }

    pub fn log(&self) {
        if let Some(path) = &self.database_path {
            info!("World database: {}", path);
        }
        if self.no_international {
            info!("International football disabled (--no-international)");
        }
        if self.no_synthetic_players {
            info!(
                "Synthetic players disabled (--no-synthetic-players) — no generated squads, \
                 no youth teams, no academy intake; thin clubs stay thin and retirements \
                 are not replaced"
            );
        }
        if self.quick_other_matches {
            info!(
                "Quick simulation for other matches (--quick-other-matches) — only the \
                 managed club's fixtures run the tick engine; the rest of the world is \
                 resolved statistically (no replays for those matches)"
            );
        }
        if self.match_events {
            info!("Match events recording enabled");
        }
        if self.match_recordings {
            info!("Match recordings mode enabled");
        }
        info!(
            "Match engine: {} threads, store: {} threads",
            self.match_threads, self.match_store_threads
        );
        if self.worker_mode {
            info!("Worker mode on, listening port {}", self.worker_port);
        }
    }
}
