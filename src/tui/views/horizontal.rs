//! Horizontal tracker: one time-aligned column per part, with a braille
//! waveform behind the note text and the playhead column highlighted.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use std::collections::HashSet;
use std::borrow::Cow;
use std::sync::atomic::Ordering;

use crate::player::envelope::{
    synth_envelope_live, synth_envelope_static, whole_sample_envelope, wrapped_pos,
};
use crate::player::notes::{value_at, ColumnLine};
use crate::player::scope::{scope_snapshot_recent, vgm_scope_slot};
use crate::player::{Project, Transport};
use crate::formats::halotext::C5_HZ;
use crate::tui::shared::braille::{braille_str, dot_bit};
use crate::tui::shared::label;
use crate::tui::shared::note_fmt::fmt_note;
use crate::tui::shared::waveform::rasterize_time_y_into;
use crate::tui::state::{RenderState, TrackerColumnHit, TrackerHit};
use crate::tui::theme::{Theme, Token};

pub fn draw(
    f: &mut Frame<'_>,
    area: Rect,
    tp: &Transport,
    p: &Project,
    state: &mut RenderState,
) {
    let theme: Theme = state.settings.theme.clone();
    let labels_cfg = state.settings.labels;
    let min_col_w = state.settings.layout.min_col_w;
    let max_col_w = state.settings.layout.max_col_w;
    let playhead_marker = state.settings.layout.playhead_marker.clone();

    let sr_comp = p.sample_rate as f64;
    let lookahead = (super::measure::PLAYHEAD_LOOKAHEAD_SECS * sr_comp) as i64;
    let pos = (tp.position.load(Ordering::Relaxed) + lookahead).min(p.total_samples);
    let current = p.current_line(pos);

    let title = match current {
        Some(li) => format!(" BlockList line {} ", li + 1),
        None => " BlockList (idle) ".into(),
    };
    let outer = theme.block().title(title);

    let Some(li) = current else {
        crate::tui::widgets::menu::draw(f, area, state);
        state.tracker_hit = None;
        return;
    };

    let Some(prep) = super::measure::prepare_measure(p, tp, state, li, &theme) else {
        f.render_widget(Paragraph::new("(silence)").block(outer), area);
        state.tracker_hit = None;
        return;
    };

    let inner = outer.inner(area);
    let inner_w = inner.width as usize;
    let inner_h = inner.height as usize;
    if inner_w < min_col_w + 1 || inner_h < 2 {
        f.render_widget(outer, area);
        state.tracker_hit = None;
        return;
    }

    let path_count = prep.parts.len();
    let max_visible = (inner_w / (min_col_w + 1)).max(1);
    let visible_cols = path_count.min(max_visible);
    let col_w = (inner_w / visible_cols).saturating_sub(1).clamp(min_col_w, max_col_w);

    let char_rows = inner_h.saturating_sub(1);
    let pixel_rows = char_rows * 4;
    let current_row = prep.current_row;

    let mut columns: Vec<Vec<ColumnLine>> = Vec::with_capacity(visible_cols);
    let mut column_rows_hit: Vec<Vec<Option<(usize, i64)>>> = Vec::with_capacity(visible_cols);

    for col in 0..visible_cols {
        let part = &prep.parts[col];
        let expanded = state.expanded_chains.contains(part.path.as_str());
        let chain_rows = if expanded { part.shim_names.len() } else { 0 };

        let mut entries: Vec<ColumnLine> = Vec::with_capacity(part.notes.len() + chain_rows + 4);
        let mut entries_bl: Vec<Option<(usize, i64)>> = Vec::with_capacity(part.notes.len() + chain_rows + 4);

        if chain_rows > 0 {
            for (ci, name) in part.shim_names.iter().enumerate() {
                let term = if ci + 1 == part.shim_names.len() { ";" } else { "" };
                let text = format!("<ƒ{}()>{}", name, term);
                let truncated = if text.chars().count() > col_w {
                    let mut s: String = text.chars().take(col_w.saturating_sub(1)).collect();
                    s.push('…');
                    s
                } else { text };
                entries.push(ColumnLine::Divider { label: truncated });
                entries_bl.push(None);
            }
        }

        let mut last_name: Option<String> = None;
        for (note_idx, pnote) in part.notes.iter().enumerate() {
            let name = &pnote.dn.source_name;
            let at_start = last_name.is_none();
            let changed = last_name.as_deref() != Some(name.as_str());
            let is_header = part.first_name == *name;
            if (at_start || changed) && !is_header {
                entries.push(ColumnLine::Divider { label: name.clone() });
                entries_bl.push(None);
            }
            entries.push(ColumnLine::Note(note_idx));
            entries_bl.push(Some((pnote.block_line, pnote.start_row)));
            last_name = Some(name.clone());
        }
        columns.push(entries);
        column_rows_hit.push(entries_bl);
    }

    if state.last_synth_env.len() < visible_cols {
        state.last_synth_env.resize_with(visible_cols, Vec::new);
    }
    if state.dot_matrices.len() < visible_cols {
        state.dot_matrices.resize_with(visible_cols, Vec::new);
    }
    for col in 0..visible_cols {
        let part = &prep.parts[col];
        let env: Cow<'_, [(f32, f32)]> = if let Some(sample) = p.preview_sample_for_part(&part.first_name) {
            let key = (part.path.clone(), pixel_rows);
            Cow::Borrowed(state.sample_env_cache.entry(key)
            .or_insert_with(|| whole_sample_envelope(&sample, pixel_rows)))
        } else {
            let live = vgm_scope_slot(&part.path)
            .or_else(|| p.iid_for_part(&part.first_name).map(|iid| iid as usize))
            .and_then(|slot| {
                scope_snapshot_recent(
                    slot,
                    pixel_rows.saturating_mul(crate::player::envelope::SCOPE_SAMPLES_PER_BIN),
                                      &mut state.snapshot,
                );
                if state.snapshot.is_empty() { None } else {
                    let e = synth_envelope_live(&state.snapshot, pixel_rows);
                    if e.is_empty() { None } else { Some(e) }
                }
            });
            match live {
                Some(env) => {
                    state.last_synth_env[col] = env;
                    Cow::Borrowed(&state.last_synth_env[col])
                }
                None => {
                    let cached = &state.last_synth_env[col];
                    if cached.len() == pixel_rows { Cow::Borrowed(cached) }
                    else {
                        Cow::Borrowed(state.synth_env_cache.entry((part.path.clone(), pixel_rows))
                        .or_insert_with(|| synth_envelope_static(p, &part.first_name, pixel_rows)))
                    }
                }
            }
        };
        rasterize_time_y_into(&env, col_w * 2, pixel_rows, &mut state.dot_matrices[col]);
    }

    let mut playhead_pys: Vec<Option<usize>> = Vec::with_capacity(visible_cols);
    for col in 0..visible_cols {
        let part = &prep.parts[col];
        let Some(sample) = p.preview_sample_for_part(&part.first_name) else {
            playhead_pys.push(None);
            continue;
        };
        let n = sample.left.len();
        if n == 0 { playhead_pys.push(None); continue; }
        let nf = n as f64;
        let loop_pts = sample.loop_pts;
        let sample_rate_f = sample.rate;

        let active = part.notes.iter().find(|pn| {
            pn.dn.display_end > 0.0
            && pn.dn.display_start <= current_row
            && current_row < pn.dn.display_end
        });

        let ph = state.playheads.entry(part.path.clone()).or_default();
        let Some(pn) = active else {
            ph.note_id = None;
            playhead_pys.push(None);
            continue;
        };

        let block_line = pn.block_line;
        let block_start_sample = p.line_start_sample(block_line);
        let id = (pn.dn.source_name.clone(), pn.dn.note.start_row, block_start_sample);

        if ph.note_id.as_ref() != Some(&id) {
            ph.position = pn.dn.note.offset as f64;
            ph.note_id = Some(id);
            ph.last_audio_pos = pos;
        } else {
            let raw_delta = pos - ph.last_audio_pos;
            let max_delta = sr_comp as i64;
            let audio_delta_secs = raw_delta.clamp(-max_delta, max_delta) as f64 / sr_comp;
            let pitch = value_at(&pn.dn.note.pitch_points, pn.lookup_row).unwrap_or(C5_HZ);
            let ratio = (pitch / C5_HZ).max(1e-6);
            ph.position += audio_delta_secs * sample_rate_f * ratio;
            ph.last_audio_pos = pos;
        }

        ph.position = if loop_pts.is_some() {
            wrapped_pos(ph.position, loop_pts)
        } else {
            ph.position.clamp(0.0, nf)
        };

        let t = (ph.position / nf).clamp(0.0, 1.0);
        let row = ((t * pixel_rows as f64).floor() as usize).min(pixel_rows.saturating_sub(1));
        playhead_pys.push(Some(row));
    }

    let wave_fg = theme.color(Token::Wave);
    let muted_fg = theme.color(Token::MutedFg);
    let muted_bg = theme.color(Token::MutedBg);
    let playhead_fg = theme.color(Token::Playhead);

    let mut header: Vec<Span> = Vec::new();
    let mut hit_columns: Vec<TrackerColumnHit> = Vec::with_capacity(visible_cols);
    let mut x_cursor: u16 = 0;

    for col in 0..visible_cols {
        let part = &prep.parts[col];
        let label_text = p.line_block_title(li, &part.first_name)
        .unwrap_or(&part.first_name).to_string();

        let has_chain = !part.shim_names.is_empty();
        let expanded = state.expanded_chains.contains(part.path.as_str());
        let marker_chars = if has_chain { 1 } else { 0 };

        let vol_tag = if part.show_vol && (part.vol - 1.0).abs() > 0.01 {
            format!(" {}%", (part.vol * 100.0).round() as i64)
        } else { String::new() };
        let tag_chars = vol_tag.chars().count();
        let avail = col_w.saturating_sub(tag_chars + marker_chars);

        let label_chars = label_text.chars().count();
        let trimmed = if label_chars > avail {
            if avail > 1 {
                let mut s: String = label_text.chars().take(avail - 1).collect();
                s.push('…');
                s
            } else { label_text.chars().take(avail).collect() }
        } else { label_text };

        let style = label::header_style(&part.path, &HashSet::new(), None, &theme);
        let fill = if has_chain { trimmed.chars().count().min(avail) } else { avail };
        let padded = format!("{:<width$}", trimmed, width = fill);

        let mut label_len = 0usize;
        for span in label::split(&padded, style, &theme, &labels_cfg, part.is_playing_now) {
            let sw = span.content.chars().count();
            if label_len + sw <= fill {
                label_len += sw;
                header.push(span);
            } else {
                let take = fill.saturating_sub(label_len);
                if take > 0 {
                    let s: String = span.content.chars().take(take).collect();
                    header.push(Span::styled(s, span.style));
                    label_len = fill;
                }
                break;
            }
        }
        while label_len < fill { header.push(Span::raw(" ")); label_len += 1; }

        if !vol_tag.is_empty() {
            header.push(Span::styled(vol_tag, style.add_modifier(Modifier::DIM)));
        }
        if has_chain {
            let marker = if expanded { "↴" } else { "@" };
            header.push(Span::styled(
                marker,
                Style::default().fg(ratatui::style::Color::Yellow).add_modifier(Modifier::BOLD),
            ));
        }
        for _ in (fill + tag_chars + marker_chars)..col_w {
            header.push(Span::raw(" "));
        }
        header.push(Span::raw(" "));

        let marker_x = if has_chain {
            let mx = x_cursor + (fill + tag_chars) as u16;
            Some((mx, mx + 1))
        } else { None };

        hit_columns.push(TrackerColumnHit {
            path: part.path.clone(),
                         x_start: x_cursor,
                         width: col_w as u16,
                         header_y: 0,
                         rows: column_rows_hit[col].clone(),
                         marker_x,
        });
        x_cursor += col_w as u16 + 1;
    }

    let mut lines: Vec<Line> = Vec::with_capacity(char_rows + 1);
    lines.push(Line::from(header));

    for r in 0..char_rows {
        let mut spans: Vec<Span> = Vec::new();
        for col in 0..visible_cols {
            let part = &prep.parts[col];
            let chain_rows = if state.expanded_chains.contains(part.path.as_str()) {
                part.shim_names.len()
            } else { 0 };

            let solo_hides_col = part.is_soloed_out;
            let playhead_char = playhead_pys[col].map(|py| py / 4 + chain_rows);

            // Resolve and format the row once per part. The old cell loop
            // repeated a note search, formatting, and chars().nth() per cell.
            let (cell_text, text_style, is_note_muted) = match columns[col].get(r) {
                Some(ColumnLine::Note(note_idx)) => {
                    let pn = &part.notes[*note_idx];
                    (Some(fmt_note(&pn.dn.note, pn.lookup_row, pn.dn.subs, pn.note_state)), Some(pn.style), pn.is_muted)
                }
                Some(ColumnLine::Divider { label }) => (
                    Some(label.clone()),
                                                        Some(Style::default().fg(ratatui::style::Color::DarkGray).add_modifier(Modifier::ITALIC)),
                                                        false,
                ),
                None => (None, None, false),
            };
            let text_len = cell_text.as_ref().map_or(0, |text| text.chars().count().min(col_w));
            if text_len > 0 {
                let base_style = text_style.unwrap_or_default();
                let style = if part.is_muted || solo_hides_col || is_note_muted {
                    Style::default().fg(muted_fg).bg(muted_bg)
                } else if playhead_char == Some(r) {
                    base_style.fg(playhead_fg)
                } else {
                    base_style
                };
                let text: String = cell_text.as_ref().unwrap().chars().take(text_len).collect();
                spans.push(Span::styled(text, style));
            }

            for cx in text_len..col_w {
                let mut bits: u8 = 0;
                for ly in 0..4 {
                    let py_signed = (r as i32 - chain_rows as i32) * 4 + ly as i32;
                    if py_signed < 0 { continue; }
                    let py = py_signed as usize;
                    if py >= pixel_rows { continue; }
                    let px0 = cx * 2;
                    let px1 = cx * 2 + 1;
                    let dot_width = col_w * 2;
                    if px0 < dot_width && state.dot_matrices[col][py * dot_width + px0] { bits |= dot_bit(0, ly); }
                    if px1 < dot_width && state.dot_matrices[col][py * dot_width + px1] { bits |= dot_bit(1, ly); }
                }

                let under_playhead = playhead_char == Some(r);
                let off = part.is_muted || solo_hides_col || is_note_muted;

                if bits != 0 {
                    let ch = braille_str(bits);
                    let style = if off {
                        Style::default().fg(muted_fg).bg(muted_bg)
                    } else if under_playhead {
                        Style::default().fg(playhead_fg)
                    } else {
                        Style::default().fg(wave_fg)
                    };
                    spans.push(Span::styled(ch, style));
                } else if off {
                    spans.push(Span::styled(" ", Style::default().bg(muted_bg)));
                } else if under_playhead {
                    spans.push(Span::styled(playhead_marker.clone(), Style::default().fg(playhead_fg)));
                } else {
                    spans.push(Span::raw(" "));
                }
            }
            spans.push(Span::raw(" "));
        }
        lines.push(Line::from(spans));
    }

    f.render_widget(Paragraph::new(lines).block(outer), area);

    state.tracker_hit = Some(TrackerHit { inner, columns: hit_columns });
}
