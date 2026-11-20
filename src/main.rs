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

    #[arg(long, global = true, value_name = "SPEC", help = "Log level or per-category spec (info, player:debug,deps:info)")]
    log: Option<String>,

    #[arg(long, global = true, value_name = "FILE", help = "Also append logs to FILE")]
    log_file: Option<PathBuf>,

    #[arg(long, global = true, help = "Only log errors")]
    quiet: bool,

    #[arg(long, global = true, help = "Force stderr logs even in the TUI")]
    log_stderr: bool,
}

#[derive(Subcommand)]
enum Command {
    #[command(disable_help_flag = true)]
    Play    { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
    #[command(disable_help_flag = true)]
    Export  { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
    #[command(disable_help_flag = true)]
    Dump    { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
    #[command(disable_help_flag = true)]
    Test    { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
    #[command(disable_help_flag = true)]
    Check   { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
    #[command(disable_help_flag = true)]
    Inspect { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
    #[command(disable_help_flag = true)]
    Census  { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
    #[command(disable_help_flag = true)]
    Measure { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
    #[command(disable_help_flag = true)]
    Plugins { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
    #[command(disable_help_flag = true)]
    Fetch   { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
    #[command(disable_help_flag = true)]
    VstSmoke { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
    #[command(disable_help_flag = true)]
    Sf2Smoke { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
    #[command(disable_help_flag = true)]
    Install  { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
    #[command(disable_help_flag = true)]
    Doctor   { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
    #[command(disable_help_flag = true)]
    Directories { #[arg(trailing_var_arg = true, allow_hyphen_values = true)] args: Vec<String> },
    /// Show the directory catalogue (alias for `doctor --dirs`).
    Paths,
    List,
}

fn main() -> Result<()> {
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    let mut cli = Cli::parse();
    let mut log_spec = cli.log.take();
    let mut log_file = cli.log_file.take();
    let mut quiet = cli.quiet;
    let mut log_stderr = cli.log_stderr;

    // Global log flags are accepted before or after the subcommand.  Clap
    // hands post-subcommand tokens to the hand-rolled parsers, so lift them
    // out here before dispatch.
    if let Some(args) = cli.command_args_mut() {
        strip_log_flags(args, &mut log_spec, &mut log_file, &mut quiet, &mut log_stderr);
    }

    let mut filter = log::LogFilter::from_env();
    if let Some(spec) = log_spec.as_deref() {
        if let Err(e) = filter.apply_spec(spec) {
            eprintln!("circolate: --log {e}");
            std::process::exit(2);
        }
    }
    if quiet {
        for cat in log::CATEGORIES { filter.set(*cat, log::Level::Error); }
    }

    // TUI sessions keep terminal output clean; headless commands use
    // stderr.  `play --render` and `play --verify-cache` are headless even
    // though they share the play verb, so detect them before choosing sinks.
    let mut wants_ring = matches!(&cli.command, Some(Command::Play { .. }) | None);
    if let Some(Command::Play { args }) = &cli.command {
        if args.iter().any(|a| a == "--render" || a == "--verify-cache") {
            wants_ring = false;
        }
    }
    let ring = wants_ring.then(|| std::sync::Arc::new(log::RingSink::new(400)));
    let mut sinks: Vec<Box<dyn log::Sink>> = Vec::new();
    if !wants_ring || log_stderr {
        sinks.push(Box::new(log::StderrSink));
    }
    let file = log_file
    .as_deref()
    .and_then(|p| log::FileSink::new(p).ok())
    .or_else(log::FileSink::from_env);
    if let Some(f) = file {
        sinks.push(Box::new(f));
    }
    log::init_with_ring(sinks, ring, Some(filter));

    if let Some(cmd) = cli.command {
        let result = match cmd {
            Command::Play    { args } => run_app(&args, cli.no_pick),
            Command::Export  { args } => cli::export::run(&pick_if_empty(args, cli.no_pick, cli::pick::FILTER_SOURCE, "Export source")),
            Command::Dump    { args } => cli::dump::run(&pick_if_empty(args, cli.no_pick, cli::pick::FILTER_HALOTEXT, "Dump halotext")),
            Command::Test    { args } => run_test(args, cli.no_pick),
            Command::Check   { args } => cli::check::run(&args),
            Command::Inspect { args } => cli::inspect::run(&args),
            Command::Census  { args } => cli::check::run_census(&args),
            Command::Measure { args } => cli::measure::run(&args),
            Command::Plugins { args } => deps::plugins::run(&args),
            Command::Fetch   { args } => deps::fetch::run(&args),
            Command::VstSmoke { args } => hosting::vst::smoke::run(&args),
            Command::Sf2Smoke { args } => hosting::sf2::smoke::run(&args),
            Command::Install { args } => deps::setup::run_install(&args),
            Command::Doctor { args } => deps::doctor::run(&args),
            Command::Directories { args } => deps::setup::run_directories(&args),
            Command::Paths => deps::doctor::run(&["--dirs".to_string()]),
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
    let result = run_app(&args, cli.no_pick);
    if cli.pause {
        eprintln!("\npress enter to close...");
        let mut s = String::new();
        let _ = std::io::stdin().read_line(&mut s);
    }
    result
}

fn run_app(args: &[String], _no_pick: bool) -> Result<()> {
    let mut play_args = crate::cli::play::cli::parse_args(args)?;
    if play_args.help {
        return Ok(());
    }

    if play_args.render.is_some() {
        return crate::cli::play::offline::render(&play_args);
    }
    if play_args.verify {
        return crate::cli::play::offline::verify_cache(&play_args);
    }

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
            Ok(tui::app::LoopOutcome::Next { path, flags }) => {
                if !flags.is_empty() {
                    play_args = crate::cli::play::cli::parse_args(&flags)?;
                }
                match paths::resolve_track(&path) {
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

impl Command {
    fn args_mut(&mut self) -> Option<&mut Vec<String>> {
        match self {
            Command::Play { args }
            | Command::Export { args }
            | Command::Dump { args }
            | Command::Test { args }
            | Command::Check { args }
            | Command::Inspect { args }
            | Command::Census { args }
            | Command::Measure { args }
            | Command::Plugins { args }
            | Command::Fetch { args }
            | Command::VstSmoke { args }
            | Command::Sf2Smoke { args }
            | Command::Install { args }
            | Command::Doctor { args }
            | Command::Directories { args } => Some(args),
            Command::Paths | Command::List => None,
        }
    }
}

impl Cli {
    fn command_args_mut(&mut self) -> Option<&mut Vec<String>> {
        self.command.as_mut().and_then(Command::args_mut)
    }
}

fn strip_log_flags(
    args: &mut Vec<String>,
    log_spec: &mut Option<String>,
    log_file: &mut Option<PathBuf>,
    quiet: &mut bool,
    log_stderr: &mut bool,
) {
    let mut kept = Vec::with_capacity(args.len());
    let mut it = std::mem::take(args).into_iter();
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
            _ => (arg.clone(), None),
        };
        let value = |inline: Option<String>, it: &mut std::vec::IntoIter<String>| {
            inline.or_else(|| it.next())
        };
        match flag.as_str() {
            "--log" => *log_spec = value(inline, &mut it),
            "--log-file" => *log_file = value(inline, &mut it).map(PathBuf::from),
            "--quiet" => *quiet = true,
            "--log-stderr" => *log_stderr = true,
            _ => kept.push(arg),
        }
    }
    *args = kept;
}

/// `test --deps` is the discoverable alias for `doctor`: it keeps the
/// familiar verb while answering "does this machine have everything?".
fn run_test(mut args: Vec<String>, no_pick: bool) -> Result<()> {
    if let Some(i) = args.iter().position(|a| a == "--deps") {
        args.remove(i);
        let flags: Vec<String> = args
        .iter()
        .filter(|a| a.starts_with('-'))
        .cloned()
        .collect();
        return deps::doctor::run(&flags);
    }
    cli::test::run(&pick_if_empty(
        args,
        no_pick,
        cli::pick::FILTER_HALOTEXT,
        "Test halotext",
    ))
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
