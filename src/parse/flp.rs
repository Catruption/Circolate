//! FL Studio project (.flp) reader — byte walking and decoding.
//!
//! Pure functions: given the same bytes, produce the same `Project`.
//! One exception: `parse_file` reads from disk. See the TODO on it.
//!
//! Design goals:
//! 1. Read the event stream top-to-bottom without ever losing alignment.
//!    All 192–255 events carry a varint length; versioned sizes do not
//!    affect alignment.
//! 2. Decode every field that circolate's output format can represent.
//! 3. Preserve raw bytes for anything we do not decode, and report what
//!    was skipped, so a downstream converter can grow without re-reading
//!    the original FLP.

use anyhow::{anyhow, bail, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::model::flp::{
    Channel, ChannelDelay, EnvelopeLfo, EventCoverage, Insert, LevelAdjusts, Levels,
    Note, Pattern, PlaylistItem, Plugin, Project, RawEvent, Tracking,
};
use crate::spec::flp::events::{
    CHANNEL_TYPE_NAMES, HANDLED_EVENT_IDS, NOTE_SIZE,
    EV_CHAN_DELAY, EV_CHAN_ENABLED, EV_CHAN_ENV_LFO, EV_CHAN_FINE_TUNE,
    EV_CHAN_LEVEL_ADJUSTS, EV_CHAN_LEVELS, EV_CHAN_MIXER_TRACK,
    EV_CHAN_PARAMETERS, EV_CHAN_POLYPHONY, EV_CHAN_ROOT_NOTE,
    EV_CHAN_SAMPLER_FLAGS, EV_CHAN_SAMPLE_END, EV_CHAN_SAMPLE_START,
    EV_CHAN_TRACKING, EV_CHAN_TYPE, EV_COLOR, EV_FINE_TEMPO, EV_NEW_CHAN,
    EV_NEW_PAT, EV_NEW_PLUGIN, EV_PAT_NOTES, EV_PLAYLIST, EV_PLUGIN_PARAMS,
    EV_REMOTE_CONTROLLER, EV_TEMPO_OLD, EV_TEXT_AUTHOR, EV_TEXT_CHAN_NAME,
    EV_TEXT_COMMENT, EV_TEXT_DATA_PATH, EV_TEXT_DEF_PLUGIN, EV_TEXT_FX_NAME,
    EV_TEXT_GENRE, EV_TEXT_PAT_NAME, EV_TEXT_PLUGIN_NAME, EV_TEXT_SAMPLE,
    EV_TEXT_TITLE, EV_TEXT_VERSION, EV_TRACK_DATA,
};

// ── little-endian field helpers ────────────────────────────────────────

#[inline]
fn i32le(b: &[u8], o: usize) -> i32 {
    i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

#[inline]
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

#[inline]
fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{:02x}", x)).collect::<Vec<_>>().join(" ")
}

// ── substructure decoders ─────────────────────────────────────────────

fn decode_env_lfo(b: &[u8]) -> Option<EnvelopeLfo> {
    if b.len() < 68 {
        return None;
    }
    Some(EnvelopeLfo {
        flags: i32le(b, 0),
        env_enabled: i32le(b, 4),
        env_predelay: i32le(b, 8),
        env_attack: i32le(b, 12),
        env_hold: i32le(b, 16),
        env_decay: i32le(b, 20),
        env_sustain: i32le(b, 24),
        env_release: i32le(b, 28),
        env_amount: i32le(b, 32),
        lfo_predelay: u32le(b, 36),
        lfo_attack: u32le(b, 40),
        lfo_amount: i32le(b, 44),
        lfo_speed: u32le(b, 48),
        lfo_shape: i32le(b, 52),
        env_attack_tension: i32le(b, 56),
        env_decay_tension: i32le(b, 60),
        env_release_tension: i32le(b, 64),
    })
}

fn decode_levels(b: &[u8]) -> Option<Levels> {
    if b.len() < 24 {
        return None;
    }
    Some(Levels {
        pan: i32le(b, 0),
        volume: u32le(b, 4),
        pitch_shift: i32le(b, 8),
        filter_mod_x: u32le(b, 12),
        filter_mod_y: u32le(b, 16),
        filter_type: u32le(b, 20),
    })
}

fn decode_tracking(b: &[u8]) -> Option<Tracking> {
    if b.len() < 16 {
        return None;
    }
    Some(Tracking {
        middle_value: u32le(b, 0),
        pan: i32le(b, 4),
        mod_x: i32le(b, 8),
        mod_y: i32le(b, 12),
    })
}

fn decode_level_adjusts(b: &[u8]) -> Option<LevelAdjusts> {
    if b.len() < 20 {
        return None;
    }
    Some(LevelAdjusts {
        pan: i32le(b, 0),
        volume: u32le(b, 4),
        mod_x: i32le(b, 12),
        mod_y: i32le(b, 16),
    })
}

fn decode_channel_delay(b: &[u8]) -> Option<ChannelDelay> {
    if b.len() < 20 {
        return None;
    }
    Some(ChannelDelay {
        feedback: i32le(b, 0),
        pan: i32le(b, 4),
        pitch_shift: i32le(b, 8),
        echoes: i32le(b, 12),
        time: i32le(b, 16),
    })
}

fn channel_type_name(id: u8) -> String {
    CHANNEL_TYPE_NAMES
        .iter()
        .find(|(k, _)| *k == id)
        .map(|(_, v)| v.to_string())
        .unwrap_or_else(|| format!("type{id}"))
}

// ── text decoding ─────────────────────────────────────────────────────

fn decode_utf16le(raw: &[u8]) -> String {
    let cap = raw.len() & !1;
    let mut n = 0;
    while n + 1 < cap {
        if raw[n] == 0 && raw[n + 1] == 0 {
            break;
        }
        n += 2;
    }
    let units: Vec<u16> = (0..n)
        .step_by(2)
        .map(|i| u16::from_le_bytes([raw[i], raw[i + 1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

fn looks_like_utf16le(raw: &[u8]) -> bool {
    if raw.len() < 4 || raw.len() % 2 != 0 {
        return false;
    }
    if raw.len() >= 2 && raw[0] == 0xFF && raw[1] == 0xFE {
        return true;
    }
    let check_len = raw.len().min(64);
    let mut odd_zeros = 0usize;
    let mut odd_total = 0usize;
    let mut i = 1;
    while i < check_len {
        odd_total += 1;
        if raw[i] == 0 {
            odd_zeros += 1;
        }
        i += 2;
    }
    odd_total >= 2 && odd_zeros * 4 >= odd_total * 3
}

fn text(ev: u8, raw: &[u8]) -> String {
    if raw.is_empty() {
        return String::new();
    }
    if ev == EV_TEXT_VERSION {
        let nul = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        return String::from_utf8_lossy(&raw[..nul]).into_owned();
    }
    if looks_like_utf16le(raw) {
        return decode_utf16le(raw);
    }
    let nul = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    String::from_utf8_lossy(&raw[..nul]).into_owned()
}

// ── VST path extraction ───────────────────────────────────────────────

fn vst_path_of(blob: &[u8]) -> String {
    let n = blob.len();
    let extensions: [&[u8]; 3] = [b".vst3", b".dll", b".vst"];
    let mut i = 0;
    while i + 3 <= n {
        if !blob[i].is_ascii_alphabetic() || blob[i + 1] != b':' || blob[i + 2] != b'\\' {
            i += 1;
            continue;
        }
        let mut j = i + 3;
        while j < n && blob[j] >= 0x20 && blob[j] <= 0x7e {
            j += 1;
        }
        let cand = &blob[i..j];
        let mut best: Option<usize> = None;
        for ext in extensions {
            let elen = ext.len();
            if cand.len() < elen {
                continue;
            }
            for k in 0..=(cand.len() - elen) {
                if cand[k..k + elen].eq_ignore_ascii_case(ext) {
                    let end = k + elen;
                    if best.map_or(true, |b| end < b) {
                        best = Some(end);
                    }
                    break;
                }
            }
        }
        if let Some(end) = best {
            return String::from_utf8_lossy(&cand[..end]).into_owned();
        }
        i += 1;
    }
    String::new()
}

// ── note decoding ─────────────────────────────────────────────────────

fn decode_notes(raw: &[u8]) -> Vec<Note> {
    let mut out = Vec::with_capacity(raw.len() / NOTE_SIZE);
    let mut i = 0;
    while i + NOTE_SIZE <= raw.len() {
        let b = &raw[i..i + NOTE_SIZE];
        let position = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        let flags = u16::from_le_bytes([b[4], b[5]]);
        let rack = u16::from_le_bytes([b[6], b[7]]);
        let length = u32::from_le_bytes([b[8], b[9], b[10], b[11]]);
        let key = u16::from_le_bytes([b[12], b[13]]);
        let group = u16::from_le_bytes([b[14], b[15]]);
        out.push(Note {
            position,
            length,
            key,
            channel: rack,
            velocity: b[21],
            pan: b[20],
            fine_pitch: b[16],
            release: b[18],
            mod_x: b[22],
            mod_y: b[23],
            flags,
            group,
        });
        i += NOTE_SIZE;
    }
    out
}

// ── playlist decoding ─────────────────────────────────────────────────

fn version_major(v: &str) -> Option<u32> {
    v.split('.').next()?.trim().parse().ok()
}

fn decode_playlist(raw: &[u8], major: Option<u32>) -> Vec<PlaylistItem> {
    let preferred = match major {
        Some(m) if m >= 21 => 60,
        Some(_) => 32,
        None => {
            if raw.len() % 60 == 0 {
                60
            } else {
                32
            }
        }
    };
    let size = if raw.len() % preferred == 0 {
        preferred
    } else if raw.len() % 60 == 0 {
        60
    } else if raw.len() % 32 == 0 {
        32
    } else {
        preferred
    };
    let mut items = Vec::with_capacity(raw.len() / size);
    let mut i = 0;
    while i + size <= raw.len() {
        let rec = &raw[i..i + size];
        let pos = u32::from_le_bytes([rec[0], rec[1], rec[2], rec[3]]);
        let base = u16::from_le_bytes([rec[4], rec[5]]);
        let idx = u16::from_le_bytes([rec[6], rec[7]]);
        let length = u32::from_le_bytes([rec[8], rec[9], rec[10], rec[11]]);
        let rv = u16::from_le_bytes([rec[12], rec[13]]);
        let flags = u16::from_le_bytes([rec[18], rec[19]]);

        if base != 0x5000 {
            i += size;
            continue;
        }

        let start_offset_ms = if size >= 28 {
            f32::from_le_bytes([rec[24], rec[25], rec[26], rec[27]])
        } else {
            0.0
        };
        let end_offset_ms = if size >= 32 {
            f32::from_le_bytes([rec[28], rec[29], rec[30], rec[31]])
        } else {
            0.0
        };

        let (kind, ref_id) = if idx > base {
            ("pattern".to_string(), idx - base)
        } else {
            ("channel".to_string(), idx)
        };

        let clip_start_ticks = if kind == "pattern" && size >= 28 {
            let s = i32::from_le_bytes([rec[24], rec[25], rec[26], rec[27]]);
            if s > 0 {
                s as u32
            } else {
                0
            }
        } else {
            0
        };

        let f32_at = |o: usize| -> f32 {
            if size >= o + 4 {
                f32::from_le_bytes([rec[o], rec[o + 1], rec[o + 2], rec[o + 3]])
            } else {
                0.0
            }
        };
        let fade_ms = |o: usize| -> f32 {
            let v = f32_at(o);
            if kind == "channel" && v.is_finite() && v > 0.0 && v < 600_000.0 {
                v
            } else {
                0.0
            }
        };
        let curve = |o: usize| -> f32 {
            let v = f32_at(o);
            if kind == "channel" && v.is_finite() {
                v
            } else {
                0.0
            }
        };
        let fade_in_ms = fade_ms(36);
        let fade_in_curve = curve(40);
        let fade_out_ms = fade_ms(44);
        let fade_out_curve = curve(48);

        items.push(PlaylistItem {
            position: pos,
            length,
            track: 500i32 - rv as i32,
            kind,
            ref_id,
            flags,
            start_offset_ms,
            end_offset_ms,
            clip_start_ticks,
            fade_in_ms,
            fade_out_ms,
            fade_in_curve,
            fade_out_curve,
            raw: rec.to_vec(),
        });
        i += size;
    }
    items
}

// ── event stream ──────────────────────────────────────────────────────

fn read_events(data: &[u8]) -> Result<(u16, Vec<(u8, Vec<u8>)>, Vec<String>)> {
    if data.len() < 14 || &data[0..4] != b"FLhd" {
        bail!("not an FL Studio project (missing FLhd)");
    }
    let hdr_len = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
    let ppq = u16::from_le_bytes([data[12], data[13]]);

    let mut p = 8 + hdr_len;
    if p + 8 > data.len() || &data[p..p + 4] != b"FLdt" {
        bail!("missing FLdt data chunk");
    }
    let length =
    u32::from_le_bytes([data[p + 4], data[p + 5], data[p + 6], data[p + 7]]) as usize;
    p += 8;
    let end = (p + length).min(data.len());

    let mut events = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    while p < end {
        let ev_off = p;
        let ev = data[p];
        p += 1;

        let size = if ev < 64 {
            1
        } else if ev < 128 {
            2
        } else if ev < 192 {
            4
        } else {
            let mut size: usize = 0;
            let mut shift: u32 = 0;
            loop {
                if p >= end {
                    warnings.push(format!(
                        "truncated varint length for event {:#04x} at offset {:#x}; stopping parse",
                        ev, ev_off
                    ));
                    return Ok((ppq, events, warnings));
                }
                let b = data[p];
                p += 1;
                size |= ((b & 0x7F) as usize) << shift;
                shift += 7;
                if b & 0x80 == 0 {
                    break;
                }
                if shift > 28 {
                    bail!("event length overflow at offset {:#x}", ev_off);
                }
            }
            size
        };

        if p + size > end {
            let have = end.saturating_sub(p);
            warnings.push(format!(
                "truncated event {:#04x} at offset {:#x}: needs {} byte(s), {} available; stopping parse",
                ev, ev_off, size, have
            ));
            return Ok((ppq, events, warnings));
        }

        events.push((ev, data[p..p + size].to_vec()));
        p += size;
    }

    if p != end {
        warnings.push(format!(
            "event stream ended at offset {:#x}, expected {:#x} ({} bytes unread); \
             event sizes are likely misparsed",
            p, end, end.saturating_sub(p)
        ));
    }

    Ok((ppq, events, warnings))
}

// ── main parse ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
enum PluginRef {
    Channel(usize),
    Insert(u32, usize),
}

impl PluginRef {
    fn get_mut<'a>(&self, prj: &'a mut Project) -> Option<&'a mut Plugin> {
        match *self {
            PluginRef::Channel(i) => prj.channels.get_mut(i)?.plugin.as_mut(),
            PluginRef::Insert(k, j) => prj.inserts.get_mut(&k)?.plugins.get_mut(j),
        }
    }
}

pub fn parse(data: &[u8]) -> Result<Project> {
    let (ppq, events, parse_warnings) = read_events(data)?;

    if std::env::var_os("FLP_DUMP_EVENTS").is_some() {
        let mut freq: std::collections::BTreeMap<u8, Vec<usize>> =
            std::collections::BTreeMap::new();
        for (ev, raw) in &events {
            freq.entry(*ev).or_default().push(raw.len());
        }
        eprintln!("flp: {} events, {} unique IDs", events.len(), freq.len());
        for (ev, lens) in &freq {
            let sample: Vec<usize> = lens.iter().take(4).copied().collect();
            eprintln!(
                "  id={:>3} ({:#04x}) count={:>3} lens={:?}",
                ev, ev, lens.len(), sample
            );
        }
    }

    let mut prj = Project {
        ppq,
        parse_warnings,
        ..Default::default()
    };

    let mut chan_idx: Option<usize> = None;
    let mut pat_idx: Option<usize> = None;
    let mut plug_ref: Option<PluginRef> = None;
    let mut pending_def_plugin: Option<String> = None;
    let mut pending_fx_names: Vec<String> = Vec::new();
    let debug = std::env::var_os("FLP_DEBUG").is_some();

    let dump_iid: Option<u16> = std::env::var("FLP_DUMP_CHAN")
        .ok()
        .and_then(|s| s.trim().parse().ok());
    let dump_ids: Option<Vec<u8>> = std::env::var("FLP_DUMP_IDS").ok().map(|s| {
        s.split(',')
            .filter_map(|x| {
                let x = x.trim();
                if let Some(h) = x.strip_prefix("0x") {
                    u8::from_str_radix(h, 16).ok()
                } else {
                    x.parse().ok()
                }
            })
            .collect()
    });

    for (ev, raw) in &events {
        if let (Some(target), Some(i)) = (dump_iid, chan_idx) {
            if prj.channels[i].iid == target {
                eprintln!(
                    "flp: CHAN{:>3} ev={:>3} ({:#04x}) len={:>3} hex={}",
                    target, *ev, *ev, raw.len(), hex(raw)
                );
            }
        }
        if let Some(ref ids) = dump_ids {
            if ids.contains(ev) {
                eprintln!(
                    "flp: EV{:>3} ({:#04x}) len={:>3} hex={}",
                    *ev, *ev, raw.len(), hex(raw)
                );
            }
        }

        match *ev {
            EV_TEXT_VERSION => prj.version = text(*ev, raw),
            EV_FINE_TEMPO if raw.len() == 4 => {
                let v = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
                prj.tempo = v as f64 / 1000.0;
            }
            EV_TEMPO_OLD if prj.tempo == 0.0 && raw.len() == 2 => {
                prj.tempo = u16::from_le_bytes([raw[0], raw[1]]) as f64;
            }
            EV_TEXT_TITLE => prj.title = text(*ev, raw),
            EV_TEXT_AUTHOR => prj.author = text(*ev, raw),
            EV_TEXT_GENRE => prj.genre = text(*ev, raw),
            EV_TEXT_COMMENT => prj.comment = text(*ev, raw),
            EV_TEXT_DATA_PATH => prj.data_path = text(*ev, raw),

            EV_NEW_CHAN if raw.len() >= 2 => {
                let iid = u16::from_le_bytes([raw[0], raw[1]]);
                if debug {
                    eprintln!(
                        "flp: NEW_CHAN iid={} raw_len={} hex={}",
                        iid, raw.len(), hex(raw)
                    );
                }
                prj.channels.push(Channel::new(iid));
                chan_idx = Some(prj.channels.len() - 1);
                pat_idx = None;
                plug_ref = None;
            }
            EV_CHAN_TYPE if raw.len() == 1 => {
                if let Some(i) = chan_idx {
                    let c = &mut prj.channels[i];
                    c.type_id = raw[0] as i16;
                    c.channel_type = channel_type_name(raw[0]);
                }
            }
            EV_CHAN_MIXER_TRACK => {
                if debug {
                    eprintln!("flp: EV178 raw={} chan_idx={:?}", hex(raw), chan_idx);
                }
                if let Some(i) = chan_idx {
                    let v = if raw.len() >= 4 {
                        (u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) & 0xFF) as u16
                    } else if raw.len() >= 2 {
                        (u16::from_le_bytes([raw[0], raw[1]]) & 0xFF) as u16
                    } else if raw.len() == 1 {
                        raw[0] as u16
                    } else {
                        0
                    };
                    prj.channels[i].insert = v;
                    if debug {
                        eprintln!(
                            "flp: chan {:>3} insert={} raw={}",
                            prj.channels[i].iid, v, hex(raw)
                        );
                    }
                }
            }
            EV_COLOR if raw.len() == 4 && pat_idx.is_none() => {
                if let Some(i) = chan_idx {
                    prj.channels[i].color =
                        Some(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]));
                }
            }
            EV_TEXT_CHAN_NAME => {
                if let Some(i) = chan_idx {
                    prj.channels[i].name = text(*ev, raw);
                }
            }
            EV_TEXT_SAMPLE => {
                let path = text(*ev, raw);
                prj.samples.push(path.clone());
                if pat_idx.is_none() {
                    if let Some(i) = chan_idx {
                        prj.channels[i].sample_path = path;
                    }
                }
            }

            EV_CHAN_ENABLED if raw.len() == 1 => {
                if let Some(i) = chan_idx {
                    prj.channels[i].enabled = Some(raw[0] != 0);
                }
            }
            EV_CHAN_LEVELS => {
                if let Some(i) = chan_idx {
                    prj.channels[i].levels = decode_levels(raw);
                }
            }
            EV_CHAN_ENV_LFO => {
                if let Some(i) = chan_idx {
                    if let Some(e) = decode_env_lfo(raw) {
                        prj.channels[i].envelopes.push(e);
                    }
                }
            }
            EV_CHAN_TRACKING => {
                if let Some(i) = chan_idx {
                    if let Some(t) = decode_tracking(raw) {
                        prj.channels[i].tracking.push(t);
                    }
                }
            }
            EV_CHAN_LEVEL_ADJUSTS => {
                if let Some(i) = chan_idx {
                    prj.channels[i].level_adjusts = decode_level_adjusts(raw);
                }
            }
            EV_CHAN_SAMPLER_FLAGS if raw.len() == 4 => {
                if let Some(i) = chan_idx {
                    let v = u32le(raw, 0);
                    prj.channels[i].sampler_flags = Some(v);
                    prj.channels[i].insert = (v & 0xFF) as u16;
                    if debug {
                        eprintln!(
                            "flp: chan {:>3} insert={} (ev143={:#x})",
                            prj.channels[i].iid, prj.channels[i].insert, v
                        );
                    }
                }
            }
            EV_CHAN_SAMPLE_START if raw.len() == 4 => {
                if let Some(i) = chan_idx {
                    prj.channels[i].sample_start = Some(u32le(raw, 0));
                }
            }
            EV_CHAN_SAMPLE_END if raw.len() == 4 => {
                if let Some(i) = chan_idx {
                    prj.channels[i].sample_end = Some(u32le(raw, 0));
                }
            }
            EV_CHAN_ROOT_NOTE if raw.len() == 4 => {
                if let Some(i) = chan_idx {
                    prj.channels[i].root_note = Some(u32le(raw, 0));
                }
            }
            EV_CHAN_FINE_TUNE if raw.len() == 4 => {
                if let Some(i) = chan_idx {
                    prj.channels[i].fine_tune = Some(i32le(raw, 0));
                }
            }
            EV_CHAN_DELAY => {
                if let Some(i) = chan_idx {
                    if let Some(d) = decode_channel_delay(raw) {
                        prj.channels[i].delay = Some(d);
                    }
                }
            }
            EV_CHAN_PARAMETERS => {
                if let Some(i) = chan_idx {
                    prj.channels[i].parameters = Some(RawEvent {
                        id: EV_CHAN_PARAMETERS,
                        len: raw.len(),
                        bytes: raw.clone(),
                    });
                }
            }
            EV_CHAN_POLYPHONY => {
                if let Some(i) = chan_idx {
                    prj.channels[i].polyphony = Some(RawEvent {
                        id: EV_CHAN_POLYPHONY,
                        len: raw.len(),
                        bytes: raw.clone(),
                    });
                }
            }
            EV_REMOTE_CONTROLLER => {
                prj.remote_controllers.push(RawEvent {
                    id: EV_REMOTE_CONTROLLER,
                    len: raw.len(),
                    bytes: raw.clone(),
                });
            }
            EV_TRACK_DATA => {
                prj.track_data.push(RawEvent {
                    id: EV_TRACK_DATA,
                    len: raw.len(),
                    bytes: raw.clone(),
                });
            }

            EV_NEW_PLUGIN => {
                let owner = if raw.len() >= 4 {
                    u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]])
                } else {
                    0
                };
                let blank = Plugin {
                    owner,
                    internal_name: pending_def_plugin.take().unwrap_or_default(),
                    wrapper_bytes: raw.clone(),
                    ..Default::default()
                };
                if owner == 0 {
                    if let Some(i) = chan_idx {
                        if prj.channels[i].plugin.is_none() {
                            prj.channels[i].plugin = Some(blank);
                            plug_ref = Some(PluginRef::Channel(i));
                        } else {
                            plug_ref = None;
                        }
                    } else {
                        plug_ref = None;
                    }
                } else {
                    let ins = prj.inserts.entry(owner).or_insert_with(|| Insert {
                        index: owner,
                        name: String::new(),
                        plugins: Vec::new(),
                    });
                    ins.plugins.push(blank);
                    let j = ins.plugins.len() - 1;
                    plug_ref = Some(PluginRef::Insert(owner, j));
                }
            }
            EV_TEXT_DEF_PLUGIN => {
                let s = text(*ev, raw);
                let mut assigned = false;
                if let Some(pr) = plug_ref {
                    if let Some(p) = pr.get_mut(&mut prj) {
                        if p.internal_name.is_empty()
                            && p.display_name.is_empty()
                            && p.state.is_empty()
                        {
                            p.internal_name = s.clone();
                            assigned = true;
                        }
                    }
                }
                pending_def_plugin = if assigned { None } else { Some(s) };
            }
            EV_TEXT_PLUGIN_NAME => {
                let s = text(*ev, raw);
                if let Some(pr) = plug_ref {
                    if let Some(p) = pr.get_mut(&mut prj) {
                        p.display_name = s.clone();
                    }
                    if let PluginRef::Channel(i) = pr {
                        if prj.channels[i].name.is_empty() {
                            prj.channels[i].name = s;
                        }
                    }
                }
            }
            EV_PLUGIN_PARAMS => {
                if let Some(pr) = plug_ref {
                    let blob = raw.clone();
                    let vst = vst_path_of(&blob);
                    if let Some(p) = pr.get_mut(&mut prj) {
                        p.state_size = blob.len();
                        p.state = blob;
                        p.vst_path = vst;
                    }
                }
            }
            EV_TEXT_FX_NAME => pending_fx_names.push(text(*ev, raw)),

            EV_NEW_PAT if raw.len() == 2 => {
                let iid = u16::from_le_bytes([raw[0], raw[1]]);
                prj.patterns.push(Pattern {
                    iid,
                    name: String::new(),
                    notes: Vec::new(),
                });
                pat_idx = Some(prj.patterns.len() - 1);
                chan_idx = None;
                plug_ref = None;
            }
            EV_TEXT_PAT_NAME => {
                if let Some(i) = pat_idx {
                    prj.patterns[i].name = text(*ev, raw);
                }
            }
            EV_PAT_NOTES => {
                if let Some(i) = pat_idx {
                    let notes = decode_notes(raw);
                    if debug {
                        eprintln!(
                            "flp: PAT_NOTES pat_idx={} iid={} {} note(s) (existing: {})",
                            i,
                            prj.patterns[i].iid,
                            notes.len(),
                            prj.patterns[i].notes.len()
                        );
                    }
                    if prj.patterns[i].notes.is_empty() {
                        prj.patterns[i].notes = notes;
                    }
                }
            }
            EV_PLAYLIST => {
                prj.playlist
                    .extend(decode_playlist(raw, version_major(&prj.version)));
            }
            _ => {}
        }
    }

    let keys: Vec<u32> = prj.inserts.keys().copied().collect();
    for (key, name) in keys.into_iter().zip(pending_fx_names.into_iter()) {
        if let Some(ins) = prj.inserts.get_mut(&key) {
            ins.name = name;
        }
    }

    // Coverage report. Every event whose ID is not in HANDLED_EVENT_IDS
    // is preserved raw and counted here, so nothing is silently lost.
    let handled: BTreeSet<u8> = HANDLED_EVENT_IDS.iter().copied().collect();
    let mut seen: BTreeSet<u8> = BTreeSet::new();
    let mut unhandled_id_counts: BTreeMap<u8, usize> = BTreeMap::new();
    for (id, raw) in &events {
        seen.insert(*id);
        if !handled.contains(id) {
            *unhandled_id_counts.entry(*id).or_insert(0) += 1;
            prj.unknown_events.push(RawEvent {
                id: *id,
                len: raw.len(),
                bytes: raw.clone(),
            });
        }
    }
    prj.coverage = EventCoverage {
        total_events: events.len(),
        unique_ids: seen.len(),
        handled_ids: seen.iter().copied().filter(|id| handled.contains(id)).collect(),
        unhandled_id_counts,
    };

    if !prj.coverage.unhandled_id_counts.is_empty() {
        let ids: Vec<String> = prj
            .coverage
            .unhandled_id_counts
            .keys()
            .map(|id| format!("{}({})", id, id))
            .collect();
        prj.parse_warnings.push(format!(
            "{} event id(s) preserved raw but not decoded: {}",
            prj.coverage.unhandled_id_counts.len(),
            ids.join(", ")
        ));
    }

    prj.events = events;
    if prj.tempo <= 0.0 {
        if let Some(tempo) = derive_tempo_from_clips(&prj) {
            prj.tempo = tempo;
            prj.parse_warnings.push(format!(
                "tempo event missing; derived {} BPM from audio clip durations",
                tempo
            ));
        }
    }
    Ok(prj)
}

fn derive_tempo_from_clips(prj: &Project) -> Option<f64> {
    let ppq = prj.ppq as f64;
    if ppq <= 0.0 {
        return None;
    }
    const MIN_SOURCE_MS: f64 = 500.0;
    const BEAT_TOLERANCE: f64 = 0.02;

    let mut candidates: std::collections::BTreeMap<i32, usize> =
        std::collections::BTreeMap::new();

    for item in &prj.playlist {
        if item.kind != "channel" || item.length == 0 {
            continue;
        }
        if item.start_offset_ms < 0.0 || item.end_offset_ms <= 0.0 {
            continue;
        }
        let window_ms = (item.end_offset_ms - item.start_offset_ms) as f64;
        if window_ms < MIN_SOURCE_MS {
            continue;
        }
        let beats = item.length as f64 / ppq;
        if beats <= 0.0 {
            continue;
        }
        let fract = beats.fract();
        if fract > BEAT_TOLERANCE && fract < 1.0 - BEAT_TOLERANCE {
            continue;
        }
        let implied_bpm = beats * 60_000.0 / window_ms;
        if !(40.0..=300.0).contains(&implied_bpm) {
            continue;
        }
        let rounded = implied_bpm.round() as i32;
        *candidates.entry(rounded).or_insert(0) += 1;
    }

    candidates
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(bpm, _)| bpm as f64)
}

/// Convenience wrapper: read an FLP file and parse it.
/// The only I/O function in this module — `parse(bytes)` is pure.
/// TODO: move to `cli/flp.rs` so this module is 100% pure.
pub fn parse_file(path: &Path) -> Result<Project> {
    let data = std::fs::read(path).map_err(|e| anyhow!("reading {}: {}", path.display(), e))?;
    parse(&data)
}