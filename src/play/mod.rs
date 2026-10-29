//! `circolate play` — play a song, or render it to WAV offline.
//!
//! Live playback owns the terminal for the entire session: the TUI comes
//! up once, stays up across song changes, and only tears down on quit.

pub mod audio;
pub mod cli;
pub mod install;
pub mod offline;
pub mod resolve;
pub mod session;
pub mod worker;
