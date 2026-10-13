//! Vertical tracker layout for tall/narrow terminals.
//!
//! Reads like the trackscript source rather than a tracker grid.  Each
//! part is a paragraph:
//!
//!   * a name line
//!   * one or more event lines, in source syntax (`<§1/1,#C5,~1>`),
//!     wrapped at the terminal edge
//!   * a multi-row braille waveform of the part's sample, full width
//!   * a blank gap
//!
//! Nothing is time-aligned.  The horizontal view splits parts into
//! separate columns so they can be compared beat-by-beat; this view
//! doesn't need that, because you read one part at a time.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::player::{Project, Sample, Transport};
use crate::tui::envelope::whole_sample_envelope;
use crate::tui::mute;
use crate::tui::notes::{fmt_num, fmt_pitch, split_notes, value_at, DisplayNote};
use crate::tui::scope::scope_snapshot;
use crate::tui::style::{braille_char, dot_bit, note_style, NoteState};
use crate::tui::{theme, RenderState};

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;

const PLAYHEAD_LOOKAHEAD_SECS: f64 = 0.05;
/// Rows of braille allocated to a part's waveform.  4 pixels of
/// amplitude per row.
const WAVE_ROWS: usize = 3;
/// Blank lines between parts.
const PART_GAP: usize = 1;
/// Maximum event lines per part before truncating with `…`.
const MAX_EVENT_LINES: usize = 8;

// ── formatting ─────────────────────────────────────────────────────────

/// `§N/D` for a measure-local row.  Whole subdivisions render as
/// integers; fractional positions keep two decimals.
fn fmt_pos(row: f64, subs: u32) -> String {
    let subs_f = subs.max(1) as f64;
    let beat = (row / subs_f).floor() as i64 + 1;
    let sub = row - subs_f * (beat as f64 - 1.0) + 1.0;
    if (sub - sub.round()).abs() < 1e-6 {
        format!("§{}/{}", beat, sub.round() as i64)
    } else {
        format!("§{}/{}", beat, fmt_num(sub))
    }
}

/// One event in trackscript source syntax.  Fields are omitted when
/// absent from the event.
fn format_event(dn: &DisplayNote, subs: u32) -> String {
    let ev = &dn.note;
    let mut parts: Vec<String> = Vec::with_capacity(4);
    parts.push(fmt_pos(ev.start_row, subs));
    if let Some(hz) = value_at(&ev.pitch_points, ev.start_row) {
        parts.push(format!("#{}", fmt_pitch(hz)));
    }
    if let Some(d) = ev.dur_beats {
        parts.push(format!("~{}", fmt_num(d)));
    }
    format!("<{}>", parts.join(","))
}

// ── waveform ───────────────────────────────────────────────────────────

/// Render a sample into a `w × h` grid of braille cells plus styles.
/// One (lo, hi) envelope value per horizontal braille pixel (2 per
/// terminal cell).  Every braille dot whose vertical position falls
/// inside the amplitude range is lit.
fn render_waveform(
    sample: &Sample,
    w: usize,
    h: usize,
    base_style: Style,
    playhead_col: Option<usize>,
    playhead_style: Style,
) -> Vec<Vec<Span<'static>>> {
    let px_w = w * 2;
    let env = whole_sample_envelope(sample, px_w);
    let total_dots = (4 * h).max(1);

    let mut out: Vec<Vec<Span<'static>>> = Vec::with_capacity(h);
    for y in 0..h {
        let mut row: Vec<Span<'static>> = Vec::with_capacity(w);
        for x in 0..w {
            let i0 = 2 * x;
            let i1 = 2 * x + 1;
            let (lo0, hi0) = env.get(i0).copied().unwrap_or((0.0, 0.0));
            let (lo1, hi1) = env.get(i1).copied().unwrap_or((0.0, 0.0));

            let lit = |lo: f32, hi: f32, frac: f64| -> bool {
                let top = (1.0 - hi as f64) / 2.0;
                let bot = (1.0 - lo as f64) / 2.0;
                frac >= top && frac <= bot
            };

            let mut bits = 0u8;
            for py in 0..4 {
                let dot = y * 4 + py;
                let frac = dot as f64 / total_dots as f64;
                if lit(lo0, hi0, frac) {
                    bits |= dot_bit(0, py);
                }
                if lit(lo1, hi1, frac) {
                    bits |= dot_bit(1, py);
                }
            }

            let ch = if bits == 0 { ' ' } else { braille_char(bits) };
            let style = if Some(x) == playhead_col {
                playhead_style
            } else {
                base_style
            };
            row.push(Span::styled(ch.to_string(), style));
        }
        out.push(row);
    }
    out
}

/// Same shape as `render_waveform`, but from a live scope snapshot
/// (for synth parts with no sample file).
fn synth_waveform(
    snapshot: &[f32],
    w: usize,
    h: usize,
    base_style: Style,
    playhead_col: Option<usize>,
    playhead_style: Style,
) -> Vec<Vec<Span<'static>>> {
    let n = snapshot.len();
    if n == 0 {
        return vec![
            vec![Span::styled(" ".to_string(), base_style); w];
            h
        ];
    }
    let px_w = w * 2;
    let total_dots = (4 * h).max(1);

    let mut out: Vec<Vec<Span<'static>>> = Vec::with_capacity(h);
    for y in 0..h {
        let mut row: Vec<Span<'static>> = Vec::with_capacity(w);
        for x in 0..w {
            let mut bits = 0u8;
            for px in [2 * x, 2 * x + 1] {
                let a = ((px as f64 / px_w as f64) * n as f64) as usize;
                let b = ((((px + 1) as f64) / px_w as f64) * n as f64) as usize;
                let a = a.min(n.saturating_sub(1));
                let b = b.max(a + 1).min(n);
                let mut lo = f32::INFINITY;
                let mut hi = f32::NEG_INFINITY;
                for &s in &snapshot[a..b] {
                    if s < lo { lo = s; }
                    if s > hi { hi = s; }
                }
                if !lo.is_finite() { continue; }
                let top = (1.0 - hi as f64) / 2.0;
                let bot = (1.0 - lo as f64) / 2.0;
                for py in 0..4 {
                    let dot = y * 4 + py;
                    let frac = dot as f64 / total_dots as f64;
                    if frac >= top && frac <= bot {
                        let bit = if px == 2 * x { 0 } else { 1 };
                        bits |= dot_bit(bit, py);
                    }
                }
            }
            let ch = if bits == 0 { ' ' } else { braille_char(bits) };
            let style = if Some(x) == playhead_col {
                playhead_style
            } else {
                base_style
            };
            row.push(Span::styled(ch.to_string(), style));
        }
        out.push(row);
    }
    out
}

// ── main entry ─────────────────────────────────────────────────────────

pub fn draw_tracker_vertical(
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
    let outer = theme::block().title(title);

    let Some(li) = current else {
        f.render_widget(Paragraph::new("(silence)").block(outer), area);
        state.tracker_hit = None;
        return;
    };

    // ── parts on this line ────────────────────────────────────────
    //
    // Same (name, offset, measure_start, measure_len) shape as the
    // horizontal view, but the vertical view ignores offset_rows: it
    // only renders the current measure.
    let mut by_path: HashMap<String, Vec<(String, f64, f64, f64)>> = HashMap::new();
    let mut present: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for call in p.line_calls(li) {
        let Some(path) = p.part_path(&call.name) else { continue };
        let key = path.to_string();
        if seen.insert(key.clone()) {
            present.push(key.clone());
        }
        by_path.entry(key).or_default().push((
            call.name.clone(),
            0.0,
            call.pattern_measure_start,
            call.pattern_measure_len,
        ));
    }

    // Preserve column_order from the previous frame so parts don't
    // jump around when the arrangement changes.
    let present_set: HashSet<&String> = present.iter().collect();
    let mut path_order: Vec<String> = state
        .column_order
        .iter()
        .filter(|path| present_set.contains(path))
        .cloned()
        .collect();
    for path in &present {
        if !path_order.iter().any(|m| m == path) {
            path_order.push(path.clone());
        }
    }
    state.column_order = path_order.clone();

    if path_order.is_empty() {
        f.render_widget(Paragraph::new("(silence)").block(outer), area);
        state.tracker_hit = None;
        return;
    }

    let inner = outer.inner(area);
    let inner_w = inner.width as usize;
    let inner_h = inner.height as usize;
    if inner_w < 16 || inner_h < 4 {
        f.render_widget(outer, area);
        state.tracker_hit = None;
        return;
    }

    let rpmeasure = p.rows_per_measure().max(1.0);
    let measure_samples = p.line_measure_samples(li).max(1);
    let line_start = p.line_start_sample(li);
    let elapsed = (pos - line_start).max(0);
    let measure_frac =
        (elapsed as f64 / measure_samples as f64).clamp(0.0, 1.0);
    let current_row = measure_frac * rpmeasure;
    let playhead_col = ((measure_frac * inner_w as f64).floor() as usize)
        .min(inner_w.saturating_sub(1));

    let (muted_parts, soloed_parts, _) = mute::snapshot();
    let solo_active = !soloed_parts.is_empty();

    // ── emit lines ────────────────────────────────────────────────
    let mut lines: Vec<Line> = Vec::with_capacity(inner_h);
    let mut remaining = inner_h;

    for path in &path_order {
        if remaining < WAVE_ROWS + 2 {
            break;
        }

        let Some(sources) = by_path.get(path) else { continue };
        if sources.is_empty() { continue; }
        let first_name = sources[0].0.clone();

        let is_muted_col = muted_parts.contains(path);
        let is_soloed_col = soloed_parts.contains(path);
        let solo_hides_col = solo_active && !is_soloed_col;
        let col_off = is_muted_col || solo_hides_col;

        // ── label line ─────────────────────────────────────────────
        let mut label = first_name.clone();
        if label.chars().count() > inner_w {
            label = label.chars().take(inner_w.saturating_sub(1)).collect();
            label.push('…');
        }
        let label_style = if is_soloed_col {
            Style::default().fg(theme::SOLOED_FG).add_modifier(Modifier::BOLD)
        } else if col_off {
            Style::default().fg(theme::MUTED_FG).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::FOCUSED_FG).add_modifier(Modifier::BOLD)
        };

        // The sample's basename goes dimly after the part name.
        let sample_note = p
            .sample_for_part(&first_name)
            .map(|_| "")
            .unwrap_or("");
        let _ = sample_note;

        lines.push(Line::from(Span::styled(label, label_style)));
        remaining -= 1;

        // ── gather measure-local notes for this part ───────────────
        struct Staged {
            dn: DisplayNote,
            text: String,
            style: Style,
        }
        let mut staged: Vec<Staged> = Vec::new();

        for (name, _offset, measure_start, measure_len) in sources {
            let subs = p.part_subs_per_beat(name);
            let measure_end = measure_start + measure_len;
            for mut note in split_notes(
                p.part_events(name),
                subs,
                p.part_sample_offset_u32(name),
            ) {
                if note.start_row < *measure_start || note.start_row >= measure_end {
                    continue;
                }
                let ms = *measure_start;
                note.start_row -= ms;
                note.end_row -= ms;
                for pt in &mut note.pitch_points { pt.row -= ms; }
                for pt in &mut note.vol_points   { pt.row -= ms; }
                for pt in &mut note.pan_points   { pt.row -= ms; }

                let dn = DisplayNote {
                    note,
                    subs,
                    display_start: 0.0,
                    display_end: 0.0,
                    value_offset: 0.0,
                    source_name: name.clone(),
                };
                let text = format_event(&dn, subs);

                let playing = dn.note.start_row <= current_row
                    && current_row < dn.note.end_row;
                let played = !playing && current_row >= dn.note.end_row;
                let state_note = if playing {
                    NoteState::Playing
                } else if played {
                    NoteState::Played
                } else if dn.note.start_row >= rpmeasure {
                    NoteState::Future
                } else {
                    NoteState::Waiting
                };
                let pitch =
                    value_at(&dn.note.pitch_points, dn.note.start_row).unwrap_or(440.0);
                let vol =
                    value_at(&dn.note.vol_points, dn.note.start_row).unwrap_or(100.0);
                let style = if col_off {
                    Style::default().fg(theme::MUTED_FG).bg(theme::MUTED_BG)
                } else {
                    note_style(pitch, vol, state_note)
                };
                staged.push(Staged { dn, text, style });
            }
        }

        staged.sort_by(|a, b| {
            a.dn.note
                .start_row
                .partial_cmp(&b.dn.note.start_row)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // ── wrap events into terminal lines ────────────────────────
        let mut event_lines: Vec<Line> = Vec::new();
        let mut spans: Vec<Span> = Vec::new();
        let mut width = 0usize;
        let mut truncated = false;

        for st in &staged {
            let tw = st.text.chars().count();
            let need = tw + 1;

            if width > 0 && width + need > inner_w {
                event_lines.push(Line::from(std::mem::take(&mut spans)));
                width = 0;
                if event_lines.len() >= MAX_EVENT_LINES {
                    truncated = true;
                    break;
                }
            }
            spans.push(Span::styled(st.text.clone(), st.style));
            spans.push(Span::raw(" "));
            width += need;
        }
        if !spans.is_empty() && event_lines.len() < MAX_EVENT_LINES {
            event_lines.push(Line::from(spans));
        }
        if truncated && event_lines.len() < MAX_EVENT_LINES {
            if let Some(last) = event_lines.last_mut() {
                last.spans.push(Span::styled(
                    "…".to_string(),
                    Style::default().fg(theme::MUTED_FG),
                ));
            }
        }

        for el in event_lines {
            if remaining <= WAVE_ROWS + PART_GAP {
                break;
            }
            lines.push(el);
            remaining -= 1;
        }

        // ── waveform ───────────────────────────────────────────────
        let base_style = if col_off {
            Style::default().fg(theme::MUTED_FG).bg(theme::MUTED_BG)
        } else {
            Style::default().fg(Color::Rgb(180, 180, 180))
        };
        let ph_style = if col_off {
            Style::default().fg(theme::MUTED_FG).bg(theme::MUTED_BG)
        } else {
            Style::default().fg(theme::PLAYHEAD_FG)
        };
        let ph_col = if col_off { None } else { Some(playhead_col) };

        let wave: Vec<Vec<Span<'static>>> =
            if let Some(sample) = p.sample_for_part(&first_name) {
                render_waveform(&sample, inner_w, WAVE_ROWS, base_style, ph_col, ph_style)
            } else if let Some(iid) = p.iid_for_part(&first_name) {
                scope_snapshot(iid as usize, &mut state.snapshot);
                if state.snapshot.is_empty() {
                    vec![
                        vec![Span::styled(" ".to_string(), base_style); inner_w];
                        WAVE_ROWS
                    ]
                } else {
                    synth_waveform(
                        &state.snapshot,
                        inner_w,
                        WAVE_ROWS,
                        base_style,
                        ph_col,
                        ph_style,
                    )
                }
            } else {
                vec![
                    vec![Span::styled(" ".to_string(), base_style); inner_w];
                    WAVE_ROWS
                ]
            };

        for row in wave {
            if remaining == 0 { break; }
            lines.push(Line::from(row));
            remaining -= 1;
        }

        // ── gap ────────────────────────────────────────────────────
        if remaining > 0 && Some(path) != path_order.last() {
            lines.push(Line::from(""));
            remaining -= 1;
        }
    }

    f.render_widget(Paragraph::new(lines).block(outer), area);
    state.tracker_hit = None;
}