//! Save slots + game session routes — added in this fork.
//!
//! The world itself (including the managed club, `player_manager`) is
//! persisted by `core::simulator::persistence`; this module owns the
//! HTTP surface: listing `*.ofs` slots, creating a career, loading a
//! slot, and manual/auto saving. All world (de)serialization runs on
//! the blocking thread pool, and every mutating route holds the same
//! `process_lock` the simulation runs under so a save/load can never
//! race a processing run.

use crate::{ApiError, ApiResult, GameAppData};
use axum::Json;
use axum::extract::{Path, State};
use chrono::{NaiveDateTime, Utc};
use core::SimulatorData;
use core::simulator::persistence::{SaveHeader, read_header, save_world};
use core::{PlayerManager, utils::random::engine::RandomEngine};
use database::DatabaseGenerator;
use log::{error, info};
use serde::{Deserialize, Serialize};
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;
use tokio::task::spawn_blocking;

/// Session bookkeeping the web layer keeps next to the shared world:
/// which slot is active and where slots live on disk.
pub struct SaveMeta {
    /// Slug of the active slot (`{slug}.ofs` in `saves_dir`), if any.
    pub current_slug: Option<String>,
    /// Directory scanned for `*.ofs` files. `OF_SAVES_DIR` overrides
    /// the `./saves` default.
    pub saves_dir: PathBuf,
    /// In-game date of the last completed autosave, for diagnostics.
    pub last_autosave_date: Option<NaiveDateTime>,
}

impl SaveMeta {
    pub fn new() -> Self {
        let dir = std::env::var("OF_SAVES_DIR").unwrap_or_else(|_| "./saves".to_string());
        SaveMeta {
            current_slug: None,
            saves_dir: PathBuf::from(dir),
            last_autosave_date: None,
        }
    }
}

impl Default for SaveMeta {
    fn default() -> Self {
        Self::new()
    }
}

/// One row of `GET /api/saves` — header data plus file metadata.
#[derive(Serialize, Clone)]
pub struct SaveListEntry {
    pub slug: String,
    pub save_name: String,
    pub in_game_date: String,
    pub managed_team_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub managed_team_name: Option<String>,
    pub engine_version: String,
    pub created_at: String,
    pub modified_at: String,
}

fn entry_from_header(
    slug: String,
    header: &SaveHeader,
    modified_at: String,
    live: Option<&SimulatorData>,
) -> SaveListEntry {
    let managed_team_name = header.managed_team_id.and_then(|id| {
        live.and_then(|world| world.team(id))
            .map(|team| team.name.clone())
    });
    SaveListEntry {
        slug,
        save_name: header.save_name.clone(),
        in_game_date: header.in_game_date.format("%Y-%m-%d %H:%M").to_string(),
        managed_team_id: header.managed_team_id,
        managed_team_name,
        engine_version: header.engine_version.clone(),
        created_at: header.created_at.format("%Y-%m-%d %H:%M").to_string(),
        modified_at,
    }
}

fn file_modified_at(path: &FsPath) -> String {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|t| {
            let dt: chrono::DateTime<Utc> = t.into();
            dt.naive_utc().format("%Y-%m-%d %H:%M").to_string()
        })
        .unwrap_or_default()
}

/// Filesystem-safe slug: lowercase ascii alphanumerics with `-`
/// separators. Non-ascii letters are dropped rather than transliterated
/// — team slugs in the world database are already ascii.
fn slugify(input: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = true;
    for c in input.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() { "save".to_string() } else { out }
}

/// First free slot name: `base`, then `base-2`, `base-3`, …
fn unique_slug(dir: &FsPath, base: &str) -> String {
    if !dir.join(format!("{base}.ofs")).exists() {
        return base.to_string();
    }
    let mut n = 2u32;
    loop {
        let candidate = format!("{base}-{n}");
        if !dir.join(format!("{candidate}.ofs")).exists() {
            return candidate;
        }
        n += 1;
    }
}

/// Reject anything that could escape the saves dir. Dots are allowed
/// (autosaves are `{slug}.auto`), path separators and `..` are not.
fn validate_slug(slug: &str) -> Result<(), ApiError> {
    let ok = !slug.is_empty()
        && !slug.contains("..")
        && slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if ok {
        Ok(())
    } else {
        Err(ApiError::BadRequest(format!("invalid save slug '{slug}'")))
    }
}

// ---------------------------------------------------------------------------
// GET /api/saves
// ---------------------------------------------------------------------------

pub async fn saves_list_action(
    State(state): State<GameAppData>,
) -> ApiResult<Json<Vec<SaveListEntry>>> {
    let dir = state.saves.read().await.saves_dir.clone();

    // Team-name resolution uses whatever world is currently live: team
    // ids are stable per world database, so names resolve for saves of
    // the same database and are simply omitted otherwise.
    let live = {
        let guard = state.data.read().await;
        guard.as_ref().map(Arc::clone)
    };

    let entries = spawn_blocking(move || -> Vec<SaveListEntry> {
        let mut entries = Vec::new();
        let Ok(read_dir) = std::fs::read_dir(&dir) else {
            return entries;
        };
        for file in read_dir.flatten() {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("ofs") {
                continue;
            }
            let Some(slug) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            // Headers only — never decompress a world body for a listing.
            match read_header(&path) {
                Ok(header) => entries.push(entry_from_header(
                    slug.to_string(),
                    &header,
                    file_modified_at(&path),
                    live.as_deref(),
                )),
                Err(e) => error!("saves list: skipping {}: {e}", path.display()),
            }
        }
        entries.sort_by(|a, b| b.modified_at.cmp(&a.modified_at));
        entries
    })
    .await
    .map_err(|e| ApiError::InternalError(format!("saves list task failed: {e}")))?;

    Ok(Json(entries))
}

// ---------------------------------------------------------------------------
// POST /api/game/create
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CreateGameRequest {
    pub team_id: u32,
    pub manager_name: String,
    pub save_name: Option<String>,
    pub seed: Option<u64>,
}

pub async fn game_create_action(
    State(state): State<GameAppData>,
    Json(request): Json<CreateGameRequest>,
) -> ApiResult<Json<SaveListEntry>> {
    // Same lock the processing run holds: a career can't be created
    // while a simulation tick (or another save/load) is in flight.
    let _guard = Arc::clone(&state.process_lock)
        .try_lock_owned()
        .map_err(|_| ApiError::BadRequest("game is busy (processing in progress)".to_string()))?;

    let saves_dir = state.saves.read().await.saves_dir.clone();
    let database = Arc::clone(&state.database);

    // World generation + serialization are CPU/IO bound — keep them off
    // the async workers.
    let (entry, world, slug) = spawn_blocking(move || -> Result<_, ApiError> {
        // Seed FIRST so generation itself is reproducible.
        if let Some(seed) = request.seed {
            RandomEngine::set_seed(seed);
        }

        let mut world = DatabaseGenerator::generate(&database);

        let (team_name, team_slug) = world
            .team(request.team_id)
            .map(|t| (t.name.clone(), t.slug.clone()))
            .ok_or_else(|| {
                ApiError::NotFound(format!("team with id {} not found", request.team_id))
            })?;

        world.player_manager = Some(PlayerManager {
            team_id: request.team_id,
            name: request.manager_name.clone(),
        });

        let save_name = request
            .save_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| team_name.clone());

        // Slot slug: slugified save name, else the team's own slug.
        let base = if request.save_name.as_deref().map(str::trim).is_some_and(|s| !s.is_empty()) {
            slugify(&save_name)
        } else {
            slugify(&team_slug)
        };

        std::fs::create_dir_all(&saves_dir)
            .map_err(|e| ApiError::InternalError(format!("cannot create saves dir: {e}")))?;
        let slug = unique_slug(&saves_dir, &base);
        let path = saves_dir.join(format!("{slug}.ofs"));

        let header = SaveHeader::for_world(
            save_name,
            Some(request.team_id),
            Utc::now().naive_utc(),
            &world,
        );
        save_world(&path, &header, &world).map_err(ApiError::InternalError)?;

        let entry = entry_from_header(slug.clone(), &header, file_modified_at(&path), Some(&world));
        Ok((entry, world, slug))
    })
    .await
    .map_err(|e| ApiError::InternalError(format!("create task failed: {e}")))??;

    publish_world(&state, world).await;

    {
        let mut meta = state.saves.write().await;
        meta.current_slug = Some(slug.clone());
        meta.last_autosave_date = None;
    }

    info!("career created: slot '{slug}' (team {})", request_team(&entry));
    Ok(Json(entry))
}

fn request_team(entry: &SaveListEntry) -> String {
    entry
        .managed_team_name
        .clone()
        .or_else(|| entry.managed_team_id.map(|id| id.to_string()))
        .unwrap_or_default()
}

/// Swap `world` into the shared slot, mirroring `ProcessingRun::swap`:
/// the write-lock critical section is a pointer swap only, and the
/// outgoing world graph is dropped off-lock on a blocking thread so
/// page handlers never stall behind a multi-second deallocation.
async fn publish_world(state: &GameAppData, world: SimulatorData) {
    state.i18n.set_date(world.date);
    let previous = {
        let mut guard = state.data.write().await;
        guard.replace(Arc::new(world))
    };
    if let Some(previous) = previous {
        let _ = spawn_blocking(move || drop(previous));
    }
}

// ---------------------------------------------------------------------------
// POST /api/saves/{slug}/load
// ---------------------------------------------------------------------------

pub async fn save_load_action(
    State(state): State<GameAppData>,
    Path(slug): Path<String>,
) -> ApiResult<Json<SaveListEntry>> {
    validate_slug(&slug)?;

    let _guard = Arc::clone(&state.process_lock)
        .try_lock_owned()
        .map_err(|_| ApiError::BadRequest("game is busy (processing in progress)".to_string()))?;

    let saves_dir = state.saves.read().await.saves_dir.clone();
    let path = saves_dir.join(format!("{slug}.ofs"));
    if !path.exists() {
        return Err(ApiError::NotFound(format!("save '{slug}' not found")));
    }

    let load_slug = slug.clone();
    let (entry, world) = spawn_blocking(move || -> Result<_, ApiError> {
        let (header, world) =
            core::simulator::persistence::load_world(&path).map_err(ApiError::InternalError)?;
        let entry = entry_from_header(load_slug, &header, file_modified_at(&path), Some(&world));
        Ok((entry, world))
    })
    .await
    .map_err(|e| ApiError::InternalError(format!("load task failed: {e}")))??;

    publish_world(&state, world).await;

    {
        let mut meta = state.saves.write().await;
        // Loading an autosave re-attaches the session to its main slot,
        // so manual saves keep writing `{slug}.ofs` (not `…auto.ofs`).
        meta.current_slug = Some(slug.trim_end_matches(".auto").to_string());
    }

    info!("save '{slug}' loaded");
    Ok(Json(entry))
}

// ---------------------------------------------------------------------------
// POST /api/game/save
// ---------------------------------------------------------------------------

pub async fn game_save_action(State(state): State<GameAppData>) -> ApiResult<Json<SaveListEntry>> {
    let _guard = Arc::clone(&state.process_lock)
        .try_lock_owned()
        .map_err(|_| ApiError::BadRequest("game is busy (processing in progress)".to_string()))?;

    let (slug, saves_dir) = {
        let meta = state.saves.read().await;
        let slug = meta
            .current_slug
            .clone()
            .ok_or_else(|| ApiError::BadRequest("no active save slot".to_string()))?;
        (slug, meta.saves_dir.clone())
    };

    // Snapshot the shared world: a cheap Arc clone under the read lock.
    let world = {
        let guard = state.data.read().await;
        guard
            .as_ref()
            .map(Arc::clone)
            .ok_or_else(|| ApiError::InternalError("simulator data not loaded".to_string()))?
    };

    let entry = spawn_blocking(move || -> Result<SaveListEntry, ApiError> {
        write_slot(&saves_dir, &slug, &world)
    })
    .await
    .map_err(|e| ApiError::InternalError(format!("save task failed: {e}")))??;

    info!("save '{}' written", entry.slug);
    Ok(Json(entry))
}

/// Serialize `world` into `{slug}.ofs`, preserving the slot's original
/// save name and creation timestamp when the file already exists.
fn write_slot(
    saves_dir: &FsPath,
    slug: &str,
    world: &SimulatorData,
) -> Result<SaveListEntry, ApiError> {
    std::fs::create_dir_all(saves_dir)
        .map_err(|e| ApiError::InternalError(format!("cannot create saves dir: {e}")))?;
    let path = saves_dir.join(format!("{slug}.ofs"));

    let (save_name, created_at) = match read_header(&path) {
        Ok(existing) => (existing.save_name, existing.created_at),
        Err(_) => (
            world
                .player_manager
                .as_ref()
                .and_then(|m| world.team(m.team_id))
                .map(|t| t.name.clone())
                .unwrap_or_else(|| slug.to_string()),
            Utc::now().naive_utc(),
        ),
    };

    let managed_team_id = world.player_manager.as_ref().map(|m| m.team_id);
    let header = SaveHeader::for_world(save_name, managed_team_id, created_at, world);
    save_world(&path, &header, world).map_err(ApiError::InternalError)?;

    Ok(entry_from_header(
        slug.to_string(),
        &header,
        file_modified_at(&path),
        Some(world),
    ))
}

// ---------------------------------------------------------------------------
// Autosave (called from the process handler after a completed run)
// ---------------------------------------------------------------------------

/// Fire-and-forget rotating autosave into `{slug}.auto.ofs`. Snapshots
/// the shared world (Arc clone) and serializes on the blocking pool, so
/// the HTTP path that triggered processing returns immediately.
pub fn spawn_autosave(state: &GameAppData) {
    let state = state.clone();
    tokio::spawn(async move {
        let (slug, saves_dir) = {
            let meta = state.saves.read().await;
            match meta.current_slug.clone() {
                Some(slug) => (slug, meta.saves_dir.clone()),
                None => return, // no active session — nothing to autosave
            }
        };

        let world = {
            let guard = state.data.read().await;
            match guard.as_ref().map(Arc::clone) {
                Some(world) => world,
                None => return,
            }
        };
        let date = world.date;

        let auto_slug = format!("{slug}.auto");
        let result =
            spawn_blocking(move || write_slot(&saves_dir, &auto_slug, &world)).await;

        match result {
            Ok(Ok(entry)) => {
                state.saves.write().await.last_autosave_date = Some(date);
                info!("autosave '{}' written", entry.slug);
            }
            Ok(Err(e)) => error!("autosave failed: {e:?}"),
            Err(e) => error!("autosave task failed: {e}"),
        }
    });
}

// ---------------------------------------------------------------------------
// POST /api/game/takeover — switch the managed club within the same save
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct TakeoverRequest {
    pub team_id: u32,
}

/// Mid-career club switch: the manager resigns and takes charge of
/// another club inside the same save. No new career, no questions —
/// the manager name carries over, `player_manager.team_id` moves to
/// the new club's main team, the current slot is re-saved and the new
/// world is published. The old club simply returns to AI control.
pub async fn game_takeover_action(
    State(state): State<GameAppData>,
    Json(request): Json<TakeoverRequest>,
) -> ApiResult<Json<SaveListEntry>> {
    let _guard = Arc::clone(&state.process_lock)
        .try_lock_owned()
        .map_err(|_| ApiError::BadRequest("game is busy (processing in progress)".to_string()))?;

    let (slug, saves_dir) = {
        let meta = state.saves.read().await;
        let slug = meta
            .current_slug
            .clone()
            .ok_or_else(|| ApiError::BadRequest("no active career".to_string()))?;
        (slug, meta.saves_dir.clone())
    };

    let world_arc = {
        let guard = state.data.read().await;
        guard
            .as_ref()
            .map(Arc::clone)
            .ok_or_else(|| ApiError::InternalError("simulator data not loaded".to_string()))?
    };

    // Validate on the shared snapshot before paying for the deep clone.
    {
        let manager = world_arc
            .player_manager
            .as_ref()
            .ok_or_else(|| ApiError::BadRequest("no active career".to_string()))?;
        if manager.team_id == request.team_id {
            return Err(ApiError::BadRequest(
                "already managing this club".to_string(),
            ));
        }
        let team = world_arc.team(request.team_id).ok_or_else(|| {
            ApiError::NotFound(format!("team with id {} not found", request.team_id))
        })?;
        if team.team_type != core::TeamType::Main {
            return Err(ApiError::BadRequest(
                "only a club's main team can be managed".to_string(),
            ));
        }
    }

    let team_id = request.team_id;
    let (entry, world) = spawn_blocking(move || -> Result<_, ApiError> {
        // Deep clone outside any lock (the shared slot still holds a
        // reference), mutate, then persist into the current slot.
        let mut world = Arc::unwrap_or_clone(world_arc);
        if let Some(manager) = world.player_manager.as_mut() {
            manager.team_id = team_id;
        }
        let entry = write_slot(&saves_dir, &slug, &world)?;
        Ok((entry, world))
    })
    .await
    .map_err(|e| ApiError::InternalError(format!("takeover task failed: {e}")))??;

    publish_world(&state, world).await;

    info!(
        "takeover: now managing team {} in slot '{}'",
        team_id, entry.slug
    );
    Ok(Json(entry))
}

// ---------------------------------------------------------------------------
// GET /api/game/session — powers the layout session chrome
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct SessionInfo {
    pub active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team_slug: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manager_name: Option<String>,
    /// Added for the club-first sidebar: the managed team's league, so
    /// the layout can link straight to the league table.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub league_slug: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub league_name: Option<String>,
}

pub async fn game_session_action(State(state): State<GameAppData>) -> Json<SessionInfo> {
    let slug = state.saves.read().await.current_slug.clone();

    let manager = {
        let guard = state.data.read().await;
        guard.as_ref().and_then(|world| {
            world.player_manager.as_ref().map(|m| {
                let team = world.team(m.team_id);
                let league = team
                    .and_then(|t| t.league_id)
                    .and_then(|id| world.league(id));
                (
                    m.team_id,
                    m.name.clone(),
                    team.map(|t| t.name.clone()),
                    team.map(|t| t.slug.clone()),
                    league.map(|l| l.slug.clone()),
                    league.map(|l| l.name.clone()),
                )
            })
        })
    };

    match manager {
        Some((team_id, manager_name, team_name, team_slug, league_slug, league_name)) => {
            Json(SessionInfo {
                active: true,
                slug,
                team_id: Some(team_id),
                team_name,
                team_slug,
                manager_name: Some(manager_name),
                league_slug,
                league_name,
            })
        }
        None => Json(SessionInfo {
            active: false,
            slug: None,
            team_id: None,
            team_name: None,
            team_slug: None,
            manager_name: None,
            league_slug: None,
            league_name: None,
        }),
    }
}
