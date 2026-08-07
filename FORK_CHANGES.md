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
- `src/web/src/map/` (`mod.rs`, `routes.rs`, `index.html`, `geometry.rs`) —
  interactive club-selection map of Poland at `GET /{lang}/map`
  (voivodeship SVG with live club counts; `?region={voivodeship}` drills
  into that voivodeship's football districts with their leagues and clubs).
  `geometry.rs` is generated from
  https://github.com/ppatrzyk/polska-geojson
  (`wojewodztwa/wojewodztwa-min.geojson`, MIT License, © 2019 Piotr
  Patrzyk; boundaries derived from GUGiK PRG public-sector open data),
  projected to a 600×560 viewBox. Powiat-level geometry was deliberately
  not used: the pyramid's district codes are football okręgi
  ("warszawa-i", "podhale", "wielkopolskie-iii"), which do not correspond
  to powiat boundaries, so level 2 renders as a styled district list
  instead of a sub-map.

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
- `src/web/src/lib.rs` — export `RunMode`; registered the new `map` module.
- `src/web/src/routes.rs` — merged the club-selection map routes.
- `src/web/src/views/mod.rs` — added `map_section` / `map_menu` sidebar
  entries ("Mapa") to the main menus.
- `src/web/src/countries/list/index.html` — home page links to the
  club-selection map ("Nowa kariera — wybierz klub z mapy") above the
  saves panel.
- `src/web/assets/static/css/style.css` — styles for the club-selection
  map (voivodeship SVG, legend, district/league/club lists, home CTA).
- `src/web/assets/i18n/en.json`, `pl.json` — added `map`, `choose_region`,
  `districts`, `clubs`, `new_career_map` keys.
- `src/main.rs` — subcommand dispatch; headless modes default to quiet
  logging.
