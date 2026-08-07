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
    pub name: String,
    pub club_count: usize,
    pub districts: Vec<DistrictDto>,
}

pub struct DistrictDto {
    pub name: String,
    pub leagues: Vec<DistrictLeagueDto>,
    pub clubs: Vec<DistrictClubDto>,
}

pub struct DistrictLeagueDto {
    pub name: String,
    pub slug: String,
    pub tier: u8,
    pub club_count: usize,
}

pub struct DistrictClubDto {
    pub name: String,
    pub slug: String,
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

/// District part of a region code (everything after the voivodeship),
/// empty for voivodeship-level codes.
fn district_of<'c>(region_code: &'c str, voivodeship: &str) -> &'c str {
    region_code
        .strip_prefix(voivodeship)
        .map(|rest| rest.trim_start_matches('-'))
        .unwrap_or("")
}

/// Human label for a district code: dash-separated tokens are title-cased,
/// roman-numeral group suffixes are upper-cased ("nowy-sacz-ii" ->
/// "Nowy Sacz II").
fn district_label(district: &str) -> String {
    district
        .split('-')
        .map(|token| {
            if matches!(
                token,
                "i" | "ii" | "iii" | "iv" | "v" | "vi" | "vii" | "viii" | "ix" | "x"
            ) {
                token.to_uppercase()
            } else {
                let mut chars = token.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                    None => String::new(),
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
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

    // Level 2: districts of the selected voivodeship, derived from the
    // region codes actually present in the world. BTreeMap keeps the
    // voivodeship-level bucket (empty district key) first, then the
    // named districts alphabetically.
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
            // Display name of a district bucket: the voivodeship's own
            // name for its plain-code (tier-5) bucket, "Mazowieckie I"
            // for bare group numerals, the prettified district otherwise.
            let district_display = |district: &str| -> String {
                if district.is_empty() {
                    region_name.to_string()
                } else {
                    let label = district_label(district);
                    if label.chars().all(|c| matches!(c, 'I' | 'V' | 'X')) {
                        format!("{} {}", region_name, label)
                    } else {
                        label
                    }
                }
            };

            let mut teams_per_league: HashMap<u32, usize> = HashMap::new();
            for club in &country.clubs {
                for team in &club.teams.teams {
                    if let Some(league_id) = team.league_id {
                        *teams_per_league.entry(league_id).or_insert(0) += 1;
                    }
                }
            }

            let mut districts: BTreeMap<String, DistrictDto> = BTreeMap::new();

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
                let district = district_of(code, region_code);
                let entry =
                    districts
                        .entry(district.to_string())
                        .or_insert_with(|| DistrictDto {
                            name: district_display(district),
                            leagues: Vec::new(),
                            clubs: Vec::new(),
                        });
                entry.leagues.push(DistrictLeagueDto {
                    name: league.name.clone(),
                    slug: league.slug.clone(),
                    tier: league.settings.tier,
                    club_count: teams_per_league.get(&league.id).copied().unwrap_or(0),
                });
            }

            let mut club_count = 0usize;
            for club in &country.clubs {
                let Some(code) = club.location.region_code.as_deref() else {
                    continue;
                };
                if voivodeship_of(code) != Some(region_code) {
                    continue;
                }
                // A club links through its main team's page (that page
                // carries the "manage this club" button).
                let Some(team) = club
                    .teams
                    .teams
                    .iter()
                    .find(|t| t.team_type == core::TeamType::Main)
                    .or_else(|| club.teams.teams.first())
                else {
                    continue;
                };
                club_count += 1;
                let district = district_of(code, region_code);
                let entry =
                    districts
                        .entry(district.to_string())
                        .or_insert_with(|| DistrictDto {
                            name: district_display(district),
                            leagues: Vec::new(),
                            clubs: Vec::new(),
                        });
                entry.clubs.push(DistrictClubDto {
                    name: club.name.clone(),
                    slug: team.slug.clone(),
                });
            }

            let mut districts: Vec<DistrictDto> = districts.into_values().collect();
            for district in &mut districts {
                district.leagues.sort_by(|a, b| {
                    a.tier.cmp(&b.tier).then_with(|| a.name.cmp(&b.name))
                });
                district.clubs.sort_by(|a, b| a.name.cmp(&b.name));
            }

            SelectedRegionDto {
                name: region_name.to_string(),
                club_count,
                districts,
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
