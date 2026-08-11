mod adaptation;
pub mod decision;
pub mod instructions;
pub mod set_pieces;
pub mod tactics;
// Added in this fork: the manager's ten dials and the presets that move
// them together. See `team_instructions.rs` for why they land where they do.
pub mod plan;
pub mod team_instructions;

pub use decision::*;
pub use instructions::*;
pub use set_pieces::*;
pub use tactics::*;
pub use plan::*;
pub use team_instructions::*;
