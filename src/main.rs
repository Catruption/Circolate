//! Trackscript player — dispatcher.

#![allow(dead_code)]

mod archive;
mod asset;
mod audio_decode;
mod check;
mod convert;
mod deps;
mod display;
mod dump;
mod effects;
mod export;
mod fidelity;
mod formats;
mod hosting;
mod measure;
mod mixer;
mod paths;
mod pick;
mod play;
mod player;
mod spec;
mod test;
mod timing;
mod tui;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "circolate")]
#[command(version = "0.1.0")]
#[command(about = "Trackscript player and FL Studio/tracker converter")]
struct Cli {
    #[arg(value_name = "FILE", help = "Implicit play if a file is provided")]
    file: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,

    #[arg(long, global = true, help = "Hold the terminal open on exit (.desktop use)")]
    pause: bool,

    #[arg(long, global = true, help = "Never show a file picker when no FILE is given")]
    no_pick: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Play a song
    Play { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
    /// Convert a source file to trackscript
    Export { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
    /// Print a file's parsed structure as JSON
    Dump { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
    /// Run diagnostics
    Test { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
    /// Recursively probe .flp files for missing deps
    Check { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
    /// Tally plugin names across all .flp files
    Census { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
    /// Measure plugin default output levels
    Measure { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
    /// List discovered VST3 / LV2 / CLAP
    Plugins { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
    /// Download missing fallback plugins
    Fetch { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
    /// Load a VST3, play a note, report peak
    VstSmoke { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
    /// Play a note through the native SF2 engine
    Sf2Smoke { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
    /// List available trackscripts
    List,
}

fn main() -> Result<()> {
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    let cli = Cli::parse();

    let result = match cli.command {
        Some(Command::Play { args }) => play::run(&pick_if_empty(
            args,
            cli.no_pick,
            pick::FILTER_TRACKSCRIPT,
            "Open Trackscript",
        )),
        Some(Command::Export { args }) => export::run(&pick_if_empty(
            args,
            cli.no_pick,
            pick::FILTER_SOURCE,
            "Export source",
        )),
        Some(Command::Dump { args }) => dump::run(&pick_if_empty(
            args,
            cli.no_pick,
            pick::FILTER_TRACKSCRIPT,
            "Dump trackscript",
        )),
        Some(Command::Test { args }) => test::run(&pick_if_empty(
            args,
            cli.no_pick,
            pick::FILTER_TRACKSCRIPT,
            "Test trackscript",
        )),
        Some(Command::Check { args }) => check::run(&args),
        Some(Command::Census { args }) => check::run_census(&args),
        Some(Command::Measure { args }) => measure::run(&args),
        Some(Command::Plugins { args }) => deps::index::run(&args),
        Some(Command::Fetch { args }) => deps::fetch::run(&args),
        Some(Command::VstSmoke { args }) => hosting::vst::smoke::run(&args),
        Some(Command::Sf2Smoke { args }) => hosting::sf2::smoke::run(&args),
        Some(Command::List) => cmd_list(),
        None => {
            if let Some(file) = cli.file {
                play::run(&[file])
            } else {
                play::run(&[])
            }
        }
    };

    if cli.pause {
        eprintln!("\npress enter to close...");
        let mut s = String::new();
        let _ = std::io::stdin().read_line(&mut s);
    }

    result
}

/// If `args` is empty and the picker is enabled, prompt for a file and
/// append it. Cancelling returns the original empty vec; downstream
/// verbs treat that as "nothing to do" and exit cleanly.
fn pick_if_empty(mut args: Vec<String>, no_pick: bool, filter: &str, title: &str) -> Vec<String> {
    if no_pick || !args.is_empty() {
        return args;
    }
    if let Some(p) = pick::pick_file(&picker_start_dir(), filter, title) {
        args.push(p.to_string_lossy().into_owned());
    }
    args
}

/// Where the file picker opens: first trackscripts root that has any
/// entries, else $HOME, else cwd.
fn picker_start_dir() -> PathBuf {
    let user = paths::user_trackscripts();
    if dir_has_entries(&user) {
        return user;
    }
    let bundled = paths::bundled_trackscripts();
    if dir_has_entries(&bundled) {
        return bundled;
    }
    std::env::var_os("HOME")
    .map(PathBuf::from)
    .unwrap_or_else(|| PathBuf::from("."))
}

fn dir_has_entries(p: &std::path::Path) -> bool {
    std::fs::read_dir(p)
    .map(|mut r| r.next().is_some())
    .unwrap_or(false)
}

fn cmd_list() -> Result<()> {
    use std::collections::HashSet;

    let mut any = false;
    let mut seen: HashSet<String> = HashSet::new();

    // User first, so shadow dedupe lets user-tracked names hide bundled.
    for (tag, dir) in [
        ("user ", paths::user_trackscripts()),
        ("share", paths::bundled_trackscripts()),
    ] {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<_> = rd.filter_map(Result::ok).collect();
        entries.sort_by_key(|e| e.file_name());

        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !seen.insert(name.clone()) {
                continue;
            }
            any = true;
            if entry.path().is_dir() {
                println!("  {:<40}  {:<5}  [dir]", name, tag);
            } else {
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                println!("  {:<40}  {:<5}  {:>10} bytes", name, tag, size);
            }
        }
    }

    if !any {
        println!("no trackscripts found.");
        println!("  user:    {}", paths::user_trackscripts().display());
        println!("  bundled: {}", paths::bundled_trackscripts().display());
    }
    Ok(())
}
