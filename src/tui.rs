//! Ratatui front-end for live playback.
//!
//! Tracker view with synesthetic coloring:
//!   - pitch class (C, C#, D…) maps to hue on a 12-step color wheel
//!   - volume maps to saturation (quiet = pale, loud = vivid)
//!   - playing notes invert: background takes the note's color
//!   - non-playing notes show the same color as foreground, dimmed
//!
//! Carried notes (from earlier lines that are still sounding) are shown
//! in the column at the same blocklist index as the part they came from,
//! stacked above the current line's notes.
//!
//! A carried note persists as long as it's either still inside its own
//! [start_row, end_row) window, OR it's a sustained note whose volume
//! hasn't reached zero.  Short percussive hits drop as soon as their
//! window passes; long drones and slides keep showing.

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
    widgets::{Block, Borders, Cell, Gauge, Paragraph, Row, Table},
    Frame, Terminal,
};
use std::io::{self, Stdout};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use crate::player::{Project, Transport};
use crate::sources::trackscript::{hz_to_note, TrackscriptEvent};

const SEEK_STEP_SECS: f64 = 5.0;
const TICK_MS: u64 = 50;
const LOOKBACK_LINES: usize = 128;

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
    let term = Terminal::new(CrosstermBackend::new(out))?;
    Ok(term)
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
                          Span::styled(
                              p.title.clone(),
                                       Style::default().add_modifier(Modifier::BOLD),
                          ),
                          Span::raw(format!(
                              "    {} BPM    {}/{}    {} voices    {} lines",
                              p.initial_bpm.round() as i64,
                                            num,
                                            den,
                                            p.voice_count,
                                            p.line_count()
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

struct Point {
    row: f64,
    value: f64,
    ramp_rows: f64,
}

struct NoteRow {
    start_row: f64,
    end_row: f64,
    dur_beats: Option<f64>,
    pitch_points: Vec<Point>,
    vol_points: Vec<Point>,
    pan_points: Vec<Point>,
}

struct DisplayNote {
    note: NoteRow,
    /// Row in the current measure's coordinate space at which this note
    /// becomes highlightable, and at which it stops being highlightable.
    /// A note with display_end == 0 never highlights (used for notes that
    /// have already ended but whose volume hasn't reached zero).
    display_start: f64,
    display_end: f64,
    /// Rows to add to `current_row` when evaluating the note's automation.
    /// Zero for notes starting on this line; `k * rpmeasure` for carried
    /// notes from k lines back.
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
                if wp_end > n.end_row {
                    n.end_row = wp_end;
                }
            }
        } else {
            let dur_beats = ev.dur_beats.filter(|d| *d > 0.0).unwrap_or(1.0);
            let end_row = ev.row + dur_beats * subs_f;
            let mut nr = NoteRow {
                start_row: ev.row,
                end_row,
                dur_beats: ev.dur_beats,
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
            if let Some(c) = cur {
                prev = Some(c.value);
            }
            cur = Some(p);
        } else {
            break;
        }
    }
    let p = cur?;
    if p.ramp_rows <= 0.0 || row >= p.row + p.ramp_rows {
        return Some(p.value);
    }
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
    if s.is_empty() { "0".to_string() } else { s.to_string() }
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
    if let Some(d) = n.dur_beats {
        parts.push(format!("~{}", fmt_num(d)));
    }
    if let Some(v) = value_at(&n.vol_points, row) {
        parts.push(format!("%{}", fmt_num(v)));
    }
    if let Some(p) = value_at(&n.pan_points, row) {
        if p.abs() > 0.01 {
            parts.push(format!("¶{}", fmt_num(p)));
        }
    }
    format!("<{}>", parts.join(","))
}

// ── tracker ────────────────────────────────────────────────────────────

fn draw_tracker(f: &mut Frame<'_>, area: Rect, tp: &Transport, p: &Project) {
    let pos = tp.position.load(Ordering::Relaxed);
    let current = p.current_line(pos);

    let title = match current {
        Some(li) => format!(" BlockList line {} ", li + 1),
        None => " BlockList (idle) ".to_string(),
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

    let rpmeasure = p.rows_per_measure();

    let mut columns: Vec<Vec<DisplayNote>> = Vec::with_capacity(parts.len());
    for (i, name) in parts.iter().enumerate() {
        let subs = p.part_subs_per_beat(name);
        let mut notes: Vec<DisplayNote> = Vec::new();

        // Carried notes from the part at the same blocklist index on
        // earlier lines.
        let lookback = LOOKBACK_LINES.min(li);
        for k in 1..=lookback {
            let prev_li = li - k;
            let prev_parts = p.line_parts(prev_li);
            let Some(prev_name) = prev_parts.get(i) else { continue };
            let prev_subs = p.part_subs_per_beat(prev_name);
            let offset_rows = k as f64 * rpmeasure;

            for note in split_notes(p.part_events(prev_name), prev_subs) {
                let active = note.end_row > offset_rows;

                // A note whose window has passed is kept only if it's a
                // sustained note whose volume hasn't hit zero.  Short
                // percussive hits drop immediately.  Notes with no volume
                // waypoints at all drop immediately (their effective level
                // is constant, so if the window ended the note is done).
                let dur_beats = note.dur_beats.unwrap_or(1.0);
                let is_sustained = dur_beats >= 2.0;
                let end_vol = value_at(&note.vol_points, note.end_row).unwrap_or(0.0);
                let still_fading = is_sustained && end_vol > 0.01;

                if !active && !still_fading {
                    continue;
                }

                let display_end = if active {
                    (note.end_row - offset_rows).min(rpmeasure)
                } else {
                    0.0
                };

                notes.push(DisplayNote {
                    note,
                    display_start: 0.0,
                    display_end,
                    value_offset: offset_rows,
                });
            }
        }

        // Current line's notes.
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

        // Carried notes first (display_start == 0), more recent carry
        // first; then current-line notes by start_row.
        notes.sort_by(|a, b| {
            a.display_start
            .partial_cmp(&b.display_start)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                a.value_offset
                .partial_cmp(&b.value_offset)
                .unwrap_or(std::cmp::Ordering::Equal)
            })
        });

        columns.push(notes);
    }

    // Playhead row within this line.
    let current_row = {
        let line_start = p.line_start_sample(li);
        let measure_samples = p.line_measure_samples(li).max(1);
        let elapsed = (pos - line_start).max(0);
        let frac = (elapsed as f64 / measure_samples as f64).clamp(0.0, 1.0);
        frac * rpmeasure
    };

    let height = columns.iter().map(|c| c.len()).max().unwrap_or(0);

    let mut rows: Vec<Row> = Vec::with_capacity(height);
    for r in 0..height {
        let mut cells: Vec<Cell> = Vec::with_capacity(parts.len());
        for col in 0..parts.len() {
            if let Some(dn) = columns[col].get(r) {
                let subs = p.part_subs_per_beat(&parts[col]);
                let note_local_row = dn.value_offset + current_row;
                let text = fmt_note(&dn.note, note_local_row, subs);

                // display_end == 0 means "already ended; never highlight".
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
                let style = note_style(pitch, vol, state);
                cells.push(Cell::from(text).style(style));
            } else {
                cells.push(Cell::from(""));
            }
        }
        rows.push(Row::new(cells));
    }

    let header: Vec<Cell> = parts.iter().map(|n| Cell::from(n.clone())).collect();
    let widths: Vec<Constraint> = parts.iter().map(|_| Constraint::Length(30)).collect();

    let table = Table::new(rows, widths)
    .header(Row::new(header).style(Style::default().add_modifier(Modifier::BOLD)))
    .column_spacing(1)
    .block(outer);
    f.render_widget(table, area);
}

fn draw_live_panel(f: &mut Frame<'_>, area: Rect, tp: &Transport, p: &Project) {
    let pos = tp.position.load(Ordering::Relaxed);
    let active = tp.active_voices.load(Ordering::Relaxed);
    let loaded = tp.loaded_lines.load(Ordering::Relaxed);
    let done = tp.done.load(Ordering::Relaxed);
    let paused = tp.paused.load(Ordering::Relaxed);

    let state_str = if done {
        "finished"
    } else if paused {
        "paused"
    } else {
        "playing"
    };
    let state_col = if done {
        Color::DarkGray
    } else if paused {
        Color::Yellow
    } else {
        Color::Green
    };

    let now_line = p
    .current_line(pos)
    .map(|i| format!("{}", i + 1))
    .unwrap_or_else(|| "—".into());

    let legend_spans: Vec<Span> = (0..12)
    .flat_map(|class| {
        let (r, g, b) = hsv_to_rgb(class as f64 * 30.0, 0.9, 1.0);
        vec![
            Span::styled("  ", Style::default().bg(Color::Rgb(r, g, b))),
              Span::raw(" "),
        ]
    })
    .collect();

    let text = vec![
        Line::from(vec![
            Span::styled("state:          ", Style::default().fg(Color::DarkGray)),
                   Span::styled(state_str, Style::default().fg(state_col)),
        ]),
        Line::from(format!("now on line:    {}", now_line)),
        Line::from(format!("active voices:  {}", active)),
        Line::from(format!("loader cursor:  {}", loaded)),
        Line::from(""),
        Line::from(Span::styled(
            "C  C# D  D# E  F  F# G  G# A  A# B",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(legend_spans),
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
    let ratio = if total_secs > 0.0 {
        (secs / total_secs).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let paused = tp.paused.load(Ordering::Relaxed);
    let label = format!(
        "{}  {:>6.1}s / {:.1}s",
        if paused { "❚❚" } else { "▶" },
            secs,
            total_secs
    );
    let gauge = Gauge::default()
    .block(Block::default().borders(Borders::ALL))
    .gauge_style(Style::default().fg(if paused {
        Color::Yellow
    } else {
        Color::Green
    }))
    .ratio(ratio)
    .label(label);
    f.render_widget(gauge, area);
}
