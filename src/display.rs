//! Startup banner for live playback.
//!
//! One `println!` block with title, tempo, time signature, measure count,
//! duration, and voice count.  The live progress display that used to
//! live here was replaced by the ratatui TUI in `tui.rs`.

use crate::player::Project;

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

fn fmt_time(sec: f64) -> String {
    let s = sec.max(0.0).round() as u32;
    format!("{}:{:02}", s / 60, s % 60)
}
