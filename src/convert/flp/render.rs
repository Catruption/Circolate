// src/convert/flp/render.rs
//! Part rendering: notes in a measure → Trackscript `@beats/subs[...]`
//! block, one part per channel per measure, with dedup across identical
//! renders.

use std::collections::{BTreeMap, HashMap};

use crate::convert::{fmt, identifier, note_name, pos_str};
use crate::sources::flp::Channel;

use super::place::Placed;
use super::{MAX_FULL_BEATS, PLAY_FULL_SAMPLE};

pub(super) struct Part {
    pub(super) name: String,
    pub(super) iid: u16,
    pub(super) rendered: String,
    pub(super) kind: &'static str,
}

pub(super) struct EmbeddedSample {
    pub(super) wav: Vec<u8>,
    pub(super) root_key: Option<u8>,
    pub(super) files_found: usize,
    pub(super) raw_len: usize,
}

#[derive(Clone, Copy)]
pub(super) struct NoteStyle {
    pub(super) transpose: i32,
    pub(super) full_secs: Option<f64>,
    pub(super) bpm: f64,
    pub(super) vol_scale: f64,
    pub(super) pan_offset: f64,
    pub(super) sample_rate: u32,
}

impl NoteStyle {
    pub(super) fn neutral(bpm: f64) -> Self {
        NoteStyle {
            transpose: 0,
            full_secs: None,
            bpm,
            vol_scale: 1.0,
            pan_offset: 0.0,
            sample_rate: 0,
        }
    }
}

fn vel_to_pct(v: u8) -> f64 {
    if v == 0 {
        0.0
    } else {
        (v as f64 * 100.0 / 127.0).min(100.0)
    }
}

fn pan_norm(p: u8) -> f64 {
    ((p as f64 - 64.0) / 64.0).clamp(-1.0, 1.0)
}

fn render_part(
    notes: &[&Placed],
    measure_start: u32,
    subs: u32,
    ppq: u32,
    style: NoteStyle,
    source_path: &str,
) -> String {
    let mut by_row: BTreeMap<i64, Vec<&Placed>> = BTreeMap::new();
    for p in notes {
        let ticks = p.abs as i64 - measure_start as i64;
        let row = ticks as f64 / ppq as f64 * subs as f64;
        let key = (row * 1000.0).round() as i64;
        by_row.entry(key).or_default().push(p);
    }

    let mut pieces: Vec<String> = Vec::new();
    for (row_key, mut row_notes) in by_row {
        row_notes.sort_by_key(|p| p.note.key);
        let row = row_key as f64 / 1000.0;
        for p in row_notes {
            let n = &p.note;
            let key = n.key as i32 + style.transpose;
            let mut dur_beats = (n.length as f64 / ppq as f64).max(0.05);
            if let (false, Some(secs), None) =
                (PLAY_FULL_SAMPLE, style.full_secs, p.clip_offset_ms)
                {
                    let ratio = 2f64.powf((key as f64 - 60.0) / 12.0);
                    let full = (secs / ratio * style.bpm / 60.0).min(MAX_FULL_BEATS);
                    dur_beats = dur_beats.max(full);
                }
                let mut fields = vec![
                    pos_str(row, subs),
                    format!("#{}", note_name(key)),
                        format!("~{}", fmt(dur_beats, 2)),
                ];
                let vol = (vel_to_pct(n.velocity) * style.vol_scale).clamp(0.0, 100.0);
                if (vol - 100.0).abs() > 0.5 {
                    fields.push(format!("%{}", fmt(vol, 1)));
                }
                let pan = (pan_norm(n.pan) + style.pan_offset).clamp(-1.0, 1.0);
                if pan.abs() > 0.01 {
                    fields.push(format!("¶{}", fmt(pan, 2)));
                }
                if let Some(ms) = p.clip_offset_ms {
                    if ms > 0.0 && style.sample_rate > 0 && !source_path.is_empty() {
                        let samples =
                        (ms as f64 * style.sample_rate as f64 / 1000.0).round() as u32;
                        if samples > 0 {
                            fields.push(format!("ƒ'{}'({})", source_path, samples));
                        }
                    }
                }
                pieces.push(format!("<{}>", fields.join(",")));
        }
    }

        pieces.join(",\n")
}

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_part(
    seen: &mut HashMap<(u16, String), usize>,
                        parts: &mut Vec<Part>,
                        part_name_used: &mut HashMap<String, u32>,
                        chan: &Channel,
                        notes: &[&Placed],
                        m_start: u32,
                        subs: u32,
                        ppq: u32,
                        kind: &'static str,
                        style: NoteStyle,
                        source_path: &str,
) -> usize {
    let rendered = render_part(notes, m_start, subs, ppq, style, source_path);
    let key = (chan.iid, rendered.clone());
    if let Some(&idx) = seen.get(&key) {
        return idx;
    }
    let base_raw = if chan.name.is_empty() {
        format!("Ch{:02}", chan.iid)
    } else {
        chan.name.clone()
    };
    let base = identifier(&base_raw);
    let count_ref = part_name_used.entry(base.clone()).or_insert(0);
    *count_ref += 1;
    let count = *count_ref;
    let name = if count == 1 {
        base
    } else {
        format!("{}{:02}", base, count)
    };
    let idx = parts.len();
    seen.insert(key, idx);
    parts.push(Part {
        name,
        iid: chan.iid,
        rendered,
        kind,
    });
    idx
}
