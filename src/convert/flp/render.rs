// src/convert/flp/render.rs
//! Part rendering: notes in a measure → Trackscript `<...>` events,
//! combined into one section per measure by the caller.

use std::collections::BTreeMap;

use crate::convert::{fmt, note_name, pos_str};
use crate::formats::flp::{Channel, ENV_VOLUME};

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
pub(super) struct VolumeEnv {
    pub(super) attack_ms: i32,
    pub(super) hold_ms: i32,
    pub(super) decay_ms: i32,
    pub(super) sustain: f64,
    pub(super) release_ms: i32,
    pub(super) amount: f64,
}

#[derive(Clone, Copy)]
pub(super) struct NoteStyle {
    pub(super) transpose: i32,
    pub(super) full_secs: Option<f64>,
    pub(super) bpm: f64,
    pub(super) vol_scale: f64,
    pub(super) pan_offset: f64,
    pub(super) sample_rate: u32,
    pub(super) sample_start: u32,
    pub(super) vol_env: Option<VolumeEnv>,
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
            sample_start: 0,
            vol_env: None,
        }
    }
}

pub(super) fn extract_vol_env(chan: &Channel) -> Option<VolumeEnv> {
    let e = chan.envelope(ENV_VOLUME)?;
    if e.env_enabled == 0 {
        return None;
    }
    let amount = if e.env_amount <= 0 {
        100.0
    } else {
        e.env_amount as f64
    };
    Some(VolumeEnv {
        attack_ms: e.env_attack,
         hold_ms: e.env_hold,
         decay_ms: e.env_decay,
         sustain: (e.env_sustain as f64 / 100.0).clamp(0.0, 1.0),
         release_ms: e.env_release,
         amount: (amount / 100.0).clamp(0.0, 1.0),
    })
}

/// Channel pitch knob (event 219, `Levels.pitch_shift`, assumed to be cents)
/// split into whole semitones plus the leftover cents.  Values outside
/// ±4800 cents are treated as misdecoded and ignored.
pub(super) fn channel_pitch(chan: &Channel) -> (i32, i32) {
    let cents = chan.levels.as_ref().map_or(0, |l| l.pitch_shift);
    if cents == 0 || cents.abs() > 4800 {
        return (0, 0);
    }
    let semis = (cents as f64 / 100.0).round() as i32;
    (semis, cents - semis * 100)
}

/// `FLP_ENV_LEGACY=1` restores the old volume-envelope waypoints (no
/// attack ramp, decay/release as bare waypoints).
fn env_legacy() -> bool {
    std::env::var_os("FLP_ENV_LEGACY").is_some()
}

fn ms_to_beats(ms: i32, bpm: f64) -> f64 {
    if ms <= 0 || bpm <= 0.0 {
        return 0.0;
    }
    (ms as f64 / 60000.0) * bpm
}

/// Milliseconds → beats for an `f32` fade length; ignores anything under
/// 1 ms (nothing audible, and not worth a waypoint).
fn ms_to_beats_f(ms: f32, bpm: f64) -> f64 {
    if !(ms >= 1.0) || bpm <= 0.0 {
        return 0.0;
    }
    (ms as f64 / 60000.0) * bpm
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
    // FL squeezes the clip's source window into the clip's length, so any
    // mismatch is a time-stretch.  The old 0.25..4 limit rejected heavy
    // stretches (e.g. a 2.9 s sample squeezed into 0.18 s, speed ~16).
    if (1.0 / 64.0..=64.0).contains(&speed) && (speed - 1.0).abs() > tolerance {
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

                // Audio-clip fades (FL's clip fade handles).  If both are
                // set and don't fit in the clip, they shrink proportionally.
                let (fade_in_b, fade_out_b) = if p.clip_offset_ms.is_some() {
                    let mut fi = ms_to_beats_f(p.clip_fade_in_ms, style.bpm);
                    let mut fo = ms_to_beats_f(p.clip_fade_out_ms, style.bpm);
                    let total = fi + fo;
                    if total > dur_beats && total > 0.0 {
                        let k = dur_beats / total;
                        fi *= k;
                        fo *= k;
                    }
                    (fi, fo)
                } else {
                    (0.0, 0.0)
                };

                // A fade-in starts the note silent; the waypoint below
                // ramps it up to `vol` over the fade length.
                // `level` is where the note settles once any attack or
                // fade-in is over: the envelope's peak when a volume
                // envelope is active, otherwise the plain volume.
                let legacy = env_legacy();
                let env_attack_b = style
                .vol_env
                .map_or(0.0, |e| ms_to_beats(e.attack_ms, style.bpm));
                let level = match style.vol_env {
                    Some(e) if !legacy => vol * e.amount,
                    _ => vol,
                };
                let start_vol = if fade_in_b > 0.0 {
                    0.0
                } else if env_attack_b > 0.0 && !legacy {
                    0.0
                } else {
                    level
                };
                if (start_vol - 100.0).abs() > 0.5 {
                    fields.push(format!("%{}", fmt(start_vol, 1)));
                }

                let pan = (pan_norm(n.pan) + style.pan_offset).clamp(-1.0, 1.0);
                if pan.abs() > 0.01 {
                    fields.push(format!("¶{}", fmt(pan, 2)));
                }

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

                let mut start_samples = style.sample_start;
                if let Some(ms) = p.clip_offset_ms {
                    if ms > 0.0 && style.sample_rate > 0 {
                        start_samples = start_samples.saturating_add(
                            (ms as f64 * style.sample_rate as f64 / 1000.0).round() as u32
                        );
                    }
                }

                if start_samples > 0 && style.sample_rate > 0 && !source_path.is_empty() {
                    fields.push(format!("ƒ'{}'({})", source_path, start_samples));
                }

                if let Some(bpm) = source_bpm(p, ppq, &style) {
                    fields.push(format!("|{}", fmt(bpm, 2)));
                }

                let mut note_str = format!("<{}>", fields.join(","));

                // Volume waypoints: SF2-style channel envelope plus clip
                // fades.  Positions are in rows (beats * subs), collected
                // and sorted so the part's waypoint chain stays in order.
                let mut wps: Vec<(f64, String)> = Vec::new();
                let b2r = |beats: f64| row + beats * subs as f64;

                if let Some(env) = style.vol_env {
                    let a_b = ms_to_beats(env.attack_ms, style.bpm);
                    let h_b = ms_to_beats(env.hold_ms, style.bpm);
                    let d_b = ms_to_beats(env.decay_ms, style.bpm);
                    let r_b = ms_to_beats(env.release_ms, style.bpm);

                    let peak = vol * env.amount;
                    let sus = peak * env.sustain;

                    if legacy {
                        if a_b > 0.0 {
                            wps.push((b2r(a_b), format!("%{}", fmt(peak, 1))));
                        }
                        if h_b > 0.0 {
                            wps.push((b2r(a_b + h_b), format!("%{}", fmt(peak, 1))));
                        }
                        if d_b > 0.0 {
                            wps.push((b2r(a_b + h_b + d_b), format!("%{}", fmt(sus, 1))));
                        }
                        if r_b > 0.0 {
                            wps.push((b2r(dur_beats + r_b), "%0".to_string()));
                        }
                    } else {
                        // Same waypoint form as the clip fades: hold the
                        // previous value until the position, then ramp to
                        // the target over `~`.  The note starts silent when
                        // there is an attack (see `start_vol`).  A clip
                        // fade-in takes over from the attack ramp.
                        if a_b > 0.0 && fade_in_b <= 0.0 {
                            wps.push((
                                b2r(0.0),
                                      format!("%{},~{}", fmt(peak, 1), fmt(a_b, 3)),
                            ));
                        }
                        // Hold keeps the peak; decay starts after it.
                        if d_b > 0.0 {
                            wps.push((
                                b2r(a_b + h_b),
                                      format!("%{},~{}", fmt(sus, 1), fmt(d_b, 3)),
                            ));
                        }
                        // Release ramps down from the note's end.
                        if r_b > 0.0 {
                            wps.push((
                                b2r(dur_beats),
                                      format!("%0,~{}", fmt(r_b, 3)),
                            ));
                        }
                    }
                }

                // Clip fades.  A waypoint holds the previous value until its
                // position and then ramps to its target over `~`, so each
                // fade is one waypoint with an explicit ramp length.
                if fade_in_b > 0.0 {
                    wps.push((
                        b2r(0.0),
                              format!("%{},~{}", fmt(level, 1), fmt(fade_in_b, 3)),
                    ));
                }
                if fade_out_b > 0.0 {
                    wps.push((
                        b2r(dur_beats - fade_out_b),
                              format!("%0,~{}", fmt(fade_out_b, 3)),
                    ));
                }

                // Slicex slide notes: keep the slice, glide the pitch.  Each
                // is a pitch waypoint at the slide's start, ramping to the
                // target key over the slide's length.
                for b in &p.bends {
                    if b.at_ticks >= n.length {
                        continue;
                    }
                    let at = b.at_ticks as f64 / ppq as f64;
                    let len = (b.len_ticks as f64 / ppq as f64).max(0.01);
                    wps.push((
                        b2r(at),
                              format!(
                                  "#{},~{}",
                                  note_name(b.key as i32 + style.transpose),
                                      fmt(len, 3)
                              ),
                    ));
                }

                wps.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
                for (r, v) in wps {
                    note_str.push_str(&format!("&<{},{}>", pos_str(r, subs), v));
                }

                pieces.push(note_str);
        }
    }

    pieces.join(",")
}

#[allow(dead_code)]
fn _assert_channel_in_scope(_: &Channel) {}
