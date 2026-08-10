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
