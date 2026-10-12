//! Ratatui front-end for live playback.
//!
//! Tracker view with synesthetic coloring and a waveform overlay, plus a
//! two-button transport at the bottom modelled on the DSi sound player:
//! pressing left or right plays in that direction and the button turns
//! into a stop; holding the key accelerates.  There is no pause button —
//! the only states are stopped, forward, and reverse, with a speed
//! multiplier on top.
//!
//!   - pitch class maps to hue on a 12-step wheel
//!   - volume maps to saturation
//!   - playing notes invert; others dim
//!   - a grey waveform is drawn behind the note text
//!   - sample parts render the source PCM as a static background,
//!     with a playhead tracking the pitch-scaled source read position
//!   - VST parts render a live scrolling scope of the plugin's actual
//!     output, fed by a global ring buffer indexed by channel iid
//!   - a `cover.jpg` next to the trackscript fills the whole terminal
//!     as a mosaic (one dimmed color per cell), painted as a background
//!     pass after every widget, so any cell without an explicit
//!     background picks it up

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    },
};
use image::GenericImageView;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame, Terminal,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, Stdout};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use crate::player::{Project, Sample, Transport};
use crate::sources::trackscript::{hz_to_note, TrackscriptEvent, C5_HZ};

const SEEK_STEP_SECS: f64 = 5.0;
const TICK_MS: u64 = 50;
const LOOKBACK_LINES: usize = 128;
/// Cap source samples read per pixel row.  Long samples otherwise stall
/// the renderer at 20 FPS.
const MAX_SLICE: usize = 512;
/// Minimum and maximum width of a tracker column, in terminal cells.
/// Width is computed per frame: whatever fits, distributed evenly across
/// the parts currently playing, clamped to this range.  At MIN_COL_W the
/// column still shows position, pitch name, and the start of the
/// duration field; at MAX_COL_W the full note token is visible.
const MIN_COL_W: usize = 10;
const MAX_COL_W: usize = 40;
/// Recent samples retained per part for the live scope.  At 48 kHz this
/// is ~85 ms — comfortably more than one frame.
const SCOPE_LEN: usize = 4096;
/// Scope slots are indexed by VST channel iid, a u16.  In practice the
/// FLP-derived projects we handle use 0..=127.
const SCOPE_MAX_PARTS: usize = 256;
/// Latency compensation for the visual playhead.  Raise if the playhead
/// trails what you hear, lower if it leads.  0 disables compensation.
const PLAYHEAD_LOOKAHEAD_SECS: f64 = 0.05;
/// Brightness multiplier applied to the cover image.  0.5 = "half
/// transparency" — the image is visible but text on top stays readable.
const COVER_DIM: f32 = 0.5;
/// Speed multipliers the direction keys step through on hold.
const SPEED_STEPS: &[i64] = &[1, 4, 16];
/// Two presses of the same direction key within this window count as a
/// hold.  Most terminals don't send `KeyEventKind::Repeat` (that's a
/// Kitty keyboard protocol feature), so we detect holds by timing.
const HOLD_WINDOW_MS: u128 = 250;
/// Foreground for unplayed notes (waiting or future).  White reads on
/// most backgrounds; switch to `Color::Black` if your cover image is
/// mostly light and white gets lost in it.
const UNPLAYED_FG: Color = Color::White;
// ── cover art ──────────────────────────────────────────────────────────

/// A cover image resampled to one dimmed RGB color per terminal cell.
/// Sampled at the full terminal size, painted as a background pass over
/// every cell that doesn't carry its own explicit background style.
struct Cover {
    cells: Vec<Color>,
    cols: usize,
    rows: usize,
}

impl Cover {
    fn load(path: &Path, cols: usize, rows: usize, dim: f32) -> Option<Self> {
        if cols == 0 || rows == 0 {
            return None;
        }
        let img = image::open(path).ok()?;
        let (w, h) = img.dimensions();
        if w == 0 || h == 0 {
            return None;
        }
        let rgb = img.to_rgb8();

        // Terminal cells are roughly twice as tall as they are wide, so
        // the cell grid's pixel aspect is cols : rows*2.  Cover-fit: crop
        // the image to that aspect, then sample one pixel per cell.
        let grid_aspect = cols as f32 / (rows as f32 * 2.0);
        let img_aspect = w as f32 / h as f32;
        let (src_x, src_y, src_w, src_h) = if img_aspect > grid_aspect {
            let new_w = (h as f32 * grid_aspect).round().max(1.0) as u32;
            ((w - new_w) / 2, 0, new_w, h)
        } else {
            let new_h = (w as f32 / grid_aspect).round().max(1.0) as u32;
            (0, (h - new_h) / 2, w, new_h)
        };

        let mut cells = Vec::with_capacity(cols * rows);
        for cy in 0..rows {
            for cx in 0..cols {
                let sx = src_x + ((cx as f32 + 0.5) / cols as f32 * src_w as f32) as u32;
                let sy = src_y + ((cy as f32 + 0.5) / rows as f32 * src_h as f32) as u32;
                let sx = sx.min(rgb.width().saturating_sub(1));
                let sy = sy.min(rgb.height().saturating_sub(1));
                let p = rgb.get_pixel(sx, sy);
                let r = (p[0] as f32 * dim).round().clamp(0.0, 255.0) as u8;
                let g = (p[1] as f32 * dim).round().clamp(0.0, 255.0) as u8;
                let b = (p[2] as f32 * dim).round().clamp(0.0, 255.0) as u8;
                cells.push(Color::Rgb(r, g, b));
            }
        }
        Some(Cover { cells, cols, rows })
    }

    fn get(&self, row: usize, col: usize) -> Option<Color> {
        if row >= self.rows || col >= self.cols {
            return None;
        }
        Some(self.cells[row * self.cols + col])
    }
}

// ── global scope ring buffer ───────────────────────────────────────────

struct Scope {
    inner: Vec<Mutex<VecDeque<f32>>>,
}

static SCOPE: OnceLock<Scope> = OnceLock::new();

fn scope() -> &'static Scope {
    SCOPE.get_or_init(|| {
        let mut inner = Vec::with_capacity(SCOPE_MAX_PARTS);
        for _ in 0..SCOPE_MAX_PARTS {
            inner.push(Mutex::new(VecDeque::with_capacity(SCOPE_LEN)));
        }
        Scope { inner }
    })
}

/// Call from the audio callback, once per part, after the synth produces
/// the part's sample for this frame.  Never blocks.
pub fn scope_push(part: usize, sample: f32) {
    let s = scope();
    let Some(slot) = s.inner.get(part) else { return };
    if let Ok(mut q) = slot.try_lock() {
        if q.len() >= SCOPE_LEN {
            q.pop_front();
        }
        q.push_back(sample);
    }
}

/// Called from the render thread.  Copies the current window into `out`
/// (cleared first).  Doesn't drain, so a paused synth leaves its last
/// window frozen on screen.
fn scope_snapshot(part: usize, out: &mut Vec<f32>) {
    out.clear();
    let s = scope();
    let Some(slot) = s.inner.get(part) else { return };
    if let Ok(q) = slot.lock() {
        out.extend(q.iter().copied());
    }
}

// ── render state ───────────────────────────────────────────────────────

struct RenderState {
    last_synth_env: Vec<Vec<(f32, f32)>>,
    snapshot: Vec<f32>,
    column_order: Vec<String>,
    cover: Option<Cover>,
    cover_source: Option<PathBuf>,
    cover_dims: (usize, usize),
    /// Last direction key (Left/Right) and when it was pressed.  Used to
    /// tell a fresh tap (toggle play/stop) from a hold (accelerate),
    /// since most terminals don't emit `KeyEventKind::Repeat`.
    last_dir_press: Option<(KeyCode, std::time::Instant)>,
}

impl RenderState {
    fn new() -> Self {
        Self {
            last_synth_env: Vec::new(),
            snapshot: Vec::with_capacity(SCOPE_LEN),
            column_order: Vec::new(),
            cover: None,
            cover_source: None,
            cover_dims: (0, 0),
            last_dir_press: None,
        }
    }
}

// ── entry point ────────────────────────────────────────────────────────

pub fn run(transport: Arc<Transport>, project: Arc<Project>, sample_rate: u32) -> Result<()> {
    let mut term = setup()?;
    let mut state = RenderState::new();
    let result = event_loop(&mut term, &transport, &project, sample_rate, &mut state);
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
    state: &mut RenderState,
) -> Result<()> {
    let tick = Duration::from_millis(TICK_MS);

    loop {
        term.draw(|f| draw(f, tp, project, sample_rate, state))?;

        if event::poll(tick)? {
            if let Event::Key(k) = event::read()? {
                // Quit and rewind fire on press or repeat, not on release.
                if k.kind != KeyEventKind::Release {
                    match k.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        KeyCode::Char('c')
                            if k.modifiers.contains(KeyModifiers::CONTROL) =>
                        {
                            break;
                        }
                        KeyCode::Home => {
                            tp.seek_to.store(0, Ordering::Release);
                            tp.speed.store(0, Ordering::Release);
                        }
                        _ => {}
                    }
                }

                // Direction keys.  A fresh tap toggles play/stop in that
                // direction; a rapid repeat (key held) accelerates.  We
                // can't rely on `KeyEventKind::Repeat` — it only fires
                // under the Kitty keyboard protocol, which most terminals
                // don't enable.  Instead, treat any press within the
                // hold window of the previous press of the same key as
                // a hold.
                if k.kind == KeyEventKind::Press
                    && matches!(k.code, KeyCode::Left | KeyCode::Right)
                {
                    let now = std::time::Instant::now();
                    let is_hold = matches!(
                        state.last_dir_press,
                        Some((last, t))
                            if last == k.code
                                && now.duration_since(t).as_millis() < HOLD_WINDOW_MS
                    );
                    state.last_dir_press = Some((k.code, now));

                    let cur = tp.speed.load(Ordering::Relaxed);
                    let want_sign: i64 = if k.code == KeyCode::Left { -1 } else { 1 };

                    if is_hold {
                        let mag = if cur.signum() == want_sign {
                            cur.unsigned_abs() as i64
                        } else {
                            0
                        };
                        let next = SPEED_STEPS
                            .iter()
                            .copied()
                            .find(|s| *s > mag)
                            .unwrap_or(*SPEED_STEPS.last().unwrap());
                        tp.speed.store(want_sign * next, Ordering::Release);
                    } else {
                        let already = cur.signum() == want_sign;
                        tp.speed.store(
                            if already { 0 } else { want_sign },
                            Ordering::Release,
                        );
                    }
                }
            }
        }

        if tp.done.load(Ordering::Acquire) {
            term.draw(|f| draw(f, tp, project, sample_rate, state))?;
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
    state: &mut RenderState,
) {
    let area = f.area();

    // ── Cover image ──────────────────────────────────────────────────
    let cover_dims = (area.width as usize, area.height as usize);
    if state.cover_source.as_deref() != project.cover_path() || state.cover_dims != cover_dims {
        state.cover = project
        .cover_path()
        .and_then(|path| Cover::load(path, cover_dims.0, cover_dims.1, COVER_DIM));
        state.cover_source = project.cover_path().map(Path::to_path_buf);
        state.cover_dims = cover_dims;
    }

    let chunks = Layout::default()
    .direction(Direction::Vertical)
    .constraints([
        Constraint::Length(3),
                 Constraint::Min(6),
                 Constraint::Length(3),
    ])
    .split(area);

    draw_header(f, chunks[0], project, tp, sample_rate);
    draw_body(f, chunks[1], tp, project, state);
    draw_transport(f, chunks[2], tp);

    // ── Mosaic background pass ──────────────────────────────────────
    //
    // Fill in cover color for any cell without an explicit background.
    // In ratatui `Cell::bg` is a `Color` (default `Color::Reset`), not
    // an Option, so we test against `Color::Reset` directly.
    if let Some(cover) = &state.cover {
        let buf = f.buffer_mut();
        let w = buf.area.width;
        let h = buf.area.height;
        for y in 0..h {
            for x in 0..w {
                let Some(cc) = cover.get(y as usize, x as usize) else { continue };
                if let Some(cell) = buf.cell_mut((x, y)) {
                    if cell.bg == Color::Reset {
                        cell.bg = cc;
                    }
                }
            }
        }
    }
}

fn draw_header(
    f: &mut Frame<'_>,
    area: Rect,
    p: &Project,
    tp: &Transport,
    sr: u32,
) {
    let (num, den) = p.time_signature;
    let pos = tp.position.load(Ordering::Relaxed).max(0) as f64;
    let secs = pos / sr as f64;
    let total = p.total_secs();
    let speed = tp.speed.load(Ordering::Relaxed);
    let speed_tag = match speed {
        0 => "■".to_string(),
        s if s > 0 => format!("▶{}x", s),
        s => format!("◀{}x", -s),
    };
    let line = Line::from(vec![
        Span::styled(" ♪ ", Style::default().fg(Color::Cyan)),
                          Span::styled(p.title.clone(), Style::default().add_modifier(Modifier::BOLD)),
                          Span::raw(format!(
                              "    {} BPM    {}/{}    {} voices    {} lines    {}  {:>5.1}s / {:.1}s",
                              p.initial_bpm.round() as i64,
                                            num, den, p.voice_count, p.line_count(),
                                            speed_tag, secs, total
                          )),
    ]);
    f.render_widget(
        Paragraph::new(line).block(Block::default().borders(Borders::ALL)),
                    area,
    );
}

fn draw_body(
    f: &mut Frame<'_>,
    area: Rect,
    tp: &Transport,
    p: &Project,
    state: &mut RenderState,
) {
    let chunks = Layout::default()
    .direction(Direction::Horizontal)
    .constraints([Constraint::Percentage(78), Constraint::Percentage(22)])
    .split(area);
    draw_tracker(f, chunks[0], tp, p, state);
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
    /// Brightness multiplier for the note's hue.  Unplayed and
    /// currently-playing notes are full brightness; only notes carried
    /// over from earlier lines are dimmed, so the pattern on the
    /// current line reads as a whole while you can still see what's
    /// already sounded.
    fn value(self) -> f64 {
        match self {
            NoteState::Playing => 1.0,
            NoteState::Waiting => 1.0,
            NoteState::Future => 1.0,
            NoteState::Carried => 0.4,
        }
    }
}

fn note_style(pitch_hz: f64, vol: f64, state: NoteState) -> Style {
    let midi = hz_to_note(pitch_hz);
    let hue = pitch_hue(midi);
    let sat = (vol / 100.0).clamp(0.0, 1.0);
    match state {
        NoteState::Playing => {
            // Currently sounding: inverted, still colored by pitch.
            let (r, g, b) = hsv_to_rgb(hue, sat, 1.0);
            Style::default()
            .fg(Color::Black)
            .bg(Color::Rgb(r, g, b))
            .add_modifier(Modifier::BOLD)
        }
        NoteState::Carried => {
            // Still ringing from an earlier line.  Dimmed hue so you
            // can tell it apart from this line's notes.
            let (r, g, b) = hsv_to_rgb(hue, sat, 0.4);
            Style::default().fg(Color::Rgb(r, g, b))
        }
        NoteState::Waiting | NoteState::Future => {
            // The pattern itself.  Full brightness, bold, single color,
            // so the whole line reads at a glance.
            Style::default()
            .fg(UNPLAYED_FG)
            .add_modifier(Modifier::BOLD)
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
    subs: u32,
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
        out.push(slice_minmax(sample, s0, s1, None));
    }
    out
}

fn synth_envelope_live(snapshot: &[f32], pixel_rows: usize) -> Vec<(f32, f32)> {
    if pixel_rows == 0 || snapshot.is_empty() {
        return Vec::new();
    }
    let n = snapshot.len();
    let mut out = Vec::with_capacity(pixel_rows);
    for py in 0..pixel_rows {
        let s0 = (py as f64 / pixel_rows as f64) * n as f64;
        let s1 = ((py + 1) as f64 / pixel_rows as f64) * n as f64;
        let a = (s0.floor() as usize).min(n.saturating_sub(1));
        let b = (s1.ceil() as usize).max(a + 1).min(n);
        let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
        for &s in &snapshot[a..b] {
            if s < lo { lo = s; }
            if s > hi { hi = s; }
        }
        if !lo.is_finite() {
            out.push((0.0, 0.0));
        } else {
            out.push((lo, hi));
        }
    }
    out
}

fn synth_envelope_static(
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

        let active = notes.iter().rev().find(|n| n.start_row <= row && row < n.end_row);
        let Some(note) = active else {
            out.push((0.0, 0.0));
            continue;
        };

        let pitch = value_at(&note.pitch_points, row).unwrap_or(C5_HZ);
        let vol = (value_at(&note.vol_points, row).unwrap_or(100.0) / 100.0) as f32;

        let cycles_total = (pitch / C5_HZ) * 2.0;
        phase += cycles_total / pixel_rows as f64;
        let s = (phase * std::f64::consts::TAU).sin() as f32 * vol;
        out.push((s, s));
    }
    out
}

// ── tracker with wave overlay ──────────────────────────────────────────

fn draw_tracker(
    f: &mut Frame<'_>,
    area: Rect,
    tp: &Transport,
    p: &Project,
    state: &mut RenderState,
) {
    let sr_comp = p.sample_rate as f64;
    let lookahead = (PLAYHEAD_LOOKAHEAD_SECS * sr_comp) as i64;
    let pos = (tp.position.load(Ordering::Relaxed) + lookahead).min(p.total_samples);
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

    let rpmeasure = p.rows_per_measure();
    let lookback = LOOKBACK_LINES.min(li);
    let current_line_parts = p.line_parts(li);

    let mut by_path: HashMap<String, Vec<(String, f64)>> = HashMap::new();
    let mut discovery: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for name in current_line_parts {
        let Some(path) = p.part_path(name) else { continue };
        let key = path.to_string();
        if seen.insert(key.clone()) {
            discovery.push(key.clone());
        }
        by_path.entry(key).or_default().push((name.clone(), 0.0));
    }

    for k in 1..=lookback {
        let prev_li = li - k;
        let offset_rows = k as f64 * rpmeasure;
        for name in p.line_parts(prev_li) {
            let Some(path) = p.part_path(name) else { continue };
            let subs = p.part_subs_per_beat(name);
            let still_on = split_notes(p.part_events(name), subs)
                .iter()
                .any(|n| n.end_row > offset_rows);
            if !still_on { continue; }
            let key = path.to_string();
            if seen.insert(key.clone()) {
                discovery.push(key.clone());
            }
            by_path.entry(key).or_default().push((name.clone(), offset_rows));
        }
    }

    if discovery.is_empty() {
        f.render_widget(Paragraph::new("(silence)").block(outer), area);
        return;
    }

    let present: HashSet<&String> = discovery.iter().collect();
    let mut merged: Vec<String> = state
    .column_order
    .iter()
    .filter(|path| present.contains(path))
    .cloned()
    .collect();
    for path in &discovery {
        if !merged.iter().any(|m| m == path) {
            merged.push(path.clone());
        }
    }
    state.column_order = merged.clone();
    let path_order = merged;

    let inner = outer.inner(area);
    let inner_w = inner.width as usize;
    let inner_h = inner.height as usize;
    if inner_w < MIN_COL_W + 1 || inner_h < 2 {
        f.render_widget(outer, area);
        return;
    }

    // ── Column layout ────────────────────────────────────────────────
    //
    // Width is dynamic.  As many columns as there are parts, up to as
    // many as fit at MIN_COL_W each.  Whatever the terminal gives us is
    // distributed evenly across the visible columns, then clamped to
    // [MIN_COL_W, MAX_COL_W].  So three parts get wide columns, ten
    // parts get narrow ones, and if there are more parts than fit at
    // minimum width, we render the first N and let the rest fall off
    // the right edge.  The alternative — a scrolling window — would
    // fight the persistent column order, which the user wants stable.
    let path_count = path_order.len();
    let max_visible = (inner_w / (MIN_COL_W + 1)).max(1);
    let visible_cols = path_count.min(max_visible);
    let col_w = (inner_w / visible_cols)
        .saturating_sub(1)
        .clamp(MIN_COL_W, MAX_COL_W);

    let char_rows = inner_h.saturating_sub(1);
    let pixel_rows = char_rows * 4;

    let sr = p.sample_rate as f64;
    let measure_samples = p.line_measure_samples(li).max(1);
    let measure_secs = measure_samples as f64 / sr;

    let line_start = p.line_start_sample(li);
    let elapsed = (pos - line_start).max(0);
    let playhead_frac = (elapsed as f64 / measure_samples as f64).clamp(0.0, 1.0);
    let current_row = playhead_frac * rpmeasure;
    let rows_per_sec = rpmeasure / measure_secs.max(1e-9);

    // ── Notes per column ─────────────────────────────────────────────
    let mut columns: Vec<Vec<DisplayNote>> = Vec::with_capacity(visible_cols);
    for col in 0..visible_cols {
        let path = &path_order[col];
        let sources = &by_path[path];
        let mut notes: Vec<DisplayNote> = Vec::new();

        for (name, offset_rows) in sources {
            let subs = p.part_subs_per_beat(name);
            let is_current = *offset_rows == 0.0;
            for note in split_notes(p.part_events(name), subs) {
                let active = note.end_row > *offset_rows;
                let has_vol_waypoints = note.vol_points.len() > 1;
                let dur_beats = note.dur_beats.unwrap_or(1.0);
                let is_sustained = dur_beats >= 2.0;
                let end_vol = value_at(&note.vol_points, note.end_row).unwrap_or(0.0);
                let still_fading = has_vol_waypoints && is_sustained && end_vol > 0.01;
                if !active && !still_fading { continue; }
                let (display_start, display_end) = if is_current {
                    (note.start_row, note.end_row)
                } else if active {
                    (0.0, (note.end_row - *offset_rows).min(rpmeasure))
                } else {
                    (0.0, 0.0)
                };
                notes.push(DisplayNote {
                    note,
                    subs,
                    display_start,
                    display_end,
                    value_offset: *offset_rows,
                });
            }
        }

        notes.sort_by(|a, b| {
            let ka = a.note.start_row - a.value_offset;
            let kb = b.note.start_row - b.value_offset;
            ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
        });
        columns.push(notes);
    }

    if state.last_synth_env.len() < visible_cols {
        state.last_synth_env.resize_with(visible_cols, Vec::new);
    }
    let mut envelopes: Vec<Vec<(f32, f32)>> = Vec::with_capacity(visible_cols);
    for col in 0..visible_cols {
        let path = &path_order[col];
        let Some((name, _)) = by_path[path].first() else {
            envelopes.push(vec![(0.0, 0.0); pixel_rows]);
            continue;
        };
        if let Some(sample) = p.sample_for_part(name) {
            envelopes.push(whole_sample_envelope(&sample, pixel_rows));
        } else {
            let env = p.iid_for_part(name).and_then(|iid| {
                scope_snapshot(iid as usize, &mut state.snapshot);
                if state.snapshot.is_empty() {
                    None
                } else {
                    let e = synth_envelope_live(&state.snapshot, pixel_rows);
                    if e.is_empty() { None } else { Some(e) }
                }
            });
            match env {
                Some(env) => {
                    state.last_synth_env[col] = env.clone();
                    envelopes.push(env);
                }
                None => {
                    let cached = &state.last_synth_env[col];
                    if cached.len() == pixel_rows {
                        envelopes.push(cached.clone());
                    } else {
                        envelopes.push(synth_envelope_static(p, name, pixel_rows));
                    }
                }
            }
        }
    }

    let mut playhead_pys: Vec<Option<usize>> = Vec::with_capacity(visible_cols);
    for col in 0..visible_cols {
        let path = &path_order[col];
        let Some((name, _)) = by_path[path].first() else {
            playhead_pys.push(None);
            continue;
        };
        let Some(sample) = p.sample_for_part(name) else {
            playhead_pys.push(None);
            continue;
        };
        let n = sample.left.len();
        if n == 0 {
            playhead_pys.push(None);
            continue;
        }
        let nf = n as f64;
        let loop_pts = sample.loop_pts;
        let sample_rate_f = sample.rate;

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
    }

    let wave_fg = Color::Rgb(200, 200, 200);

    let mut header: Vec<Span> = Vec::new();
    for col in 0..visible_cols {
        let path = &path_order[col];
        let label = by_path[path]
            .first()
            .map(|(n, _)| n.clone())
            .unwrap_or_else(|| "(?)".to_string());
        let mut l = label;
        if l.len() >= col_w {
            if col_w > 1 {
                l.truncate(col_w - 1);
                l.push('…');
            } else {
                l.truncate(col_w);
            }
        }
        header.push(Span::styled(
            format!("{:<width$}", l, width = col_w),
                Style::default().add_modifier(Modifier::BOLD),
        ));
        header.push(Span::raw(" "));
    }
    let mut lines: Vec<Line> = Vec::with_capacity(char_rows + 1);
    lines.push(Line::from(header));

    for r in 0..char_rows {
        let mut spans: Vec<Span> = Vec::new();
        for col in 0..visible_cols {
            let playhead_char: Option<usize> = playhead_pys[col].map(|py| py / 4);

            for cx in 0..col_w {
                let mut bits: u8 = 0;
                for ly in 0..4 {
                    let py = r * 4 + ly;
                    if py >= pixel_rows { continue; }
                    let (lo, hi) = envelopes[col].get(py).copied().unwrap_or((0.0, 0.0));
                    let half = (col_w * 2) as f32 / 2.0;
                    let center = half;
                    let px_lo = (center + lo * half).round() as isize;
                    let px_hi = (center + hi * half).round() as isize;
                    let px_lo = px_lo.max(0) as usize;
                    let px_hi = (px_hi.min((col_w * 2 - 1) as isize)).max(px_lo as isize) as usize;
                    let cell_x0 = cx * 2;
                    let cell_x1 = cx * 2 + 1;
                    if (px_lo..=px_hi).contains(&cell_x0) { bits |= dot_bit(0, ly); }
                    if (px_lo..=px_hi).contains(&cell_x1) { bits |= dot_bit(1, ly); }
                }

                let cell_text: Option<(String, Style)> = columns[col].get(r).map(|dn| {
                    let note_local_row = dn.value_offset + current_row;
                    // For notes that haven't started yet, clamp the
                    // lookup row to the note's own start.  Without this,
                    // `value_at` returns `None` before the first
                    // waypoint and the pitch/volume/pan fields never
                    // render — you'd only see position and duration on
                    // unplayed notes.
                    let lookup_row = note_local_row.max(dn.note.start_row);
                    let text = fmt_note(&dn.note, lookup_row, dn.subs);
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
                    let pitch = value_at(&dn.note.pitch_points, lookup_row).unwrap_or(440.0);
                    let vol = value_at(&dn.note.vol_points, lookup_row).unwrap_or(100.0);
                    (text, note_style(pitch, vol, state))
                });

                let text_char: Option<char> = cell_text
                .as_ref()
                .and_then(|(t, _)| t.chars().nth(cx));

                let under_playhead = playhead_char == Some(r);

                if let Some(ch) = text_char {
                    let mut style = cell_text.as_ref().unwrap().1;
                    if under_playhead {
                        style = style.bg(Color::Yellow).fg(Color::Black);
                    }
                    spans.push(Span::styled(ch.to_string(), style));
                } else if bits != 0 {
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
    let speed = tp.speed.load(Ordering::Relaxed);

    let (state_str, state_col) = if done {
        ("finished", Color::DarkGray)
    } else if speed == 0 {
        ("stopped", Color::DarkGray)
    } else if speed > 0 {
        ("forward", Color::Green)
    } else {
        ("reverse", Color::Cyan)
    };
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
        Line::from(Span::styled("← →    reverse / forward", Style::default().fg(Color::DarkGray))),
        Line::from(Span::styled("hold   accelerate", Style::default().fg(Color::DarkGray))),
        Line::from(Span::styled("home   rewind", Style::default().fg(Color::DarkGray))),
        Line::from(Span::styled("q      quit", Style::default().fg(Color::DarkGray))),
    ];
    f.render_widget(
        Paragraph::new(text).block(Block::default().borders(Borders::ALL).title(" Live ")),
                    area,
    );
}

// ── transport (DSi-style two buttons) ──────────────────────────────────

fn draw_transport(f: &mut Frame<'_>, area: Rect, tp: &Transport) {
    let speed = tp.speed.load(Ordering::Relaxed);
    let chunks = Layout::default()
    .direction(Direction::Horizontal)
    .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
    .split(area);

    let rev_active = speed < 0;
    let rev_label = if rev_active {
        format!("■   stop    {}x", -speed)
    } else {
        "◀   rewind".to_string()
    };
    let rev_style = if rev_active {
        Style::default()
        .fg(Color::Black)
        .bg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    let rev_block = Block::default()
    .borders(Borders::ALL)
    .title(" left ");
    let rev = Paragraph::new(rev_label)
    .alignment(Alignment::Center)
    .block(rev_block)
    .style(rev_style);
    f.render_widget(rev, chunks[0]);

    let fwd_active = speed > 0;
    let fwd_label = if fwd_active {
        format!("■   stop    {}x", speed)
    } else {
        "▶   play".to_string()
    };
    let fwd_style = if fwd_active {
        Style::default()
        .fg(Color::Black)
        .bg(Color::Green)
        .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    let fwd_block = Block::default()
    .borders(Borders::ALL)
    .title(" right ");
    let fwd = Paragraph::new(fwd_label)
    .alignment(Alignment::Center)
    .block(fwd_block)
    .style(fwd_style);
    f.render_widget(fwd, chunks[1]);
}