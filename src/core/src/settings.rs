//! Process-wide feature switches.
//!
//! New file in this fork (not present upstream). Follows the established
//! `MatchRuntime` pattern of process-global runtime settings applied once at
//! startup, before the world is generated.

use std::sync::atomic::{AtomicBool, Ordering};

/// Whether international football (national-team call-ups and competitions,
/// continental club competitions, the bundled UEFA U21 layer) is simulated.
/// The Polish Football Manager world models a single country's pyramid, where
/// these layers have no participants and are switched off via
/// `--no-international`.
static INTERNATIONAL_ENABLED: AtomicBool = AtomicBool::new(true);

pub fn set_international_enabled(enabled: bool) {
    INTERNATIONAL_ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn international_enabled() -> bool {
    INTERNATIONAL_ENABLED.load(Ordering::Relaxed)
}

/// Whether the engine may invent players.
///
/// Upstream mints players freely: clubs with no data in the world database
/// get a fully synthetic squad, youth teams are generated from the academy
/// model, and academy intake keeps producing newgens every intake window.
///
/// Polish Football Manager is built on the premise that every footballer in
/// the world is a real one, imported from public sources. A generated player
/// is indistinguishable from a real one once he is in the save, so the rule
/// has to hold at the source rather than be filtered later — hence a switch
/// applied before world generation, checked at every mint site.
///
/// Turned off via `--no-synthetic-players`. The cost is real and deliberate:
/// clubs whose source data is thin stay thin (a club with seven real players
/// keeps seven), and retirements are not replaced, so the player pool shrinks
/// season by season until fresh data is imported.
static SYNTHETIC_PLAYERS_ENABLED: AtomicBool = AtomicBool::new(true);

pub fn set_synthetic_players_enabled(enabled: bool) {
    SYNTHETIC_PLAYERS_ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn synthetic_players_enabled() -> bool {
    SYNTHETIC_PLAYERS_ENABLED.load(Ordering::Relaxed)
}

/// Whether fixtures the manager is not involved in are resolved by the
/// statistical model in `match::quick` instead of the tick engine.
///
/// The tick engine costs ~250 ms of CPU per fixture. On a full Polish
/// pyramid a matchday is ~400 fixtures, of which the manager watches at
/// most one — measured, that is 98 s of wall clock to advance a single
/// day, against 5-9 s for a day with no football on it. The statistical
/// model produces the same `MatchResultRaw` shape (scoreline, scorers,
/// assists, cards, minutes, per-player stat lines, ratings) from squad
/// ability, so tables, season stats and player development carry on
/// unchanged; what it cannot produce is position data, which is why the
/// managed club's own fixtures always go to the real engine.
///
/// Turned on via `--quick-other-matches`. Off by default: a headless
/// simulation run studying the engine's own output must not silently get
/// a different model.
static QUICK_OTHER_MATCHES: AtomicBool = AtomicBool::new(false);

pub fn set_quick_other_matches(enabled: bool) {
    QUICK_OTHER_MATCHES.store(enabled, Ordering::Relaxed);
}

pub fn quick_other_matches() -> bool {
    QUICK_OTHER_MATCHES.load(Ordering::Relaxed)
}
