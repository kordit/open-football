//! World persistence (save/load) — new in this fork, upstream has none.
//!
//! On-disk layout of a save file (`.ofs`):
//!
//! ```text
//! [0..4)   magic          b"OFSV"                (raw bytes)
//! [4..8)   format_version u32 little-endian      (raw bytes)
//! [8..12)  header_len     u32 little-endian      (raw bytes)
//! [12..12+header_len)     bincode(SaveHeader)    (uncompressed)
//! [rest]                  zstd(bincode(SimulatorData))
//! ```
//!
//! Magic + version + header live outside the compressed body so
//! [`read_header`] can list/inspect saves without decompressing a
//! multi-megabyte world.

use crate::SimulatorData;
use crate::shared::SimulatorDataIndexes;
use crate::utils::random::engine as rng_engine;
use chrono::NaiveDateTime;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

/// File magic — "Open Football SaVe".
pub const SAVE_MAGIC: [u8; 4] = *b"OFSV";
/// Bump on any incompatible change to the header or body encoding.
pub const SAVE_FORMAT_VERSION: u32 = 1;
/// zstd compression level for the world body. Level 3 is the codec's
/// balanced default: near-max ratio on this kind of struct soup at a
/// fraction of the CPU of the higher levels.
const ZSTD_LEVEL: i32 = 3;

/// Save-file header: everything a "load game" screen needs without
/// touching the compressed world body.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SaveHeader {
    pub magic: [u8; 4],
    pub format_version: u32,
    /// Engine crate version that wrote the save (`CARGO_PKG_VERSION`).
    pub engine_version: String,
    /// Sim RNG seed pinned when the world was created, if any. Re-pinned
    /// on load so post-load randomness stays on the same stream family.
    pub world_seed: Option<u64>,
    /// The world's simulated date at save time.
    pub in_game_date: NaiveDateTime,
    pub save_name: String,
    /// Reserved for the career layer — always `None` from headless runs.
    pub managed_team_id: Option<u32>,
    /// Wall-clock creation time, supplied by the caller (core avoids
    /// calling `Utc::now()` itself).
    pub created_at: NaiveDateTime,
}

impl SaveHeader {
    /// Build a header for `data` as it stands. `magic` / `format_version`
    /// / `engine_version` / `world_seed` are filled in automatically;
    /// the caller supplies the display name and wall-clock timestamp.
    pub fn for_world(
        save_name: String,
        managed_team_id: Option<u32>,
        created_at: NaiveDateTime,
        data: &SimulatorData,
    ) -> Self {
        SaveHeader {
            magic: SAVE_MAGIC,
            format_version: SAVE_FORMAT_VERSION,
            engine_version: env!("CARGO_PKG_VERSION").to_string(),
            world_seed: rng_engine::current_seed(),
            in_game_date: data.date,
            save_name,
            managed_team_id,
            created_at,
        }
    }
}

fn bincode_config() -> bincode::config::Configuration {
    bincode::config::standard()
}

/// Serialize the whole world to `path`. Overwrites any existing file.
/// The header's magic / format_version are normalised to the current
/// constants regardless of what the caller passed.
pub fn save_world(path: &Path, header: &SaveHeader, data: &SimulatorData) -> Result<(), String> {
    let mut header = header.clone();
    header.magic = SAVE_MAGIC;
    header.format_version = SAVE_FORMAT_VERSION;

    let header_bytes = bincode::serde::encode_to_vec(&header, bincode_config())
        .map_err(|e| format!("save: header encode failed: {e}"))?;

    let file = File::create(path)
        .map_err(|e| format!("save: cannot create {}: {e}", path.display()))?;
    let mut writer = BufWriter::new(file);

    writer
        .write_all(&SAVE_MAGIC)
        .and_then(|_| writer.write_all(&SAVE_FORMAT_VERSION.to_le_bytes()))
        .and_then(|_| writer.write_all(&(header_bytes.len() as u32).to_le_bytes()))
        .and_then(|_| writer.write_all(&header_bytes))
        .map_err(|e| format!("save: header write failed: {e}"))?;

    let mut encoder = zstd::stream::Encoder::new(writer, ZSTD_LEVEL)
        .map_err(|e| format!("save: zstd init failed: {e}"))?;
    bincode::serde::encode_into_std_write(data, &mut encoder, bincode_config())
        .map_err(|e| format!("save: world encode failed: {e}"))?;
    let mut writer = encoder
        .finish()
        .map_err(|e| format!("save: zstd finish failed: {e}"))?;
    writer
        .flush()
        .map_err(|e| format!("save: flush failed: {e}"))?;
    Ok(())
}

/// Read magic + version + header from `path`, leaving the reader
/// positioned at the start of the compressed body. Returns the header
/// and the still-open reader.
fn read_preamble(path: &Path) -> Result<(SaveHeader, BufReader<File>), String> {
    let file =
        File::open(path).map_err(|e| format!("load: cannot open {}: {e}", path.display()))?;
    let mut reader = BufReader::new(file);

    let mut magic = [0u8; 4];
    reader
        .read_exact(&mut magic)
        .map_err(|e| format!("load: cannot read magic: {e}"))?;
    if magic != SAVE_MAGIC {
        return Err(format!(
            "load: {} is not an open_football save (bad magic {:?}, expected {:?})",
            path.display(),
            magic,
            SAVE_MAGIC
        ));
    }

    let mut ver = [0u8; 4];
    reader
        .read_exact(&mut ver)
        .map_err(|e| format!("load: cannot read format version: {e}"))?;
    let version = u32::from_le_bytes(ver);
    if version != SAVE_FORMAT_VERSION {
        return Err(format!(
            "load: unsupported save format version {version} (this engine reads version {SAVE_FORMAT_VERSION})"
        ));
    }

    let mut len = [0u8; 4];
    reader
        .read_exact(&mut len)
        .map_err(|e| format!("load: cannot read header length: {e}"))?;
    let header_len = u32::from_le_bytes(len) as usize;
    let mut header_bytes = vec![0u8; header_len];
    reader
        .read_exact(&mut header_bytes)
        .map_err(|e| format!("load: cannot read header ({header_len} bytes): {e}"))?;
    let (header, _): (SaveHeader, usize) =
        bincode::serde::decode_from_slice(&header_bytes, bincode_config())
            .map_err(|e| format!("load: header decode failed: {e}"))?;
    Ok((header, reader))
}

/// Read only the save header — cheap; never decompresses the world body.
pub fn read_header(path: &Path) -> Result<SaveHeader, String> {
    read_preamble(path).map(|(header, _)| header)
}

/// Load a full world from `path`. After deserialization the derived
/// state a save deliberately skips is reconstructed: lookup indexes are
/// rebuilt, the procedural player-id sequence is re-seeded past every
/// id in the world, and the sim RNG is re-pinned to the world's seed
/// (when one was recorded).
pub fn load_world(path: &Path) -> Result<(SaveHeader, SimulatorData), String> {
    let (header, reader) = read_preamble(path)?;

    let mut decoder = zstd::stream::Decoder::new(reader)
        .map_err(|e| format!("load: zstd init failed: {e}"))?;
    let mut data: SimulatorData =
        bincode::serde::decode_from_std_read(&mut decoder, bincode_config())
            .map_err(|e| format!("load: world decode failed: {e}"))?;

    // Re-pin the sim RNG before any post-load logic draws from it.
    if let Some(seed) = header.world_seed {
        rng_engine::RandomEngine::set_seed(seed);
    }

    // Rebuild the derived lookup indexes the save skips.
    let mut indexes = SimulatorDataIndexes::new();
    indexes.refresh(&data);
    data.indexes = Some(indexes);
    data.dirty_player_index = false;

    // Runtime academy intake / regens must never collide with an id
    // already present in the loaded world.
    data.seed_player_id_sequence();

    Ok((header, data))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::competitions::global::GlobalCompetitions;
    use crate::continent::Continent;
    use crate::country::Country;
    use crate::league::LeagueCollection;
    use chrono::NaiveDate;

    fn make_world(date: NaiveDate) -> SimulatorData {
        let country = Country::builder()
            .id(1)
            .code("pl".to_string())
            .slug("poland".to_string())
            .name("Poland".to_string())
            .continent_id(1)
            .reputation(4000)
            .leagues(LeagueCollection::new(Vec::new()))
            .clubs(Vec::new())
            .build()
            .unwrap();
        let continent = Continent::new(1, "Europe".to_string(), vec![country], Vec::new());
        SimulatorData::new(
            date.and_hms_opt(12, 0, 0).unwrap(),
            vec![continent],
            GlobalCompetitions::new(Vec::new()),
        )
    }

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("of_persistence_test_{}_{name}", std::process::id()))
    }

    #[test]
    fn save_load_round_trip_preserves_date_and_world_shape() {
        let date = NaiveDate::from_ymd_opt(2026, 8, 7).unwrap();
        let data = make_world(date);
        let path = temp_path("roundtrip.ofs");

        let created = NaiveDate::from_ymd_opt(2026, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        let header = SaveHeader::for_world("test save".to_string(), None, created, &data);
        save_world(&path, &header, &data).expect("save must succeed");

        // Header is readable without loading the body.
        let peeked = read_header(&path).expect("header must read back");
        assert_eq!(peeked.magic, SAVE_MAGIC);
        assert_eq!(peeked.format_version, SAVE_FORMAT_VERSION);
        assert_eq!(peeked.save_name, "test save");
        assert_eq!(peeked.in_game_date, data.date);

        let (loaded_header, loaded) = load_world(&path).expect("load must succeed");
        assert_eq!(loaded_header.save_name, "test save");
        assert_eq!(loaded.date, data.date, "in-game date must round-trip");
        assert_eq!(
            loaded.continents.len(),
            data.continents.len(),
            "continent count must round-trip"
        );
        assert_eq!(loaded.continents[0].countries.len(), 1);
        assert_eq!(loaded.continents[0].countries[0].name, "Poland");
        assert!(
            loaded.indexes.is_some(),
            "indexes must be rebuilt after load"
        );
        assert!(!loaded.dirty_player_index);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn bad_magic_is_a_clear_error() {
        let path = temp_path("badmagic.ofs");
        std::fs::write(&path, b"NOPE0000000000000000").unwrap();
        let err = read_header(&path).unwrap_err();
        assert!(err.contains("bad magic"), "got: {err}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn wrong_version_is_a_clear_error() {
        let path = temp_path("badver.ofs");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&SAVE_MAGIC);
        bytes.extend_from_slice(&999u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        std::fs::write(&path, &bytes).unwrap();
        let err = read_header(&path).unwrap_err();
        assert!(err.contains("unsupported save format version 999"), "got: {err}");
        let _ = std::fs::remove_file(&path);
    }
}
