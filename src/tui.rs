//! Ratatui front-end for live playback.
//!
//! Tracker view with synesthetic coloring and a waveform overlay:
//!   - pitch class maps to hue on a 12-step wheel
//!   - volume maps to saturation
//!   - playing notes invert; others dim
//!   - a grey waveform of each part's source sample is drawn *behind*
//!     the note text
//!   - sample parts render real PCM; synth parts render a synthesized
//!     sine at the note's pitch
//!   - a per-column yellow playhead follows the actual source read
//!     position of the note currently playing in that column, so it
//!     snaps when a note retriggers or its offset changes, and moves
//!     at the source's pitch-scaled read rate
//!
//! The waveform uses braille (2×4 dots per cell) so a 30-char column is
//! 60 pixels wide and a 20-line row is 80 pixels tall.

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    },
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, Paragraph},
    Frame, Terminal,
};
use std::io::{self, Stdout};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use crate::player::{Project, Sample, Transport};
use crate::sources::trackscript::{hz_to_note, TrackscriptEvent, C5_HZ};

const SEEK_STEP_SECS: f64 = 5.0;
const TICK_MS: u64 = 50;
const LOOKBACK_LINES: usize = 128;
/// Cap source samples read per pixel row.  Long samples otherwise stall
/// the renderer at 20 FPS.
const MAX_SLICE: usize = 512;
/// Column width in terminal cells.  30 gives 60 braille pixels, wide
/// enough to see low-frequency detail without crowding the note text.
const COL_W: usize = 30;
/// Latency compensation for the visual playhead.  The position is read
/// at the top of a frame, but the frame is not visible until the draw
/// finishes and the terminal refreshes, and there is extra audio
/// output buffering on top of that.  This offset is added to the
/// position used by the tracker view so the playhead lines up with
/// what is actually being heard.  Tune by ear: raise if the playhead
/// trails, lower if it leads.  0 disables compensation.
const PLAYHEAD_LOOKAHEAD_SECS: f64 = 0.05;

// ── entry point ────────────────────────────────────────────────────────

pub fn run(transport: Arc<Transport>, project: Arc<Project>, sample_rate: u32) -> Result<()> {
    let mut term = setup()?;
    let result = event_loop(&mut term, &transport, &project, sample_rate);
    let _ = restore(&mut term);
    transport.stop.store(true, Ordering::Relaxed);
    result
}

fn setup() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen)?;
    Ok(Terminal::new(CrosstermBackend::new(out))?)
}

fn restore(t: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(t.backend_mut(), LeaveAlternateScreen)?;
    t.show_cursor()?;
    Ok(())
}

fn event_loop(
    term: &mut Terminal<CrosstermBackend<Stdout>>,
    tp: &Arc<Transport>,
    project: &Arc<Project>,
    sample_rate: u32,
) -> Result<()> {
    let sr = sample_rate as f64;
    let total_secs = project.total_secs();
    let seek_step = (SEEK_STEP_SECS * sr) as i64;
    let tick = Duration::from_millis(TICK_MS);

    loop {
        term.draw(|f| draw(f, tp, project, sample_rate, total_secs))?;

        if event::poll(tick)? {
            if let Event::Key(k) = event::read()? {
                if k.kind == KeyEventKind::Press {
                    match (k.code, k.modifiers) {
                        (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => break,
                        (KeyCode::Char('c'), KeyModifiers::CONTROL) => break,
                        (KeyCode::Char(' '), _) => {
                            let p = tp.paused.load(Ordering::Relaxed);
                            tp.paused.store(!p, Ordering::Relaxed);
                        }
                        (KeyCode::Left, _) => {
                            let pos = tp.position.load(Ordering::Relaxed);
                            tp.seek_to.store((pos - seek_step).max(0), Ordering::Release);
                        }
                        (KeyCode::Right, _) => {
                            let pos = tp.position.load(Ordering::Relaxed);
                            tp.seek_to.store(
                                (pos + seek_step).min(project.total_samples),
                                             Ordering::Release,
                            );
                        }
                        (KeyCode::Home, _) => {
                            tp.seek_to.store(0, Ordering::Release);
                        }
                        _ => {}
                    }
                }
            }
        }

        if tp.done.load(Ordering::Acquire) {
            term.draw(|f| draw(f, tp, project, sample_rate, total_secs))?;
            std::thread::sleep(Duration::from_millis(500));
            break;
        }
    }
    Ok(())
}

fn draw(
    f: &mut Frame<'_>,
    tp: &Transport,
    project: &Project,
    sample_rate: u32,
    total_secs: f64,
) {
    let area = f.area();
    let chunks = Layout::default()
    .direction(Direction::Vertical)
    .constraints([
        Constraint::Length(3),
                 Constraint::Min(6),
                 Constraint::Length(3),
    ])
    .split(area);

    draw_header(f, chunks[0], project);
    draw_body(f, chunks[1], tp, project);
    draw_transport(f, chunks[2], tp, sample_rate, total_secs);
}

fn draw_header(f: &mut Frame<'_>, area: Rect, p: &Project) {
    let (num, den) = p.time_signature;
    let line = Line::from(vec![
        Span::styled(" ♪ ", Style::default().fg(Color::Cyan)),
                          Span::styled(p.title.clone(), Style::default().add_modifier(Modifier::BOLD)),
                          Span::raw(format!(
                              "    {} BPM    {}/{}    {} voices    {} lines",
                              p.initial_bpm.round() as i64,
                                            num, den, p.voice_count, p.line_count()
                          )),
    ]);
    f.render_widget(
        Paragraph::new(line).block(Block::default().borders(Borders::ALL)),
                    area,
    );
}

fn draw_body(f: &mut Frame<'_>, area: Rect, tp: &Transport, p: &Project) {
    let chunks = Layout::default()
    .direction(Direction::Horizontal)
    .constraints([Constraint::Percentage(78), Constraint::Percentage(22)])
    .split(area);
    draw_tracker(f, chunks[0], tp, p);
    draw_live_panel(f, chunks[1], tp, p);
}

// ── color mapping ──────────────────────────────────────────────────────

fn hsv_to_rgb(h: f64, s: f64, v: f64) -> (u8, u8, u8) {
    let h = h.rem_euclid(360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match h as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (
        ((r + m) * 255.0).clamp(0.0, 255.0) as u8,
     ((g + m) * 255.0).clamp(0.0, 255.0) as u8,
     ((b + m) * 255.0).clamp(0.0, 255.0) as u8,
    )
}

fn pitch_hue(midi: i32) -> f64 {
    midi.rem_euclid(12) as f64 * 30.0
}

#[derive(Clone, Copy)]
enum NoteState {
    Playing,
    Waiting,
    Future,
    Carried,
}

impl NoteState {
    fn value(self) -> f64 {
        match self {
            NoteState::Playing => 1.0,
            NoteState::Waiting => 0.55,
            NoteState::Future => 0.22,
            NoteState::Carried => 0.7,
        }
    }
}

fn note_style(pitch_hz: f64, vol: f64, state: NoteState) -> Style {
    let midi = hz_to_note(pitch_hz);
    let hue = pitch_hue(midi);
    let sat = (vol / 100.0).clamp(0.0, 1.0);
    match state {
        NoteState::Playing => {
            let (r, g, b) = hsv_to_rgb(hue, sat, 1.0);
            Style::default()
            .fg(Color::Black)
            .bg(Color::Rgb(r, g, b))
            .add_modifier(Modifier::BOLD)
        }
        _ => {
            let (r, g, b) = hsv_to_rgb(hue, sat, state.value());
            Style::default().fg(Color::Rgb(r, g, b))
        }
    }
}

// ── note model ─────────────────────────────────────────────────────────

#[derive(Clone)]
struct Point {
    row: f64,
    value: f64,
    ramp_rows: f64,
}

#[derive(Clone)]
struct NoteRow {
    start_row: f64,
    end_row: f64,
    dur_beats: Option<f64>,
    offset: u32,
    pitch_points: Vec<Point>,
    vol_points: Vec<Point>,
    pan_points: Vec<Point>,
}

struct DisplayNote {
    note: NoteRow,
    display_start: f64,
    display_end: f64,
    value_offset: f64,
}

fn split_notes(events: &[TrackscriptEvent], subs: u32) -> Vec<NoteRow> {
    let subs_f = subs.max(1) as f64;
    let mut out: Vec<NoteRow> = Vec::new();
    let mut current: Option<usize> = None;

    for ev in events {
        if ev.is_waypoint() {
            if let Some(idx) = current {
                let n = &mut out[idx];
                let ramp = ev.dur_beats.unwrap_or(0.0) * subs_f;
                if let Some(hz) = ev.pitch_hz {
                    n.pitch_points.push(Point { row: ev.row, value: hz, ramp_rows: ramp });
                }
                if let Some(v) = ev.vol {
                    n.vol_points.push(Point { row: ev.row, value: v, ramp_rows: ramp });
                }
                if let Some(p) = ev.pan {
                    n.pan_points.push(Point { row: ev.row, value: p, ramp_rows: ramp });
                }
                let wp_end = ev.row + ramp;
                if wp_end > n.end_row { n.end_row = wp_end; }
            }
        } else {
            let dur_beats = ev.dur_beats.filter(|d| *d > 0.0).unwrap_or(1.0);
            let end_row = ev.row + dur_beats * subs_f;
            let mut nr = NoteRow {
                start_row: ev.row,
                end_row,
                dur_beats: ev.dur_beats,
                offset: ev.offset.unwrap_or(0),
                pitch_points: Vec::new(),
                vol_points: Vec::new(),
                pan_points: Vec::new(),
            };
            if let Some(hz) = ev.pitch_hz {
                nr.pitch_points.push(Point { row: ev.row, value: hz, ramp_rows: 0.0 });
            }
            if let Some(v) = ev.vol {
                nr.vol_points.push(Point { row: ev.row, value: v, ramp_rows: 0.0 });
            }
            if let Some(p) = ev.pan {
                nr.pan_points.push(Point { row: ev.row, value: p, ramp_rows: 0.0 });
            }
            out.push(nr);
            current = Some(out.len() - 1);
        }
    }
    out
}

fn value_at(points: &[Point], row: f64) -> Option<f64> {
    let mut prev: Option<f64> = None;
    let mut cur: Option<&Point> = None;
    for p in points {
        if p.row <= row {
            if let Some(c) = cur { prev = Some(c.value); }
            cur = Some(p);
        } else { break; }
    }
    let p = cur?;
    if p.ramp_rows <= 0.0 || row >= p.row + p.ramp_rows { return Some(p.value); }
    let from = prev.unwrap_or(p.value);
    let frac = (row - p.row) / p.ramp_rows;
    Some(from + (p.value - from) * frac)
}

fn fmt_pitch(hz: f64) -> String {
    let midi = hz_to_note(hz);
    let names = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    let oct = midi.div_euclid(12);
    format!("{}{}", names[midi.rem_euclid(12) as usize], oct)
}

fn fmt_num(v: f64) -> String {
    let s = format!("{:.2}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() { "0".into() } else { s.to_string() }
}

fn fmt_note(n: &NoteRow, row: f64, subs: u32) -> String {
    let subs_f = subs.max(1) as f64;
    let beat = (n.start_row / subs_f).floor() + 1.0;
    let sub = (n.start_row % subs_f) + 1.0;
    let mut parts: Vec<String> = Vec::new();
    parts.push(format!("§{}/{}", beat as i64, fmt_num(sub)));
    if let Some(hz) = value_at(&n.pitch_points, row) {
        parts.push(format!("#{}", fmt_pitch(hz)));
    }
    if let Some(d) = n.dur_beats { parts.push(format!("~{}", fmt_num(d))); }
    if let Some(v) = value_at(&n.vol_points, row) { parts.push(format!("%{}", fmt_num(v))); }
    if let Some(p) = value_at(&n.pan_points, row) {
        if p.abs() > 0.01 { parts.push(format!("¶{}", fmt_num(p))); }
    }
    format!("<{}>", parts.join(","))
}

// ── braille helpers ────────────────────────────────────────────────────

#[inline]
fn dot_bit(lx: usize, ly: usize) -> u8 {
    match (lx, ly) {
        (0, 0) => 0x01, (0, 1) => 0x02, (0, 2) => 0x04,
        (1, 0) => 0x08, (1, 1) => 0x10, (1, 2) => 0x20,
        (0, 3) => 0x40, (1, 3) => 0x80,
        _ => 0,
    }
}

fn braille_char(bits: u8) -> char {
    if bits == 0 { ' ' } else { char::from_u32(0x2800 + bits as u32).unwrap_or(' ') }
}

fn wrapped_pos(raw: f64, loop_pts: Option<(i64, i64)>) -> f64 {
    if let Some((ls, le)) = loop_pts {
        let ls_f = ls as f64;
        let le_f = le as f64;
        if le_f > ls_f && raw >= le_f {
            return ls_f + (raw - le_f) % (le_f - ls_f);
        }
    }
    raw
}

// ── wave envelopes ─────────────────────────────────────────────────────

fn slice_minmax(
    sample: &Sample,
    s0: f64,
    s1: f64,
    loop_pts: Option<(i64, i64)>,
) -> (f32, f32) {
    let n = sample.left.len();
    if n == 0 { return (0.0, 0.0); }

    let span = (s1 - s0).abs();
    if span < 1.0 {
        let i = wrapped_pos(s0, loop_pts).round() as i64;
        let i = i.clamp(0, n as i64 - 1) as usize;
        let l = sample.left[i];
        let r = if sample.stereo { sample.right[i] } else { l };
        let m = (l + r) * 0.5;
        return (m, m);
    }

    let step = (span / MAX_SLICE as f64).max(1.0);
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    let mut p = s0.min(s1);
    let end = s0.max(s1);
    while p < end {
        let i = wrapped_pos(p, loop_pts).round() as i64;
        let i = i.clamp(0, n as i64 - 1) as usize;
        let l = sample.left[i];
        let r = if sample.stereo { sample.right[i] } else { l };
        let m = (l + r) * 0.5;
        if m < lo { lo = m; }
        if m > hi { hi = m; }
        p += step;
    }
    if !lo.is_finite() || !hi.is_finite() { (0.0, 0.0) } else { (lo, hi) }
}

/// Envelope of an entire source sample, one (min, max) pair per pixel
/// row.  Used for the static column background.  The playhead is drawn
/// separately, based on the actual source read position of the note.
fn whole_sample_envelope(sample: &Sample, pixel_rows: usize) -> Vec<(f32, f32)> {
    let n = sample.left.len();
    if n == 0 || pixel_rows == 0 {
        return vec![(0.0, 0.0); pixel_rows];
    }
    let nf = n as f64;
    let mut out = Vec::with_capacity(pixel_rows);
    for py in 0..pixel_rows {
        let s0 = (py as f64 / pixel_rows as f64) * nf;
        let s1 = ((py + 1) as f64 / pixel_rows as f64) * nf;
        // No loop wrapping here: the background is a static view of the
        // whole sample, not the looping playback region.
        out.push(slice_minmax(sample, s0, s1, None));
    }
    out
}

/// Synthesized envelope for a synth part: a sine whose pitch-driven
/// period scales with the note's pitch, amplitude from the note's volume.
fn synth_envelope(
    p: &Project,
    part_name: &str,
    pixel_rows: usize,
) -> Vec<(f32, f32)> {
    let subs = p.part_subs_per_beat(part_name);
    let rpmeasure = p.rows_per_measure();
    let notes = split_notes(p.part_events(part_name), subs);

    let mut out = Vec::with_capacity(pixel_rows);
    let mut phase = 0.0f64;

    for py in 0..pixel_rows {
        let t = py as f64 / pixel_rows as f64;
        let row = t * rpmeasure;

        // The note currently under this pixel row.
        let active = notes.iter().rev().find(|n| n.start_row <= row && row < n.end_row);
        let Some(note) = active else {
            out.push((0.0, 0.0));
            continue;
        };

        let pitch = value_at(&note.pitch_points, row).unwrap_or(C5_HZ);
        let vol = (value_at(&note.vol_points, row).unwrap_or(100.0) / 100.0) as f32;

        // Two cycles across the visible window at C5, scaling with pitch.
        // Higher pitch = more cycles across the same vertical span.
        let cycles_total = (pitch / C5_HZ) * 2.0;
        phase += cycles_total / pixel_rows as f64;
        let s = (phase * std::f64::consts::TAU).sin() as f32 * vol;
        out.push((s, s));
    }
    out
}

// ── tracker with wave overlay ──────────────────────────────────────────

fn draw_tracker(f: &mut Frame<'_>, area: Rect, tp: &Transport, p: &Project) {
    // Position for the tracker view: real position plus a small
    // lookahead to compensate for render + terminal + audio-buffer
    // latency.  The transport gauge below still uses the raw position.
    let sr_comp = p.sample_rate as f64;
    let lookahead = (PLAYHEAD_LOOKAHEAD_SECS * sr_comp) as i64;
    let pos = (tp.position.load(Ordering::Relaxed) + lookahead)
    .min(p.total_samples);
    let current = p.current_line(pos);

    let title = match current {
        Some(li) => format!(" BlockList line {} ", li + 1),
        None => " BlockList (idle) ".into(),
    };
    let outer = Block::default().borders(Borders::ALL).title(title);

    let Some(li) = current else {
        f.render_widget(Paragraph::new("(silence)").block(outer), area);
        return;
    };
    let parts = p.line_parts(li);
    if parts.is_empty() {
        f.render_widget(Paragraph::new("(silence)").block(outer), area);
        return;
    }

    let inner = outer.inner(area);
    let inner_w = inner.width as usize;
    let inner_h = inner.height as usize;
    if inner_w < COL_W + 1 || inner_h < 2 {
        f.render_widget(outer, area);
        return;
    }

    let visible_cols = (inner_w / (COL_W + 1)).min(parts.len());
    let char_rows = inner_h.saturating_sub(1); // one line for the header
    let pixel_rows = char_rows * 4;

    // Measure duration in seconds.
    let sr = p.sample_rate as f64;
    let measure_samples = p.line_measure_samples(li).max(1);
    let measure_secs = measure_samples as f64 / sr;

    // Current playhead position within the measure (0..1).
    let line_start = p.line_start_sample(li);
    let elapsed = (pos - line_start).max(0);
    let playhead_frac = (elapsed as f64 / measure_samples as f64).clamp(0.0, 1.0);
    let rpmeasure = p.rows_per_measure();
    let current_row = playhead_frac * rpmeasure;
    let rows_per_sec = rpmeasure / measure_secs.max(1e-9);

    // Build columns of DisplayNote for the visible parts.
    let mut columns: Vec<Vec<DisplayNote>> = Vec::with_capacity(visible_cols);
    for (i, name) in parts.iter().take(visible_cols).enumerate() {
        let subs = p.part_subs_per_beat(name);
        let mut notes: Vec<DisplayNote> = Vec::new();
        let lookback = LOOKBACK_LINES.min(li);
        for k in 1..=lookback {
            let prev_li = li - k;
            let prev_parts = p.line_parts(prev_li);
            let Some(prev_name) = prev_parts.get(i) else { continue };
            let prev_subs = p.part_subs_per_beat(prev_name);
            let offset_rows = k as f64 * rpmeasure;

            for note in split_notes(p.part_events(prev_name), prev_subs) {
                let active = note.end_row > offset_rows;
                let has_vol_waypoints = note.vol_points.len() > 1;
                let dur_beats = note.dur_beats.unwrap_or(1.0);
                let is_sustained = dur_beats >= 2.0;
                let end_vol = value_at(&note.vol_points, note.end_row).unwrap_or(0.0);
                let still_fading = has_vol_waypoints && is_sustained && end_vol > 0.01;
                if !active && !still_fading { continue; }
                let display_end = if active {
                    (note.end_row - offset_rows).min(rpmeasure)
                } else { 0.0 };
                notes.push(DisplayNote {
                    note,
                    display_start: 0.0,
                    display_end,
                    value_offset: offset_rows,
                });
            }
        }
        for note in split_notes(p.part_events(name), subs) {
            let display_start = note.start_row;
            let display_end = note.end_row;
            notes.push(DisplayNote {
                note,
                display_start,
                display_end,
                value_offset: 0.0,
            });
        }
        notes.sort_by(|a, b| {
            a.display_start.partial_cmp(&b.display_start).unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.value_offset.partial_cmp(&b.value_offset).unwrap_or(std::cmp::Ordering::Equal))
        });
        columns.push(notes);
    }

    // Wave envelopes, one per visible column.  Sample parts show the
    // whole source sample as a static background; synth parts show a
    // synthesized sine over the measure.
    let mut envelopes: Vec<Vec<(f32, f32)>> = Vec::with_capacity(visible_cols);
    for name in parts.iter().take(visible_cols) {
        if let Some(sample) = p.sample_for_part(name) {
            envelopes.push(whole_sample_envelope(&sample, pixel_rows));
        } else {
            envelopes.push(synth_envelope(p, name, pixel_rows));
        }
    }

    // Per-column playhead pixel row.
    //
    // For sample parts this is derived from the actual source read
    // position of the note currently active in that column: offset +
    // elapsed_secs * sample_rate * pitch_ratio, wrapped through the
    // sample's loop if it has one.  That means it snaps when the note
    // retriggers (or its offset changes) and moves at the pitch-scaled
    // read rate, so a high note visibly scrolls the playhead faster.
    //
    // For synth parts (no source sample) we fall back to a plain
    // measure-linear sweep.
    let mut playhead_pys: Vec<Option<usize>> = Vec::with_capacity(visible_cols);
    for col in 0..visible_cols {
        let part_name = &parts[col];
        if let Some(sample) = p.sample_for_part(part_name) {
            let n = sample.left.len();
            if n == 0 {
                playhead_pys.push(None);
                continue;
            }
            let nf = n as f64;
            let loop_pts = sample.loop_pts;
            let sample_rate_f = sample.rate;

            // Most recently started note that's still sounding at
            // current_row.  Notes are sorted ascending by display_start,
            // so reverse-iteration finds the newest one first.
            let active = columns[col].iter().rev().find(|dn| {
                let local_row = dn.value_offset + current_row;
                dn.note.start_row <= local_row && local_row < dn.note.end_row
            });

            let py = active.and_then(|dn| {
                let local_row = dn.value_offset + current_row;
                let elapsed_rows = (local_row - dn.note.start_row).max(0.0);
                let elapsed_secs = elapsed_rows / rows_per_sec;
                let pitch = value_at(&dn.note.pitch_points, local_row).unwrap_or(C5_HZ);
                let ratio = (pitch / C5_HZ).max(1e-6);
                let src_now = dn.note.offset as f64 + elapsed_secs * sample_rate_f * ratio;
                let wrapped = wrapped_pos(src_now, loop_pts);
                let t = (wrapped / nf).clamp(0.0, 1.0);
                let row = ((t * pixel_rows as f64).floor() as usize)
                .min(pixel_rows.saturating_sub(1));
                Some(row)
            });
            playhead_pys.push(py);
        } else {
            let row = ((playhead_frac * pixel_rows as f64).floor() as usize)
            .min(pixel_rows.saturating_sub(1));
            playhead_pys.push(Some(row));
        }
    }

    // Neutral grey wave.  Text cells keep their own style, so the wave
    // does not tint note highlights underneath it.
    let wave_fg = Color::Rgb(128, 128, 128);

    // Header line: part names.
    let mut header: Vec<Span> = Vec::new();
    for name in parts.iter().take(visible_cols) {
        let mut label = name.clone();
        if label.len() >= COL_W {
            label.truncate(COL_W - 1);
            label.push('…');
        }
        header.push(Span::styled(
            format!("{:<width$}", label, width = COL_W),
                Style::default().add_modifier(Modifier::BOLD),
        ));
        header.push(Span::raw(" "));
    }
    let mut lines: Vec<Line> = Vec::with_capacity(char_rows + 1);
    lines.push(Line::from(header));

    for r in 0..char_rows {
        let mut spans: Vec<Span> = Vec::new();
        for col in 0..visible_cols {
            let name = &parts[col];
            let subs = p.part_subs_per_beat(name);

            // Playhead char row for this column (if any).
            let playhead_char: Option<usize> = playhead_pys[col].map(|py| py / 4);

            // Wave bits for each cell in this part column.
            for cx in 0..COL_W {
                let mut bits: u8 = 0;
                for ly in 0..4 {
                    let py = r * 4 + ly;
                    if py >= pixel_rows { continue; }
                    let (lo, hi) = envelopes[col].get(py).copied().unwrap_or((0.0, 0.0));
                    // Amplitude [-1, 1] -> pixel [0, COL_W*2-1].
                    let half = (COL_W * 2) as f32 / 2.0;
                    let center = half;
                    let px_lo = (center + lo * half).round() as isize;
                    let px_hi = (center + hi * half).round() as isize;
                    let px_lo = px_lo.max(0) as usize;
                    let px_hi = (px_hi.min((COL_W * 2 - 1) as isize)).max(px_lo as isize) as usize;
                    let cell_x0 = cx * 2;
                    let cell_x1 = cx * 2 + 1;
                    if (px_lo..=px_hi).contains(&cell_x0) { bits |= dot_bit(0, ly); }
                    if (px_lo..=px_hi).contains(&cell_x1) { bits |= dot_bit(1, ly); }
                }

                // Find text occupying this exact cell.  Notes are drawn
                // as a single span across the whole part column, so we
                // check if the note's text covers this cell index.
                let cell_text: Option<(String, Style)> = columns[col].get(r).map(|dn| {
                    let note_local_row = dn.value_offset + current_row;
                    let text = fmt_note(&dn.note, note_local_row, subs);
                    let playing = dn.display_end > 0.0
                    && dn.display_start <= current_row
                    && current_row < dn.display_end;
                    let state = if playing {
                        NoteState::Playing
                    } else if dn.value_offset > 0.0 {
                        NoteState::Carried
                    } else if dn.note.start_row >= rpmeasure {
                        NoteState::Future
                    } else {
                        NoteState::Waiting
                    };
                    let pitch = value_at(&dn.note.pitch_points, note_local_row).unwrap_or(440.0);
                    let vol = value_at(&dn.note.vol_points, note_local_row).unwrap_or(100.0);
                    (text, note_style(pitch, vol, state))
                });

                // Determine if the note text covers this cx.
                let text_char: Option<char> = cell_text.as_ref().and_then(|(t, _)| {
                    t.chars().nth(cx)
                });

                let under_playhead = playhead_char == Some(r);

                if let Some(ch) = text_char {
                    // Text cell: keep the note's own style so its
                    // highlight colors are not polluted by the wave or
                    // the playhead.
                    let style = cell_text.as_ref().unwrap().1;
                    spans.push(Span::styled(ch.to_string(), style));
                } else if bits != 0 {
                    // Wave-only cell.
                    let ch = braille_char(bits);
                    let style = if under_playhead {
                        Style::default().fg(Color::Yellow)
                    } else {
                        Style::default().fg(wave_fg)
                    };
                    spans.push(Span::styled(ch.to_string(), style));
                } else {
                    let style = if under_playhead {
                        Style::default().bg(Color::Rgb(80, 80, 40))
                    } else {
                        Style::default()
                    };
                    spans.push(Span::styled(" ", style));
                }
            }
            spans.push(Span::raw(" "));
        }
        lines.push(Line::from(spans));
    }

    f.render_widget(Paragraph::new(lines).block(outer), area);
}

// ── live panel ─────────────────────────────────────────────────────────

fn draw_live_panel(f: &mut Frame<'_>, area: Rect, tp: &Transport, p: &Project) {
    let pos = tp.position.load(Ordering::Relaxed);
    let active = tp.active_voices.load(Ordering::Relaxed);
    let loaded = tp.loaded_lines.load(Ordering::Relaxed);
    let done = tp.done.load(Ordering::Relaxed);
    let paused = tp.paused.load(Ordering::Relaxed);

    let state_str = if done { "finished" } else if paused { "paused" } else { "playing" };
    let state_col = if done { Color::DarkGray } else if paused { Color::Yellow } else { Color::Green };
    let now_line = p.current_line(pos).map(|i| format!("{}", i + 1)).unwrap_or_else(|| "—".into());

    let legend: Vec<Span> = (0..12).flat_map(|class| {
        let (r, g, b) = hsv_to_rgb(class as f64 * 30.0, 0.9, 1.0);
        vec![Span::styled("  ", Style::default().bg(Color::Rgb(r, g, b))), Span::raw(" ")]
    }).collect();

    let text = vec![
        Line::from(vec![
            Span::styled("state:          ", Style::default().fg(Color::DarkGray)),
                   Span::styled(state_str, Style::default().fg(state_col)),
        ]),
        Line::from(format!("now on line:    {}", now_line)),
        Line::from(format!("active voices:  {}", active)),
        Line::from(format!("loader cursor:  {}", loaded)),
        Line::from(""),
        Line::from(Span::styled("C  C# D  D# E  F  F# G  G# A  A# B", Style::default().fg(Color::DarkGray))),
        Line::from(legend),
        Line::from(""),
        Line::from(Span::styled("space  pause", Style::default().fg(Color::DarkGray))),
        Line::from(Span::styled("← →    seek ±5s", Style::default().fg(Color::DarkGray))),
        Line::from(Span::styled("home   rewind", Style::default().fg(Color::DarkGray))),
        Line::from(Span::styled("q      quit", Style::default().fg(Color::DarkGray))),
    ];
    f.render_widget(
        Paragraph::new(text).block(Block::default().borders(Borders::ALL).title(" Live ")),
                    area,
    );
}

fn draw_transport(f: &mut Frame<'_>, area: Rect, tp: &Transport, sr: u32, total_secs: f64) {
    let pos = tp.position.load(Ordering::Relaxed).max(0) as f64;
    let secs = pos / sr as f64;
    let ratio = if total_secs > 0.0 { (secs / total_secs).clamp(0.0, 1.0) } else { 0.0 };
    let paused = tp.paused.load(Ordering::Relaxed);
    let label = format!("{}  {:>6.1}s / {:.1}s", if paused { "❚❚" } else { "▶" }, secs, total_secs);
    let gauge = Gauge::default()
    .block(Block::default().borders(Borders::ALL))
    .gauge_style(Style::default().fg(if paused { Color::Yellow } else { Color::Green }))
    .ratio(ratio)
    .label(label);
    f.render_widget(gauge, area);
}
