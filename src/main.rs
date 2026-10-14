//! Trackscript player — dispatcher.
//!
//! One job: route argv to a subcommand.  Everything else lives in the
//! subcommand modules.
#![allow(dead_code)]

mod requires;
mod fidelity;
mod display;
mod check;
mod convert;
mod dump;
mod effects;
mod export;
mod fallbacks;
mod fetch;
mod instruments;
mod play;
mod player;
mod plugins;
mod sf2;
mod sf2_engine;
mod sf2_smoke;
mod sfl2;
mod sources;
mod test;
mod timing;
mod tui;
mod vst_host_ffi;
mod vst_smoke;
mod spec;
mod archive;
mod asset;
mod audio_decode;

use anyhow::{bail, Result};

fn main() -> Result<()> {
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

    let mut args: Vec<String> = std::env::args().collect();

    let pause = args.iter().any(|a| a == "--pause");
    args.retain(|a| a != "--pause");

    let rest: &[String] = if args.len() > 2 { &args[2..] } else { &[] };

    let result = match args.get(1).map(String::as_str) {
        Some("play")      => play::run(rest),
        Some("export")    => export::run(rest),
        Some("dump")      => dump::run(rest),
        Some("test")      => test::run(rest),
        Some("check")     => check::run(rest),
        Some("plugins")   => plugins::run(rest),
        Some("fetch")     => fetch::run(rest),
        Some("vst-smoke") => vst_smoke::run(rest),
        Some("sf2-smoke") => sf2_smoke::run(rest),
        Some("-h") | Some("--help") | None => {
            print_help();
            Ok(())
        }
        Some(other) if std::path::Path::new(other).exists() => play::run(&args[1..]),
        Some(other) => bail!("unknown command: {other}"),
    };

    if pause {
        eprintln!("\npress enter to close...");
        let mut s = String::new();
        let _ = std::io::stdin().read_line(&mut s);
    }

    result
}

fn print_help() {
    println!("usage: trackscript-player <command> <file> [options]");
    println!();
    println!("  play      <file>            play a song (trackscript, flp, or it)");
    println!("  export    <file> [-o DIR]   convert a source file to trackscript");
    println!("  dump      <file>            print a file's parsed structure as JSON");
    println!("  test      <file>            run diagnostics");
    println!("  check     [path]            recursively probe .flp files for missing deps");
    println!("  plugins   [--paths]         list discovered VST3 / LV2 / CLAP");
    println!("  fetch     [--dry] [--only NAME]");
    println!("                              download missing fallback plugins");
    println!("  vst-smoke <plugin.vst3>     load a VST3, play a note, report peak");
    println!("  sf2-smoke <file.sf2>        play a note through the native SF2 engine");
    println!();
    println!("  --pause                     hold the terminal open on exit (.desktop use)");
}
