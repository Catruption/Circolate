//! Terminal progress display for live playback.
//!
//! One redrawn line: a bar, position/duration, active voice count, and how
//! many BlockList measures the loader has already prepared.  Not a TUI — no
//! alternate screen, no raw mode, no keyboard.  Just something to look at.

use std::io::Write;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::player::{Project, Transport};

const BAR_WIDTH: usize = 44;

pub fn banner(project: &Project, sample_rate: u32) {
    let total_sec = project.total_samples as f64 / sample_rate as f64;
    println!();
    println!("  {}", project.title);
    println!(
        "  {:.0} BPM  ·  {}/{}  ·  {} measures  ·  {}  ·  {} voices",
        project.initial_bpm,
        project.time_signature.0,
        project.time_signature.1,
        project.line_count(),
             fmt_time(total_sec),
             project.voice_count,
    );
    println!();
}

pub fn run(transport: Arc<Transport>, project: Arc<Project>, sample_rate: u32) {
    let total_sec = project.total_samples as f64 / sample_rate as f64;
    let total_lines = project.line_count() as i64;
    let mut last_len = 0usize;

    while !transport.done.load(Ordering::Acquire) {
        let pos = transport.position.load(Ordering::Relaxed) as f64 / sample_rate as f64;
        let voices = transport.active_voices.load(Ordering::Relaxed);
        let loaded = transport.loaded_lines.load(Ordering::Relaxed);
        let line = progress_line(pos, total_sec, voices, loaded, total_lines);
        redraw(&line, &mut last_len);
        std::thread::sleep(std::time::Duration::from_millis(80));
    }

    // Final frame at 100%.
    let line = progress_line(total_sec, total_sec, 0, total_lines, total_lines);
    redraw(&line, &mut last_len);
    println!();
}

fn redraw(line: &str, last_len: &mut usize) {
    let pad = " ".repeat(last_len.saturating_sub(line.len()));
    print!("\r{}{}", line, pad);
    let _ = std::io::stdout().flush();
    *last_len = line.len();
}

fn progress_line(pos: f64, total: f64, voices: i64, loaded: i64, total_lines: i64) -> String {
    let pct = (pos / total.max(1e-9)).clamp(0.0, 1.0);
    let filled = (pct * BAR_WIDTH as f64).round() as usize;
    let bar: String = "█".repeat(filled) + &"░".repeat(BAR_WIDTH - filled);
    format!(
        "  [{}]  {:>6} / {:<6}  {:>3} voices  {:>2}/{} lines",
        bar,
        fmt_time(pos),
            fmt_time(total),
            voices,
            loaded,
            total_lines,
    )
}

fn fmt_time(sec: f64) -> String {
    let s = sec.max(0.0).round() as u32;
    format!("{}:{:02}", s / 60, s % 60)
}
