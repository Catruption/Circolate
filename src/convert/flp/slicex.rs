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
//! `FLP_SLICEX_BASE=<0..127>` while the real mapping is being confirmed.

use crate::convert::{identifier, AuxFile};
use crate::sources::flp::{Channel, Plugin};
use crate::sources::flp_audio;

use super::MIN_EMBEDDED_BYTES;

const DEFAULT_BASE_KEY: u32 = 60;

pub(super) struct SlicexOut {
    /// Bundle-relative `.instrument` sidecar.
    pub(super) instrument_rel: String,
    pub(super) base_key: u32,
    pub(super) slices: usize,
}

pub(super) fn is_slicex(p: &Plugin) -> bool {
    p.internal_name.eq_ignore_ascii_case("slicex") || p.display_name.eq_ignore_ascii_case("slicex")
}

fn base_key() -> u32 {
    std::env::var("FLP_SLICEX_BASE")
    .ok()
    .and_then(|s| s.trim().parse::<u32>().ok())
    .filter(|k| *k < 128)
    .unwrap_or(DEFAULT_BASE_KEY)
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
    v.extend_from_slice(&[0u8; 12]); // library, genre, morphology
    v
}

/// One preset (bank 0, preset 0) with one single-key zone per slice.
/// Zone `i` answers only to key `base + i` and has root key `base + i`,
/// so it plays at the sample's original pitch.
fn build_sf2(name: &str, pcm: &Pcm, ranges: &[(usize, usize)], base: u32) -> Vec<u8> {
    const GENS_PER_ZONE: u16 = 4;
    const GEN_RELEASE_VOL_ENV: u16 = 38;
    const GEN_KEY_RANGE: u16 = 43;
    const GEN_SAMPLE_ID: u16 = 53;
    const GEN_ROOT_KEY: u16 = 58;
    const GEN_INSTRUMENT: u16 = 41;
    // 50 ms release, in timecents: 1200 * log2(0.05).
    let release_tc = (1200.0f64 * 0.05f64.log2()).round() as i16 as u16;

    let n = ranges.len();

    // sample pool + sample headers
    let mut pool: Vec<i16> = Vec::new();
    let mut shdr: Vec<u8> = Vec::with_capacity((n + 1) * 46);
    for (i, &(s, e)) in ranges.iter().enumerate() {
        let start = pool.len() as u32;
        pool.extend_from_slice(&pcm.data[s..e]);
        let end = pool.len() as u32;
        pool.extend(std::iter::repeat(0i16).take(46));

        shdr.extend_from_slice(&name20(&format!("slice{:02}", i)));
        shdr.extend_from_slice(&start.to_le_bytes());
        shdr.extend_from_slice(&end.to_le_bytes());
        shdr.extend_from_slice(&start.to_le_bytes()); // loop start (unused)
        shdr.extend_from_slice(&end.to_le_bytes()); // loop end (unused)
        shdr.extend_from_slice(&pcm.rate.to_le_bytes());
        shdr.push(60); // original pitch
        shdr.push(0); // pitch correction
        shdr.extend_from_slice(&0u16.to_le_bytes()); // sample link
        shdr.extend_from_slice(&1u16.to_le_bytes()); // mono
    }
    shdr.extend_from_slice(&name20("EOS"));
    shdr.extend_from_slice(&[0u8; 26]);

    let mut smpl = Vec::with_capacity(pool.len() * 2);
    for s in &pool {
        smpl.extend_from_slice(&s.to_le_bytes());
    }

    // preset layer
    let mut phdr_b = phdr(name, 0, 0, 0);
    phdr_b.extend_from_slice(&phdr("EOP", 0, 0, 1));
    let mut pbag = Vec::new();
    pbag.extend_from_slice(&[0, 0, 0, 0]);
    pbag.extend_from_slice(&[1, 0, 0, 0]);
    let pmod = vec![0u8; 10];
    let mut pgen = Vec::new();
    pgen.extend_from_slice(&sf_gen(GEN_INSTRUMENT, 0));
    pgen.extend_from_slice(&sf_gen(0, 0));

    // instrument layer
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
        let key = (base + i as u32) as u16;
        igen.extend_from_slice(&sf_gen(GEN_KEY_RANGE, key | (key << 8)));
        igen.extend_from_slice(&sf_gen(GEN_RELEASE_VOL_ENV, release_tc));
        igen.extend_from_slice(&sf_gen(GEN_ROOT_KEY, key));
        igen.extend_from_slice(&sf_gen(GEN_SAMPLE_ID, i as u16));
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

/// Turn a Slicex channel into a generated SF2 + `.instrument` sidecar.
/// `Err` carries a short reason the channel was skipped.
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

    let mut cues = flp_audio::wav_cue_points(&wav);
    let pcm = read_wav_pcm(&wav)
    .ok_or_else(|| "embedded WAV is not 16-bit PCM".to_string())?;
    cues.retain(|&c| (c as usize) < pcm.data.len());
    if cues.is_empty() {
        return Err("embedded WAV has no cue points (no slice markers)".into());
    }

    let base = base_key();
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
        // Keep the key mapping intact even for a degenerate slice.
        ranges.push((s, e.max(s + 1).min(pcm.data.len())));
    }

    let base_raw = if chan.name.is_empty() {
        format!("Ch{:02}", chan.iid)
    } else {
        chan.name.clone()
    };
    let inst_name = format!("{}_slicex", identifier(&base_raw));
    let sf2_rel = format!("samples/{}.sf2", inst_name);
    let inst_rel = format!("instruments/{}.instrument", inst_name);

    let sf2 = build_sf2(&inst_name, &pcm, &ranges, base);
    aux_files.push(AuxFile { rel_path: sf2_rel.clone(), bytes: sf2 });

    let text = format!(
        "Instrument {inst_name}\n{{\n    Kind: sf2;\n    File: '{sf2_rel}';\n    Bank: 0;\n    Preset: 0;\n    SourceChannel: {};\n}}\n",
        chan.iid
    );
    aux_files.push(AuxFile { rel_path: inst_rel.clone(), bytes: text.into_bytes() });

    warnings.push(format!(
        "[{:2}] {}: Slicex -> generated SF2, {} slices on keys {}..{}",
        chan.iid,
        chan.name,
        ranges.len(),
                          base,
                          base + ranges.len() as u32 - 1
    ));

    Ok(SlicexOut {
        instrument_rel: inst_rel,
       base_key: base,
       slices: ranges.len(),
    })
}
