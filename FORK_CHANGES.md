# Fork changes vs upstream Open Football

This fork adapts Open Football (https://github.com/ZOXEXIVO/open-football,
Apache-2.0) to serve as the simulation engine of Polish Football Manager.
Per Apache License 2.0 §4(b), this file is the running log of modified files.
Heavily modified files additionally carry a `// Modified from upstream` header.

Upstream base: commit f0b19d78 ("Rating system improve", v1.4.840).

## Removed

- `src/database/src/data/database.db` — upstream embedded world database
  (real-world club/player data of unclear provenance; replaced by an
  externally supplied database file generated from our own data sources).

## Added

- `NOTICE` — Apache-2.0 attribution.
- `FORK_CHANGES.md` — this file.
- `src/headless.rs` — `simulate` and `validate-db` subcommand implementations.
- `src/core/src/settings.rs` — process-wide feature switches
  (`international_enabled`).
- `src/database/src/data/test-reference.db` — reference-only test fixture
  (continents, countries, name pools; no leagues/clubs/players) used by unit
  tests in place of the removed embedded database.
- `Cargo.toml` — added the `quick` build profile (release semantics, thin
  LTO) for day-to-day simulation runs.

## Modified

- `src/database/src/loaders/compiled.rs` — world database is loaded from an
  external file (`--database=`, `OF_DATABASE_PATH`, `./polish-database.db`)
  instead of `include_bytes!`; added `set_database_path` / `database_path` /
  `load_from_path`; test builds fall back to the bundled reference fixture.
- `src/database/build.rs` — dropped the rerun-if-changed tracking of the
  removed embedded database.
- `src/database/src/loaders/mod.rs`, `src/database/src/lib.rs` — export the
  new compiled-database API.
- `src/database/src/loaders/data_tree.rs`, `loaders/country.rs`,
  `loaders/players.rs` — removed tests that asserted snapshot counts and
  content of the removed upstream embedded database.
- `src/database/src/generators/generator/mod.rs` — national-competition
  configs (incl. the bundled UEFA U21 layer) are skipped when international
  football is disabled.
- `src/core/src/lib.rs` — registered the new `settings` module.
- `src/core/src/simulator/mod.rs` — national-team call-ups, world national
  competitions, global competitions and national-team release are skipped
  when international football is disabled.
- `src/core/src/continent/result/mod.rs` — continental club competition
  draws/simulation skipped when international football is disabled.
- `src/core/src/league/simulation/matchday.rs` — bottom-5 rival check uses
  `saturating_sub`; leagues with fewer than 5 teams underflowed (panic) here.
- `src/web/src/settings.rs` — added `RunMode` (serve / simulate /
  validate-db), `--database=`, `--no-international`.
- `src/web/src/lib.rs` — export `RunMode`.
- `src/main.rs` — subcommand dispatch; headless modes default to quiet
  logging.
