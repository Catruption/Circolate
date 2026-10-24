//! `circolate play` — play a song, or render it to WAV offline.
//!
//! Live playback owns the terminal for the entire session: the TUI comes
//! up once, stays up across song changes, and only tears down on quit.
//!
//! Submodules:
//!   cli      — flag parsing
//!   offline  — non-TUI paths (`--render`, `--verify-cache`)
//!   session  — the TUI + cpal live-playback loop
//!   resolve  — turning a source path into a playable trackscript
//!   install  — attaching VST3 / SF2 / FX / shims to the audio engine
//!   worker   — loader thread + live-reload watcher
//!   audio    — soft clipper and stdout silencing

mod audio;
mod cli;
mod install;
mod offline;
mod resolve;
mod session;
mod worker;

use anyhow::Result;

pub fn run(argv: &[String]) -> Result<()> {
    let args = cli::parse_args(argv)?;

    if args.verify {
        return offline::verify_cache(&args);
    }
    if args.render.is_some() {
        return offline::render(&args);
    }

    session::run(args)
}
