pub mod routes;

use crate::common::default_handler::{COMPUTER_NAME, CPU_BRAND, CPU_CORES, CSS_VERSION};
use crate::views::MenuSection;
use crate::worker::WorkerStatus;
use crate::{ApiError, ApiResult, GameAppData, I18n, LlmSettings};
use askama::Template;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct CountryListRequest {
    lang: String,
}

/// Player-facing landing page — added in this fork. With no active
/// career it shows only the "new career" hero and the saves list; with
/// an active career the home route redirects to the managed club.
#[derive(Template, askama_web::WebTemplate)]
#[template(path = "countries/list/home.html")]
pub struct HomeTemplate {
    pub css_version: &'static str,
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
}

/// World-browsing countries listing (`/{lang}/countries`).
#[derive(Template, askama_web::WebTemplate)]
#[template(path = "countries/list/index.html")]
pub struct CountryListTemplate {
    pub css_version: &'static str,
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
    pub continents: Vec<ContinentDto>,
}

/// Operator/diagnostics page — added in this fork. Reachable only via
/// direct URL (`/{lang}/admin`); it hosts the simulator chrome that was
/// removed from the player-facing home page (machine info, workers,
/// world counters, AI settings, holiday processing).
#[derive(Template, askama_web::WebTemplate)]
#[template(path = "countries/list/admin.html")]
pub struct AdminTemplate {
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
    pub version: &'static str,
    pub total_countries: usize,
    pub total_clubs: usize,
    pub total_players: usize,
    pub workers_count: usize,
    pub workers_ready: usize,
    pub ai_enabled: bool,
    pub ai_base_url: String,
    pub ai_model: String,
    pub ai_api_key: String,
}

pub struct ContinentDto {
    pub name: String,
    pub countries: Vec<CountryDto>,
}

/// Build the i18n key for a continent's display name. Lowercases, then
/// folds non-alphanumeric runs into a single `_` (so "North America"
/// becomes `continent_north_america`). The handler uses this with a
/// fall-through to the raw English name when no translation exists,
/// so an unrecognised continent never renders the bare key.
fn continent_i18n_key(name: &str) -> String {
    let mut out = String::from("continent_");
    let mut prev_underscore = true;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            for lc in c.to_lowercase() {
                out.push(lc);
            }
            prev_underscore = false;
        } else if !prev_underscore {
            out.push('_');
            prev_underscore = true;
        }
    }
    out.trim_end_matches('_').to_string()
}

pub struct CountryDto {
    pub slug: String,
    pub code: String,
    pub name: String,
}

fn build_continents(
    simulator_data: &core::SimulatorData,
    i18n: &I18n,
) -> Vec<ContinentDto> {
    simulator_data
        .continents
        .iter()
        .map(|continent| {
            let mut countries: Vec<CountryDto> = continent
                .countries
                .iter()
                .filter(|c| !c.leagues.leagues.is_empty())
                .map(|country| CountryDto {
                    slug: country.slug.clone(),
                    code: country.code.clone(),
                    name: country.name.clone(),
                })
                .collect();
            countries.sort_by(|a, b| a.slug.cmp(&b.slug));
            let key = continent_i18n_key(&continent.name);
            let translated = i18n.t(&key);
            let localized = if translated == key {
                continent.name.clone()
            } else {
                translated.to_string()
            };
            ContinentDto {
                name: localized,
                countries,
            }
        })
        .collect()
}

/// `/{lang}` — the game's front door. An active career sends the
/// manager straight to their club; otherwise the landing page offers
/// only "start a new career" and the saves list.
pub async fn country_home_action(
    State(state): State<GameAppData>,
    Path(route_params): Path<CountryListRequest>,
) -> ApiResult<Response> {
    let i18n = state.i18n.for_lang(&route_params.lang);

    // Active session → the club is the home page.
    {
        let guard = state.data.read().await;
        if let Some(simulator_data) = guard.as_ref() {
            if let Some(manager) = simulator_data.player_manager.as_ref() {
                if let Some(team) = simulator_data.team(manager.team_id) {
                    let url = format!("/{}/teams/{}", route_params.lang, team.slug);
                    return Ok(Redirect::temporary(&url).into_response());
                }
            }
        }
    }

    Ok(HomeTemplate {
        css_version: CSS_VERSION,
        title: i18n.t("site_name").to_string(),
        sub_title_prefix: String::new(),
        sub_title_suffix: String::new(),
        sub_title: String::new(),
        sub_title_link: String::new(),
        sub_title_country_code: String::new(),
        header_color: String::new(),
        foreground_color: String::new(),
        menu_sections: vec![],
        lang: route_params.lang,
        i18n,
    }
    .into_response())
}

/// `/{lang}/saves` — the landing page rendered directly (no redirect),
/// so a running career can still reach the saves list to switch slots.
pub async fn saves_page_action(
    State(state): State<GameAppData>,
    Path(route_params): Path<CountryListRequest>,
) -> ApiResult<impl IntoResponse> {
    let i18n = state.i18n.for_lang(&route_params.lang);
    Ok(HomeTemplate {
        css_version: CSS_VERSION,
        title: i18n.t("saves").to_string(),
        sub_title_prefix: String::new(),
        sub_title_suffix: String::new(),
        sub_title: String::new(),
        sub_title_link: String::new(),
        sub_title_country_code: String::new(),
        header_color: String::new(),
        foreground_color: String::new(),
        menu_sections: vec![],
        lang: route_params.lang,
        i18n,
    })
}

/// `/{lang}/countries` — world browsing (continents → countries).
pub async fn country_list_action(
    State(state): State<GameAppData>,
    Path(route_params): Path<CountryListRequest>,
) -> ApiResult<impl IntoResponse> {
    let i18n = state.i18n.for_lang(&route_params.lang);
    let guard = state.data.read().await;

    let simulator_data = guard
        .as_ref()
        .ok_or_else(|| ApiError::InternalError("Simulator data not loaded".to_string()))?;

    let continents = build_continents(simulator_data, &i18n);

    let current_path = format!("/{}/countries", route_params.lang);
    let menu_sections =
        crate::views::search_menu(&i18n, &route_params.lang, &current_path);

    Ok(CountryListTemplate {
        css_version: CSS_VERSION,
        title: i18n.t("world_section").to_string(),
        sub_title_prefix: String::new(),
        sub_title_suffix: String::new(),
        sub_title: i18n.t("select_country_sub").to_string(),
        sub_title_link: format!("/{}", route_params.lang),
        sub_title_country_code: String::new(),
        header_color: String::new(),
        foreground_color: String::new(),
        menu_sections,
        lang: route_params.lang,
        i18n,
        continents,
    })
}

/// `/{lang}/admin` — operator page, not linked from any menu.
pub async fn admin_action(
    State(state): State<GameAppData>,
    Path(route_params): Path<CountryListRequest>,
) -> ApiResult<impl IntoResponse> {
    let i18n = state.i18n.for_lang(&route_params.lang);
    let guard = state.data.read().await;

    let simulator_data = guard
        .as_ref()
        .ok_or_else(|| ApiError::InternalError("Simulator data not loaded".to_string()))?;

    let continents = build_continents(simulator_data, &i18n);
    let total_countries = continents.iter().map(|c| c.countries.len()).sum();
    let mut total_clubs = 0usize;
    let mut total_players = 0usize;
    for continent in &simulator_data.continents {
        for country in &continent.countries {
            total_clubs += country.clubs.len();
            for club in &country.clubs {
                for team in &club.teams.teams {
                    total_players += team.players.players.len();
                }
            }
        }
    }

    let workers_snapshot = state.workers.snapshot().await;
    let workers_count = workers_snapshot.len();
    let workers_ready = workers_snapshot
        .iter()
        .filter(|w| matches!(w.status, WorkerStatus::Ready))
        .count();

    let ai_saved = state.ai.get().await;
    let ai_enabled = ai_saved.is_some();
    let ai_settings = ai_saved.unwrap_or_else(LlmSettings::defaults);

    Ok(AdminTemplate {
        css_version: CSS_VERSION,
        computer_name: &COMPUTER_NAME,
        cpu_brand: &CPU_BRAND,
        cores_count: *CPU_CORES,
        title: i18n.t("admin_page_title").to_string(),
        sub_title_prefix: String::new(),
        sub_title_suffix: String::new(),
        sub_title: String::new(),
        sub_title_link: format!("/{}", route_params.lang),
        sub_title_country_code: String::new(),
        header_color: String::new(),
        foreground_color: String::new(),
        menu_sections: vec![],
        lang: route_params.lang,
        i18n,
        version: env!("CARGO_PKG_VERSION"),
        total_countries,
        total_clubs,
        total_players,
        workers_count,
        workers_ready,
        ai_enabled,
        ai_base_url: ai_settings.base_url,
        ai_model: ai_settings.model,
        ai_api_key: ai_settings.api_key,
    })
}
