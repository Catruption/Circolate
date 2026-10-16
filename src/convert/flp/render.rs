// src/convert/flp/render.rs
//! Part rendering: notes in a measure → Trackscript `<...>` events,
//! combined into one section per measure by the caller.

use std::collections::BTreeMap;

use crate::convert::{fmt, note_name, pos_str};
use crate::formats::flp::Channel;

use super::place::Placed;
use super::{MAX_FULL_BEATS, PLAY_FULL_SAMPLE};

pub(super) struct EmbeddedSample {
    pub(super) wav: Vec<u8>,
    pub(super) root_key: Option<u8>,
    pub(super) files_found: usize,
    pub(super) raw_len: usize,
    /// Every zone found: (wav bytes, root key).  Zone 0 is the primary.
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

/// The sample's native tempo for an audio clip that FL has time-stretched,
/// or `None` if the clip plays at its natural speed.
fn source_bpm(p: &Placed, ppq: u32, style: &NoteStyle) -> Option<f64> {
    let start_ms = p.clip_offset_ms? as f64;
    let ticks = p.note.length;
    if ticks == 0 || ppq == 0 || style.bpm <= 0.0 {
        return None;
    }
    let clip_ms = ticks as f64 / ppq as f64 * 60_000.0 / style.bpm;

    let end_ms = match p.clip_end_ms {
        Some(e) if e >= 0.0 => e as f64,
        _ => style.full_secs? * 1000.0,
    };

    let speed = (end_ms - start_ms) / clip_ms;
    let tolerance: f64 = (1.0 / ticks as f64).max(0.005);
    if (0.25..=4.0).contains(&speed) && (speed - 1.0).abs() > tolerance {
        Some(style.bpm / speed)
    } else {
        None
    }
}

pub(super) fn render_part(
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

            let (gain, vol_max) = if p.clip_offset_ms.is_some() {
                (p.clip_gain as f64, 400.0)
            } else {
                (1.0, 100.0)
            };
            let vol = (vel_to_pct(n.velocity) * style.vol_scale * gain).clamp(0.0, vol_max);
            if (vol - 100.0).abs() > 0.5 {
                fields.push(format!("%{}", fmt(vol, 1)));
            }

            let pan = (pan_norm(n.pan) + style.pan_offset).clamp(-1.0, 1.0);
            if pan.abs() > 0.01 {
                fields.push(format!("¶{}", fmt(pan, 2)));
            }

            // Extended per-note fields, emitted only when off default.
            // Defaults from the FLP format: fine_pitch 120 (centred),
            // release 64, mod_x 128, mod_y 128.  The trackscript form
            // stores the signed delta from each default.
            let fp_delta = n.fine_pitch as i16 - 120;
            if fp_delta != 0 {
                fields.push(format!("¢{:+}", fp_delta));
            }
            let rel_delta = n.release as i16 - 64;
            if rel_delta != 0 {
                fields.push(format!("₨{:+}", rel_delta));
            }
            let mx_delta = n.mod_x as i16 - 128;
            if mx_delta != 0 {
                fields.push(format!("₥{:+}", mx_delta));
            }
            let my_delta = n.mod_y as i16 - 128;
            if my_delta != 0 {
                fields.push(format!("¥{:+}", my_delta));
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

            if let Some(bpm) = source_bpm(p, ppq, &style) {
                fields.push(format!("|{}", fmt(bpm, 2)));
            }

            pieces.push(format!("<{}>", fields.join(",")));
        }
    }

    pieces.join(",")
}

// `Channel` is imported for the type in the signature of the
// `render_part` caller, but is otherwise unused inside this module.
// Silence the warning if nothing here mentions it.
#[allow(dead_code)]
fn _assert_channel_in_scope(_: &Channel) {}