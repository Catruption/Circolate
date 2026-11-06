//! Halotext player — dispatcher.

#![allow(dead_code)]

// Logging must be declared before any module that uses `log_event!`,
// because `#[macro_use]` makes the macro visible only to subsequent
// module declarations.
#[macro_use]
mod log;

mod archive;
mod audio;
mod asset;
mod bundle;
mod cli;
mod conversion;
mod pipe;
mod deps;
mod effects;
mod formats;
mod hosting;
mod mixer;
mod paths;
mod player;
mod timing;
mod tui;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "circolate")]
#[command(version)]
#[command(about = "Halotext player and FL Studio/tracker converter")]
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
    Play    { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
    Export  { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
    Dump    { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
    Test    { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
    Check   { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
    Census  { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
    Measure { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
    Plugins { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
    Fetch   { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
    VstSmoke { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
    Sf2Smoke { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
    List,
}

fn main() -> Result<()> {
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    let cli = Cli::parse();

    let wants_ring = match &cli.command {
        Some(Command::Play { .. }) | None => true,
        Some(_) => false,
    };
    let ring = wants_ring.then(|| std::sync::Arc::new(log::RingSink::new(400)));
    let mut sinks: Vec<Box<dyn log::Sink>> = Vec::new();
    if ring.is_none() {
        sinks.push(Box::new(log::StderrSink));
    }
    if let Some(f) = log::FileSink::from_env() {
        sinks.push(Box::new(f));
    }
    log::init_with_ring(sinks, ring, None);

    if let Some(cmd) = cli.command {
        let result = match cmd {
            Command::Play    { args } => run_app(&args, cli.no_pick),
            Command::Export  { args } => cli::export::run(&pick_if_empty(args, cli.no_pick, cli::pick::FILTER_SOURCE, "Export source")),
            Command::Dump    { args } => cli::dump::run(&pick_if_empty(args, cli.no_pick, cli::pick::FILTER_HALOTEXT, "Dump halotext")),
            Command::Test    { args } => cli::test::run(&pick_if_empty(args, cli.no_pick, cli::pick::FILTER_HALOTEXT, "Test halotext")),
            Command::Check   { args } => cli::check::run(&args),
            Command::Census  { args } => cli::check::run_census(&args),
            Command::Measure { args } => cli::measure::run(&args),
            Command::Plugins { args } => deps::index::run(&args),
            Command::Fetch   { args } => deps::fetch::run(&args),
            Command::VstSmoke { args } => hosting::vst::smoke::run(&args),
            Command::Sf2Smoke { args } => hosting::sf2::smoke::run(&args),
            Command::List => cmd_list(),
        };

        if cli.pause {
            eprintln!("\npress enter to close...");
            let mut s = String::new();
            let _ = std::io::stdin().read_line(&mut s);
        }
        return result;
    }

    let args = cli.file.map(|f| vec![f]).unwrap_or_default();
    run_app(&args, cli.no_pick)
}

fn run_app(args: &[String], _no_pick: bool) -> Result<()> {
    let play_args = crate::cli::play::cli::parse_args(args)?;

    // --- ADD THIS BLOCK ---
    if play_args.render.is_some() {
        return crate::cli::play::offline::render(&play_args);
    }
    if play_args.verify {
        return crate::cli::play::offline::verify_cache(&play_args);
    }
    // ----------------------

    let mut current_song = play_args.song.clone();
    let queue = tui::state::QueueState::new();

    loop {
        // Setup TUI for this iteration
        let mut term = tui::setup()?;

        let dir = current_song.as_deref().and_then(|p| p.parent());
        let settings = tui::config::Config::load_layered(dir).resolve();

        let msg = match &current_song {
            Some(p) => format!("loading {}...", p.display()),
            None => "".into(),
        };
        tui::show_loading(&mut term, &queue, &settings, &msg, &[])?;

        match crate::cli::play::session::play_one(
            &mut term,
            &play_args,
            current_song.as_deref(),
                                      queue.clone(),
                                      settings.clone(),
        ) {
            Ok(tui::app::LoopOutcome::Quit) => return Ok(()),
            Ok(tui::app::LoopOutcome::Home) => {
                current_song = None;
            }
            Ok(tui::app::LoopOutcome::Next(name)) => {
                match paths::resolve_track(&name) {
                    Ok(p) => current_song = Some(p),
                    Err(_) => current_song = None,
                }
            }
            Err(e) => {
                let msg = format!("load failed: {e:#}");
                tui::show_loading(&mut term, &queue, &settings, &msg, &[])?;
                std::thread::sleep(Duration::from_secs(3));
                current_song = None;
            }
        }
    }
}

fn pick_if_empty(mut args: Vec<String>, no_pick: bool, filter: &str, title: &str) -> Vec<String> {
    if no_pick || !args.is_empty() {
        return args;
    }
    if let Some(p) = cli::pick::pick_file(&picker_start_dir(), filter, title) {
        args.push(p.to_string_lossy().into_owned());
    }
    args
}

fn picker_start_dir() -> PathBuf {
    let user = paths::user_halotexts();
    if dir_has_entries(&user) {
        return user;
    }
    let bundled = paths::bundled_halotexts();
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

    for (tag, dir) in [
        ("user ", paths::user_halotexts()),
        ("share", paths::bundled_halotexts()),
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
        println!("no halotexts found.");
        println!("  user:    {}", paths::user_halotexts().display());
        println!("  bundled: {}", paths::bundled_halotexts().display());
    }
    Ok(())
}
