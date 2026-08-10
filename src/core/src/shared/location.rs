#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct Location {
    pub city_id: u32,
    /// Added in this fork: hierarchical region code (e.g. "lu-zamosc") for
    /// region-aware relegation routing.
    pub region_code: Option<String>,
}

impl Location {
    pub fn new(city_id: u32) -> Self {
        Location {
            city_id,
            region_code: None,
        }
    }
}
