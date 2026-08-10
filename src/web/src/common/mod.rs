//! Modified from upstream: what used to be a grab-bag of page helpers
//! (CSS versioning, static-file serving with language-prefix redirects,
//! slugs, "potential stars" and friendly-source lookups for the team and
//! player pages) is down to the two things a UI-less engine still needs:
//! the embedded asset bundle the i18n catalogues read from, and the
//! machine identity a worker reports during its handshake.

use rust_embed::RustEmbed;
use std::sync::LazyLock;
use sysinfo::{CpuRefreshKind, RefreshKind, System};

/// Embedded `assets/` tree. Only `assets/i18n/**` is read now — the
/// `assets/static/**` half was the stylesheet and fonts of the removed
/// UI and is dead weight in the binary until someone prunes it.
#[derive(RustEmbed)]
#[folder = "assets/"]
pub struct Assets;

/// Machine hostname, resolved once at startup.
pub static COMPUTER_NAME: LazyLock<String> = LazyLock::new(|| {
    hostname::get()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "unknown".to_string())
});

/// CPU brand string (e.g. "AMD Ryzen 9 7950X 16-Core Processor"), resolved once at startup.
pub static CPU_BRAND: LazyLock<String> = LazyLock::new(|| {
    let sys =
        System::new_with_specifics(RefreshKind::nothing().with_cpu(CpuRefreshKind::nothing()));
    sys.cpus()
        .first()
        .map(|c| c.brand().trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Unknown CPU".to_string())
});
