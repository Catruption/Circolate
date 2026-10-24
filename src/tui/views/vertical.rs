//! Vertical tracker layout for tall/narrow terminals.
//!
//! Reads like the trackscript source rather than a tracker grid.  Each
//! part is a paragraph: a name line with accented block number and `ƒ`
//! glyph, one or more event lines in source syntax, a multi-row braille
//! waveform of the part's sample at full width, then a blank gap.
//!
//! When the part has inline shims (`ƒpianoer(2)@<ƒFruity_Limiter()>@…`)
//! the label line ends in `@`.  When `state.expanded_chains` contains its
//! path the marker becomes `↦` directly after the name and the effects
//! follow inline as `<ƒTarget()>` cells (`;` after the last), wrapping onto
//! extra rows when they don't fit.  Shims are looked up on the BlockList
//! line each source call came from, so carried-over notes keep their chain.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;

use crate::mixer::{mute, part_volume};
use crate::player::envelope::wrapped_pos;
use crate::player::notes::{split_notes, value_at, DisplayNote};
use crate::player::scope::scope_snapshot;
use crate::player::{Project, Transport};
use crate::formats::trackscript::C5_HZ;
use crate::tui::shared::label;
use crate::tui::shared::note_fmt::{fmt_num, fmt_pitch};
use crate::tui::shared::note_style::{note_style, NoteState};
use crate::tui::shared::waveform::{render_waveform, synth_waveform};
use crate::tui::state::{RenderState, VerticalHit, VerticalRow};
use crate::tui::theme::{Theme, Token};

const PLAYHEAD_LOOKAHEAD_SECS: f64 = 0.05;

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

struct PartBlock {
    path: String,
    first_name: String,
    label_spans: Vec<Span<'static>>,
    event_lines: Vec<EventLine>,
    col_off: bool,
    playhead_col: Option<usize>,
    has_chain: bool,
    marker_x: Option<(u16, u16)>,
    extra_rows: Vec<Vec<Span<'static>>>,
}

struct EventLine {
    line: Line<'static>,
    first_note: Option<(usize, i64)>,
}

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
    let min_wave_rows = state.settings.layout.min_wave_rows;
    let part_gap = state.settings.layout.part_gap;
    let max_event_lines = state.settings.layout.max_event_lines;
    let lookback_lines = state.settings.layout.lookback_lines;

    state.tracker_hit = None;
    state.vertical_hit = None;

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
        return;
    };

    let rpmeasure = p.rows_per_measure().max(1.0);
    let lookback = lookback_lines.min(li);

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
                present.push(key.clone());
            }
            by_path.entry(key).or_default().push((
                call.name.clone(),
                                                  offset_rows,
                                                  call.pattern_measure_start,
                                                  call.pattern_measure_len,
            ));
        }
    }

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
        return;
    }

    let inner = outer.inner(area);
    let inner_w = inner.width as usize;
    let inner_h = inner.height as usize;
    if inner_w < 16 || inner_h < 4 {
        f.render_widget(outer, area);
        return;
    }

    let measure_samples = p.line_measure_samples(li).max(1);
    let line_start = p.line_start_sample(li);
    let elapsed = (pos - line_start).max(0);
    let measure_frac = (elapsed as f64 / measure_samples as f64).clamp(0.0, 1.0);
    let current_row = measure_frac * rpmeasure;

    let snap = mute::snapshot();
    let muted_parts = &snap.muted_parts;
    let soloed_part = snap.soloed_part.as_deref();

    let mut blocks: Vec<PartBlock> = Vec::with_capacity(path_order.len());

    for path in &path_order {
        let Some(sources) = by_path.get(path) else { continue };
        if sources.is_empty() { continue; }
        let first_name = sources[0].0.clone();

        let label_base = label::header_style(path, muted_parts, soloed_part, &theme);
        let col_off = label::is_dimmed(path, muted_parts, soloed_part);

        struct Staged {
            dn: DisplayNote,
            text: String,
            style: Style,
        }
        let mut staged: Vec<Staged> = Vec::new();

        for (name, offset_rows, measure_start, measure_len) in sources {
            let subs = p.part_subs_per_beat(name);
            let is_current = *offset_rows == 0.0;
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

                if !is_current {
                    note.start_row = 0.0;
                }

                let dn = DisplayNote {
                    note,
                    subs,
                    display_start,
                    display_end,
                    value_offset: *offset_rows,
                    source_name: name.clone(),
                };
                let text = format_event(&dn, subs);

                let playing = dn.display_end > 0.0
                && dn.display_start <= current_row
                && current_row < dn.display_end;
                let played = !playing && (dn.display_end == 0.0 || current_row >= dn.display_end);

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
                    Style::default()
                    .fg(theme.color(Token::MutedFg))
                    .bg(theme.color(Token::MutedBg))
                } else {
                    note_style(pitch, vol, state_note, &theme, &synth)
                };
                staged.push(Staged { dn, text, style });
            }
        }

        let playhead_col: Option<usize> = {
            let active = staged.iter().find(|st| {
                st.dn.display_end > 0.0
                && st.dn.display_start <= current_row
                && current_row < st.dn.display_end
            });

            match p.preview_sample_for_part(&first_name) {
                Some(sample) if !sample.left.is_empty() => match active {
                    Some(st) => {
                        let n = sample.left.len();
                        let nf = n as f64;
                        let ph = state.playheads.entry(path.clone()).or_default();

                        let lines_back = (st.dn.value_offset / rpmeasure).round() as usize;
                        let block_line = li.saturating_sub(lines_back);
                        let block_start_sample = p.line_start_sample(block_line);

                        let id = (
                            st.dn.source_name.clone(),
                                  st.dn.note.start_row,
                                  block_start_sample,
                        );

                        if ph.note_id.as_ref() != Some(&id) {
                            ph.position = st.dn.note.offset as f64;
                            ph.note_id = Some(id);
                            ph.last_audio_pos = pos;
                        } else {
                            let raw_delta = pos - ph.last_audio_pos;
                            let max_delta = sr_comp as i64;
                            let audio_delta_secs =
                            raw_delta.clamp(-max_delta, max_delta) as f64 / sr_comp;
                            let pitch = value_at(&st.dn.note.pitch_points, current_row)
                            .unwrap_or(C5_HZ);
                            let ratio = (pitch / C5_HZ).max(1e-6);
                            ph.position += audio_delta_secs * sample.rate * ratio;
                            ph.last_audio_pos = pos;
                        }

                        if sample.loop_pts.is_some() {
                            ph.position = wrapped_pos(ph.position, sample.loop_pts);
                        } else {
                            ph.position = ph.position.clamp(0.0, nf);
                        }

                        let t = (ph.position / nf).clamp(0.0, 1.0);
                        Some(((t * inner_w as f64).floor() as usize)
                        .min(inner_w.saturating_sub(1)))
                    }
                    None => {
                        if let Some(ph) = state.playheads.get_mut(path) {
                            ph.note_id = None;
                        }
                        None
                    }
                },
                _ => {
                    if let Some(ph) = state.playheads.get_mut(path) {
                        ph.note_id = None;
                    }
                    None
                }
            }
        };

        let block_playing = !col_off && staged.iter().any(|st| {
            st.dn.display_end > 0.0
            && st.dn.display_start <= current_row
            && current_row < st.dn.display_end
        });

        let mut label = p
        .line_block_title(li, &first_name)
        .unwrap_or(&first_name)
        .to_string();

        // Inline shim chain.  Look on the BlockList line each source call
        // came from (current line first, then lookback lines), so a part
        // carried over from an earlier line still shows its chain.
        let shim_names: Vec<String> = sources
        .iter()
        .map(|(_, off, _, _)| li.saturating_sub((off / rpmeasure).round() as usize))
        .map(|l| p.shims_for_line_path(l, path))
        .find(|v| !v.is_empty())
        .map(|v| v.iter().map(|sh| sh.target.clone()).collect())
        .unwrap_or_default();
        let has_chain = !shim_names.is_empty();
        let expanded = state.expanded_chains.contains(path.as_str());
        let marker_chars = if has_chain { 1 } else { 0 };

        let vol = part_volume::get(path);
        let show_vol = p.part_supports_volume(path);
        let vol_tag = if show_vol && (vol - 1.0).abs() > 0.01 {
            format!(" {}%", (vol * 100.0).round() as i64)
        } else {
            String::new()
        };
        let tag_chars = vol_tag.chars().count();
        let avail = inner_w.saturating_sub(tag_chars + marker_chars);

        if label.chars().count() > avail {
            if avail > 1 {
                label = label.chars().take(avail - 1).collect();
                label.push('…');
            } else {
                label = label.chars().take(avail).collect();
            }
        }

        // Parts with a chain keep the marker right after the name, so the
        // label isn't padded out to the right edge in that case.
        let fill = if has_chain {
            label.chars().count().min(avail)
        } else {
            avail
        };
        let padded = format!("{:<width$}", label, width = fill);
        let mut label_len = 0usize;
        let mut label_spans: Vec<Span<'static>> = Vec::new();
        for span in label::split(&padded, label_base, &theme, &labels_cfg, block_playing) {
            let sw = span.content.chars().count();
            if label_len + sw <= fill {
                label_len += sw;
                label_spans.push(span);
            } else {
                let take = fill.saturating_sub(label_len);
                if take > 0 {
                    let s: String = span.content.chars().take(take).collect();
                    label_spans.push(Span::styled(s, span.style));
                    label_len = fill;
                }
                break;
            }
        }
        while label_len < fill {
            label_spans.push(Span::raw(" "));
            label_len += 1;
        }
        if !vol_tag.is_empty() {
            label_spans.push(Span::styled(vol_tag, label_base.add_modifier(Modifier::DIM)));
        }
        let marker_x = if has_chain {
            let mx = (fill + tag_chars) as u16;
            Some((mx, mx + 1))
        } else {
            None
        };
        let mut extra_rows: Vec<Vec<Span<'static>>> = Vec::new();
        if has_chain {
            let marker = if expanded { "↦" } else { "@" };
            label_spans.push(Span::styled(
                marker,
                Style::default()
                .fg(ratatui::style::Color::Yellow)
                .add_modifier(Modifier::BOLD),
            ));
            if expanded {
                let disabled = state.disabled_chains.contains(path.as_str());
                let chain_style = if col_off || disabled {
                    Style::default()
                    .fg(theme.color(Token::MutedFg))
                    .bg(theme.color(Token::MutedBg))
                } else {
                    Style::default().fg(theme.color(Token::BlockInstance))
                };
                let n = shim_names.len();
                let mut used = fill + tag_chars + 1;
                let mut cur: Vec<Span<'static>> = Vec::new();
                let mut on_label_row = true;
                let mut placed = 0usize;
                for (i, name) in shim_names.iter().enumerate() {
                    let term = if i + 1 == n { ";" } else { "" };
                    let text = format!("<ƒ{}()>{}", name, term);
                    let w = text.chars().count();
                    let mut lead = if on_label_row || placed > 0 { 1 } else { 0 };
                    if used + lead + w > inner_w && (placed > 0 || on_label_row) {
                        if !on_label_row {
                            extra_rows.push(std::mem::take(&mut cur));
                        }
                        on_label_row = false;
                        used = 0;
                        placed = 0;
                        lead = 0;
                    }
                    let target: &mut Vec<Span<'static>> =
                    if on_label_row { &mut label_spans } else { &mut cur };
                    if lead > 0 {
                        target.push(Span::raw(" "));
                    }
                    target.push(Span::styled(text, chain_style));
                    used += lead + w;
                    placed += 1;
                }
                if !on_label_row && !cur.is_empty() {
                    extra_rows.push(cur);
                }
            }
        }

        staged.sort_by(|a, b| {
            a.dn.note
            .start_row
            .partial_cmp(&b.dn.note.start_row)
            .unwrap_or(std::cmp::Ordering::Equal)
        });

        let mut event_lines: Vec<EventLine> = Vec::new();
        let mut spans: Vec<Span> = Vec::new();
        let mut width = 0usize;
        let mut truncated = false;
        let mut first_on_line: Option<(usize, i64)> = None;

        for st in &staged {
            let tw = st.text.chars().count();
            let need = tw + 1;

            if width > 0 && width + need > inner_w {
                event_lines.push(EventLine {
                    line: Line::from(std::mem::take(&mut spans)),
                                 first_note: first_on_line.take(),
                });
                width = 0;
                if event_lines.len() >= max_event_lines {
                    truncated = true;
                    break;
                }
            }
            if first_on_line.is_none() {
                first_on_line = Some((li, st.dn.note.start_row as i64));
            }
            spans.push(Span::styled(st.text.clone(), st.style));
            spans.push(Span::raw(" "));
            width += need;
        }
        if !spans.is_empty() && event_lines.len() < max_event_lines {
            event_lines.push(EventLine {
                line: Line::from(spans),
                             first_note: first_on_line,
            });
        }
        if truncated {
            if let Some(last) = event_lines.last_mut() {
                last.line.spans.push(Span::styled(
                    "…".to_string(),
                                                  Style::default().fg(theme.color(Token::MutedFg)),
                ));
            }
        }

        blocks.push(PartBlock {
            path: path.clone(),
                    first_name,
                    label_spans,
                    event_lines,
                    col_off,
                    playhead_col,
                    has_chain,
                    marker_x,
                    extra_rows,
        });
    }

    // ── pass 2: fit blocks, allocate wave rows ────────────────────
    let mut total_fixed = 0usize;
    let mut included = 0usize;
    for (i, b) in blocks.iter().enumerate() {
        let shim_rows = b.extra_rows.len();
        let this_fixed = 1
        + shim_rows
        + b.event_lines.len()
        + if i > 0 { part_gap } else { 0 };
        let projected =
        total_fixed + this_fixed + (included + 1) * min_wave_rows;
        if projected > inner_h {
            break;
        }
        total_fixed += this_fixed;
        included += 1;
    }
    blocks.truncate(included);

    let available = inner_h.saturating_sub(total_fixed);
    let per_wave = if included == 0 { 0 } else { available / included };
    let wave_rows = per_wave.max(min_wave_rows);

    // ── pass 3: emit lines and hit-test rows ──────────────────────
    let mut lines: Vec<Line> = Vec::with_capacity(inner_h);
    let mut hit_rows: Vec<(u16, VerticalRow)> = Vec::new();
    let mut y: u16 = 0;

    for (i, block) in blocks.iter().enumerate() {
        let marker_x = block.marker_x;

        if y < inner_h as u16 {
            lines.push(Line::from(block.label_spans.clone()));
            hit_rows.push((
                y,
                VerticalRow::Label {
                    path: block.path.clone(),
                           marker_x,
                },
            ));
            y += 1;
        }

        for row in &block.extra_rows {
            if y >= inner_h as u16 { break; }
            lines.push(Line::from(row.clone()));
            y += 1;
        }

        for ev in &block.event_lines {
            if y >= inner_h as u16 { break; }
            lines.push(ev.line.clone());
            if let Some((bl, sr)) = ev.first_note {
                hit_rows.push((
                    y,
                    VerticalRow::Event {
                        path: block.path.clone(),
                               block_line: bl,
                               start_row: sr,
                    },
                ));
            }
            y += 1;
        }

        let base_style = if block.col_off {
            Style::default()
            .fg(theme.color(Token::MutedFg))
            .bg(theme.color(Token::MutedBg))
        } else {
            Style::default().fg(Color::Rgb(180, 180, 180))
        };
        let ph_style = if block.col_off {
            Style::default()
            .fg(theme.color(Token::MutedFg))
            .bg(theme.color(Token::MutedBg))
        } else {
            Style::default().fg(theme.color(Token::Playhead))
        };
        let ph_col = if block.col_off { None } else { block.playhead_col };

        let wave: Vec<Vec<Span<'static>>> =
        if let Some(sample) = p.preview_sample_for_part(&block.first_name) {
            render_waveform(
                &sample,
                inner_w,
                wave_rows,
                base_style,
                ph_col,
                ph_style,
            )
        } else if let Some(iid) = p.iid_for_part(&block.first_name) {
            scope_snapshot(iid as usize, &mut state.snapshot);
            if state.snapshot.is_empty() {
                vec![
                    vec![Span::styled(" ".to_string(), base_style); inner_w];
                    wave_rows
                ]
            } else {
                synth_waveform(
                    &state.snapshot,
                    inner_w,
                    wave_rows,
                    base_style,
                    ph_col,
                    ph_style,
                )
            }
        } else {
            vec![
                vec![Span::styled(" ".to_string(), base_style); inner_w];
                wave_rows
            ]
        };

        for row in wave {
            if y >= inner_h as u16 { break; }
            lines.push(Line::from(row));
            hit_rows.push((
                y,
                VerticalRow::Waveform {
                    path: block.path.clone(),
                           line: li,
                },
            ));
            y += 1;
        }

        if y < inner_h as u16 && i + 1 < blocks.len() {
            lines.push(Line::from(""));
            y += 1;
        }
    }

    f.render_widget(Paragraph::new(lines).block(outer), area);
    state.vertical_hit = Some(VerticalHit {
        inner,
        rows: hit_rows,
    });
}
