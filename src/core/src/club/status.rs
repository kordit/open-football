#[derive(Debug, Clone)]
#[derive(serde::Deserialize, serde::Serialize)]
pub enum ClubStatus {
    Amateur,
    SemiProfessional,
    Professional,
}
