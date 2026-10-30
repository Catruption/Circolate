//! Shared measure preparation for horizontal and vertical tracker views.
//!
//! Both views need identical data: which parts are playing on the current
//! line (plus lookback), and for each part a sorted list of notes with
//! resolved state, pitch, volume, mute flag, and motion-highlight flag.
//! Views own only layout/rendering; this module owns the collection pipeline.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;

use ratatui::style::Style;

use crate::formats::trackscript::C5_HZ;
use crate::mixer::mute;
use crate::player::notes::{split_notes, value_at, DisplayNote};
use crate::player::{Project, Transport};
use crate::tui::shared::note_style::{note_style, NoteState};
use crate::tui::state::RenderState;
use crate::tui::theme::Theme;

pub const PLAYHEAD_LOOKAHEAD_SECS: f64 = 0.05;

pub struct PreparedNote {
    pub dn: DisplayNote,
    pub block_line: usize,
    pub start_row: i64,
    pub lookup_row: f64,
    pub note_state: NoteState,
    pub style: Style,
    pub is_muted: bool,
}

pub struct PreparedPart {
    pub path: String,
    pub first_name: String,
    pub shim_names: Vec<String>,
    pub is_muted: bool,
    pub is_soloed_out: bool,
    pub vol: f64,
    pub show_vol: bool,
    pub notes: Vec<PreparedNote>,
    pub is_playing_now: bool,
}

pub struct PreparedMeasure {
    pub parts: Vec<PreparedPart>,
    pub current_row: f64,
    pub rpmeasure: f64,
}

pub fn prepare_measure(
    p: &Project,
    tp: &Transport,
    state: &mut RenderState,
    li: usize,
    theme: &Theme,
) -> Option<PreparedMeasure> {
    let synth = state.settings.synesthesia;
    let lookback_lines = state.settings.layout.lookback_lines;
    let rpmeasure = p.rows_per_measure().max(1.0);
    let lookback = lookback_lines.min(li);

    let sr_comp = p.sample_rate as f64;
    let lookahead = (PLAYHEAD_LOOKAHEAD_SECS * sr_comp) as i64;
    let pos = (tp.position.load(Ordering::Relaxed) + lookahead).min(p.total_samples);

    let measure_samples = p.line_measure_samples(li).max(1);
    let line_start = p.line_start_sample(li);
    let elapsed = (pos - line_start).max(0);
    let measure_frac = (elapsed as f64 / measure_samples as f64).clamp(0.0, 1.0);
    let current_row = measure_frac * rpmeasure;

    // Build by_path from current line + lookback.
    let mut by_path: HashMap<String, Vec<(String, f64, f64, f64)>> = HashMap::new();
    let mut discovery: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for call in p.line_calls(li) {
        let Some(path) = p.part_path(&call.name) else { continue };
        let key = path.to_string();
        if seen.insert(key.clone()) { discovery.push(key.clone()); }
        by_path.entry(key).or_default().push((
            call.name.clone(), 0.0,
                                              call.pattern_measure_start, call.pattern_measure_len,
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
                p.part_events(&call.name), subs,
                                       p.part_sample_offset_u32(&call.name),
            ).iter().any(|n| {
                n.start_row >= call.pattern_measure_start
                && n.start_row < measure_end
                && (n.end_row - call.pattern_measure_start) > offset_rows
            });
            if !still_on { continue; }
            let key = path.to_string();
            if seen.insert(key.clone()) { discovery.push(key.clone()); }
            by_path.entry(key).or_default().push((
                call.name.clone(), offset_rows,
                                                  call.pattern_measure_start, call.pattern_measure_len,
            ));
        }
    }

    if discovery.is_empty() { return None; }

    // Merge with existing column order to preserve user-set ordering.
    let present: HashSet<&String> = discovery.iter().collect();
    let mut merged: Vec<String> = state.column_order.iter()
    .filter(|path| present.contains(path))
    .cloned().collect();
    for path in &discovery {
        if !merged.iter().any(|m| m == path) { merged.push(path.clone()); }
    }
    state.column_order = merged.clone();

    // Prune dead playheads.
    {
        let present_set: HashSet<&String> = merged.iter().collect();
        state.playheads.retain(|k, _| present_set.contains(k));
    }

    let snap = mute::snapshot();
    let muted_parts = &snap.muted_parts;
    let soloed_part = snap.soloed_part.as_deref();
    let muted_notes = &snap.muted_notes;

    let mut parts: Vec<PreparedPart> = Vec::with_capacity(merged.len());

    for path in &merged {
        let Some(sources) = by_path.get(path) else { continue };
        if sources.is_empty() { continue; }

        let first_name = sources[0].0.clone();

        let shim_names: Vec<String> = sources.iter()
        .map(|(_, off, _, _)| li.saturating_sub((off / rpmeasure).round() as usize))
        .map(|l| p.shims_for_line_path(l, path))
        .find(|v| !v.is_empty())
        .map(|v| v.iter().map(|sh| sh.target.clone()).collect())
        .unwrap_or_default();

        let is_muted = muted_parts.contains(path);
        let is_soloed_out = soloed_part.map_or(false, |s| s != path.as_str());
        let vol = crate::mixer::part_volume::get(path);
        let show_vol = p.part_supports_volume(path);

        let mut notes: Vec<PreparedNote> = Vec::new();

        for (name, offset_rows, measure_start, measure_len) in sources {
            let subs = p.part_subs_per_beat(name);
            let is_current = *offset_rows == 0.0;
            let offset_lines = (offset_rows / rpmeasure).round() as usize;
            let note_block_line = li.saturating_sub(offset_lines);
            let measure_end = measure_start + measure_len;

            for mut note in split_notes(
                p.part_events(name), subs,
                                        p.part_sample_offset_u32(name),
            ) {
                if note.start_row < *measure_start || note.start_row >= measure_end { continue; }

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
                if !active && !still_fading { continue; }

                let (display_start, display_end) = if is_current {
                    (note.start_row, note.end_row)
                } else if active {
                    (0.0, (note.end_row - *offset_rows).min(rpmeasure))
                } else {
                    (0.0, 0.0)
                };

                let original_start = note.start_row as i64;

                let dn = DisplayNote {
                    note, subs,
                    display_start, display_end,
                    value_offset: *offset_rows,
                    source_name: name.clone(),
                };

                let note_local_row = dn.value_offset + current_row;
                let lookup_row = note_local_row.max(original_start as f64);

                let playing = dn.display_end > 0.0
                && dn.display_start <= current_row
                && current_row < dn.display_end;
                let played = !playing
                && (dn.display_end == 0.0 || current_row >= dn.display_end);

                let note_state = if playing {
                    NoteState::Playing
                } else if played {
                    NoteState::Played
                } else {
                    NoteState::Waiting
                };

                let pitch_hz = value_at(&dn.note.pitch_points, lookup_row).unwrap_or(C5_HZ);
                let vol_note = value_at(&dn.note.vol_points, lookup_row).unwrap_or(100.0);

                let is_motion_highlight = state
                .motion_highlights
                .contains(&(note_block_line, original_start));
                let is_note_muted = muted_notes.contains(&(
                    name.clone(), note_block_line, original_start,
                ));

                let style = note_style(
                    pitch_hz, vol_note, note_state,
                    theme, &synth, is_motion_highlight,
                );

                notes.push(PreparedNote {
                    dn, block_line: note_block_line,
                    start_row: original_start,
                    lookup_row, note_state, style,
                    is_muted: is_note_muted,
                });
            }
        }

        // Canonical order: value_offset desc, then start_row asc, then pitch asc.
        notes.sort_by(|a, b| {
            b.dn.value_offset.partial_cmp(&a.dn.value_offset).unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.dn.note.start_row.partial_cmp(&b.dn.note.start_row)
            .unwrap_or(std::cmp::Ordering::Equal))
            .then_with(|| {
                let pa = a.dn.note.pitch_points.first().map(|p| p.value as i64).unwrap_or(0);
                let pb = b.dn.note.pitch_points.first().map(|p| p.value as i64).unwrap_or(0);
                pa.cmp(&pb)
            })
        });

        let is_playing_now = notes.iter().any(|n| {
            n.dn.display_end > 0.0
            && n.dn.display_start <= current_row
            && current_row < n.dn.display_end
        });

        parts.push(PreparedPart {
            path: path.clone(),
                   first_name, shim_names,
                   is_muted, is_soloed_out,
                   vol, show_vol, notes, is_playing_now,
        });
    }

    Some(PreparedMeasure { parts, current_row, rpmeasure })
}
