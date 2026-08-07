#[derive(Debug, Clone)]
#[derive(serde::Deserialize, serde::Serialize)]
pub struct BoardMood {
    pub state: BoardMoodState,
}

impl BoardMood {
    pub fn default() -> Self {
        BoardMood {
            state: BoardMoodState::Normal,
        }
    }
}

#[derive(Debug, Clone)]
#[derive(serde::Deserialize, serde::Serialize)]
pub enum BoardMoodState {
    Poor,
    Normal,
    Good,
    Excellent,
}
