//! Audio-side control surfaces: application volume (via pactl) and
//! global mute/solo state.  Both are read or written by the audio
//! callback and the TUI; neither depends on ratatui.  They live here
//! rather than under `tui/` because the audio thread must be able to
//! reach them without pulling in a terminal-rendering crate.

pub mod mute;
pub mod volume;
