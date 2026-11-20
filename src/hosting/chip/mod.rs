//! Data-driven chip backends.

pub use crate::formats::chip::{chip, patch};
// pub mod bus;  // VGM path, being replaced. Re-enable when bus protocol is wired up.
pub mod engine;
pub mod adapter;
pub mod player;
pub mod dac;
pub mod ymfm;

pub use chip::Chip;
pub use patch::Patch;
pub use player::{ChipPlayer, ChipNote, ChipAutomation};
