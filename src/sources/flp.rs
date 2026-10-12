// src/sources/flp.rs
//! FL Studio project (.flp) reader — Rust port of flp_parser.py.
//!
//! Reads the FLP event stream and groups it into project info, channels,
//! plugins (with their saved-state blobs kept opaque), patterns with notes,
//! mixer inserts, and the playlist.  Does not run or emulate any plugin.
//!
//! Confidence notes mirror the Python module:
//!   * Header, event framing, tempo, version, text events, channel types,
//!     note records (24 bytes) and plugin wrapper paths are well established.
//!   * Playlist items use a 60-byte record in FL 21+ (32 before that).  Only
//!     the first fields are decoded; the rest is kept as raw bytes.
//!   * Insert/effect slot ownership is read from the first dword of the
//!     NewPlugin event (0 for channels, otherwise the insert index).  Treat
//!     as inferred.
//!   * Channel sound-shaping structs (EnvelopeLFO, Levels, Tracking,
//!     LevelAdjusts) are ported from PyFLP v2.2.1 `channel.py`.  All ints
//!     are little-endian.  The 5 EnvelopeLFO events per channel arrive in
//!     stream order and map to (Panning, Volume, Mod X, Mod Y, Pitch).
//!   * Anything not decoded keeps its raw event id and bytes in
//!     `Project::events`.

use anyhow::{anyhow, bail, Result};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

// ── event ids (only the ones this parser acts on) ──────────────────────

const EV_CHAN_ENABLED: u8 = 0;
const EV_CHAN_TYPE: u8 = 21;
const EV_CHAN_ROOT_NOTE: u8 = 135;
const EV_CHAN_FINE_TUNE: u8 = 142;
const EV_CHAN_SAMPLER_FLAGS: u8 = 143;
const EV_CHAN_ENV_LFO: u8 = 218;
const EV_CHAN_LEVELS: u8 = 219;
const EV_CHAN_TRACKING: u8 = 228;
const EV_CHAN_LEVEL_ADJUSTS: u8 = 229;

const EV_NEW_CHAN: u8 = 64;
const EV_NEW_PAT: u8 = 65;
const EV_TEMPO_OLD: u8 = 66;
const EV_FINE_TEMPO: u8 = 156;
const EV_COLOR: u8 = 128;
const EV_TEXT_CHAN_NAME: u8 = 192;
const EV_TEXT_PAT_NAME: u8 = 193;
const EV_TEXT_TITLE: u8 = 194;
const EV_TEXT_COMMENT: u8 = 195;
const EV_TEXT_SAMPLE: u8 = 196;
const EV_TEXT_VERSION: u8 = 199;
const EV_TEXT_DEF_PLUGIN: u8 = 201;
const EV_TEXT_DATA_PATH: u8 = 202;
const EV_TEXT_PLUGIN_NAME: u8 = 203;
const EV_TEXT_FX_NAME: u8 = 204;
const EV_TEXT_GENRE: u8 = 206;
const EV_TEXT_AUTHOR: u8 = 207;
const EV_NEW_PLUGIN: u8 = 212;
const EV_PLUGIN_PARAMS: u8 = 213;
const EV_PAT_NOTES: u8 = 224;
const EV_PLAYLIST: u8 = 233;

const NOTE_SIZE: usize = 24;

// ── little-endian field helpers ────────────────────────────────────────

#[inline]
fn i32le(b: &[u8], o: usize) -> i32 {
    i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

#[inline]
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

// ── channel substructures (PyFLP channel.py v2.2.1) ────────────────────

/// One of the five `EnvelopeLFO` events a channel can carry (event 218,
/// 68 bytes).  Stream order matches PyFLP's `EnvelopeName.__args__`:
/// index 0 = Panning, 1 = Volume, 2 = Mod X, 3 = Mod Y, 4 = Pitch.
/// The LFO fields in the same event modulate the same target.
#[derive(Debug, Clone, Default, Serialize)]
pub struct EnvelopeLfo {
    pub flags: i32,
    pub env_enabled: i32,
    pub env_predelay: i32,
    pub env_attack: i32,
    pub env_hold: i32,
    pub env_decay: i32,
    pub env_sustain: i32,
    pub env_release: i32,
    pub env_amount: i32,
    pub lfo_predelay: u32,
    pub lfo_attack: u32,
    pub lfo_amount: i32,
    pub lfo_speed: u32,
    pub lfo_shape: i32,
    pub env_attack_tension: i32,
    pub env_decay_tension: i32,
    pub env_release_tension: i32,
}

pub const ENV_PANNING: usize = 0;
pub const ENV_VOLUME: usize = 1;
pub const ENV_MOD_X: usize = 2;
pub const ENV_MOD_Y: usize = 3;
pub const ENV_PITCH: usize = 4;

/// Channel default pan/volume/pitch and filter (event 219, 24 bytes).
#[derive(Debug, Clone, Default, Serialize)]
pub struct Levels {
    /// 0..12800, 6400 = center (see `Channel::pan_norm`).
    pub pan: i32,
    /// 0..12800, 10000 = unity (0 dB).
    pub volume: u32,
    /// -4800..4800 cents.
    pub pitch_shift: i32,
    /// Filter cutoff (0..256 typical).
    pub filter_mod_x: u32,
    /// Filter resonance.
    pub filter_mod_y: u32,
    /// 0 = FastLP, 1 = LP, 2 = BP, 3 = HP, 4 = BS,
    /// 5 = LPx2, 6 = SVFLP, 7 = SVFLPx2.
    pub filter_type: u32,
}

/// One of the (up to two) Tracking events (event 228, 16 bytes).
/// Index 0 = volume tracking, index 1 = keyboard tracking.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Tracking {
    pub middle_value: u32,
    pub pan: i32,
    pub mod_x: i32,
    pub mod_y: i32,
}

/// Level adjustments (event 229, 20 bytes).
#[derive(Debug, Clone, Default, Serialize)]
pub struct LevelAdjusts {
    pub pan: i32,
    pub volume: u32,
    pub mod_x: i32,
    pub mod_y: i32,
}

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

// ── data model ─────────────────────────────────────────────────────────

fn channel_type_name(id: u8) -> String {
    match id {
        0 => "sampler".into(),
        2 => "native".into(),
        3 => "layer".into(),
        4 => "instrument".into(),
        5 => "automation".into(),
        _ => format!("type{id}"),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Note {
    pub position: u32,
    pub length: u32,
    pub key: u16,
    pub channel: u16,
    pub velocity: u8,
    pub pan: u8,
    pub fine_pitch: u8,
    pub release: u8,
    pub mod_x: u8,
    pub mod_y: u8,
    pub flags: u16,
    pub group: u16,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Plugin {
    pub owner: u32,
    pub internal_name: String,
    pub display_name: String,
    pub vst_path: String,
    pub state_size: usize,
    #[serde(skip)]
    pub state: Vec<u8>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Channel {
    pub iid: u16,
    pub type_id: i16,
    #[serde(rename = "type")]
    pub channel_type: String,
    pub name: String,
    pub color: Option<u32>,
    pub sample_path: String,
    pub plugin: Option<Plugin>,

    // ── sound-shaping fields (ported from PyFLP channel.py) ──────────
    /// `IsEnabled` (event 0).  `None` means the flag was not present in
    /// the file; treat as enabled.
    pub enabled: Option<bool>,
    /// Channel default pan/volume/pitch/filter (event 219).
    pub levels: Option<Levels>,
    /// Up to 5 events (218), in stream order:
    /// 0 = Panning, 1 = Volume, 2 = Mod X, 3 = Mod Y, 4 = Pitch.
    pub envelopes: Vec<EnvelopeLfo>,
    /// Up to 2 events (228), in stream order: 0 = volume, 1 = keyboard.
    pub tracking: Vec<Tracking>,
    /// Level adjustments (event 229).
    pub level_adjusts: Option<LevelAdjusts>,
    /// `SamplerFlags` (event 143).  Bit 0 = Resample, 1 = LoadRegions,
    /// 2 = LoadSliceMarkers, 3 = UsesLoopPoints, 8 = KeepOnDisk.
    pub sampler_flags: Option<u32>,
    /// Sample root key (event 135).  FL's C5 = 60.
    pub root_note: Option<u32>,
    /// Fine tune in cents (event 142).
    pub fine_tune: Option<i32>,
}

impl Channel {
    fn new(iid: u16) -> Self {
        Channel {
            iid,
            type_id: -1,
            channel_type: "?".into(),
            name: String::new(),
            color: None,
            sample_path: String::new(),
            plugin: None,
            enabled: None,
            levels: None,
            envelopes: Vec::new(),
            tracking: Vec::new(),
            level_adjusts: None,
            sampler_flags: None,
            root_note: None,
            fine_tune: None,
        }
    }

    /// Accessor for a specific envelope by slot, if it was present.
    pub fn envelope(&self, slot: usize) -> Option<&EnvelopeLfo> {
        self.envelopes.get(slot)
    }

    /// Normalised pan in -1.0..1.0.
    ///
    /// FL has two independent pan stages: `Levels.pan` (channel rack,
    /// 0..12800 with 6400 = centre) and `LevelAdjusts.pan` (mixer strip
    /// adjustment, signed with 0 = centre).  Both are offsets from
    /// centre and combine additively.
    pub fn pan_norm(&self) -> f64 {
        let rack = self
        .levels
        .as_ref()
        .map(|l| (l.pan as f64 - 6400.0) / 6400.0)
        .unwrap_or(0.0);
        let adj = self
        .level_adjusts
        .as_ref()
        .map(|l| l.pan as f64 / 12800.0)
        .unwrap_or(0.0);
        (rack + adj).clamp(-1.0, 1.0)
    }

    /// Linear volume scalar combining both gain stages.
    ///
    /// `Levels.volume` (channel rack, 10000 = unity) and
    /// `LevelAdjusts.volume` (mixer strip, 10000 = unity) are
    /// independent multiplicative gains in FL.  The old code applied
    /// only `Levels`, which dropped the mixer-strip adjustment on
    /// every channel that had one — a real, if small, gain loss.
    pub fn volume_scale(&self) -> f64 {
        let rack = self
        .levels
        .as_ref()
        .map(|l| l.volume as f64 / 10000.0)
        .unwrap_or(1.0);
        let adj = self
        .level_adjusts
        .as_ref()
        .map(|l| l.volume as f64 / 10000.0)
        .unwrap_or(1.0);
        if rack <= 0.0 || adj <= 0.0 {
            return 0.0;
        }
        (rack * adj).clamp(0.0, 4.0)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Pattern {
    pub iid: u16,
    pub name: String,
    pub notes: Vec<Note>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlaylistItem {
    pub position: u32,
    pub length: u32,
    pub track: i32,
    pub kind: String,
    #[serde(rename = "ref")]
    pub ref_id: u16,
    pub flags: u16,
    /// Clip start offset into the source file, in milliseconds.
    pub start_offset_ms: f32,
    /// Clip end offset into the source file, in milliseconds.
    pub end_offset_ms: f32,
    /// Pattern clips only: tick inside the pattern where this clip starts
    /// playing (the clip shows pattern ticks [start, start + length)).
    /// 0 when untrimmed; the file stores -1 for "no trim".
    pub clip_start_ticks: u32,
    #[serde(skip)]
    pub raw: Vec<u8>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Insert {
    pub index: u32,
    pub name: String,
    pub plugins: Vec<Plugin>,
}

#[derive(Debug, Default, Serialize)]
pub struct Project {
    pub version: String,
    pub ppq: u16,
    pub tempo: f64,
    pub title: String,
    pub author: String,
    pub genre: String,
    pub comment: String,
    pub data_path: String,
    pub channels: Vec<Channel>,
    pub patterns: Vec<Pattern>,
    pub inserts: BTreeMap<u32, Insert>,
    pub playlist: Vec<PlaylistItem>,
    pub samples: Vec<String>,
    #[serde(skip)]
    pub events: Vec<(u8, Vec<u8>)>,
}

// ── event stream ───────────────────────────────────────────────────────

fn read_events(data: &[u8]) -> Result<(u16, Vec<(u8, Vec<u8>)>)> {
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
    while p < end {
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
                if p >= data.len() {
                    bail!("truncated event length");
                }
                let b = data[p];
                p += 1;
                size |= ((b & 0x7F) as usize) << shift;
                shift += 7;
                if b & 0x80 == 0 {
                    break;
                }
                if shift > 28 {
                    bail!("event length overflow");
                }
            }
            size
        };
        if p + size > data.len() {
            bail!("truncated event body");
        }
        events.push((ev, data[p..p + size].to_vec()));
        p += size;
    }
    Ok((ppq, events))
}

// ── text decoding ──────────────────────────────────────────────────────

fn text(ev: u8, raw: &[u8]) -> String {
    if ev == EV_TEXT_VERSION {
        let nul = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        return String::from_utf8_lossy(&raw[..nul]).into_owned();
    }
    if raw.len() % 2 != 0 {
        let nul = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        return raw[..nul].iter().map(|&b| b as char).collect();
    }
    let mut end = 0;
    while end + 1 < raw.len() {
        if raw[end] == 0 && raw[end + 1] == 0 {
            break;
        }
        end += 2;
    }
    let units: Vec<u16> = (0..end)
    .step_by(2)
    .map(|i| u16::from_le_bytes([raw[i], raw[i + 1]]))
    .collect();
    String::from_utf16_lossy(&units)
}

// ── VST path extraction ────────────────────────────────────────────────

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

// ── note decoding ──────────────────────────────────────────────────────

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

// ── playlist decoding ──────────────────────────────────────────────────

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

        // Real entries always carry pattern_base == 20480 (0x5000).
        // Anything else — fillers (0) and misaligned reads — skip.
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
        // For pattern clips bytes 24..28 are an integer tick offset into the
        // pattern (not an f32 as for audio clips); 0xFFFFFFFF (-1) = no trim.
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
            raw: rec.to_vec(),
        });
        i += size;
    }
    items
}

// ── main parse ─────────────────────────────────────────────────────────

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
    let (ppq, events) = read_events(data)?;
    let mut prj = Project {
        ppq,
        ..Default::default()
    };

    let mut chan_idx: Option<usize> = None;
    let mut pat_idx: Option<usize> = None;
    let mut plug_ref: Option<PluginRef> = None;
    let mut pending_fx_names: Vec<String> = Vec::new();

    for (ev, raw) in &events {
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
                    prj.channels[i].sampler_flags = Some(u32le(raw, 0));
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

            EV_NEW_PLUGIN => {
                let owner = if raw.len() >= 4 {
                    u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]])
                } else {
                    0
                };
                let blank = Plugin {
                    owner,
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
                if let Some(pr) = plug_ref {
                    if let Some(p) = pr.get_mut(&mut prj) {
                        p.internal_name = s;
                    }
                }
            }
            EV_TEXT_PLUGIN_NAME => {
                let s = text(*ev, raw);
                if let Some(pr) = plug_ref {
                    if let Some(p) = pr.get_mut(&mut prj) {
                        p.display_name = s.clone();
                    }
                }
                if let Some(pr) = plug_ref {
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
                // FL emits NewPattern twice per pattern and can fire several
                // of them in a row before any Notes events arrive.  Reusing
                // the existing entry here misroutes notes to the wrong
                // pattern.  Keep both entries; the converter merges by
                // preferring the one with more notes.
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
                    prj.patterns[i].notes = decode_notes(raw);
                }
            }
            EV_PLAYLIST => prj.playlist = decode_playlist(raw, version_major(&prj.version)),
            _ => {}
        }
    }

    // Attach pending fx names to inserts in sorted key order.
    let keys: Vec<u32> = prj.inserts.keys().copied().collect();
    for (key, name) in keys.into_iter().zip(pending_fx_names.into_iter()) {
        if let Some(ins) = prj.inserts.get_mut(&key) {
            ins.name = name;
        }
    }

    prj.events = events;
    Ok(prj)
}

pub fn parse_file(path: &Path) -> Result<Project> {
    let data = std::fs::read(path).map_err(|e| anyhow!("reading {}: {}", path.display(), e))?;
    parse(&data)
}