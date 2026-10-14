//! All rendering.  The main `draw` splits the frame into header /
//! tracker and dispatches to the individual painters.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::tui::cover::{Cover, COVER_DIM};
use crate::tui::envelope::{
    synth_envelope_live, synth_envelope_static, whole_sample_envelope, wrapped_pos,
};
use crate::tui::mute;
use crate::tui::notes::{fmt_note, split_notes, value_at, ColumnLine, DisplayNote};
use crate::tui::queue;
use crate::tui::scope::scope_snapshot;
use crate::tui::spiral::{build_spiral, SpiralLayout, SpiralState, SPIRAL_MAX_H};
use crate::tui::style::{braille_char, dot_bit, note_style, NoteState};
use crate::tui::{theme, RenderState, TrackerColumnHit, TrackerHit};
use crate::player::{Project, Transport};
use crate::sources::trackscript::C5_HZ;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

const LOOKBACK_LINES: usize = 128;
const MIN_COL_W: usize = 10;
const MAX_COL_W: usize = 40;
const PLAYHEAD_LOOKAHEAD_SECS: f64 = 0.05;
/// How long the "reloaded" flash stays on screen after a successful
/// reload.  Long enough to register, short enough not to linger.
const RELOAD_FLASH_MS: u64 = 1500;

pub fn draw(
    f: &mut Frame<'_>,
    tp: &Transport,
    project: &Project,
    sample_rate: u32,
    state: &mut RenderState,
) {
    let area = f.area();

    // Poll the reload counter.  When it ticks past the last value we
    // saw, start the flash timer (RELOAD_FLASH_MS).  Nothing displays
    // the flash since the footer went away.
    let rc = tp.reload_count.load(Ordering::Relaxed);
    if rc > state.player.last_reload_count {
        state.player.last_reload_count = rc;
        state.player.reload_flash_until =
            Some(Instant::now() + Duration::from_millis(RELOAD_FLASH_MS));
    }

    // ── Cover image ──────────────────────────────────────────────────
    let cover_dims = (area.width as usize, area.height as usize);
    if state.cover_source.as_deref() != project.cover_path() || state.cover_dims != cover_dims {
        state.cover = project
            .cover_path()
            .and_then(|path| Cover::load(path, cover_dims.0, cover_dims.1, COVER_DIM));
        state.cover_source = project.cover_path().map(Path::to_path_buf);
        state.cover_dims = cover_dims;
    }

    let total_measures = project.line_count();
    let spiral_layout = build_spiral(total_measures, queue::VISIBLE_LINES.min(SPIRAL_MAX_H));

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(6)])
        .split(area);

    draw_header(f, chunks[0], project, tp, sample_rate, state.loading.as_deref());
    if area.height as f32 * 2.0 > area.width as f32 {
        crate::tui::draw_vertical::draw_tracker_vertical(
            f, chunks[1], tp, project, state,
        );
    } else {
        draw_tracker(f, chunks[1], tp, project, state);
    }

    // ── Mosaic background pass ──────────────────────────────────────
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

    // ── Queue box + spiral ──────────────────────────────────────────
    let spd = tp.speed.load(Ordering::Relaxed);
    state.queue.direction = match spd.signum() {
        1 => '▶',
        -1 => '◀',
        _ => '■',
    };
    state.queue.speed_label = format!("{}x", spd.abs().max(1));

    let spiral_size = (spiral_layout.grid_w.max(1), spiral_layout.grid_h);
    let placed = queue::render(
        &state.queue,
        chunks[1],
        f.buffer_mut(),
        &queue::Theme::default(),
        spiral_size,
    );
    state.queue_placed = placed;
    match placed {
        Some(p) => draw_spiral(f, p.spiral, tp, project, state, spiral_layout),
        None => state.spiral = None,
    }

    // ── Help overlay ────────────────────────────────────────────────
    if state.player.show_help {
        draw_help(f, area);
    }
}

fn draw_header(
    f: &mut Frame<'_>,
    area: Rect,
    p: &Project,
    tp: &Transport,
    sr: u32,
    loading: Option<&str>,
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
    let title = loading.unwrap_or(&p.title);
    let line = Line::from(vec![
        Span::styled(" 🎵 ", Style::default().fg(Color::Cyan)),
        Span::styled(title.to_string(), Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(format!(
            "    {} BPM    {}/{}    {} voices    {} lines    {}  {:>5.1}s / {:.1}s",
            p.initial_bpm.round() as i64,
            num, den, p.voice_count, p.line_count(),
            speed_tag, secs, total
        )),
    ]);
    f.render_widget(Paragraph::new(line).block(theme::block()), area);
}

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
    let outer = theme::block().title(title);

    let Some(li) = current else {
        f.render_widget(Paragraph::new("(silence)").block(outer), area);
        state.tracker_hit = None;
        return;
    };

    let rpmeasure = p.rows_per_measure();
    let lookback = LOOKBACK_LINES.min(li);

    let mut by_path: HashMap<String, Vec<(String, f64, f64, f64)>> = HashMap::new();
    let mut discovery: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for call in p.line_calls(li) {
        let Some(path) = p.part_path(&call.name) else { continue };
        let key = path.to_string();
        if seen.insert(key.clone()) {
            discovery.push(key.clone());
        }
        by_path.entry(key).or_default().push((
            call.name.clone(),
            0.0,
            call.pattern_measure_start,
            call.pattern_measure_len,
        ));
    }

    for k in 1..=lookback {
        let prev_li = li - k;
        let offset_rows = k as f64 * rpmeasure;
        for call in p.line_calls(prev_li) {
            let Some(path) = p.part_path(&call.name) else { continue };
            let subs = p.part_subs_per_beat(&call.name);
            let measure_end = call.pattern_measure_start + call.pattern_measure_len;
            let still_on = split_notes(
                p.part_events(&call.name),
                subs,
                p.part_sample_offset_u32(&call.name),
            )
            .iter()
            .any(|n| {
                n.start_row >= call.pattern_measure_start
                    && n.start_row < measure_end
                    && (n.end_row - call.pattern_measure_start) > offset_rows
            });
            if !still_on {
                continue;
            }
            let key = path.to_string();
            if seen.insert(key.clone()) {
                discovery.push(key.clone());
            }
            by_path.entry(key).or_default().push((
                call.name.clone(),
                offset_rows,
                call.pattern_measure_start,
                call.pattern_measure_len,
            ));
        }
    }

    if discovery.is_empty() {
        f.render_widget(Paragraph::new("(silence)").block(outer), area);
        state.tracker_hit = None;
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
        state.tracker_hit = None;
        return;
    }

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
    let line_start = p.line_start_sample(li);
    let elapsed = (pos - line_start).max(0);
    let playhead_frac = (elapsed as f64 / measure_samples as f64).clamp(0.0, 1.0);
    let current_row = playhead_frac * rpmeasure;

    let header_names: Vec<Option<String>> = path_order
        .iter()
        .map(|path| by_path[path].first().map(|(n, _, _, _)| n.clone()))
        .collect();

    let (muted_parts, soloed_parts, muted_notes) = mute::snapshot();
    let solo_active = !soloed_parts.is_empty();

    let mut columns: Vec<Vec<ColumnLine>> = Vec::with_capacity(visible_cols);
    let mut column_rows_hit: Vec<Vec<Option<(usize, i64)>>> = Vec::with_capacity(visible_cols);

    for col in 0..visible_cols {
        let path = &path_order[col];
        let sources = &by_path[path];
        let mut notes: Vec<DisplayNote> = Vec::new();
        let mut block_line_for_note: Vec<usize> = Vec::new();

        for (name, offset_rows, measure_start, measure_len) in sources {
            let subs = p.part_subs_per_beat(name);
            let is_current = *offset_rows == 0.0;
            let offset_lines = (offset_rows / rpmeasure).round() as usize;
            let note_block_line = li.saturating_sub(offset_lines);
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

                let active = note.end_row > *offset_rows;
                let has_vol_waypoints = note.vol_points.len() > 1;
                let dur_beats = note.dur_beats.unwrap_or(1.0);
                let is_sustained = dur_beats >= 2.0;
                let end_vol = value_at(&note.vol_points, note.end_row).unwrap_or(0.0);
                let still_fading = has_vol_waypoints && is_sustained && end_vol > 0.01;
                if !active && !still_fading {
                    continue;
                }
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
                    source_name: name.clone(),
                });
                block_line_for_note.push(note_block_line);
            }
        }

        let mut idx: Vec<usize> = (0..notes.len()).collect();
        idx.sort_by(|&a, &b| {
            notes[a]
                .value_offset
                .partial_cmp(&notes[b].value_offset)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    notes[a]
                        .note
                        .start_row
                        .partial_cmp(&notes[b].note.start_row)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        });
        let notes: Vec<DisplayNote> = idx.iter().map(|&i| notes[i].clone()).collect();
        let bl: Vec<usize> = idx.iter().map(|&i| block_line_for_note[i]).collect();

        let header_name = header_names[col].clone();
        let mut entries: Vec<ColumnLine> = Vec::with_capacity(notes.len() + 4);
        let mut entries_bl: Vec<Option<(usize, i64)>> = Vec::with_capacity(notes.len() + 4);
        let mut last_name: Option<String> = None;
        for (note, block_line) in notes.into_iter().zip(bl) {
            let name = note.source_name.clone();
            let at_start = last_name.is_none();
            let changed = last_name.as_deref() != Some(name.as_str());
            let is_header = header_name.as_deref() == Some(name.as_str());
            if (at_start || changed) && !is_header {
                entries.push(ColumnLine::Divider { label: name.clone() });
                entries_bl.push(None);
            }
            let start_row = note.note.start_row as i64;
            entries.push(ColumnLine::Note(note));
            entries_bl.push(Some((block_line, start_row)));
            last_name = Some(name);
        }
        columns.push(entries);
        column_rows_hit.push(entries_bl);
    }

    // Envelopes.  Preview lookup uses `preview_sample_for_part`, which
    // resolves WAV samplers through `path_to_id` and SF2 instruments
    // through the first zone's PCM slice.  VST and other kinds fall
    // through to the live-scope / static-synth branches below.
    if state.last_synth_env.len() < visible_cols {
        state.last_synth_env.resize_with(visible_cols, Vec::new);
    }
    let mut envelopes: Vec<Vec<(f32, f32)>> = Vec::with_capacity(visible_cols);
    for col in 0..visible_cols {
        let path = &path_order[col];
        let Some((name, _, _, _)) = by_path[path].first() else {
            envelopes.push(vec![(0.0, 0.0); pixel_rows]);
            continue;
        };
        if let Some(sample) = p.preview_sample_for_part(name) {
            let key = (path.clone(), pixel_rows);
            let env = state
                .sample_env_cache
                .entry(key)
                .or_insert_with(|| whole_sample_envelope(&sample, pixel_rows));
            envelopes.push(env.clone());
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

    // ── Sample-position playhead ────────────────────────────────────
    {
        let present: HashSet<&String> = path_order.iter().collect();
        state.playheads.retain(|k, _| present.contains(k));
    }

    let mut playhead_pys: Vec<Option<usize>> = Vec::with_capacity(visible_cols);
    for col in 0..visible_cols {
        let path = &path_order[col];
        let Some((name, _, _, _)) = by_path[path].first() else {
            playhead_pys.push(None);
            continue;
        };
        let Some(sample) = p.preview_sample_for_part(name) else {
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

        let active = columns[col].iter().find_map(|line| {
            let ColumnLine::Note(dn) = line else { return None };
            let local_row = dn.value_offset + current_row;
            if dn.note.start_row <= local_row && local_row < dn.note.end_row {
                Some(dn)
            } else {
                None
            }
        });

        let ph = state.playheads.entry(path.clone()).or_default();

        let Some(dn) = active else {
            ph.note_id = None;
            playhead_pys.push(None);
            continue;
        };

        let lines_back = (dn.value_offset / rpmeasure).round() as usize;
        let block_line = li.saturating_sub(lines_back);
        let block_start_sample = p.line_start_sample(block_line);

        let local_row = dn.value_offset + current_row;
        let pitch = value_at(&dn.note.pitch_points, local_row).unwrap_or(C5_HZ);
        let ratio = (pitch / C5_HZ).max(1e-6);

        let id = (dn.source_name.clone(), dn.note.start_row, block_start_sample);
        if ph.note_id.as_ref() != Some(&id) {
            ph.position = dn.note.offset as f64;
            ph.note_id = Some(id);
            ph.last_audio_pos = pos;
        } else {
            let raw_delta = pos - ph.last_audio_pos;
            let max_delta = sr as i64;
            let audio_delta_samples = raw_delta.clamp(-max_delta, max_delta);
            let audio_delta_secs = audio_delta_samples as f64 / sr;
            ph.position += audio_delta_secs * sample_rate_f * ratio;
            ph.last_audio_pos = pos;
        }

        if loop_pts.is_some() {
            ph.position = wrapped_pos(ph.position, loop_pts);
        } else {
            ph.position = ph.position.clamp(0.0, nf);
        }

        let t = (ph.position / nf).clamp(0.0, 1.0);
        let row = ((t * pixel_rows as f64).floor() as usize)
            .min(pixel_rows.saturating_sub(1));
        playhead_pys.push(Some(row));
    }

    let wave_fg = Color::Rgb(200, 200, 200);

    // Build header row and hit-test layout together.
    let mut header: Vec<Span> = Vec::new();
    let mut hit_columns: Vec<TrackerColumnHit> = Vec::with_capacity(visible_cols);
    let mut x_cursor: u16 = 0;
    for col in 0..visible_cols {
        let path = &path_order[col];
        let label = by_path[path]
            .first()
            .map(|(n, _, _, _)| n.clone())
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

        let is_focused = state.player.focused_part.as_deref() == Some(path.as_str());
        let is_soloed = soloed_parts.contains(path);
        let is_muted = muted_parts.contains(path) || (solo_active && !is_soloed);

        let mut style = Style::default().add_modifier(Modifier::BOLD);
        if is_soloed {
            style = style.fg(theme::SOLOED_FG);
        } else if is_muted {
            style = style.fg(theme::MUTED_FG);
        } else if is_focused {
            style = style.fg(theme::FOCUSED_FG);
        }

        header.push(Span::styled(format!("{:<width$}", l, width = col_w), style));
        header.push(Span::raw(" "));

        hit_columns.push(TrackerColumnHit {
            path: path.clone(),
            x_start: x_cursor,
            width: col_w as u16,
            header_y: 0,
            rows: column_rows_hit[col].clone(),
        });

        x_cursor += col_w as u16 + 1;
    }

    let mut lines: Vec<Line> = Vec::with_capacity(char_rows + 1);
    lines.push(Line::from(header));

    for r in 0..char_rows {
        let mut spans: Vec<Span> = Vec::new();
        for col in 0..visible_cols {
            let path = &path_order[col];
            let is_muted_col = muted_parts.contains(path);
            let is_soloed_col = soloed_parts.contains(path);
            let solo_hides_col = solo_active && !is_soloed_col;

            let playhead_char: Option<usize> = playhead_pys[col].map(|py| py / 4);

            for cx in 0..col_w {
                let mut bits: u8 = 0;
                for ly in 0..4 {
                    let py = r * 4 + ly;
                    if py >= pixel_rows {
                        continue;
                    }
                    let (lo, hi) = envelopes[col].get(py).copied().unwrap_or((0.0, 0.0));
                    let half = (col_w * 2) as f32 / 2.0;
                    let center = half;
                    let px_lo = (center + lo * half).round() as isize;
                    let px_hi = (center + hi * half).round() as isize;
                    let px_lo = px_lo.max(0) as usize;
                    let px_hi = (px_hi.min((col_w * 2 - 1) as isize)).max(px_lo as isize)
                        as usize;
                    let cell_x0 = cx * 2;
                    let cell_x1 = cx * 2 + 1;
                    if (px_lo..=px_hi).contains(&cell_x0) {
                        bits |= dot_bit(0, ly);
                    }
                    if (px_lo..=px_hi).contains(&cell_x1) {
                        bits |= dot_bit(1, ly);
                    }
                }

                let row_entry = columns[col].get(r);
                let (cell_text, is_note_muted): (Option<(String, Style)>, bool) =
                    match row_entry {
                        Some(ColumnLine::Note(dn)) => {
                            let lines_back = (dn.value_offset / rpmeasure).round() as usize;
                            let block_line = li.saturating_sub(lines_back);
                            let start_row = dn.note.start_row as i64;
                            let note_muted = muted_notes.contains(&(
                                dn.source_name.clone(),
                                block_line,
                                start_row,
                            ));

                            let note_local_row = dn.value_offset + current_row;
                            let lookup_row = note_local_row.max(dn.note.start_row);

                            let playing = dn.display_end > 0.0
                                && dn.display_start <= current_row
                                && current_row < dn.display_end;
                            let played = !playing
                                && (dn.display_end == 0.0
                                    || current_row >= dn.display_end);

                            let state_note = if playing {
                                NoteState::Playing
                            } else if played {
                                NoteState::Played
                            } else if dn.note.start_row >= rpmeasure {
                                NoteState::Future
                            } else {
                                NoteState::Waiting
                            };

                            let text = fmt_note(&dn.note, lookup_row, dn.subs, state_note);
                            let pitch = value_at(&dn.note.pitch_points, lookup_row)
                                .unwrap_or(440.0);
                            let vol = value_at(&dn.note.vol_points, lookup_row)
                                .unwrap_or(100.0);
                            (Some((text, note_style(pitch, vol, state_note))), note_muted)
                        }
                        Some(ColumnLine::Divider { label }) => (
                            Some((
                                label.clone(),
                                Style::default()
                                    .fg(Color::DarkGray)
                                    .add_modifier(Modifier::ITALIC),
                            )),
                            false,
                        ),
                        None => (None, false),
                    };

                let text_char: Option<char> = cell_text
                    .as_ref()
                    .and_then(|(t, _)| t.chars().nth(cx));

                let under_playhead = playhead_char == Some(r);
                let off = is_muted_col || solo_hides_col || is_note_muted;

                if let Some(ch) = text_char {
                    let style = if off {
                        Style::default()
                            .fg(theme::MUTED_FG)
                            .bg(theme::MUTED_BG)
                    } else if under_playhead {
                        cell_text.as_ref().unwrap().1.fg(theme::PLAYHEAD_FG)
                    } else {
                        cell_text.as_ref().unwrap().1
                    };
                    spans.push(Span::styled(ch.to_string(), style));
                } else if bits != 0 {
                    let ch = braille_char(bits);
                    let style = if off {
                        Style::default()
                            .fg(theme::MUTED_FG)
                            .bg(theme::MUTED_BG)
                    } else if under_playhead {
                        Style::default().fg(theme::PLAYHEAD_FG)
                    } else {
                        Style::default().fg(wave_fg)
                    };
                    spans.push(Span::styled(ch.to_string(), style));
                } else if off {
                    spans.push(Span::styled(" ", Style::default().bg(theme::MUTED_BG)));
                } else if under_playhead {
                    spans.push(Span::styled(
                        theme::PLAYHEAD_MARKER,
                        Style::default().fg(theme::PLAYHEAD_FG),
                    ));
                } else {
                    spans.push(Span::raw(" "));
                }
            }
            spans.push(Span::raw(" "));
        }
        lines.push(Line::from(spans));
    }

    f.render_widget(Paragraph::new(lines).block(outer), area);

    state.tracker_hit = Some(TrackerHit {
        inner,
        columns: hit_columns,
    });
}

fn spiral_dirs(layout: &SpiralLayout, gx: usize, gy: usize) -> theme::PathDirs {
    let gw = layout.grid_w as usize;
    let gh = layout.grid_h as usize;
    let Some(m) = layout.map.get(gy * gw + gx).copied().flatten() else {
        return theme::PathDirs::default();
    };
    let step = layout.measures_per_cell as i64;
    let m = m as i64;
    let check = |nx: usize, ny: usize| -> bool {
        layout
            .map
            .get(ny * gw + nx)
            .copied()
            .flatten()
            .map(|n| (n as i64 - m).abs() == step)
            .unwrap_or(false)
    };
    let mut dirs = theme::PathDirs {
        left: gx > 0 && check(gx - 1, gy),
        right: gx + 1 < gw && check(gx + 1, gy),
        up: gy > 0 && check(gx, gy - 1),
        down: gy + 1 < gh && check(gx, gy + 1),
    };

    if layout.is_center(gx, gy) && !dirs.down {
        dirs.down = true;
    }

    dirs
}

fn draw_spiral(
    f: &mut Frame<'_>,
    area: Rect,
    tp: &Transport,
    p: &Project,
    state: &mut RenderState,
    layout: SpiralLayout,
) {
    state.spiral = Some(SpiralState { layout: layout.clone(), rect: area });

    let lookahead = (PLAYHEAD_LOOKAHEAD_SECS * p.sample_rate as f64) as i64;
    let pos = (tp.position.load(Ordering::Relaxed) + lookahead).min(p.total_samples);
    let current = p.current_line(pos);

    let gw = layout.grid_w as usize;
    let gh = layout.grid_h as usize;
    let mpcell = layout.measures_per_cell;

    let mut lines: Vec<Line> = Vec::with_capacity(gh);
    for gy in 0..gh {
        let mut spans: Vec<Span> = Vec::with_capacity(gw);
        for gx in 0..gw {
            let idx = gy * gw + gx;
            let measure = layout.map.get(idx).copied().flatten();
            let (glyph, mut style) = match measure {
                None => (" ", Style::default()),
                Some(m) => {
                    let dirs = spiral_dirs(&layout, gx, gy);
                    let g = theme::path_glyph(dirs, theme::BORDER_STYLE);
                    let style = match current {
                        Some(cur) if cur >= m && cur < m + mpcell => Style::default()
                            .fg(theme::SPIRAL_CURRENT_FG)
                            .add_modifier(Modifier::BOLD),
                        Some(cur) if cur >= m + mpcell => {
                            Style::default().fg(theme::SPIRAL_PLAYED_FG)
                        }
                        _ => Style::default().fg(theme::SPIRAL_FUTURE_FG),
                    };
                    (g, style)
                }
            };

            if let Some(m) = measure {
                let in_loop = (m..m + mpcell)
                    .any(|mm| state.player.loop_measures.contains(&mm));
                if in_loop {
                    style = style.bg(theme::SPIRAL_LOOP_BG);
                }
            }

            spans.push(Span::styled(glyph.to_string(), style));
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), area);
}

// ── help overlay ───────────────────────────────────────────────────────

fn draw_help(f: &mut Frame<'_>, area: Rect) {
    let w = 56u16.min(area.width.saturating_sub(4));
    let h = 22u16.min(area.height.saturating_sub(4));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let rect = Rect::new(x, y, w, h);

    f.render_widget(Clear, rect);

    let key = Style::default()
        .fg(theme::HELP_KEY_FG)
        .bg(theme::HELP_BG)
        .add_modifier(Modifier::BOLD);
    let desc = Style::default().fg(theme::HELP_FG).bg(theme::HELP_BG);
    let dim = Style::default().fg(theme::HELP_DIM_FG).bg(theme::HELP_BG);

    let row = |k: &str, d: &str| -> Line<'static> {
        Line::from(vec![
            Span::styled(format!("  {:<14}", k), key),
            Span::styled(d.to_string(), desc),
        ])
    };

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled("", desc)));
    lines.push(row("← →", "play reverse / forward"));
    lines.push(row("hold ← →", "accelerate through 4x, 16x"));
    lines.push(row("home", "rewind to start"));
    lines.push(row("[ ]", "prev / next block"));
    lines.push(row("h l", "prev / next column focus"));
    lines.push(row("m / s", "mute / solo focused column"));
    lines.push(row("c", "clear all mutes and solos"));
    lines.push(row("L", "toggle loop playback"));
    lines.push(row("v", "toggle mute mode (clicks = right-click)"));
    lines.push(row("click queue", "type · enter plays · esc leaves"));
    lines.push(row("pgup / pgdn", "loop count (or click [n])"));
    lines.push(row("?", "toggle this help"));
    lines.push(row("q / esc", "quit"));
    lines.push(Line::from(Span::styled("", desc)));
    lines.push(Line::from(Span::styled(
        "  Mouse:",
        Style::default().fg(theme::HELP_KEY_FG).bg(theme::HELP_BG),
    )));
    lines.push(Line::from(Span::styled(
        "  Left-click    spiral: seek   note: jump   header: solo",
        dim,
    )));
    lines.push(Line::from(Span::styled(
        "  In mute mode  spiral: loop   note: mute   header: mute",
        dim,
    )));
    lines.push(Line::from(Span::styled(
        "  Double-click  clear all mutes and solos",
        dim,
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(theme::border_set(theme::BORDER_STYLE))
        .border_style(Style::default().fg(theme::HELP_KEY_FG))
        .title(" Help ")
        .style(Style::default().bg(theme::HELP_BG));

    f.render_widget(Paragraph::new(lines).block(block), rect);
}