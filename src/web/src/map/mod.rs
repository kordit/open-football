//! Added in this fork: interactive club-selection map of Poland.
//!
//! `GET /{lang}/map` renders the 16 voivodeships (regions with clubs are
//! highlighted and clickable); `GET /{lang}/map?region={voivodeship}`
//! drills into that voivodeship and lists its football districts (okręgi)
//! with their leagues and clubs, linking through to the league and club
//! pages where a career can be started.

pub mod geometry;
pub mod routes;

use crate::common::default_handler::{COMPUTER_NAME, CPU_BRAND, CPU_CORES, CSS_VERSION};
use crate::views::{self, MenuSection};
use crate::{ApiError, ApiResult, GameAppData, I18n};
use askama::Template;
use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use geometry::{MAP_VIEWBOX_HEIGHT, MAP_VIEWBOX_WIDTH, VOIVODESHIP_PATHS, VoivodeshipPath};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};

#[derive(Deserialize)]
pub struct MapRequest {
    pub lang: String,
}

#[derive(Deserialize)]
pub struct MapQuery {
    pub region: Option<String>,
    /// Pyramid level inside the selected region (level 3 of the drill-down).
    pub tier: Option<u8>,
}

#[derive(Template, askama_web::WebTemplate)]
#[template(path = "map/index.html")]
pub struct MapTemplate {
    pub css_version: &'static str,
    pub computer_name: &'static str,
    pub cpu_brand: &'static str,
    pub cores_count: usize,
    pub title: String,
    pub sub_title_prefix: String,
    pub sub_title_suffix: String,
    pub sub_title: String,
    pub sub_title_link: String,
    pub sub_title_country_code: String,
    pub header_color: String,
    pub foreground_color: String,
    pub menu_sections: Vec<MenuSection>,
    pub i18n: I18n,
    pub lang: String,
    pub viewbox_width: u32,
    pub viewbox_height: u32,
    /// Level 1: every voivodeship with its geometry and live club count.
    pub regions: Vec<MapRegionDto>,
    /// Level 2: the drilled-into voivodeship, when `?region=` matches one.
    pub selected: Option<SelectedRegionDto>,
}

pub struct MapRegionDto {
    pub code: &'static str,
    pub name: &'static str,
    pub label_x: i32,
    pub label_y: i32,
    pub path: &'static str,
    pub club_count: usize,
}

pub struct SelectedRegionDto {
    pub code: String,
    pub name: String,
    pub club_count: usize,
    /// False → level list (pyramid levels present in the region);
    /// true → group list of the selected level.
    pub show_groups: bool,
    /// Display label of the selected level ("Klasa okręgowa", …).
    pub tier_label: String,
    pub levels: Vec<LevelDto>,
    pub groups: Vec<GroupDto>,
}

/// One pyramid level present in the region (e.g. "IV liga",
/// "Klasa okręgowa"). The label is derived from the league names that
/// actually exist at that tier, never hardcoded.
pub struct LevelDto {
    pub tier: u8,
    pub label: String,
    pub league_count: usize,
    pub club_count: usize,
    /// Set when the level has exactly one group — the level entry then
    /// links straight to that league's page.
    pub single_slug: String,
}

/// One group of the selected level (e.g. "Klasa okręgowa Zamość").
pub struct GroupDto {
    pub name: String,
    pub slug: String,
    pub club_count: usize,
}

/// Resolve the voivodeship (first hierarchical segment) of a region code.
/// Voivodeship names themselves contain dashes ("kujawsko-pomorskie"), so
/// this matches against the known 16 rather than splitting on the first
/// dash. Returns the voivodeship code, or `None` for national-level codes
/// ("pl", "pl-g1") and anything unrecognised.
fn voivodeship_of(region_code: &str) -> Option<&'static str> {
    VOIVODESHIP_PATHS.iter().map(|v| v.code).find(|v| {
        region_code == *v
            || (region_code.starts_with(v) && region_code.as_bytes().get(v.len()) == Some(&b'-'))
    })
}

/// Longest common word-prefix of the league names at one level —
/// "Klasa okręgowa Zamość" + "Klasa okręgowa Lublin" → "Klasa okręgowa".
/// Falls back to the first name when the names share nothing.
fn common_level_label(names: &[&str]) -> String {
    let Some(first) = names.first() else {
        return String::new();
    };
    let first_words: Vec<&str> = first.split_whitespace().collect();
    let mut shared = first_words.len();
    for name in &names[1..] {
        let words: Vec<&str> = name.split_whitespace().collect();
        let mut k = 0;
        while k < shared && k < words.len() && words[k] == first_words[k] {
            k += 1;
        }
        shared = k;
    }
    if shared > 0 {
        return first_words[..shared].join(" ");
    }
    // Mixed tier (e.g. "5. Liga mazowiecka I" alongside "Liga okręgowa
    // Radom"): label with the most frequent two-word prefix instead.
    let mut counts: Vec<(String, usize)> = Vec::new();
    for name in names {
        let prefix = name
            .split_whitespace()
            .take(2)
            .collect::<Vec<_>>()
            .join(" ");
        match counts.iter_mut().find(|(p, _)| *p == prefix) {
            Some((_, n)) => *n += 1,
            None => counts.push((prefix, 1)),
        }
    }
    counts
        .into_iter()
        .max_by_key(|(_, n)| *n)
        .map(|(p, _)| p)
        .unwrap_or_else(|| first.to_string())
}

pub async fn map_action(
    State(state): State<GameAppData>,
    Path(route_params): Path<MapRequest>,
    Query(query): Query<MapQuery>,
) -> ApiResult<impl IntoResponse> {
    let i18n = state.i18n.for_lang(&route_params.lang);
    let guard = state.data.read().await;

    let simulator_data = guard
        .as_ref()
        .ok_or_else(|| ApiError::InternalError("Simulator data not loaded".to_string()))?;

    // The country the map belongs to: the first one whose clubs carry
    // voivodeship region codes (the Polish pyramid in practice).
    let country = simulator_data
        .continents
        .iter()
        .flat_map(|continent| &continent.countries)
        .find(|country| {
            country.clubs.iter().any(|club| {
                club.location
                    .region_code
                    .as_deref()
                    .and_then(voivodeship_of)
                    .is_some()
            })
        })
        .ok_or_else(|| ApiError::NotFound("No mapped country in this world".to_string()))?;

    // Level 1: live club count per voivodeship.
    let mut clubs_per_voivodeship: HashMap<&'static str, usize> = HashMap::new();
    for club in &country.clubs {
        if let Some(v) = club
            .location
            .region_code
            .as_deref()
            .and_then(voivodeship_of)
        {
            *clubs_per_voivodeship.entry(v).or_insert(0) += 1;
        }
    }

    let regions: Vec<MapRegionDto> = VOIVODESHIP_PATHS
        .iter()
        .map(|v: &VoivodeshipPath| MapRegionDto {
            code: v.code,
            name: v.name,
            label_x: v.label_x,
            label_y: v.label_y,
            path: v.path,
            club_count: clubs_per_voivodeship.get(v.code).copied().unwrap_or(0),
        })
        .collect();

    // Levels 2/3 — added in this fork's hierarchy rework: the region
    // drill-down lists the pyramid levels present in the voivodeship
    // (derived from the tiers of its leagues); picking a level lists
    // that level's groups (okręgi), each linking to its league page. A
    // level with a single group links straight through to the league.
    let selected: Option<SelectedRegionDto> = query
        .region
        .as_deref()
        .and_then(|region| {
            VOIVODESHIP_PATHS
                .iter()
                .find(|v| v.code == region)
                .map(|v| (v.code, v.name))
        })
        .map(|(region_code, region_name)| {
            let mut teams_per_league: HashMap<u32, usize> = HashMap::new();
            for club in &country.clubs {
                for team in &club.teams.teams {
                    if let Some(league_id) = team.league_id {
                        *teams_per_league.entry(league_id).or_insert(0) += 1;
                    }
                }
            }

            // Leagues that play inside this voivodeship, grouped by tier.
            let mut leagues_per_tier: BTreeMap<u8, Vec<&core::league::League>> = BTreeMap::new();
            for league in country
                .leagues
                .leagues
                .iter()
                .filter(|l| !l.friendly && !l.is_cup)
            {
                let Some(code) = league.settings.region_code.as_deref() else {
                    continue;
                };
                if voivodeship_of(code) != Some(region_code) {
                    continue;
                }
                leagues_per_tier
                    .entry(league.settings.tier)
                    .or_default()
                    .push(league);
            }
            for leagues in leagues_per_tier.values_mut() {
                leagues.sort_by(|a, b| a.name.cmp(&b.name));
            }

            let club_count = country
                .clubs
                .iter()
                .filter(|club| {
                    club.location
                        .region_code
                        .as_deref()
                        .and_then(voivodeship_of)
                        == Some(region_code)
                })
                .count();

            // Level 3: groups of the requested tier (when it exists here).
            if let Some(tier_leagues) =
                query.tier.and_then(|tier| leagues_per_tier.get(&tier))
            {
                let names: Vec<&str> = tier_leagues.iter().map(|l| l.name.as_str()).collect();
                let tier_label = common_level_label(&names);
                let groups: Vec<GroupDto> = tier_leagues
                    .iter()
                    .map(|league| GroupDto {
                        name: league.name.clone(),
                        slug: league.slug.clone(),
                        club_count: teams_per_league.get(&league.id).copied().unwrap_or(0),
                    })
                    .collect();
                return SelectedRegionDto {
                    code: region_code.to_string(),
                    name: region_name.to_string(),
                    club_count,
                    show_groups: true,
                    tier_label,
                    levels: Vec::new(),
                    groups,
                };
            }

            // Level 2: one entry per pyramid level, in pyramid order.
            let levels: Vec<LevelDto> = leagues_per_tier
                .iter()
                .map(|(tier, leagues)| {
                    let names: Vec<&str> = leagues.iter().map(|l| l.name.as_str()).collect();
                    LevelDto {
                        tier: *tier,
                        label: common_level_label(&names),
                        league_count: leagues.len(),
                        club_count: leagues
                            .iter()
                            .map(|l| teams_per_league.get(&l.id).copied().unwrap_or(0))
                            .sum(),
                        single_slug: if leagues.len() == 1 {
                            leagues[0].slug.clone()
                        } else {
                            String::new()
                        },
                    }
                })
                .collect();

            SelectedRegionDto {
                code: region_code.to_string(),
                name: region_name.to_string(),
                club_count,
                show_groups: false,
                tier_label: String::new(),
                levels,
                groups: Vec::new(),
            }
        });

    let current_path = format!("/{}/map", route_params.lang);

    Ok(MapTemplate {
        css_version: CSS_VERSION,
        computer_name: &COMPUTER_NAME,
        cpu_brand: &CPU_BRAND,
        cores_count: *CPU_CORES,
        title: i18n.t("map").to_string(),
        sub_title_prefix: String::new(),
        sub_title_suffix: String::new(),
        sub_title: country.name.clone(),
        sub_title_link: format!("/{}/countries/{}/leagues", &route_params.lang, &country.slug),
        sub_title_country_code: country.code.clone(),
        header_color: country.background_color.clone(),
        foreground_color: country.foreground_color.clone(),
        menu_sections: views::map_menu(&i18n, &route_params.lang, &current_path),
        viewbox_width: MAP_VIEWBOX_WIDTH,
        viewbox_height: MAP_VIEWBOX_HEIGHT,
        regions,
        selected,
        lang: route_params.lang,
        i18n,
    })
}
