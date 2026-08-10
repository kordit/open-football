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

- **The entire user interface.** This fork serves no HTML: the game's
  front end is the Blade panel in the parent repository, which reads the
  world through `GET /api/world/snapshot` and drives it through
  `/api/game/*`. Roughly 30 000 lines and all 59 askama templates went,
  including the sections this game has no use for at all — upstream's
  Champions League, Europa League, Conference League, Copa Libertadores
  and national-team competition pages. (International football was
  already switchable off in the simulation via `--no-international`;
  this removes its interface as well.)

  Deleted in full: `src/web/src/{about, champions_league,
  conference_league, copa_libertadores, countries, cups, europa_league,
  leagues, national_competitions, playoffs, staff, teams, watchlist,
  views, map, face, search}`, `src/web/src/layout.html`,
  `src/web/src/player/{awards, contract, events, get, history, matches,
  newspaper, personal, relations, transfers}` and
  `src/web/src/player/player_layout.html`, `src/web/src/match/get/`,
  `src/web/src/workers/index.html`.

  Also deleted: `src/web/src/ai/` — the LLM agent, client and tool
  definitions existed solely to power the "AI report" dialogs on the team
  and player pages, so it had no caller left.

  The club-selection map was not lost: `src/web/src/map/geometry.rs` was
  converted to `resources/data/poland-voivodeships.json` in the parent
  repository and is rendered by the Blade club picker. Attribution for
  that geometry moved with it (MIT, © 2019 Piotr Patrzyk; GUGiK PRG).

  Note: `assets/static/**` (the stylesheet and fonts of the removed UI)
  is still embedded in the binary as dead weight — `assets/i18n/**` in the
  same tree is live, so pruning it means narrowing the `RustEmbed` folder
  rather than deleting the directory. That pruning is still pending; what
  has already happened is the rescue of the four files in there that the
  parent repository needs, copied out ahead of the deletion:

  | From | To (parent repository) |
  |---|---|
  | `assets/static/js/pixi.min.js` | `public/vendor/pixi/pixi.min.js`, with its MIT licence text alongside in `public/vendor/pixi/LICENSE` |
  | `assets/static/images/match/field.svg`, `ball.png` | `public/img/match/` |
  | `assets/static/images/player/pole.png` | `public/img/player/` |

  PixiJS 8.6.6 and the pitch artwork are what the removed match page drew
  its 2D replay with (`src/web/src/match/get/index.html`, and the still-live
  dev harness `.dev/match/src/viewer.html`). The Blade panel is taking that
  renderer over, so the assets follow it rather than dying with the page.
  Licences travel with the files, same treatment the map geometry got above.

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
- `src/web/src/snapshot/` (`mod.rs`, `routes.rs`) — `GET /api/world/snapshot`,
  the read model consumed by the Laravel panel. Serialises the world
  (league tables, clubs, teams, fixtures with results, players with full
  skill/contract/statistics detail) as JSON, gzipped when the client
  advertises it. `scope=delta` (default) carries tables plus the managed
  club's squad; `scope=full` carries every club and player, for the
  initial sync and season-rollover resync. `since=YYYY-MM-DD` trims the
  fixture list. This endpoint exists because the fork's UI is being
  retired in favour of the Blade panel in the parent repository — the
  engine keeps ownership of career state (the save), and Postgres holds
  a projection rebuilt from this endpoint.

- `src/core/src/match/engine/engine/stepper.rs` — one period of a match,
  one tick at a time. `PeriodLoop` holds the eighteen locals `play_inner`
  used to keep on its stack, and `FootballEngine::step_period_tick` carries
  the former loop body. Upstream can only run a match to completion inside
  its own `while` loop; a match a human watches has to hand control back
  between ticks. Nothing inside a tick changed — see the gate below.

- `src/core/src/match/engine/engine/live.rs` — `LiveMatch`: a match somebody
  is watching. Owns the four pieces of live state, walks them with the
  stepper, and stops — at a horizon the caller names, and at half time,
  before extra time and before penalties. `apply` takes the manager's two
  commands (substitution, instruction) between ticks, never inside one;
  `snapshot` is what the panel draws; `finish_headless` is what happens when
  the manager closes the tab and the league still needs a result.

  Upstream has no such thing: a match runs to its final whistle or not at all.

- `src/core/src/match/dispatch.rs` — added `MatchInterceptor` and
  `MatchInterceptorRegistry`: one fixture claimed out of a matchday and played
  by hand. Deliberately not a second `MatchDispatcher` — that trait is
  all-or-nothing over a batch and its registry is a startup `OnceLock`, while
  a live match is a session that installs and clears while the process runs
  (hence `RwLock`, and `try_get` handing back an `Arc` rather than a guard: a
  read lock held for ninety minutes would deadlock the session clearing it).

- `src/core/src/match/engine/engine/live_tests.rs` — the commands actually
  land: a manual substitution keeps the outgoing player's stat line and
  physical snapshot (i.e. it went through `execute_substitution`, not around
  it), an illegal one spends no quota and moves nobody, a manual instruction
  outlives sixty evaluator passes, half time stops the match until it is told
  otherwise, and a match abandoned at the 20th minute still finishes to full
  time with stat lines. Gated `cfg(not(debug_assertions))` for the same
  reason as the identity gate.

- `src/core/src/match/engine/engine/stepper_identity_tests.rs` — that gate.
  Twenty seeds, the batch driver against an external driver that stops every
  137 ticks, compared on the whole `MatchResultRaw`: goals with their
  minutes, substitutions, every field of every player's stat line, the
  physical snapshots the post-match condition drop reads, stoppage time,
  final tactics, and the serialised replay recording.

  Two things had to be worked around to make the comparison mean anything,
  and both are properties of the engine rather than of the stepper:

  * **A match is not reproducible from `MatchEngineConfig::seed` alone.**
    Player AI states draw from `IntegerUtils::random`, i.e. the
    process-global thread-local stream in `utils::random::engine`, and that
    stream carries on between matches. The same fixture played twice in one
    process returns two different scorelines unless
    `RandomEngine::set_seed` is called in between. The test pins both
    streams and detects the case where a parallel test in the crate
    re-seeds the global one mid-match.
  * **The gate cannot run under `debug_assertions`.** A full match in a
    debug build trips a pre-existing `debug_assert_eq!` in
    `player/strategies/processor.rs` ("loose-ball yield chase-table
    mismatch"), which compares the once-per-tick chase table against a
    rescan. Verified against the unmodified engine in a clean worktree, so
    it is not something this work introduced; no test in the crate had ever
    played a full match in debug, which is why it had gone unnoticed. The
    module is therefore `#[cfg(not(debug_assertions))]` and the gate runs
    as `cargo test --profile quick -p core stepper_identity` — which is
    also the only useful profile, since `MATCH_HALF_TIME_MS` is five
    minutes under `debug_assertions` and forty-five without.

- `src/web/src/lineup/` (`mod.rs`, `routes.rs`) — `POST /api/game/lineup`,
  the manager-set starting eleven and formation for the managed club.
  Upstream has no human selection at all; this pins the requested players
  via `Player.is_force_match_selection` (clearing the pin across the whole
  club first, so the endpoint is idempotent) and sets `Team.tactics` to
  the requested `MatchTacticType`. The bench is deliberately left to the
  club's coach — pinning substitutes fights the in-match substitution
  logic, which reads form and game state. Every other club in the world
  keeps selecting for itself.

- ~~`src/web/src/map/`~~ — an interactive club-selection map of Poland was
  added here and has since been **removed with the rest of the interface**.
  Its geometry lives on in the parent repository as
  `resources/data/poland-voivodeships.json`, rendered by the Blade club
  picker, and carries the same attribution: generated from
  https://github.com/ppatrzyk/polska-geojson
  (`wojewodztwa/wojewodztwa-min.geojson`, MIT License, © 2019 Piotr
  Patrzyk; boundaries derived from GUGiK PRG public-sector open data),
  projected to a 600×560 viewBox. Powiat-level geometry was deliberately
  not used: the pyramid's district codes are football okręgi
  ("warszawa-i", "podhale", "wielkopolskie-iii"), which do not correspond
  to powiat boundaries, so the second level is a district list rather
  than a sub-map. The entry is kept here rather than dropped because §4(b)
  asks for a running log, and the geometry's licence follows the file.

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
- `src/core/src/match/pool.rs` — `play()` consults the interceptor before the
  dispatcher (which would otherwise ship the manager's own match to a remote
  worker), and when a fixture is claimed, plays **the rest of the day
  concurrently with it**. That is a departure from the plan, which ran the
  batch first: a manager watching ninety minutes is ninety minutes of wall
  clock the other ~450 fixtures were going to cost anyway, so a matchday now
  costs the longer of the two rather than their sum — and the tables are live
  while the match is on. The claimed result is re-inserted at its original
  index, because `WorldMatchdayResult::process` slices results by
  `continent_ranges` and a result in the wrong slot lands in the wrong league.
- `src/core/src/match/engine/substitution/substitutions.rs` — added
  `execute_manual_substitution`, the manager's door into the *same*
  `execute_substitution` the AI passes use. Deliberately not
  `field.substitute_player`: that swaps the players and skips the stat line,
  the physical snapshot, the entry stamp, the stoppage time and the aggregate
  invalidation, so the match finishes with wrong ratings and a wrong
  condition drop and nothing says so. Validation mirrors the AI pass —
  shared quota, no replacing a sent-off player, keeper only for keeper — and
  refusals come back as a typed `SubstitutionError` so the panel can explain
  them at the row the manager clicked. Visibility of nothing was widened: it
  is a free function in the same file.
- `src/core/src/match/engine/flow/result.rs` — added
  `SubstitutionReason::Manual` plus `is_managerial_choice()`. A player hooked
  by a human is exactly as unhappy as one hooked by the AI, so the three
  places that compared against `Discretionary` (`league/result/match_events.rs`
  ×2, `simulator/newsroom.rs`) now ask the predicate instead — adding a
  reason can no longer silently fall out of the frustration and press paths.
- `src/core/src/league/result/match_events.rs`,
  `src/core/src/simulator/newsroom.rs` — those three comparisons.
- `src/core/src/match/engine/teamplay/coach.rs` — `MatchCoach` gained
  `manual_instruction`, with `set_manual_instruction` /
  `release_manual_instruction` / `instruction_is_manual`. Releasing leaves
  the current instruction where the manager put it; the next evaluation moves
  it if it disagrees, which avoids a visible flicker for no gain.
- `src/core/src/match/engine/engine/shape.rs` — `evaluate_coaches` skips
  `evaluate_with_metrics` for a side whose instruction is manual. **Only**
  those two calls are gated: both `build_rolling_metrics` calls still run,
  because they rotate the 15-minute snapshot, and a side handing control back
  to the assistant must find a warm window rather than one that reads the
  whole match as a single delta.
- `src/core/src/match/engine/engine/run.rs` — `play_with_config` split: the
  preamble (score, tactics snapshot, field, context, chemistry seeding,
  crowd arousal, kickoff) became `setup()`, which hands back the four
  pieces of live match state, and `play_inner` became three lines around
  `stepper::PeriodLoop`. Sequence unchanged and unreordered; several steps
  there touch the RNG or stamp every player.
- `src/core/src/match/engine/engine/positions.rs` — `POSITION_RECORD_INTERVAL_MS`
  lifted out of the `FootballEngine<W, H>` impl to module scope so the
  stepper, which is not generic over the pitch size, arms its recording
  cursor from the same constant. The associated const remains as an alias.
- `src/core/src/match/engine/engine/mod.rs` — registered `stepper` and the
  identity-gate test module.
- `src/core/src/league/simulation/matchday.rs` — bottom-5 rival check uses
  `saturating_sub`; leagues with fewer than 5 teams underflowed (panic) here.
- `src/web/src/settings.rs` — added `RunMode` (serve / simulate /
  validate-db), `--database=`, `--no-international`.
- `src/web/src/lib.rs` — export `RunMode`; module list cut down to the
  JSON API (`snapshot` and `lineup` added, every page module dropped);
  `GameAppData` lost its `ai` / `ai_jobs` fields with the AI module; the
  startup log line no longer advertises a UI.
- `src/web/src/routes.rs` — rewritten as a pure JSON router: one route
  group per API area, `GET /` returns a service descriptor instead of a
  language redirect, and the language-prefix middleware, `/sitemap.xml`
  and the redirect-to-home error handler are gone with the pages.
- `src/web/src/common/mod.rs` — collapsed from a page-helper grab-bag
  (CSS versioning, embedded static-file serving with language-prefix
  redirects, slugs, "potential stars", friendly-source lookups) to the
  embedded `Assets` bundle the i18n catalogues read and the machine
  identity a worker reports in its handshake.
- `src/web/src/workers/mod.rs`, `workers/routes.rs` — the `/{lang}/workers`
  operator page and its row DTO removed; the registry JSON endpoints stay,
  because the distributed match dispatcher is driven through them.
- `src/web/src/player/mod.rs`, `match/mod.rs`, `match/routes.rs` — reduced
  to the endpoints the panel calls (manager actions on players; match
  replay metadata and chunks).
- `src/web/src/game/saves.rs` — `write_slot` and `publish_world` widened to
  `pub(crate)` so the lineup handler persists and publishes the world the
  same way takeover does. (File is itself fork-added.)
- `src/main.rs` — subcommand dispatch; headless modes default to quiet
  logging; no longer opens a browser at startup (there is no page to open).
- `src/web/assets/i18n/en.json`, `pl.json` — added `map`, `choose_region`,
  `districts`, `clubs`, `new_career_map` keys. Now unused (the pages that
  read them are gone), kept only because the catalogue completeness tests
  compare every locale against English.

## Data files

Two directories in this tree are runtime artefacts of playing the game, not
sources, and both are now ignored (`saves`, `*.ofs`, `match_results`):

- `saves/` — OFSV career saves. One save of the Polish pyramid is 5-62 MB and
  a slot autosaves on every tick, so the directory grows without bound. Five
  of these files (214 MB) had been committed by mistake, starting with
  `a793c956` (the save-slot session layer); they were removed from the index
  here and purged from the fork's commits with `git filter-repo` while the
  fork was still unpushed. The files themselves are untouched on disk — a
  save is a player's career, and the engine is its owner.

  The purge was deliberately scoped `--refs f0b19d78..master`, so only this
  fork's own commits were rewritten and every upstream commit keeps its
  original identity. Rewriting the whole history would have been simpler and
  reclaimed the same space, but it renames upstream's commits too, and a fork
  that no longer descends from upstream cannot cherry-pick upstream fixes.
  That matters here more than in most forks: the match engine under
  `src/core/src/match/` is still substantially upstream code and is where the
  next round of fork work lands.
- `match_results/` — recorded match replays: gzipped position chunks at
  ~44 MB per recorded match, already ignored upstream of this change. Nothing
  prunes them yet (a retention policy is planned alongside the live-match
  work); they are safe to delete at any time, at the cost of losing the
  ability to re-watch past matches. The save file never contained them —
  `MatchResultRaw.position_data` is `#[serde(skip)]`.

Earlier entries for `src/web/src/views/mod.rs`,
`src/web/src/countries/list/index.html` and
`src/web/assets/static/css/style.css` (sidebar entry, home-page CTA and
styles for the club-selection map) no longer apply — those files were
deleted with the rest of the interface; see Removed.
