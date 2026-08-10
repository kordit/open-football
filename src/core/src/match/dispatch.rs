use crate::r#match::{Match, MatchResult, MatchResultRaw, MatchSquad};
use std::sync::{Arc, OnceLock, RwLock};

/// Pluggable executor for match work. When installed via
/// [`MatchDispatcherRegistry::set`], `MatchPlayEnginePool` consults the
/// dispatcher first and only falls back to the local rayon thread-pool
/// when the dispatcher declines.
///
/// Distributed worker support (see `web::worker`) installs a
/// `DistributedDispatcher` here at startup, but the trait is plain
/// enough that tests / alternative back-ends can plug in too.
///
/// Ownership: the dispatcher takes the input by value. On `Ok` it
/// commits to producing all results (in the same order as the input)
/// — local failures are the dispatcher's job to backfill. On `Err` it
/// hands the input back unchanged so the pool can run the local rayon
/// path without re-allocating.
pub trait MatchDispatcher: Send + Sync {
    fn dispatch_league(&self, matches: Vec<Match>) -> Result<Vec<MatchResult>, Vec<Match>>;
    fn dispatch_squads(
        &self,
        matches: Vec<(usize, MatchSquad, MatchSquad, bool)>,
    ) -> Result<Vec<(usize, MatchResultRaw)>, Vec<(usize, MatchSquad, MatchSquad, bool)>>;
}

/// Process-wide handle to the active [`MatchDispatcher`]. The binary
/// wires a dispatcher at startup without `core` having to know what
/// `web` does.
pub struct MatchDispatcherRegistry;

static DISPATCHER: OnceLock<Box<dyn MatchDispatcher>> = OnceLock::new();

impl MatchDispatcherRegistry {
    /// Install the process-wide dispatcher. First call wins — subsequent
    /// calls are silently ignored so a duplicated startup path can't
    /// race-replace an already-published dispatcher.
    pub fn set(dispatcher: Box<dyn MatchDispatcher>) {
        let _ = DISPATCHER.set(dispatcher);
    }

    /// Borrow the active dispatcher, if any.
    pub fn try_get() -> Option<&'static dyn MatchDispatcher> {
        DISPATCHER.get().map(|b| b.as_ref())
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Added in this fork: one match taken out of the day for a human to play
// ───────────────────────────────────────────────────────────────────────────

/// Claims a single fixture out of a matchday and plays it interactively.
///
/// The seam is here, in front of the pool, rather than anywhere earlier
/// because a matchday's squads are built *inside* the day — after training,
/// after injuries, after form. Building the human's squad on the side would
/// mean a team picked from a world one phase too old, and a fork of
/// `MatchdaySimulator::build_match` to do it.
///
/// This is deliberately not a second [`MatchDispatcher`]. That trait is
/// all-or-nothing over a whole batch and its registry is a `OnceLock` claimed
/// at startup; "one of these, the rest as usual" does not fit it, and a live
/// session has to be installable and removable while the process runs.
pub trait MatchInterceptor: Send + Sync {
    /// Whether this fixture is the one being played by hand.
    fn claims(&self, match_id: &str) -> bool;

    /// Play it, blocking until the final whistle.
    ///
    /// Blocking is the contract: the caller is the simulation thread, and the
    /// day cannot be folded up until this result exists. What must NOT happen
    /// is blocking forever — an implementation owns its own watchdog and
    /// finishes the match without the human if they stop answering.
    fn play(&self, fixture: Match) -> MatchResult;
}

/// Process-wide handle to the active [`MatchInterceptor`].
///
/// `RwLock` rather than `OnceLock`: a live match is a session, not a
/// deployment choice. It is installed when the manager kicks off and removed
/// when the whistle goes, and the next one installs cleanly after it.
pub struct MatchInterceptorRegistry;

static INTERCEPTOR: RwLock<Option<Arc<dyn MatchInterceptor>>> = RwLock::new(None);

impl MatchInterceptorRegistry {
    /// Install the interceptor for one live session, replacing any previous.
    pub fn set(interceptor: Arc<dyn MatchInterceptor>) {
        if let Ok(mut slot) = INTERCEPTOR.write() {
            *slot = Some(interceptor);
        }
    }

    /// Remove it. Safe to call when nothing is installed.
    pub fn clear() {
        if let Ok(mut slot) = INTERCEPTOR.write() {
            *slot = None;
        }
    }

    /// Borrow the active interceptor, if any.
    ///
    /// Returns an owned `Arc` rather than a guard on purpose: the caller holds
    /// it across a whole match, and holding a read lock for ninety minutes
    /// would deadlock the session that wants to clear it afterwards.
    pub fn try_get() -> Option<Arc<dyn MatchInterceptor>> {
        INTERCEPTOR.read().ok().and_then(|slot| slot.clone())
    }
}
