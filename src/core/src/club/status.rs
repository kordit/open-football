#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub enum ClubStatus {
    Amateur,
    SemiProfessional,
    Professional,
}
