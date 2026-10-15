//! Horizontal tracker: one time-aligned column per part, with a braille
//! waveform behind the note text and the playhead column highlighted.
//! Reads like a tracker grid; splits parts side by side so they can be
//! compared beat-by-beat.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;

use crate::mixer::mute;
use crate::player::envelope::{
    synth_envelope_live, synth_envelope_static, whole_sample_envelope, wrapped_pos,
};
use crate::player::notes::{split_notes, value_at, ColumnLine, DisplayNote};
use crate::player::scope::scope_snapshot;
use crate::player::{Project, Transport};
use crate::sources::trackscript::C5_HZ;
use crate::tui::shared::braille::{braille_char, dot_bit};
use crate::tui::shared::label;
use crate::tui::shared::note_fmt::fmt_note;
use crate::tui::shared::note_style::{note_style, NoteState};
use crate::tui::state::{RenderState, TrackerColumnHit, TrackerHit};
use crate::tui::theme::{Theme, Token};

const PLAYHEAD_LOOKAHEAD_SECS: f64 = 0.05;

pub fn draw(
    f: &mut Frame<'_>,
    area: Rect,
    tp: &Transport,
    p: &Project,
    state: &mut RenderState,
) {
    let theme: Theme = state.settings.theme.clone();
    let synth = state.settings.synesthesia;
    let labels_cfg = state.settings.labels;
    let min_col_w = state.settings.layout.min_col_w;
    let max_col_w = state.settings.layout.max_col_w;
    let lookback_lines = state.settings.layout.lookback_lines;
    let playhead_marker = state.settings.layout.playhead_marker.clone();

    let sr_comp = p.sample_rate as f64;
    let lookahead = (PLAYHEAD_LOOKAHEAD_SECS * sr_comp) as i64;
    let pos = (tp.position.load(Ordering::Relaxed) + lookahead).min(p.total_samples);
    let current = p.current_line(pos);

    let title = match current {
        Some(li) => format!(" BlockList line {} ", li + 1),
        None => " BlockList (idle) ".into(),
    };
    let outer = theme.block().title(title);

    let Some(li) = current else {
        f.render_widget(Paragraph::new("(silence)").block(outer), area);
        state.tracker_hit = None;
        return;
    };

    let rpmeasure = p.rows_per_measure();
    let lookback = lookback_lines.min(li);

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
    if inner_w < min_col_w + 1 || inner_h < 2 {
        f.render_widget(outer, area);
        state.tracker_hit = None;
        return;
    }

    let path_count = path_order.len();
    let max_visible = (inner_w / (min_col_w + 1)).max(1);
    let visible_cols = path_count.min(max_visible);
    let col_w = (inner_w / visible_cols)
        .saturating_sub(1)
        .clamp(min_col_w, max_col_w);

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

    let snap = mute::snapshot();
    let muted_parts = &snap.muted_parts;
    let soloed_part = snap.soloed_part.as_deref();
    let muted_notes = &snap.muted_notes;

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

    // Which columns have a note currently under the playhead?  Drives
    // the block-number highlight in the header.
    let col_playing: Vec<bool> = (0..visible_cols)
        .map(|col| {
            columns[col].iter().any(|line| {
                if let ColumnLine::Note(dn) = line {
                    dn.display_end > 0.0
                        && dn.display_start <= current_row
                        && current_row < dn.display_end
                } else {
                    false
                }
            })
        })
        .collect();

    // Envelopes.
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

    // Sample-position playhead.
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

    let wave_fg = theme.color(Token::Wave);
    let muted_fg = theme.color(Token::MutedFg);
    let muted_bg = theme.color(Token::MutedBg);
    let playhead_fg = theme.color(Token::Playhead);

    // Header row + hit-test.
    let mut header: Vec<Span> = Vec::new();
    let mut hit_columns: Vec<TrackerColumnHit> = Vec::with_capacity(visible_cols);
    let mut x_cursor: u16 = 0;
    for col in 0..visible_cols {
        let path = &path_order[col];
        let label_text = by_path[path]
            .first()
            .map(|(name, _, _, _)| {
                p.line_block_title(li, name).unwrap_or(name).to_string()
            })
            .unwrap_or_else(|| "(?)".to_string());

        let label_chars = label_text.chars().count();
        let trimmed = if label_chars > col_w {
            if col_w > 1 {
                let mut s: String = label_text.chars().take(col_w - 1).collect();
                s.push('…');
                s
            } else {
                label_text.chars().take(col_w).collect()
            }
        } else {
            label_text
        };

        let style = label::header_style(path, muted_parts, soloed_part, &theme);

        let padded = format!("{:<width$}", trimmed, width = col_w);
        for span in label::split(&padded, style, &theme, &labels_cfg, col_playing[col]) {
            header.push(span);
        }
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

            let solo_hides_col = soloed_part.map_or(false, |s| s != path.as_str());
            let is_muted_col = muted_parts.contains(path);

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
                            let sty = note_style(pitch, vol, state_note, &theme, &synth);
                            (Some((text, sty)), note_muted)
                        }
                        Some(ColumnLine::Divider { label }) => (
                            Some((
                                label.clone(),
                                Style::default()
                                    .fg(ratatui::style::Color::DarkGray)
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
                        Style::default().fg(muted_fg).bg(muted_bg)
                    } else if under_playhead {
                        cell_text.as_ref().unwrap().1.fg(playhead_fg)
                    } else {
                        cell_text.as_ref().unwrap().1
                    };
                    spans.push(Span::styled(ch.to_string(), style));
                } else if bits != 0 {
                    let ch = braille_char(bits);
                    let style = if off {
                        Style::default().fg(muted_fg).bg(muted_bg)
                    } else if under_playhead {
                        Style::default().fg(playhead_fg)
                    } else {
                        Style::default().fg(wave_fg)
                    };
                    spans.push(Span::styled(ch.to_string(), style));
                } else if off {
                    spans.push(Span::styled(" ", Style::default().bg(muted_bg)));
                } else if under_playhead {
                    spans.push(Span::styled(
                        playhead_marker.clone(),
                        Style::default().fg(playhead_fg),
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