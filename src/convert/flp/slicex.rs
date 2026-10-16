// src/convert/flp/slicex.rs
//! FL Studio Slicex → generated SoundFont.
//!
//! A Slicex channel stores one big WAV inside its plugin state, with the
//! slice markers as `cue ` points in that WAV.  Notes in the piano roll
//! don't pitch the sample, they pick a slice: key `base + i` plays slice
//! `i`.  That is exactly a SoundFont with one single-key zone per slice,
//! so we build one in memory and hand it to the existing `Kind: sf2`
//! sidecar path.  Nothing downstream needs to know about Slicex.
//!
//! The slice-0 key defaults to 60 (C5) and can be overridden with
//! `FLP_SLICEX_BASE=<0..127>` for every channel, or
//! `FLP_SLICEX_BASE_<iid>=<0..127>` for one specific channel, while the
//! real per-plugin mapping is being confirmed.  Two Slicex channels
//! loaded with different keyboard bases will show the same slice at
//! different piano-roll keys, so a single global base cannot serve
//! both.
//!
//! Some Slicex instruments are chromatic keyboards whose lowest notes
//! are stored as the *last* markers (e.g. a bottom octave appended after
//! the main run).  `FLP_SLICEX_LOW_<iid>=<N>` puts the last N slices on
//! the N keys directly below the base (`base-N .. base-1`) instead of
//! above the rest.  Default is 0, which keeps the plain `base + i` layout.

use crate::convert::{identifier, AuxFile};
use crate::formats::flp::{Channel, Plugin};
use crate::formats::flp_audio;

use super::MIN_EMBEDDED_BYTES;

const DEFAULT_BASE_KEY: u32 = 60;

pub(super) struct SlicexOut {
    pub(super) instrument_rel: String,
    pub(super) base_key: u32,
    pub(super) slices: usize,
}

/// Match "Slicex" anywhere in the plugin's internal or display name.
/// The old code required an exact, case-insensitive equality with
/// "slicex", which missed FL's own "Fruity Slicex" and any localised or
/// decorated variant.
pub(super) fn is_slicex(p: &Plugin) -> bool {
    let norm = |s: &str| s.to_ascii_lowercase();
    norm(&p.internal_name).contains("slicex") || norm(&p.display_name).contains("slicex")
}

/// Per-channel base key.
///
/// Priority:
///   1. `FLP_SLICEX_BASE_<iid>` — per-channel override, e.g. `FLP_SLICEX_BASE_1=48`
///   2. `FLP_SLICEX_BASE`       — global override
///   3. `DEFAULT_BASE_KEY`
///
/// The Slicex plugin maps note numbers to slices via a "keyboard base"
/// stored in the plugin state blob, but we don't know the byte offset.
/// Two Slicex channels loaded with different bases will show the same
/// slice at different piano-roll keys while still playing the same
/// slice at the same moment; until the base can be read from the state,
/// this env var lets us specify it per channel so the generated SF2
/// matches FL's playback.
fn base_key_for(iid: u16) -> u32 {
    let per_channel = format!("FLP_SLICEX_BASE_{}", iid);
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

/// How many trailing slices sit *below* the base key for this channel.
/// Per-channel only (`FLP_SLICEX_LOW_<iid>`); a global value would be
/// wrong for any Slicex that doesn't use this layout.
fn low_count_for(iid: u16) -> usize {
    std::env::var(format!("FLP_SLICEX_LOW_{}", iid))
    .ok()
    .and_then(|s| s.trim().parse::<usize>().ok())
    .unwrap_or(0)
}

// ── WAV → mono i16 ─────────────────────────────────────────────────────

struct Pcm {
    rate: u32,
    data: Vec<i16>,
}

fn read_wav_pcm(wav: &[u8]) -> Option<Pcm> {
    if wav.len() < 12 || &wav[0..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        return None;
    }
    let (mut tag, mut chans, mut rate, mut bits) = (0u16, 0u16, 0u32, 0u16);
    let mut data: &[u8] = &[];
    let mut p = 12usize;
    while p + 8 <= wav.len() {
        let id = &wav[p..p + 4];
        let size = u32::from_le_bytes([wav[p + 4], wav[p + 5], wav[p + 6], wav[p + 7]]) as usize;
        let body = p + 8;
        let end = body.saturating_add(size).min(wav.len());
        if id == b"fmt " && end - body >= 16 {
            tag = u16::from_le_bytes([wav[body], wav[body + 1]]);
            chans = u16::from_le_bytes([wav[body + 2], wav[body + 3]]);
            rate = u32::from_le_bytes([wav[body + 4], wav[body + 5], wav[body + 6], wav[body + 7]]);
            bits = u16::from_le_bytes([wav[body + 14], wav[body + 15]]);
        } else if id == b"data" {
            data = &wav[body..end];
        }
        p = body.checked_add(size)?.checked_add(size & 1)?;
    }
    if chans == 0 || bits != 16 || rate == 0 || data.is_empty() || (tag != 1 && tag != 0xFFFE) {
        return None;
    }
    let ch = chans as usize;
    let frames = data.len() / (2 * ch);
    let mut out = Vec::with_capacity(frames);
    for f in 0..frames {
        let mut sum = 0i32;
        for c in 0..ch {
            let o = (f * ch + c) * 2;
            sum += i16::from_le_bytes([data[o], data[o + 1]]) as i32;
        }
        out.push((sum / ch as i32) as i16);
    }
    Some(Pcm { rate, data: out })
}

// ── minimal SF2 writer ─────────────────────────────────────────────────

fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(body.len() + 9);
    v.extend_from_slice(id);
    v.extend_from_slice(&(body.len() as u32).to_le_bytes());
    v.extend_from_slice(body);
    if body.len() & 1 == 1 {
        v.push(0);
    }
    v
}

fn list(kind: &[u8; 4], parts: &[Vec<u8>]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(kind);
    for p in parts {
        body.extend_from_slice(p);
    }
    chunk(b"LIST", &body)
}

fn name20(s: &str) -> [u8; 20] {
    let mut a = [0u8; 20];
    for (i, b) in s.bytes().take(19).enumerate() {
        a[i] = b;
    }
    a
}

fn zstr(s: &str) -> Vec<u8> {
    let mut v = s.as_bytes().to_vec();
    v.push(0);
    if v.len() & 1 == 1 {
        v.push(0);
    }
    v
}

fn sf_gen(oper: u16, amount: u16) -> [u8; 4] {
    let o = oper.to_le_bytes();
    let a = amount.to_le_bytes();
    [o[0], o[1], a[0], a[1]]
}

fn phdr(name: &str, preset: u16, bank: u16, bag: u16) -> Vec<u8> {
    let mut v = name20(name).to_vec();
    v.extend_from_slice(&preset.to_le_bytes());
    v.extend_from_slice(&bank.to_le_bytes());
    v.extend_from_slice(&bag.to_le_bytes());
    v.extend_from_slice(&[0u8; 12]);
    v
}

fn build_sf2(name: &str, pcm: &Pcm, ranges: &[(usize, usize)], keys: &[u32]) -> Vec<u8> {
    // Three generators per zone: KeyRange (43), ReleaseVolEnv (38),
    // SampleID (53).  Emitted in that order, *not* the spec's ascending
    // opcode order.
    //
    // sfl2's `Zone::get_key_range_gen` only inspects `gens.get(0)`; if
    // KeyRange isn't at index 0 the zone is treated as active for every
    // key and every zone fires on every note.  Summing 41 slices of the
    // same source produces a synth-like tone.  The SF2 spec does not
    // require ascending opcode order, so we put KeyRange first.
    //
    // We deliberately do *not* write OverridingRootKey (58): sfl2's
    // `walk_igens` flushes a zone when it sees SampleID, so anything
    // after it would attach to the next zone's accumulator.  We rely on
    // shdr.original_pitch, which hi.rs uses as the root when
    // OverridingRootKey is -1 (the default).
    const GENS_PER_ZONE: u16 = 3;
    const GEN_RELEASE_VOL_ENV: u16 = 38;
    const GEN_KEY_RANGE: u16 = 43;
    const GEN_SAMPLE_ID: u16 = 53;
    const GEN_INSTRUMENT: u16 = 41;
    let release_tc = (1200.0f64 * 0.05f64.log2()).round() as i16 as u16;

    // 46 samples of silence before the pool and between every slice.
    // sfl2's Hermite resampler reads up to 2 samples *before* the
    // current cursor, so slice 0 would otherwise read out of bounds
    // on the first interpolation step; giving it the same silence the
    // inner slices get from the inter-slice pad keeps the whole pool
    // bounded.
    const PAD: usize = 46;

    let n = ranges.len();

    let mut pool: Vec<i16> = Vec::new();
    pool.extend(std::iter::repeat(0i16).take(PAD));

    let mut shdr: Vec<u8> = Vec::with_capacity((n + 1) * 46);
    for (i, &(s, e)) in ranges.iter().enumerate() {
        let start = pool.len() as u32;
        pool.extend_from_slice(&pcm.data[s..e]);
        let end = pool.len() as u32;
        pool.extend(std::iter::repeat(0i16).take(PAD));

        shdr.extend_from_slice(&name20(&format!("slice{:02}", i)));
        shdr.extend_from_slice(&start.to_le_bytes());
        shdr.extend_from_slice(&end.to_le_bytes());
        shdr.extend_from_slice(&start.to_le_bytes());
        shdr.extend_from_slice(&end.to_le_bytes());
        shdr.extend_from_slice(&pcm.rate.to_le_bytes());
        // Per-slice root, used by hi.rs as the transposition reference
        // when the zone has no OverridingRootKey generator.
        shdr.push(keys[i] as u8);
        shdr.push(0); // pitch correction
        shdr.extend_from_slice(&0u16.to_le_bytes()); // sample link
        // sfl2 checks this field.  1 makes its renderer happy; 0 makes
        // it silent.  Not spec-correct (1 is "right sample" per SF2)
        // but empirically required by this engine.
        shdr.extend_from_slice(&1u16.to_le_bytes());
    }
    shdr.extend_from_slice(&name20("EOS"));
    shdr.extend_from_slice(&[0u8; 26]);

    let mut smpl = Vec::with_capacity(pool.len() * 2);
    for s in &pool {
        smpl.extend_from_slice(&s.to_le_bytes());
    }

    let mut phdr_b = phdr(name, 0, 0, 0);
    phdr_b.extend_from_slice(&phdr("EOP", 0, 0, 1));
    let mut pbag = Vec::new();
    pbag.extend_from_slice(&[0, 0, 0, 0]);
    pbag.extend_from_slice(&[1, 0, 0, 0]);
    let pmod = vec![0u8; 10];
    let mut pgen = Vec::new();
    pgen.extend_from_slice(&sf_gen(GEN_INSTRUMENT, 0));
    pgen.extend_from_slice(&sf_gen(0, 0));

    let mut inst = Vec::new();
    inst.extend_from_slice(&name20(name));
    inst.extend_from_slice(&0u16.to_le_bytes());
    inst.extend_from_slice(&name20("EOI"));
    inst.extend_from_slice(&(n as u16).to_le_bytes());

    let mut ibag = Vec::new();
    let mut igen = Vec::new();
    for i in 0..n {
        ibag.extend_from_slice(&(i as u16 * GENS_PER_ZONE).to_le_bytes());
        ibag.extend_from_slice(&0u16.to_le_bytes());
        let key = keys[i] as u16;
        // KeyRange FIRST.  sfl2's `Zone::get_key_range_gen` only
        // inspects `gens.get(0)`; if KeyRange isn't at index 0 the
        // zone is treated as active for every key and every zone
        // fires on every note — summing 41 slices into a synth-like
        // tone instead of one slice per key.  This ordering is not
        // spec-ascending (43 > 38), but the SF2 spec does not require
        // ascending order and sfl2 requires KeyRange first.
        igen.extend_from_slice(&sf_gen(GEN_KEY_RANGE, key | (key << 8)));   // 43
        igen.extend_from_slice(&sf_gen(GEN_RELEASE_VOL_ENV, release_tc));   // 38
        igen.extend_from_slice(&sf_gen(GEN_SAMPLE_ID, i as u16));           // 53
    }
    ibag.extend_from_slice(&(n as u16 * GENS_PER_ZONE).to_le_bytes());
    ibag.extend_from_slice(&0u16.to_le_bytes());
    igen.extend_from_slice(&sf_gen(0, 0));
    let imod = vec![0u8; 10];

    let info = list(
        b"INFO",
        &[
            chunk(b"ifil", &[2, 0, 1, 0]),
                    chunk(b"isng", &zstr("EMU8000")),
                    chunk(b"INAM", &zstr(name)),
        ],
    );
    let sdta = list(b"sdta", &[chunk(b"smpl", &smpl)]);
    let pdta = list(
        b"pdta",
        &[
            chunk(b"phdr", &phdr_b),
                    chunk(b"pbag", &pbag),
                    chunk(b"pmod", &pmod),
                    chunk(b"pgen", &pgen),
                    chunk(b"inst", &inst),
                    chunk(b"ibag", &ibag),
                    chunk(b"imod", &imod),
                    chunk(b"igen", &igen),
                    chunk(b"shdr", &shdr),
        ],
    );

    let mut body = Vec::new();
    body.extend_from_slice(b"sfbk");
    body.extend_from_slice(&info);
    body.extend_from_slice(&sdta);
    body.extend_from_slice(&pdta);
    chunk(b"RIFF", &body)
}

// ── entry point ────────────────────────────────────────────────────────

pub(super) fn convert(
    chan: &Channel,
    plugin: &Plugin,
    aux_files: &mut Vec<AuxFile>,
    warnings: &mut Vec<String>,
) -> Result<SlicexOut, String> {
    let found = flp_audio::extract_all(&plugin.state);
    let aud = found
    .iter()
    .find(|a| a.data.len() >= MIN_EMBEDDED_BYTES)
    .ok_or_else(|| "no embedded audio in plugin state".to_string())?;
    let wav = if aud.extension == "flac" {
        flp_audio::flac_to_wav(&aud.data).map_err(|e| format!("embedded FLAC: {e:#}"))?
    } else {
        aud.data.clone()
    };

    if std::env::var_os("FLP_DEBUG").is_some() {
        let dump = format!("/tmp/slicex_chan{}.wav", chan.iid);
        std::fs::write(&dump, &wav).ok();
        eprintln!("slicex: chan {} extracted {} bytes -> {}", chan.iid, wav.len(), dump);
    }

    let mut cues = flp_audio::wav_cue_points(&wav);
    let pcm = read_wav_pcm(&wav).ok_or_else(|| "embedded WAV is not 16-bit PCM".to_string())?;
    cues.retain(|&c| (c as usize) < pcm.data.len());
    if cues.is_empty() {
        return Err("embedded WAV has no cue points (no slice markers)".into());
    }

    let base = base_key_for(chan.iid);
    let max_slices = (128 - base) as usize;
    if cues.len() > max_slices {
        warnings.push(format!(
            "[{:2}] Slicex: {} slices but only {} keys above {}; extra slices dropped",
            chan.iid,
            cues.len(),
                              max_slices,
                              base
        ));
        cues.truncate(max_slices);
    }

    let mut ranges: Vec<(usize, usize)> = Vec::with_capacity(cues.len());
    for (i, &c) in cues.iter().enumerate() {
        let s = c as usize;
        let e = cues.get(i + 1).map_or(pcm.data.len(), |&n| n as usize);
        ranges.push((s, e.max(s + 1).min(pcm.data.len())));
    }

    // Slice -> key layout.  Slices stay in time order in `ranges`; only the
    // key each one answers to changes.  With low == 0 this is base + i.
    let n = ranges.len();
    let low = low_count_for(chan.iid).min(n).min(base as usize);
    let keys: Vec<u32> = (0..n)
    .map(|i| {
        if i >= n - low {
            base - (n - i) as u32
        } else {
            base + i as u32
        }
    })
    .collect();
    let lo_key = keys.iter().copied().min().unwrap_or(base);

    let base_raw = if chan.name.is_empty() {
        format!("Ch{:02}", chan.iid)
    } else {
        chan.name.clone()
    };
    let inst_name = format!("{}_slicex", identifier(&base_raw));
    let sf2_rel = format!("samples/{}.sf2", inst_name);
    let inst_rel = format!("instruments/{}.instrument", inst_name);

    let sf2 = build_sf2(&inst_name, &pcm, &ranges, &keys);
    aux_files.push(AuxFile {
        rel_path: sf2_rel.clone(),
                   bytes: sf2,
    });

    let text = format!(
        "Instrument {inst_name}\n{{\n    Kind: sf2;\n    File: '{sf2_rel}';\n    Bank: 0;\n    Preset: 0;\n    SourceChannel: {};\n}}\n",
        chan.iid
    );
    aux_files.push(AuxFile {
        rel_path: inst_rel.clone(),
                   bytes: text.into_bytes(),
    });

    warnings.push(format!(
        "[{:2}] {}: Slicex -> generated SF2, {} slices on keys {}..{}{}",
        chan.iid,
        chan.name,
        n,
        lo_key,
        lo_key + n as u32 - 1,
        if low > 0 {
            format!(" (last {} slices placed below base {})", low, base)
        } else {
            String::new()
        }
    ));

    Ok(SlicexOut {
        instrument_rel: inst_rel,
       base_key: lo_key,
       slices: n,
    })
}
