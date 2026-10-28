//! Ratatui front-end for live playback.  The audio-side control
//! surfaces (`volume`, `mute`, `scope`) live in `crate::mixer` and
//! `crate::player`, not here — this module only knows about terminals
//! and the state needed to draw one.

pub mod app;
pub mod config;
pub mod shared;
pub mod spiral;
pub mod state;
pub mod theme;
pub mod views;
pub mod widgets;
pub mod logs;
pub mod layout;

pub use app::{restore, run_against, setup, show_loading};
