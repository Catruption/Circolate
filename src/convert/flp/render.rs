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
    pub(super) all_zones: Vec<(Vec<u8>, Option<u8>)>,
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

/// True if this audio clip is a plain one-shot: placed at the start of
/// the sample (offset 0), playing to the sample's natural end (no trim
/// on the end either).  These clips play the whole sample; the visual
/// playlist length is only where the user dragged the boundary.
///
/// FL tags one-shot sampler channels with `sampler_flags: 2` (LoadRegions)
/// and often places a short visual clip that has no relation to the
/// sample's actual playback length.  A 30-tick clip on a 2.88-second
/// sample is a marker, not a truncation.
fn is_untrimmed_audio_clip(p: &Placed) -> bool {
    let start_is_zero = p.clip_offset_ms.map_or(false, |o| o <= 0.0);
    let end_is_open = match p.clip_end_ms {
        None => true,
        Some(e) => e < 0.0,
    };
    start_is_zero && end_is_open
}

/// The sample's native tempo for an audio clip that FL has time-stretched,
/// or `None` if the clip plays at its natural speed.
///
/// Only fires when *both* ends of the clip are trimmed.  An untrimmed
/// clip plays natural-length: the sample rings out for its own duration
/// and the visual clip length is just where the user dragged it.
fn source_bpm(p: &Placed, ppq: u32, style: &NoteStyle) -> Option<f64> {
    let start_ms = p.clip_offset_ms? as f64;
    let ticks = p.note.length;
    if ticks == 0 || ppq == 0 || style.bpm <= 0.0 {
        return None;
    }

    let start_trimmed = start_ms > 0.0;
    let end_trimmed = matches!(p.clip_end_ms, Some(e) if e >= 0.0);
    if !(start_trimmed && end_trimmed) {
        return None;
    }
    let end_ms = p.clip_end_ms? as f64;

    let clip_ms = ticks as f64 / ppq as f64 * 60_000.0 / style.bpm;
    let speed = (end_ms - start_ms) / clip_ms;
    let tolerance: f64 = (1.0 / ticks as f64).max(0.005);
    if speed > 0.0 && (speed - 1.0).abs() > tolerance {
        Some(style.bpm / speed)
    } else {
        None
    }
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

            // Untrimmed audio clips play the sample's natural length,
            // not the visual clip length.  Without this, a 2.9-second
            // sample in a 0.18-second clip slot is cut off after the
            // first 6% of its audio.
            if is_untrimmed_audio_clip(p) {
                if let Some(secs) = style.full_secs {
                    let full_beats = secs * style.bpm / 60.0;
                    if full_beats > dur_beats {
                        dur_beats = full_beats;
                    }
                }
            } else if let (false, Some(secs), None) =
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

            // Time-stretched audio clip: declare the sample's own tempo.
            if let Some(bpm) = source_bpm(p, ppq, &style) {
                fields.push(format!("|{}", fmt(bpm, 2)));
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