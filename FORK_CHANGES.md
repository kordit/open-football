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
  (`international_enabled`, `synthetic_players_enabled`,
  `quick_other_matches`).
- `src/core/src/match/quick.rs` — statistical stand-in for the tick engine,
  used for every fixture the managed club is not playing in. Produces a full
  `MatchResultRaw` (scoreline with goal/assist/card details, `FieldSquad`s,
  substitutions, a per-player `PlayerMatchEndStats` line for everyone who
  appeared, and ratings computed by the engine's own `RatingContext`) from
  squad ability instead of from simulated play. It deliberately produces no
  `position_data` — a quick result is never recorded, replayed or watched.
  Every roll comes from a `MatchRng` seeded with the same per-fixture seed the
  real engine gets, so no draw touches the process-global
  `utils::random::engine` stream and swapping one fixture between the two
  paths cannot shift any other match's result.

  Why: measured on a full Polish pyramid (890 clubs, 25 273 players), a
  400-fixture matchday cost 97.8 s of wall clock against 5-9 s for a day with
  no football on it — the tick engine was ~80% of the cost of advancing a
  single day, for 399 matches nobody watches. With this path enabled the same
  matchday takes 19.2 s and the per-chunk match cost drops from 4-8.5 s to
  0 ms; the remainder is post-match world processing (league results, player
  development, morale, news), which is untouched. Score distribution over 450
  fixtures: 3.04 goals/match, 45% home / 26% draw / 29% away, modal results
  1-1, 1-0, 2-1.

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

- `src/web/src/live/` (`mod.rs`, `routes.rs`) — `/api/live/*`, the control
  plane of a match the manager plays themselves. Five short endpoints
  (`start`, `state`, `advance`, `command`, `abandon`); the long-running thing
  is the matchday, started by `start` and then left to run.

  Separate from `POST /api/game/process` because that endpoint holds the
  connection until the day is simulated, and a day now lasts as long as a
  football match — the panel's client gives up at 900 s, a match at real speed
  is ~5400. A live match is a session plus a stream of short requests.

  Pull, never push: the panel asks for the next slice of match time. Pause
  costs nothing (stop asking and the engine sits idle), speed is the client's
  business (8× is a wider window, not more requests), and a lost reply costs
  one repeat because the cursor is in milliseconds and travels in the request.
  `advance` answers a cursor mismatch with 409 and the real cursor, so two
  panel tabs correct each other instead of silently interleaving.

  The `LiveMatch` lives on the simulation thread — the one parked inside
  `MatchPlayEnginePool::play` waiting for this fixture's result. Handlers send
  it messages and wait; that is what puts a substitution between two ticks
  rather than inside one. The waits run on `spawn_blocking`, never on a tokio
  worker.

  The watchdog is `recv_timeout` **on that same thread**, deliberately: a
  watchdog in its own task can die and leave the simulation thread — and with
  it the process lock and the whole matchday — blocked forever. Two minutes of
  silence, or a three-hour ceiling, and the assistant finishes the match. So
  does `abandon`: there is no un-playing a fixture, and the league is waiting
  for a result either way.

  `advance` also answers with `frames` and `incidents` — the picture, not just
  the scoreline. `frames` is a slice of the live recording
  (`ResultMatchPositionData::window`) covering exactly the football just
  played: ball and player positions, passes, per-player states. `incidents`
  are the goals, fouls and cards awarded inside the same window, each carrying
  its own timestamp so a viewer running at 12× does not put a goal on the
  scoreboard twelve seconds before the shot. Both ride along with `advance`
  rather than living at their own endpoint: the window is the one just
  simulated, so a separate request would carry the same two cursors and could
  only return the same answer, one round trip later.

  `POST /api/live/demo` starts a live match that is on nobody's calendar: two
  real squads, no fixture, no matchday, no process lock, and the result
  dropped at the final whistle. The world is read once (to pick the elevens)
  and never written. It exists because the 2D pitch is the hardest part of the
  panel to get right and the slowest to reach through a career — one attempt
  per matchday, and a mistake costs a season to retry. Deliberately the same
  `LiveMatch` and the same five endpoints rather than a mock: a stub fed canned
  frames would confirm the drawing and nothing else.

  A demo asked for while a demo is running **replaces** it — the match being
  replaced has no result, no calendar and no consequences, and "start another
  one" is the whole point of that screen. A fixture is never taken over: the
  league is waiting for its result and the matchday is parked inside it. The
  takeover waits for the outgoing session to report itself done, and every
  match now tidies its slot with `LiveRegistry::clear_if(session_id)` rather
  than `clear()` — the tidying runs after the session is marked done, which is
  exactly the moment the next one is allowed to move in, so an unconditional
  clear would wipe the newcomer and leave a match that starts fine and then
  reports that it does not exist.

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

### `core/src/club/team/tactics/plan.rs` — new

The manager's plan on two axes: `AttackingPlan` (balanced / possession /
direct / counter / wings) and `DefensivePlan` (mid block / high press /
low block / man marking / offside trap). The pair fills the ten
`TeamInstructions` dials the tactical bus already consumes — the attack
owns the five with-the-ball dials, the defence the five without.

Replaces the single-axis `TacticalPreset` (removed from
`team_instructions.rs`), which forced one row to decide both how a side
attacked and how it defended. `AttackingPlan::against` states which
defence each attack takes apart and which smothers it; it is a statement
of intent for the dials to produce on the pitch, not a multiplier applied
to results.


## Modified

- `src/core/src/match/game.rs` — `Match::play` routes to
  `QuickMatch::play` when `settings::quick_other_matches()` is on and the
  fixture is neither the managed club's (`Match::record`, stamped by
  `simulator/matchday.rs`) nor being recorded. Everything else is unchanged,
  including the seeded entry point into the real engine.
- `src/core/src/match/mod.rs` — registers and re-exports `quick`.
- `src/core/src/match/engine/flow/context.rs` — added `MatchIncident`,
  `MatchIncidentRecord` and `MatchContext::note_incident` /
  `incidents_between`: an append-only, timestamped log of the moments a match
  ticker names out loud. Goals, fouls and cards were already reconstructable
  from `statistics.items`; offsides were a bare counter, and corners, shots
  and saves left no dated trace at all, so a live screen could show a
  scoreline and nothing that explained it. `RefCell` because the sites that
  know these things (ball physics, the shot handler, the save credit path)
  hold `&MatchContext` — the same trade `MatchRng` makes one field below.
  Stamped at: `ball/ball/goal.rs` (corner awarded),
  `player/events/players.rs` (offside called, shot taken, shot caught,
  shot parried/punched).
- `src/core/src/match/engine/player/strategies/common/team/team.rs` —
  `compute_is_best_player_to_chase_ball` now elects exactly ONE chaser.
  It used to ask "is any teammate at least 36% better than me?"
  (`score < player_score * 0.64`) and call everyone who survived that the
  best chaser; around a loose ball the candidates sit at nearly equal
  distances, so nobody cleared 36% and the whole cluster passed at once —
  and two dozen call sites (`should_press`, running, guarding, tackling,
  marking, returning) then sent all of them at the ball. The comment above
  it said "prevents swarming"; the code did the opposite.

  Measured over full matches before → after: ball-seeking state changes
  versus shape-keeping ones went from 5.5:1 to 1.5:1, and the share of the
  match with eight or more players inside 10 m of the ball fell from 21% to
  16%. Tackle volume barely moved (48.6 → 47.0 per team per match against a
  real ~18), so this is one contributor and not the whole story.

  Ties break by player id: arbitrary, but stable within and across ticks
  while the geometry holds, which is what the original `0.8²` was presumably
  reaching for — applied as a threshold it widened the set instead of
  stabilising one holder.
- `src/core/src/match/result.rs` — added `ResultMatchPositionData::window`
  (with `PositionWindow` and the `thin` helper): one slice of a recording,
  bounded by two points on the match clock and thinned to a minimum gap
  between kept samples. The replay viewer downloads a finished match in
  five-minute chunks; a match being *watched* cannot work that way, because
  the frames the panel wants do not exist yet when it asks. `step_ms` is a
  floor, not a resample grid — the engine records every 30 ms, and ten frames
  per second of football is already past what the eye resolves. The last
  sample of every series is always kept: it is the position the viewer holds
  until the next window arrives.
- `src/core/src/match/engine/flow/context.rs`,
  `src/core/src/match/engine/engine/run.rs` — `MatchEngineConfig` gained
  `force_event_tracking`, and `setup` honours it alongside
  `MatchRuntime::events_mode()`. The global flag is an archiving decision
  covering every match in the world; a live match needs passes and player
  states for a different reason — they are what the 2D pitch draws lines and
  labels from — and one match worth of tracking is not the cost that flag
  exists to control.
- `src/core/src/match/engine/engine/live.rs` — a live match now always
  records, whatever `MatchRuntime::recordings_mode()` and `Match::record` say,
  and always tracks events. Stronger than `Match::play`'s rule and necessarily
  so: here the recording is not an archive, it is the picture on the screen.
  Added `recording()` (the accumulating `ResultMatchPositionData`) and
  `incidents(since, until)`, which reads goals, fouls and cards out of the
  players' own match statistics — every one is already stamped with
  `context.total_match_time` where it is awarded, so the alternative would be
  threading a recorder through the goal handler, the foul resolver and the
  card decision to learn what those call sites already wrote down. `LivePlayer`
  gained `side` (the half defended right now — the sides swap at half time, so
  a viewer that assumes home-on-the-left draws the second half back to front)
  and `position` (the role being played today).
- `src/web/src/settings.rs` — added the `--quick-other-matches` flag
  (parsed, applied via `core::settings::set_quick_other_matches`, logged at
  startup). Off by default: a headless run studying the engine's own output
  must not silently get a different model.
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
- `src/database/src/generators/generator/mod.rs` — the world opens on the day
  its data says, not on the day the machine's clock is in. Upstream started
  every world on 1 August of the current real year; for a world exported from
  an imported season that is simply wrong — a 2025/26 database opened in 2026
  started in August 2026 with the season it was built from already played out.
  The exporter now stamps 1 July of the season's first year into the database
  (`start_date`), and this reads it, falling back to upstream's behaviour when
  the field is absent.
- `src/database/src/loaders/compiled.rs`, `src/database/src/loaders/mod.rs`,
  `src/database/src/lib.rs` — the `start_date` field, its lenient parse (a
  malformed value is treated as absent, because the fallback is a working
  world on the wrong day rather than no world at all), and its re-export.
- `src/web/src/game/process.rs` — `POST /api/game/process` answers with JSON.
  It used to return a bare `200` with no body at all, while every other route
  here is JSON and the service descriptor says so — so the panel's client
  refused it and the manager was told "silnik zwrocil odpowiedz, ktora nie
  jest JSON-em" for a day that had in fact been simulated correctly. The busy
  branch now says `{"processed": false, "reason": "busy"}` rather than
  pretending the request did something.
- `src/web/src/lib.rs`, `src/main.rs` — `GameAppData` gained `live:
  LiveRegistry`, the one live session a process can hold (one, because the
  engine holds one world per process).
- `src/web/src/routes.rs` — merged the live route group and listed it in the
  service descriptor.
- `src/web/src/game/process.rs` — `ProcessingRun` and `execute` widened to
  `pub(crate)` so the live start handler can run a matchday without awaiting
  it. (File is itself fork-added.)
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

### `core/src/simulator/persistence.rs`

`SAVE_FORMAT_VERSION` 2 -> 3. The save body is bincode — positional, no
field names — so adding `Tactics.preset` / `Tactics.instructions` shifts
every byte after them. Without the bump an old save failed deep inside the
world decode with `UnexpectedVariant`; with it the loader says plainly that
the file is from an older format. `#[serde(default)]` does not help here
and the comment claiming it did has been corrected.


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

## Mecz: strzały z dystansu i pomiar linii obrony

`common/players/ops/forward_shot_decision.rs` — próg xG dostał trzeci
przedział dystansu. Tabela `distance_floor_base` schodzi powyżej 25 m do
0.008, ale `min_xg.clamp(lo, hi)` miał tylko dwa progi i dla wszystkiego
za 7,5 m podnosił wynik z powrotem do 0.025–0.040. Ten sam silnik wycenia
sytuację z 30 m na ~0.008, więc warunek był niespełnialny: przez 16 meczów
padło **zero** strzałów z 30 m+ przy realnym udziale ~5%, a 31,4% decyzji
w tym paśmie odrzucał sam `min_xg`. Po zmianie: 2,2% strzałów z 30 m+,
odrzuty na `min_xg` w tym paśmie 0,0%, strzały ogółem 9,9 → 11,0 na drużynę.

`defenders/states/state.rs` — `depth_override` doklejał się warunkiem
`if let (Some(depth), Some(velocity))`, więc omijał obrońcę, którego stan
zwrócił `velocity: None`. Stojący obrońca przed piłką nie dostawał nakazu
powrotu. Zmierzone jako neutralne, zostawione, bo brak prędkości jest
powodem do biegu, nie do jego pominięcia.

`match/result.rs` + `.dev/match/src/shape.rs` — `shape_report` liczy teraz,
ilu obrońców stoi przed piłką, gdy piłka jest w ich tercji i najbliżej niej
jest rywal (bez tego warunku pomiar liczył własne rozegranie od bramkarza
jako błąd: 63,1% zamiast 38,2%).

`teamplay/tactical.rs` — clamp linii obrony do piłki BYŁ napisany
i USUNIĘTY po pomiarze. Wynik A/B jest w komentarzu przy `defensive_line_x`.
