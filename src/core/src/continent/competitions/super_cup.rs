#[derive(Debug, Clone)]
#[derive(serde::Deserialize, serde::Serialize)]
pub struct SuperCup {
    pub prize_pool: f64,
}

impl SuperCup {
    pub fn new() -> Self {
        SuperCup {
            prize_pool: 10_000_000.0,
        }
    }
}
