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
