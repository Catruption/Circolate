//! Data-driven chip backends.

pub use crate::formats::chip::{bank, chip, patch};
// pub mod bus;  // VGM path, being replaced. Re-enable when bus protocol is wired up.
pub mod engine;
pub mod adapter;
pub mod player;
pub mod dac;
pub mod ymfm;

pub use chip::Chip;
pub use patch::Patch;
pub use bank::Bank;
pub use player::{ChipPlayer, ChipNote, ChipAutomation};

/// Parse a `.chip` file.
pub fn load_chip(path: &std::path::Path) -> anyhow::Result<Chip> {
    let text = std::fs::read_to_string(path)?;
    chip::parse(&text).map_err(|e| anyhow::anyhow!("{}: {}", path.display(), e))
}

pub fn load_patch(path: &std::path::Path) -> anyhow::Result<Patch> {
    let text = std::fs::read_to_string(path)?;
    patch::parse(&text).map_err(|e| anyhow::anyhow!("{}: {}", path.display(), e))
}

pub fn load_bank(path: &std::path::Path) -> anyhow::Result<Bank> {
    let text = std::fs::read_to_string(path)?;
    bank::parse(&text).map_err(|e| anyhow::anyhow!("{}: {}", path.display(), e))
}
