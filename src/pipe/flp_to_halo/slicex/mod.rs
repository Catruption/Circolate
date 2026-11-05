//! FL Studio Slicex → generated SoundFont.
//!
//! A Slicex channel stores an uncompressed plugin state blob inside its
//! FLP event 213.  Every structure we know about is parsed by one of the
//! sibling modules in this folder:
//!
//!   outer      96-byte outer header (toggles + flags)
//!   wav        the embedded WAV's RIFF chunks (cue, tlst, inst, smpl,
//!              acid, strc, LIST/INFO)
//!   settings   402-byte settings header
//!   records    68-byte per-slice records
//!   trest      region records (envelopes, articulator params)
//!   params     108-byte region parameter block
//!   envelope   envelope points → SF2 ADSR (amp + filter)
//!   sf2        the SF2 writer
//!
//! Whether or not a structure is currently applied to the generated
//! SoundFont, it is still parsed.  Set `SLICEX_DEBUG=1` (or
//! `CIRCOLATE_DEBUG=flp.slicex`) to dump every parsed structure in a
//! stable, greppable format via the log bus.
//!
//! Optional application env vars:
//!
//!   FLP_SLICEX_APPLY_PARAMS=1       apply region params (filter/pan/atten)
//!   FLP_SLICEX_APPLY_FILTER_ENV=1   apply tag=2 filter envelope
//!   SLICEX_ENV_CURVE_WEIGHT=<0..1>  envelope segment curvature weight
//!   SLICEX_ENV_SCALE=<seconds>      envelope time scale override
//!   SLICEX_ENV_CURVE_K=<float>      curve law strength (default 2.0)
//!   SLICEX_SPEED_OCTAVES=<float>    pitch range of SPEED envelope extremes
//!   FLP_SLICEX_APPLY_SPEED=1        bake the SPEED envelope
//!   SLICEX_DUMP_SLICES=<dir>        write every baked slice as a WAV for pitchtrack.py
//!   SLICEX_STATE_OUT=<dir>          dump raw state + trest blobs for trest_diff.py
//!
//! ## Provenance tags (for fact-checking against Slicex_x64.dll)
//!
//! Comments of the form `SRC[loc=… decomp=… conf=…]` mark every structure
//! that comes from the plugin's state format.  See git history for the
//! full provenance map — this module preserves those comments unchanged.

mod envelope;
mod outer;
mod params;
mod records;
mod settings;
mod sf2;
mod trest;
mod wav;

use std::collections::HashMap;
use std::fmt::Write as _;

use crate::convert::{identifier, AuxFile};
use crate::formats::flp::{Channel, Plugin};
use crate::formats::flp_audio;

use super::place::Placed;
use super::MIN_EMBEDDED_BYTES;

use envelope::{AmpEnvelope, FilterEnvelope};
use outer::OuterHeader;
use records::SliceRecords;
use settings::SettingsHeader;
use trest::Trest;
use wav::WavChunks;

const DEFAULT_BASE_KEY: u32 = 60;
const SLICEX_HDR_LEN: usize = 402;
const SLICEX_REC_LEN: usize = 68;

pub(super) struct SlicexOut {
    pub(super) instrument_rel: String,
    pub(super) base_key: u32,
    pub(super) slices: usize,
}

pub(super) fn is_slicex(p: &Plugin) -> bool {
    let norm = |s: &str| s.to_ascii_lowercase();
    norm(&p.internal_name).contains("slicex") || norm(&p.display_name).contains("slicex")
}

fn debug_on() -> bool {
    std::env::var_os("SLICEX_DEBUG").is_some() || std::env::var_os("FLP_DEBUG").is_some()
}

fn low_count_for(iid: u16) -> usize {
    std::env::var(format!("FLP_SLICEX_LOW_{}", iid))
    .ok()
    .and_then(|s| s.trim().parse::<usize>().ok())
    .unwrap_or(0)
}

fn slicex_base_key(
    chan: &Channel,
    _has_tlst: bool,
    _placed: &HashMap<u16, Vec<Placed>>,
) -> u32 {
    let per_channel = format!("FLP_SLICEX_BASE_{}", chan.iid);
    if let Ok(s) = std::env::var(&per_channel) {
        if let Ok(k) = s.trim().parse::<u32>() {
            if k < 128 {
                return k;
            }
        }
    }
    if let Ok(s) = std::env::var("FLP_SLICEX_BASE") {
        if let Ok(k) = s.trim().parse::<u32>() {
            if k < 128 {
                return k;
            }
        }
    }
    DEFAULT_BASE_KEY
}

pub struct ParsedState<'a> {
    pub outer: OuterHeader<'a>,
    pub source_path: String,
    pub wav1: &'a [u8],
    pub wav1_chunks: WavChunks<'a>,
    pub settings: SettingsHeader<'a>,
    pub records: SliceRecords<'a>,
    pub wav2: Option<&'a [u8]>,
    pub trest: Trest<'a>,
}

fn parse_state(data: &[u8]) -> Result<ParsedState<'_>, String> {
    if data.len() < 0x61 {
        return Err(format!(
            "state too short for outer header + path length ({} bytes)",
                           data.len()
        ));
    }
    let outer = OuterHeader::parse(data).ok_or("outer header parse failed")?;

    let path_len = data[0x60] as usize;
    let path_start = 0x61;
    let path_end = path_start + path_len;
    if path_end + 4 > data.len() {
        return Err(format!(
            "path length {} at 0x60 overruns state ({} bytes)",
                           path_len,
                           data.len()
        ));
    }
    let source_path = String::from_utf8_lossy(&data[path_start..path_end]).into_owned();

    let wav1_len = u32::from_le_bytes(data[path_end..path_end + 4].try_into().unwrap()) as usize;
    let wav1_start = path_end + 4;
    let wav1_end = wav1_start
    .checked_add(wav1_len)
    .ok_or_else(|| format!("WAV #1 length {} overflows", wav1_len))?;
    if wav1_end > data.len() {
        return Err(format!(
            "WAV #1 length {} exceeds remaining {} bytes",
            wav1_len,
            data.len() - wav1_start
        ));
    }
    let wav1 = &data[wav1_start..wav1_end];
    let wav1_chunks = WavChunks::parse(wav1).ok_or("WAV #1 is not RIFF/WAVE")?;

    let hdr_end = wav1_end
    .checked_add(SLICEX_HDR_LEN)
    .ok_or("settings header offset overflow")?;
    if hdr_end > data.len() {
        return Err(format!(
            "state too short for {}-byte settings header",
            SLICEX_HDR_LEN
        ));
    }
    let settings = SettingsHeader::parse(&data[wav1_end..hdr_end])
    .ok_or("settings header parse failed")?;

    let rec_start = hdr_end;
    let rec_len = (settings.slice_count as usize)
    .checked_mul(SLICEX_REC_LEN)
    .ok_or("slice count overflow")?;
    let rec_end = rec_start
    .checked_add(rec_len)
    .ok_or("record block offset overflow")?;
    if rec_end > data.len() {
        return Err(format!(
            "slice block ({} × {} = {} bytes) overruns state by {} bytes",
                           settings.slice_count,
                           SLICEX_REC_LEN,
                           rec_len,
                           rec_end - data.len()
        ));
    }
    let records = SliceRecords::parse(&data[rec_start..rec_end], settings.slice_count, SLICEX_REC_LEN);

    let (wav2, after_wav2) = if rec_end + 16 <= data.len()
    && &data[rec_end + 4..rec_end + 8] == b"RIFF"
    && &data[rec_end + 12..rec_end + 16] == b"WAVE"
    {
        let w2_len = u32::from_le_bytes(data[rec_end..rec_end + 4].try_into().unwrap()) as usize;
        let w2_total = 4usize.checked_add(w2_len).ok_or("wav2 length overflow")?;
        let w2_end = rec_end.saturating_add(w2_total).min(data.len());
        (Some(&data[rec_end..w2_end]), w2_end)
    } else {
        (None, rec_end)
    };

    let trest = Trest::parse(&data[after_wav2..]);

    Ok(ParsedState {
        outer,
       source_path,
       wav1,
       wav1_chunks,
       settings,
       records,
       wav2,
       trest,
    })
}

// ── debug dump (atomic blocks) ────────────────────────────────────────
//
// Each function returns a `String` and is emitted via `log_event!`.  The
// block split mirrors the visual sections in the old `eprintln!` dump;
// the log filter (SLICEX_DEBUG / CIRCOLATE_DEBUG=flp.slicex) decides
// whether they reach a sink.

fn dump_header(chan: &Channel, s: &ParsedState<'_>) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "=== Slicex chan {} ({}) ===", chan.iid, chan.name);
    let _ = writeln!(out, "  source:  {}", s.source_path);

    let _ = writeln!(out, "  outer header (96 B):");
    for off in (0..0x60).step_by(16) {
        let row = &s.outer.raw[off..off + 16];
        let _ = writeln!(out, "    +{off:>3}  {}",
                         row.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" "));
    }
    let _ = writeln!(out, "    toggle_60={} toggle_71={} auto_dump={} has_override={} last_slice={}",
                     s.outer.sub_tab, s.outer.toggle_71, s.outer.auto_dump,
                     s.outer.has_override, s.outer.last_slice);

    let _ = writeln!(out, "  wav1: {} B, fmt={:?}, data={} B",
                     s.wav1.len(), s.wav1_chunks.fmt, s.wav1_chunks.data_size);
    let _ = writeln!(out, "    cues: {}", s.wav1_chunks.cues.len());
    let _ = writeln!(out, "    tlst: {} entries", s.wav1_chunks.tlst.len());
    for t in s.wav1_chunks.tlst.iter().take(12) {
        let _ = writeln!(out, "      cue_id={} count={} key={} (byte={:#04x})",
                         t.cue_id, t.count, t.key_byte, t.key_byte);
    }
    if let Some(inst) = &s.wav1_chunks.inst {
        let _ = writeln!(out, "    inst: root={} fine={} gain={} low={} high={}",
                         inst[0], inst[1], inst[4], inst[5], inst[6]);
    }
    if let Some(smpl) = &s.wav1_chunks.smpl {
        let _ = writeln!(out, "    smpl: period={} unity={} loops={}",
                         smpl.period, smpl.unity_note, smpl.num_loops);
    }
    if let Some(acid) = &s.wav1_chunks.acid {
        let _ = writeln!(out, "    acid: type={:#x} root={} beats={} meter={}/{} tempo={}",
                         acid.kind, acid.root_note, acid.beats,
                         acid.meter_num, acid.meter_den, acid.tempo);
    }
    if let Some(strc) = &s.wav1_chunks.strc {
        let _ = writeln!(out, "    strc: {} B", strc.len());
    }
    if let Some(t) = s.wav1_chunks.strc_table() {
        let _ = writeln!(out, "    strc header: {:?}", t.header);
        let _ = writeln!(out, "    strc records: {} (first 6)", t.records.len());
        for (i, r) in t.records.iter().take(6).enumerate() {
            let _ = writeln!(out, "      [{:>2}] {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}",
                             i, r.a, r.b, r.c, r.d, r.e, r.f);
        }
    }
    for (name, body) in &s.wav1_chunks.info_list {
        let _ = writeln!(out, "    LIST/{name}: {} B", body.len());
    }
    for u in &s.wav1_chunks.unknown {
        let tag = String::from_utf8_lossy(&u.id);
        let _ = writeln!(out, "    unknown chunk {tag:?} at wav+{} ({} B)", u.offset, u.size);
    }
    let _ = writeln!(out, "  wav2:    {} B", s.wav2.map_or(0, |w| w.len()));
    out
}

fn dump_settings(s: &ParsedState<'_>) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "  settings header (402 B):");
    let _ = writeln!(out, "    sentinels:      {:?} {:?} {:?}",
                     s.settings.sentinel_a, s.settings.sentinel_b, s.settings.zero_a);
    let _ = writeln!(out, "    duration_x2:    {}", s.settings.duration_x2);
    let _ = writeln!(out, "    rate32:         {}", s.settings.rate32);
    let _ = writeln!(out, "    filter_type:    {}", s.settings.filter_type);
    let _ = writeln!(out, "    5×80B block durations: {:?}", s.settings.block_durations);
    let _ = writeln!(out, "    slice_count:    {}", s.settings.slice_count);

    let _ = writeln!(out, "  per-slice records ({} × 68 B):", s.settings.slice_count);
    let uniq = s.records.unique();
    let _ = writeln!(out, "    {} distinct record(s):", uniq.len());
    for (r, idx) in uniq.iter().take(4) {
        let _ = writeln!(out, "      x{:>3}: {}", idx.len(),
                         r.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" "));
    }
    let varying = s.records.varying_columns();
    let _ = writeln!(out, "    {} varying column(s): {:?}", varying.len(), varying);
    out
}

fn dump_trest(s: &ParsedState<'_>) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "  trest:   {} B, {} region record(s)",
                     s.trest.raw.len(), s.trest.records.len());

    for (i, r) in s.trest.records.iter().enumerate().take(40) {
        let _ = writeln!(out, "    [{:>2}] +{:<6} tag={} cnt={} size={}",
                         i, r.offset, r.tag, r.count, r.size);
        for (j, p) in r.points.iter().enumerate().take(6) {
            let _ = writeln!(out, "         p{}: dx={:>12.6} y={:>10.6} curve={:>10.6}",
                             j, p.dx, p.y, p.curve);
        }
        if r.count > 6 {
            let _ = writeln!(out, "         ... +{} more points", r.count - 6);
        }
    }
    if s.trest.records.len() > 40 {
        let _ = writeln!(out, "    ... +{} more records", s.trest.records.len() - 40);
    }

    if let Some(prm) = &s.trest.params {
        let _ = writeln!(out, "  region param block (108 B @ trest+396):");
        let _ = writeln!(out, "    lead: {}", prm.lead);
        let _ = writeln!(out, "    17 u32s:");
        for (i, v) in prm.values.iter().enumerate() {
            let hint = match *v {
                12800 => "  ← 100% (FL level)",
                128 => "  ← centre (FL pan)",
                8000 => "  ← ~62.5%",
                _ => "",
            };
            let _ = writeln!(out, "      [{:>2}] {:>10}  ({:#010x}){hint}", i, v, v);
        }
    }
    out
}

fn dump_computed(
    s: &ParsedState<'_>,
    layout: &str, base: u32, lo_key: u32, hi_key: u32,
    slice_secs: f64, env_scale: f64,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "  computed:");
    let _ = writeln!(out, "    base   = {}", base);
    let _ = writeln!(out, "    layout = {}", layout);
    let _ = writeln!(out, "    keys   = {} slices, {}..{}", s.records.slice_count, lo_key, hi_key);
    let _ = writeln!(out, "    slice_secs (avg) = {:.4}", slice_secs);
    let _ = writeln!(out, "    env time scale   = {:.4}", env_scale);
    out
}

fn dump_env(s: &ParsedState<'_>, amp_env: Option<&AmpEnvelope>) -> String {
    let mut out = String::new();
    if let Some(env) = s.trest.amp_envelope() {
        let _ = writeln!(out, "  amp envelope @ +{} (tag={}, {} points):", env.offset, env.tag, env.count);
        for (i, p) in env.points.iter().enumerate() {
            let _ = writeln!(out, "    [{:>2}] dx={:>12.6} y={:>10.6} curve={:>10.6}", i, p.dx, p.y, p.curve);
        }
    } else {
        let _ = writeln!(out, "  amp envelope: none found");
    }

    if let Some(e) = amp_env {
        let _ = writeln!(out, "  ADSR decomposition:");
        let _ = writeln!(out, "    delay    = {:>10.3} ms", e.delay_ms);
        let _ = writeln!(out, "    attack   = {:>10.3} ms", e.attack_ms);
        let _ = writeln!(out, "    hold     = {:>10.3} ms", e.hold_ms);
        let _ = writeln!(out, "    decay    = {:>10.3} ms", e.decay_ms);
        let _ = writeln!(out, "    sustain  = {:>10.4}", e.sustain);
        let _ = writeln!(out, "    release  = {:>10.3} ms", e.release_ms);
    }
    out
}

fn dump_application(
    apply_params: bool,
    apply_filter_env: bool,
    s: &ParsedState<'_>,
    filter_env: Option<&FilterEnvelope>,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "  application:");
    let _ = writeln!(out, "    apply_params     = {}", apply_params);
    let _ = writeln!(out, "    apply_filter_env = {}", apply_filter_env);
    if let Some(p) = &s.trest.params {
        let _ = writeln!(out, "    cutoff_raw       = {}", p.cutoff_raw());
        let _ = writeln!(out, "    cutoff_sf2_cents = {}", p.cutoff_sf2_cents());
        let _ = writeln!(out, "    pan_l / pan_r    = {} / {}", p.pan_l_raw(), p.pan_r_raw());
        let _ = writeln!(out, "    pan_sf2          = {}", p.pan_sf2());
        let _ = writeln!(out, "    levels_raw       = {:?}", p.levels_raw());
        let _ = writeln!(out, "    amp_atten_cb     = {}", p.amp_attenuation_cb());
    }
    if let Some(fe) = filter_env {
        let _ = writeln!(out, "  filter envelope (tag=2):");
        let _ = writeln!(out, "    delay    = {:>10.3} ms", fe.delay_ms);
        let _ = writeln!(out, "    attack   = {:>10.3} ms", fe.attack_ms);
        let _ = writeln!(out, "    hold     = {:>10.3} ms", fe.hold_ms);
        let _ = writeln!(out, "    decay    = {:>10.3} ms", fe.decay_ms);
        let _ = writeln!(out, "    sustain  = {:>10.4}", fe.sustain);
        let _ = writeln!(out, "    release  = {:>10.3} ms", fe.release_ms);
        let _ = writeln!(out, "    depth    = {} cents", fe.depth_cents);
    } else if apply_filter_env {
        let _ = writeln!(out, "  filter envelope: no tag=2 record with count >= 2");
    }
    out
}

// ── entry point ───────────────────────────────────────────────────────

pub(super) fn convert(
    chan: &Channel,
    plugin: &Plugin,
    placed: &HashMap<u16, Vec<Placed>>,
    aux_files: &mut Vec<AuxFile>,
    warnings: &mut Vec<String>,
    project_bpm: f64,
) -> Result<SlicexOut, String> {
    let state = parse_state(&plugin.state).map_err(|e| format!("Slicex state parse: {e}"))?;

    if state.wav1.len() < MIN_EMBEDDED_BYTES {
        return Err(format!(
            "Slicex WAV #1 is only {} bytes (minimum {})",
                           state.wav1.len(),
                           MIN_EMBEDDED_BYTES
        ));
    }

    if debug_on() {
        let dump = format!("/tmp/slicex_chan{}.wav", chan.iid);
        std::fs::write(&dump, state.wav1).ok();
    }

    if let Some(dir) = std::env::var_os("SLICEX_STATE_OUT") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).ok();
        std::fs::write(dir.join(format!("state_chan{}.bin", chan.iid)), &plugin.state).ok();
        std::fs::write(dir.join(format!("trest_chan{}.bin", chan.iid)), state.trest.raw).ok();
    }

    let wav = state.wav1.to_vec();

    let mut cues = flp_audio::wav_cue_points(&wav);
    let mut pcm = sf2::read_wav_pcm(&wav)
    .ok_or_else(|| "embedded WAV is not PCM 16/24/32-bit or float32".to_string())?;
    cues.retain(|&c| (c as usize) < pcm.data.len());
    if cues.is_empty() {
        return Err("embedded WAV has no cue points (no slice markers)".into());
    }

    let acid_tempo = state.wav1_chunks.acid.as_ref().map(|a| a.tempo);
    let acid_beats = state.wav1_chunks.acid.as_ref().map(|a| a.beats);
    let (sample_bpm, bpm_src) = sample_bpm_for(acid_tempo);
    {
        let secs = pcm.data.len() as f64 / pcm.rate.max(1) as f64;
        crate::log_event!(
            "flp.slicex.stretch",
            body: format!(
                "  stretch: acid tempo {:?}, acid beats {:?} ({:.3} s, beats-derived {:.3} bpm), \
using {:.3} bpm ({}), project {:.3} bpm",
                          acid_tempo, acid_beats, secs,
                          acid_beats.map_or(0.0, |b| b as f64 * 60.0 / secs.max(1e-9)),
                          sample_bpm, bpm_src, project_bpm,
            ),
        );
    }
    if let Some(f) = stretch_factor(sample_bpm, project_bpm) {
        let before = pcm.data.len();
        pcm.data = stretch_pcm(&pcm, f);
        for c in cues.iter_mut() {
            *c = (*c as f64 * f).round() as _;
        }
        let len = pcm.data.len();
        cues.retain(|&c| (c as usize) < len);
        warnings.push(format!(
            "[{:2}] {}: Slicex stretched x{:.4} (sample {:.2} bpm [{}] -> project {:.2}, {} -> {} samples)",
                              chan.iid, chan.name, f, sample_bpm, bpm_src, project_bpm, before, len
        ));
        if cues.is_empty() {
            return Err("no cue points left after tempo stretch".into());
        }
    }

    let all: Vec<(usize, usize)> = cues
    .iter()
    .enumerate()
    .map(|(i, &c)| {
        let s = c as usize;
        let e = cues.get(i + 1).map_or(pcm.data.len(), |&n| n as usize);
        (s, e.max(s + 1).min(pcm.data.len()))
    })
    .collect();
    let n_all = all.len();

    let tlst = state.wav1_chunks.tlst.clone();
    let has_tlst = !tlst.is_empty();
    let base = slicex_base_key(chan, has_tlst, placed);
    let low = low_count_for(chan.iid).min(n_all).min(base as usize);

    let tlst_for_keys: Vec<(u32, u8)> = tlst.iter().map(|t| (t.cue_id, t.key_byte)).collect();
    let (all_keys, layout) = assign_keys(n_all, base, low, &tlst_for_keys);

    let mut ranges: Vec<(usize, usize)> = Vec::with_capacity(n_all);
    let mut keys: Vec<u32> = Vec::with_capacity(n_all);
    let mut dropped = 0usize;
    for (r, k) in all.into_iter().zip(all_keys) {
        if k < 128 {
            ranges.push(r);
            keys.push(k);
        } else {
            dropped += 1;
        }
    }
    if dropped > 0 {
        warnings.push(format!(
            "[{:2}] Slicex: {} slice(s) would land above key 127 and were dropped",
                              chan.iid, dropped
        ));
    }
    if ranges.is_empty() {
        return Err("no slice could be assigned a key".into());
    }
    let n = ranges.len();
    let lo_key = keys.iter().copied().min().unwrap_or(base);
    let hi_key = keys.iter().copied().max().unwrap_or(base);

    let slice_secs = if n_all > 0 && pcm.rate > 0 {
        let total = pcm.data.len() as f64 / pcm.rate as f64;
        total / n_all as f64
    } else {
        1.0
    };
    let env_scale_override = std::env::var("SLICEX_ENV_SCALE")
    .ok()
    .and_then(|s| s.trim().parse::<f64>().ok())
    .filter(|s| *s > 0.0);
    let env_scale = env_scale_override.unwrap_or(slice_secs);

    let amp_env = state
    .trest
    .amp_envelope()
    .and_then(|r| AmpEnvelope::from_points(&r.points, env_scale));

    let apply_params = std::env::var_os("FLP_SLICEX_APPLY_PARAMS").is_some();
    let filt = if apply_params {
        match &state.trest.params {
            Some(p) => sf2::FilterSettings {
                cutoff_cents: Some(p.cutoff_sf2_cents()),
                pan: Some(p.pan_sf2()),
                attenuation_cb: Some(p.amp_attenuation_cb()),
            },
            None => sf2::FilterSettings::default(),
        }
    } else {
        sf2::FilterSettings::default()
    };

    let apply_filter_env = std::env::var_os("FLP_SLICEX_APPLY_FILTER_ENV").is_some();
    let filter_env = if apply_filter_env {
        state
        .trest
        .filter_envelope()
        .and_then(|r| FilterEnvelope::from_points(&r.points, env_scale, 4800))
    } else {
        None
    };

    // ── emit the atomic debug blocks ─────────────────────────────────
    crate::log_event!("flp.slicex.header",     body: dump_header(chan, &state));
    crate::log_event!("flp.slicex.settings",   body: dump_settings(&state));
    crate::log_event!("flp.slicex.trest",      body: dump_trest(&state));
    crate::log_event!(
        "flp.slicex.computed",
        body: dump_computed(&state, layout, base, lo_key, hi_key, slice_secs, env_scale),
    );
    crate::log_event!("flp.slicex.env",        body: dump_env(&state, amp_env.as_ref()));
    crate::log_event!(
        "flp.slicex.application",
        body: dump_application(apply_params, apply_filter_env, &state, filter_env.as_ref()),
    );

    let base_raw = if chan.name.is_empty() {
        format!("Ch{:02}", chan.iid)
    } else {
        chan.name.clone()
    };
    let inst_name = format!("{}_slicex", identifier(&base_raw));
    let sf2_rel = format!("samples/{}.sf2", inst_name);
    let inst_rel = format!("instruments/{}.instrument", inst_name);

    let speed_pts: Option<&[trest::EnvelopePoint]> = if std::env::var_os("FLP_SLICEX_APPLY_SPEED").is_some() {
        state.trest.speed_envelope().map(|r| r.points.as_slice())
    } else {
        None
    };
    let speed_octaves = std::env::var("SLICEX_SPEED_OCTAVES")
    .ok()
    .and_then(|s| s.trim().parse::<f64>().ok())
    .unwrap_or(1.0);
    {
        let mut sp = String::new();
        match state.trest.speed_envelope() {
            Some(r) => {
                let _ = writeln!(sp, "  speed envelope @ +{} ({} points, octaves={}):",
                                 r.offset, r.count, speed_octaves);
                for (i, p) in r.points.iter().enumerate() {
                    let _ = writeln!(sp, "    [{:>2}] dx={:>12.6} y={:>10.6} curve={:>10.6}",
                                     i, p.dx, p.y, p.curve);
                }
            }
            None => { let _ = writeln!(sp, "  speed envelope: none (flat)"); }
        }
        crate::log_event!("flp.slicex.speed_env", body: sp);
    }

    let sf2 = sf2::build(
        &inst_name,
        &pcm,
        &ranges,
        &keys,
        state.trest.amp_envelope().map(|r| r.points.as_slice()),
                         env_scale_override,
                         speed_pts,
                         speed_octaves,
                         filter_env.as_ref(),
                         &filt,
    );
    aux_files.push(AuxFile { rel_path: sf2_rel.clone(), bytes: sf2 });

    let env_line = match state.trest.amp_envelope() {
        Some(r) => {
            let mut s = String::from("    SlicexAmpEnv: ");
            for (i, p) in r.points.iter().enumerate() {
                if i > 0 { s.push('|'); }
                s.push_str(&format!("{:.6},{:.6},{:.4}", p.dx, p.y, p.curve));
            }
            s.push_str(";\n");
            s
        }
        None => String::new(),
    };

    let env_adsr_line = match amp_env.as_ref() {
        Some(e) => format!(
            "    SlicexAmpAdsr: D={:.3} A={:.3} H={:.3} Dc={:.3} S={:.4} R={:.3};\n",
            e.delay_ms, e.attack_ms, e.hold_ms, e.decay_ms, e.sustain, e.release_ms
        ),
        None => String::new(),
    };

    let filter_env_line = match filter_env.as_ref() {
        Some(fe) => format!(
            "    SlicexFilterEnv: D={:.3} A={:.3} H={:.3} Dc={:.3} S={:.4} R={:.3} Depth={}c;\n",
            fe.delay_ms, fe.attack_ms, fe.hold_ms, fe.decay_ms,
            fe.sustain, fe.release_ms, fe.depth_cents
        ),
        None => String::new(),
    };

    let params_line = if apply_params {
        match &state.trest.params {
            Some(p) => format!(
                "    SlicexParams: cutoff_raw={} cutoff_cents={} pan_lr={},{} pan_sf2={} levels={:?} atten_cb={};\n",
                p.cutoff_raw(), p.cutoff_sf2_cents(),
                               p.pan_l_raw(), p.pan_r_raw(),
                               p.pan_sf2(),
                               p.levels_raw(),
                               p.amp_attenuation_cb(),
            ),
            None => String::new(),
        }
    } else {
        String::new()
    };

    let text = format!(
        "Instrument {inst_name}\n{{\n    Kind: sf2;\n    File: '{sf2_rel}';\n    Bank: 0;\n    Preset: 0;\n    SourceChannel: {};\n    Slicex: true;\n{env_line}{env_adsr_line}{filter_env_line}{params_line}}}\n",
        chan.iid,
    );
    aux_files.push(AuxFile { rel_path: inst_rel.clone(), bytes: text.into_bytes() });

    let env_note = match amp_env.as_ref() {
        Some(e) => format!(", amp_env={:.0}ms-{:.2}-{:.0}ms", e.decay_ms, e.sustain, e.release_ms),
        None => ", amp_env=none".to_string(),
    };
    let filt_note = if filter_env.is_some() { ", filter_env" } else { "" };
    let params_note = if apply_params { ", params" } else { "" };

    warnings.push(format!(
        "[{:2}] {}: Slicex -> SF2, {} slices on keys {}..{} (layout: {}, base {}{}{}{})",
                          chan.iid, chan.name, n, lo_key, hi_key, layout, base, env_note, filt_note, params_note,
    ));

    Ok(SlicexOut {
        instrument_rel: inst_rel,
       base_key: lo_key,
       slices: n,
    })
}

const SLICEX_IMPLICIT_BPM: f64 = 120.0;

fn sample_bpm_for(acid_tempo: Option<f32>) -> (f64, &'static str) {
    if let Some(v) = std::env::var("FLP_SLICEX_SAMPLE_BPM")
        .ok()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v > 0.0)
        {
            return (v, "env");
        }
        match acid_tempo {
            Some(t) if t.is_finite() && (20.0..=400.0).contains(&(t as f64)) => (t as f64, "acid"),
            _ => (SLICEX_IMPLICIT_BPM, "implicit"),
        }
}

fn stretch_factor(sample_bpm: f64, project_bpm: f64) -> Option<f64> {
    if std::env::var_os("FLP_SLICEX_NO_STRETCH").is_some() {
        return None;
    }
    if !sample_bpm.is_finite() || sample_bpm <= 0.0 {
        return None;
    }
    if !project_bpm.is_finite() || project_bpm <= 0.0 {
        return None;
    }
    let f = sample_bpm / project_bpm;
    if (f - 1.0).abs() < 0.002 {
        None
    } else {
        Some(f)
    }
}

fn stretch_pcm(pcm: &sf2::Pcm, factor: f64) -> Vec<i16> {
    let x: Vec<f32> = pcm.data.iter().map(|&v| v as f32 / 32768.0).collect();
    let resample = std::env::var("FLP_SLICEX_STRETCH_MODE")
    .map_or(false, |m| m.trim().eq_ignore_ascii_case("resample"));
    let out = if resample {
        let out_len = (x.len() as f64 * factor).round() as usize;
        crate::player::stretch::linear(&[&x], out_len)
    } else {
        crate::player::stretch::stretch(&[&x], factor, pcm.rate as f64)
    };
    out.into_iter()
    .next()
    .unwrap_or_default()
    .iter()
    .map(|&v| (v * 32768.0).round().clamp(-32768.0, 32767.0) as i16)
    .collect()
}

const MAX_SLICE_KEY: u32 = 99;

fn assign_keys(n: usize, base: u32, low: usize, tlst: &[(u32, u8)]) -> (Vec<u32>, &'static str) {
    use std::collections::HashSet;

    let overflow_keys = |low: usize| -> Vec<u32> {
        (0..n)
        .map(|i| if i >= n - low { base.saturating_sub((n - i) as u32) } else { base + i as u32 })
        .collect()
    };

    if low > 0 {
        return (overflow_keys(low), "FLP_SLICEX_LOW override");
    }
    if !tlst.is_empty() {
        let one_based = tlst.iter().any(|&(id, _)| id as usize >= n);
        let mut fixed: Vec<Option<u32>> = vec![None; n];
        for &(id, key) in tlst {
            let idx = if one_based { (id as usize).wrapping_sub(1) } else { id as usize };
            if idx < n {
                fixed[idx] = Some(key as u32);
            }
        }
        let claimed: HashSet<u32> = fixed.iter().flatten().copied().collect();
        let mut next = base;
        let keys = fixed.into_iter().map(|k| match k {
            Some(k) => k,
                                         None => {
                                             while claimed.contains(&next) { next += 1; }
                                             let v = next;
                                             next += 1;
                                             v
                                         }
        }).collect();
        return (keys, if one_based { "tlst (1-based ids)" } else { "tlst (0-based ids)" });
    }
    if base <= MAX_SLICE_KEY {
        let up = (MAX_SLICE_KEY - base + 1) as usize;
        if n > up {
            return (overflow_keys(n - up), "base + index, overflow below base");
        }
    }
    ((0..n).map(|i| base + i as u32).collect(), "base + index")
}
