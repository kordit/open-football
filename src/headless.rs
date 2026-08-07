//! Headless run modes: `simulate` and `validate-db`.
//!
//! New file in this fork (not present upstream). `simulate` is the
//! standalone world-tick driver lifted from `.dev/simulate`; `validate-db`
//! loads a world database file, runs the structural checks the exporter
//! pipeline gates on, and exits non-zero on errors.

use database::{CompiledDatabase, DatabaseGenerator, DatabaseLoader};
use simulator_core::{FootballSimulator, SimulatorData};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};
use std::time::Instant;

/// Minimal executor for a future guaranteed ready on its first poll —
/// `FootballSimulator::simulate` is declared `async` but never awaits an
/// I/O point (it drives rayon internally).
fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
            return output;
        }
        std::hint::spin_loop();
    }
}

fn arg_value(name: &str) -> Option<String> {
    let prefix = format!("--{name}=");
    std::env::args()
        .find(|arg| arg.starts_with(&prefix))
        .map(|arg| arg[prefix.len()..].to_string())
}

/// `simulate [--days=N] [--seed=N]` — generate the world from the
/// configured database and tick it N simulated days, printing per-day
/// timings and a summary. `--seed` pins the sim RNG and stamps stable
/// per-fixture engine seeds.
pub fn run_simulate() -> i32 {
    let days: u32 = arg_value("days").and_then(|v| v.parse().ok()).unwrap_or(60);
    if let Some(seed) = arg_value("seed").and_then(|v| v.parse::<u64>().ok()) {
        simulator_core::utils::random::engine::RandomEngine::set_seed(seed);
        eprintln!("sim seed pinned: {seed}");
    }

    eprintln!("loading world database…");
    let gen_start = Instant::now();
    let database = DatabaseLoader::load();
    let mut data: SimulatorData = DatabaseGenerator::generate(&database);
    eprintln!(
        "world generated in {:.2} s — simulating {days} days",
        gen_start.elapsed().as_secs_f64(),
    );

    let start_date = data.date.date();
    let overall = Instant::now();
    let mut total_matches: u64 = 0;
    let mut slowest_day = 0u32;
    let mut slowest_ms = 0.0f64;

    for day in 1..=days {
        let tick_start = Instant::now();
        let result = block_on(FootballSimulator::simulate(&mut data));
        let ms = tick_start.elapsed().as_secs_f64() * 1000.0;

        let matches = result.match_results.len();
        let goals: u32 = result
            .match_results
            .iter()
            .map(|m| (m.score.home_team.get() + m.score.away_team.get()) as u32)
            .sum();
        total_matches += matches as u64;
        if ms > slowest_ms {
            slowest_ms = ms;
            slowest_day = day;
        }

        println!(
            "day {day:>4}  {date}  {ms:>9.2} ms  matches={matches} goals={goals}",
            date = data.date.date(),
        );
    }

    let total_ms = overall.elapsed().as_secs_f64() * 1000.0;
    println!(
        "\n{days} days  {start} → {end}\n\
         total {total_ms:.1} ms  mean {mean:.2} ms/day  \
         slowest day {slowest_day} ({slowest_ms:.2} ms)  \
         matches {total_matches}",
        start = start_date,
        end = data.date.date(),
        mean = total_ms / days as f64,
    );

    0
}

/// `validate-db [--database=path] [--full]` — structural validation of a
/// world database file. With `--full` it additionally generates the whole
/// world as a smoke test. Returns a process exit code.
pub fn run_validate_db() -> i32 {
    let path = database::database_path();
    println!("validating {}", path.display());

    let db = match database::load_from_path(&path) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("ERROR: {e}");
            return 1;
        }
    };

    let mut errors: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    validate_structure(&db, &mut errors, &mut warnings);

    for w in &warnings {
        println!("WARN:  {w}");
    }
    for e in &errors {
        println!("ERROR: {e}");
    }

    if errors.is_empty() && std::env::args().any(|a| a == "--full") {
        println!("running full world generation smoke test…");
        let start = Instant::now();
        let database = DatabaseLoader::load();
        let data = DatabaseGenerator::generate(&database);
        println!(
            "world generated in {:.2} s (date {})",
            start.elapsed().as_secs_f64(),
            data.date.date()
        );
        report_world(&data);
    }

    if errors.is_empty() {
        println!("OK ({} warnings)", warnings.len());
        0
    } else {
        println!("FAILED: {} errors, {} warnings", errors.len(), warnings.len());
        1
    }
}

fn validate_structure(db: &CompiledDatabase, errors: &mut Vec<String>, warnings: &mut Vec<String>) {
    println!(
        "sections: continents={} countries={} leagues={} clubs={} names={} cups={} nat-comps={} players={}",
        db.continents.len(),
        db.countries.len(),
        db.leagues.len(),
        db.clubs.len(),
        db.names.len(),
        db.domestic_cups.len(),
        db.national_competitions.len(),
        db.players.len()
    );

    let country_ids: HashSet<u32> = db.countries.iter().map(|c| c.id).collect();
    let country_codes: HashSet<&str> = db.countries.iter().map(|c| c.code.as_str()).collect();

    // Duplicate ids per section.
    check_duplicates("country", db.countries.iter().map(|c| c.id), errors);
    check_duplicates("league", db.leagues.iter().map(|l| l.id), errors);
    check_duplicates("club", db.clubs.iter().map(|c| c.id), errors);
    check_duplicates(
        "team",
        db.clubs.iter().flat_map(|c| c.teams.iter().map(|t| t.id)),
        errors,
    );
    check_duplicates("player", db.players.iter().map(|p| p.id), errors);

    // Leagues.
    let enabled: HashMap<u32, &str> = db
        .leagues
        .iter()
        .filter(|l| l.enabled)
        .map(|l| (l.id, l.slug.as_str()))
        .collect();
    let disabled_count = db.leagues.len() - enabled.len();
    if disabled_count > 0 {
        warnings.push(format!(
            "{disabled_count} disabled league(s) present — their clubs will be dropped"
        ));
    }
    if enabled.is_empty() {
        errors.push("no enabled leagues".into());
    }
    for league in &db.leagues {
        if !country_codes.contains(league.country_code.as_str()) {
            errors.push(format!(
                "league '{}' has unknown country_code '{}'",
                league.slug, league.country_code
            ));
        }
        for fp in &league.foreign_players {
            if !country_ids.contains(&fp.country_id) {
                errors.push(format!(
                    "league '{}' foreign_players references unknown country id {}",
                    league.slug, fp.country_id
                ));
            }
        }
    }

    // Per-tier league/club counts (enabled only).
    let mut tier_leagues: HashMap<u8, u32> = HashMap::new();
    for league in db.leagues.iter().filter(|l| l.enabled) {
        *tier_leagues.entry(league.tier).or_insert(0) += 1;
    }

    // Clubs: the #1 exporter mistake is a Main team not pointing at an
    // enabled league — the loader silently drops the club. Surface it loudly.
    let mut clubs_per_league: HashMap<u32, u32> = HashMap::new();
    let mut dropped = 0u32;
    for club in &db.clubs {
        if !country_codes.contains(club.country_code.as_str()) {
            errors.push(format!(
                "club '{}' has unknown country_code '{}'",
                club.name, club.country_code
            ));
        }
        let main = club.teams.iter().find(|t| t.team_type == "Main");
        match main.and_then(|t| t.league_id) {
            Some(lid) if enabled.contains_key(&lid) => {
                *clubs_per_league.entry(lid).or_insert(0) += 1;
            }
            Some(lid) => {
                dropped += 1;
                errors.push(format!(
                    "club '{}' Main team points at non-enabled league {lid} — club will be DROPPED",
                    club.name
                ));
            }
            None => {
                dropped += 1;
                errors.push(format!(
                    "club '{}' has no Main team with a league_id — club will be DROPPED",
                    club.name
                ));
            }
        }
    }
    if dropped > 0 {
        errors.push(format!("{dropped} club(s) would be silently dropped by the loader"));
    }

    for (lid, slug) in &enabled {
        let count = clubs_per_league.get(lid).copied().unwrap_or(0);
        if count == 0 {
            errors.push(format!("enabled league '{slug}' has no clubs"));
        } else if count < 2 {
            errors.push(format!("enabled league '{slug}' has only {count} club(s)"));
        }
    }

    let mut tiers: Vec<_> = tier_leagues.iter().collect();
    tiers.sort();
    for (tier, league_count) in tiers {
        let clubs: u32 = db
            .leagues
            .iter()
            .filter(|l| l.enabled && l.tier == *tier)
            .filter_map(|l| clubs_per_league.get(&l.id))
            .sum();
        println!("tier {tier}: {league_count} league(s), {clubs} club(s)");
    }

    // Players.
    let team_club: HashMap<u32, u32> = db
        .clubs
        .iter()
        .flat_map(|c| c.teams.iter().map(move |t| (t.id, c.id)))
        .collect();
    let club_ids: HashSet<u32> = db.clubs.iter().map(|c| c.id).collect();
    let mut players_per_club: HashMap<u32, u32> = HashMap::new();
    let mut free_agents = 0u32;
    for player in &db.players {
        if player.club_id == 0 {
            free_agents += 1;
        } else if club_ids.contains(&player.club_id) || team_club.contains_key(&player.club_id) {
            *players_per_club.entry(player.club_id).or_insert(0) += 1;
        } else {
            errors.push(format!(
                "player {} '{} {}' references unknown club {}",
                player.id, player.first_name, player.last_name, player.club_id
            ));
        }
        if !country_ids.contains(&player.country_id) {
            errors.push(format!(
                "player {} '{} {}' has unknown country id {}",
                player.id, player.first_name, player.last_name, player.country_id
            ));
        }
        if player.current_ability == 0 || player.current_ability > 200 {
            errors.push(format!(
                "player {} current_ability {} out of 1-200",
                player.id, player.current_ability
            ));
        }
        if player.positions.is_empty() {
            warnings.push(format!(
                "player {} '{} {}' has no positions (defaults to MC)",
                player.id, player.first_name, player.last_name
            ));
        }
    }
    if free_agents > 0 {
        println!("free agents: {free_agents}");
    }

    // Squad sizes for clubs that carry ODB players: the generator does NOT
    // top these squads up, so an undersized squad cannot field a matchday XI.
    for (club_id, count) in &players_per_club {
        if *count < 11 {
            let name = db
                .clubs
                .iter()
                .find(|c| c.id == *club_id || c.teams.iter().any(|t| t.id == *club_id))
                .map(|c| c.name.as_str())
                .unwrap_or("?");
            errors.push(format!(
                "club '{name}' ({club_id}) has only {count} player(s) — ODB squads get no generated fill"
            ));
        }
    }

    // Name pools for countries that have leagues (newgen generation).
    let league_countries: HashSet<&str> = db
        .leagues
        .iter()
        .filter(|l| l.enabled)
        .map(|l| l.country_code.as_str())
        .collect();
    let name_codes: HashSet<&str> = db.names.iter().map(|n| n.country_code.as_str()).collect();
    for code in &league_countries {
        if !name_codes.contains(code) {
            errors.push(format!("country '{code}' has leagues but no name pool"));
        }
    }
}

fn check_duplicates<I: Iterator<Item = u32>>(kind: &str, ids: I, errors: &mut Vec<String>) {
    let mut seen = HashSet::new();
    let mut dupes = HashSet::new();
    for id in ids {
        if !seen.insert(id) {
            dupes.insert(id);
        }
    }
    if !dupes.is_empty() {
        let mut sample: Vec<u32> = dupes.iter().copied().take(5).collect();
        sample.sort();
        errors.push(format!(
            "{} duplicate {kind} id(s), e.g. {:?}",
            dupes.len(),
            sample
        ));
    }
}

fn report_world(data: &SimulatorData) {
    let mut countries = 0usize;
    let mut leagues = 0usize;
    let mut clubs = 0usize;
    let mut players = 0usize;
    for continent in &data.continents {
        countries += continent.countries.len();
        for country in &continent.countries {
            leagues += country.leagues.leagues.len();
            clubs += country.clubs.len();
            players += country
                .clubs
                .iter()
                .flat_map(|c| c.teams.teams.iter())
                .map(|t| t.players.players.len())
                .sum::<usize>();
        }
    }
    println!(
        "world: {countries} countries, {leagues} leagues, {clubs} clubs, {players} players, {} free agents",
        data.free_agents.len()
    );
}
