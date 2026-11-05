//! Minimal SF2 writer and PCM decoder.
//!
//! ## Provenance
//!
//! SRC[loc=n/a (output side) decomp=n/a conf=n/a]
//!
//! Nothing here is read from the plugin state; this is the SoundFont 2.01 writer.
//! Only the inputs it is handed come from the decompile (see params.rs and
//! envelope.rs).  Assumptions it encodes, to re-check when output differs:
//!
//!   * amp envelope and SPEED are baked into the PCM (linear amplitude, one-shot)
//!   * the SF2 volume envelope only gets a 10 ms release (our choice, avoids clicks)
//!   * each slice is its own zone, key range = root key = the slice's key
//!   * stereo WAVs are mixed to mono

use super::envelope::{curve_k, gain_at, speed_mult, ms_to_timecents, FilterEnvelope};
use super::trest::EnvelopePoint;

pub(super) struct Pcm {
    pub(super) rate: u32,
    pub(super) data: Vec<i16>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FilterSettings {
    pub cutoff_cents: Option<i16>,
    pub pan: Option<i16>,
    pub attenuation_cb: Option<u16>,
}

pub(super) fn read_wav_pcm(wav: &[u8]) -> Option<Pcm> {
    if wav.len() < 12 || &wav[0..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        return None;
    }
    let (mut tag, mut chans, mut rate, mut bits) = (0u16, 0u16, 0u32, 0u16);
    let mut data: &[u8] = &[];
    let mut p = 12usize;
    while p + 8 <= wav.len() {
        let id = &wav[p..p + 4];
        let size = u32::from_le_bytes([wav[p+4], wav[p+5], wav[p+6], wav[p+7]]) as usize;
        let body = p + 8;
        let end = body.saturating_add(size).min(wav.len());
        if id == b"fmt " && end - body >= 16 {
            tag = u16::from_le_bytes([wav[body], wav[body + 1]]);
            chans = u16::from_le_bytes([wav[body + 2], wav[body + 3]]);
            rate = u32::from_le_bytes([wav[body+4], wav[body+5], wav[body+6], wav[body+7]]);
            bits = u16::from_le_bytes([wav[body + 14], wav[body + 15]]);
            if tag == 0xFFFE && end - body >= 26 {
                tag = u16::from_le_bytes([wav[body + 24], wav[body + 25]]);
            }
        } else if id == b"data" {
            data = &wav[body..end];
        }
        p = body.checked_add(size)?.checked_add(size & 1)?;
    }
    if chans == 0 || rate == 0 || data.is_empty() {
        return None;
    }
    if !matches!((tag, bits), (1, 16) | (1, 24) | (1, 32) | (3, 32)) {
        return None;
    }

    let ch = chans as usize;
    let bps = (bits / 8) as usize;
    let frames = data.len() / (bps * ch);
    let sample = |o: usize| -> f32 {
        match (tag, bits) {
            (1, 16) => i16::from_le_bytes([data[o], data[o + 1]]) as f32 / 32768.0,
            (1, 24) => {
                let v = ((data[o + 2] as i32) << 24
                | (data[o + 1] as i32) << 16
                | (data[o] as i32) << 8) >> 8;
                v as f32 / 8_388_608.0
            }
            (1, 32) => {
                i32::from_le_bytes([data[o], data[o+1], data[o+2], data[o+3]]) as f32
                / 2_147_483_648.0
            }
            _ => f32::from_le_bytes([data[o], data[o+1], data[o+2], data[o+3]]),
        }
    };

    let mut out = Vec::with_capacity(frames);
    for f in 0..frames {
        let mut sum = 0f32;
        for c in 0..ch {
            sum += sample((f * ch + c) * bps);
        }
        let m = (sum / ch as f32).clamp(-1.0, 1.0);
        out.push((m * 32767.0).round() as i16);
    }
    Some(Pcm { rate, data: out })
}

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

fn wav16_mono(data: &[i16], rate: u32) -> Vec<u8> {
    let n = (data.len() * 2) as u32;
    let mut v = Vec::with_capacity(44 + n as usize);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + n).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&rate.to_le_bytes());
    v.extend_from_slice(&(rate * 2).to_le_bytes());
    v.extend_from_slice(&2u16.to_le_bytes());
    v.extend_from_slice(&16u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&n.to_le_bytes());
    for s in data {
        v.extend_from_slice(&s.to_le_bytes());
    }
    v
}

fn hermite(y: &[f32], pos: f64) -> f32 {
    let i = pos.floor() as isize;
    let f = (pos - i as f64) as f32;
    let at = |j: isize| y[j.clamp(0, y.len() as isize - 1) as usize];
    let (ym1, y0, y1, y2) = (at(i - 1), at(i), at(i + 1), at(i + 2));
    let c0 = y0;
    let c1 = 0.5 * (y1 - ym1);
    let c2 = ym1 - 2.5 * y0 + 2.0 * y1 - 0.5 * y2;
    let c3 = 0.5 * (y2 - ym1) + 1.5 * (y0 - y1);
    ((c3 * f + c2) * f + c1) * f + c0
}

/// Apply the SPEED envelope (resampling: changes pitch and duration) and
/// then the amp envelope, both evaluated in real output time.  Slicex
/// envelopes are one-shot so both are baked, not sent to SF2.
// SRC[loc=n/a decomp=n/a conf=guess]  the order (speed first, then amp on the output timeline) is a guess.
fn render_slice(
    src: &[i16],
    rate: u32,
    amp_pts: Option<&[EnvelopePoint]>,
    speed_pts: Option<&[EnvelopePoint]>,
    scale: f64,
    k: f64,
    speed_octaves: f64,
) -> Vec<i16> {
    let rate_f = rate.max(1) as f64;
    let samples: Vec<f32> = src.iter().map(|&v| v as f32).collect();

    // 1. speed -> resampled signal
    let resampled: Vec<f32> = match speed_pts {
        Some(sp) if !sp.is_empty() && !samples.is_empty() => {
            let max_out = (samples.len() * 4).min((rate_f * 30.0) as usize);
            let mut out = Vec::with_capacity(samples.len());
            let mut pos = 0.0f64;
            let mut n = 0usize;
            while pos < (samples.len() - 1) as f64 && n < max_out {
                out.push(hermite(&samples, pos));
                let t = (n as f64 / rate_f) / scale;
                pos += speed_mult(gain_at(sp, t, k), speed_octaves);
                n += 1;
            }
            out
        }
        _ => samples,
    };

    // 2. amp envelope on the output timeline
    resampled
    .iter()
    .enumerate()
    .map(|(j, &x)| {
        let g = match amp_pts {
            Some(p) if !p.is_empty() => gain_at(p, (j as f64 / rate_f) / scale, k),
         _ => 1.0,
        };
        (x as f64 * g).round().clamp(-32768.0, 32767.0) as i16
    })
    .collect()
}

pub(super) fn build(
    name: &str,
    pcm: &Pcm,
    ranges: &[(usize, usize)],
                    keys: &[u32],
                    amp_pts: Option<&[EnvelopePoint]>,
                    env_scale: Option<f64>,
                    speed_pts: Option<&[EnvelopePoint]>,
                    speed_octaves: f64,
                    filter_env: Option<&FilterEnvelope>,
                    filt: &FilterSettings,
) -> Vec<u8> {
    const GEN_INITIAL_FILTER_FC: u16 = 8;
    const GEN_MOD_ENV_TO_FILTER_FC: u16 = 11;
    const GEN_PAN: u16 = 17;
    const GEN_DELAY_MOD_ENV: u16 = 25;
    const GEN_ATTACK_MOD_ENV: u16 = 26;
    const GEN_HOLD_MOD_ENV: u16 = 27;
    const GEN_DECAY_MOD_ENV: u16 = 28;
    const GEN_SUSTAIN_MOD_ENV: u16 = 29;
    const GEN_RELEASE_MOD_ENV: u16 = 30;
    const GEN_DELAY_VOL_ENV: u16 = 33;
    const GEN_ATTACK_VOL_ENV: u16 = 34;
    const GEN_HOLD_VOL_ENV: u16 = 35;
    const GEN_DECAY_VOL_ENV: u16 = 36;
    const GEN_SUSTAIN_VOL_ENV: u16 = 37;
    const GEN_RELEASE_VOL_ENV: u16 = 38;
    const GEN_INSTRUMENT: u16 = 41;
    const GEN_KEY_RANGE: u16 = 43;
    const GEN_INITIAL_ATTENUATION: u16 = 48;
    const GEN_SAMPLE_ID: u16 = 53;

    let mut globals: Vec<[u8; 4]> = Vec::new();

    if let Some(c) = filt.cutoff_cents {
        globals.push(sf_gen(GEN_INITIAL_FILTER_FC, c as u16));
    }
    if let Some(p) = filt.pan {
        if p != 0 {
            globals.push(sf_gen(GEN_PAN, p as u16));
        }
    }
    if let Some(a) = filt.attenuation_cb {
        if a != 0 {
            globals.push(sf_gen(GEN_INITIAL_ATTENUATION, a));
        }
    }

    // The amp envelope is baked into the PCM below (linear amplitude, one-shot),
    // so the SF2 volume envelope only needs a short release to avoid clicks
    // at note-off.
    globals.push(sf_gen(GEN_RELEASE_VOL_ENV, ms_to_timecents(10.0)));

    if let Some(fe) = filter_env {
        if fe.delay_ms > 0.5 {
            globals.push(sf_gen(GEN_DELAY_MOD_ENV, ms_to_timecents(fe.delay_ms)));
        }
        if fe.attack_ms > 0.5 {
            globals.push(sf_gen(GEN_ATTACK_MOD_ENV, ms_to_timecents(fe.attack_ms)));
        }
        if fe.hold_ms > 0.5 {
            globals.push(sf_gen(GEN_HOLD_MOD_ENV, ms_to_timecents(fe.hold_ms)));
        }
        if fe.decay_ms > 0.5 {
            globals.push(sf_gen(GEN_DECAY_MOD_ENV, ms_to_timecents(fe.decay_ms)));
        }
        let sus_cb = ((1.0 - fe.sustain.clamp(0.0, 1.0)) * 1000.0).round() as i16;
        globals.push(sf_gen(GEN_SUSTAIN_MOD_ENV, sus_cb as u16));
        if fe.release_ms > 0.5 {
            globals.push(sf_gen(GEN_RELEASE_MOD_ENV, ms_to_timecents(fe.release_ms)));
        }
        globals.push(sf_gen(GEN_MOD_ENV_TO_FILTER_FC, fe.depth_cents as u16));
    }

    let gens_per_zone: u16 = 2 + globals.len() as u16;
    const PAD: usize = 46;
    let n = ranges.len();
    let k = curve_k();

    let mut pool: Vec<i16> = Vec::new();
    pool.extend(std::iter::repeat(0i16).take(PAD));

    let mut shdr: Vec<u8> = Vec::with_capacity((n + 1) * 46);
    for (i, &(s, e)) in ranges.iter().enumerate() {
        let start = pool.len() as u32;
        let slice_secs = (e - s) as f64 / pcm.rate.max(1) as f64;
        let scale = env_scale.unwrap_or(slice_secs).max(0.001);
        let rendered = render_slice(
            &pcm.data[s..e], pcm.rate, amp_pts, speed_pts, scale, k, speed_octaves,
        );
        if let Some(dir) = std::env::var_os("SLICEX_DUMP_SLICES") {
            let dir = std::path::PathBuf::from(dir);
            std::fs::create_dir_all(&dir).ok();
            std::fs::write(
                dir.join(format!("{}_slice{:02}_key{}.wav", name, i, keys[i])),
                           wav16_mono(&rendered, pcm.rate),
            )
            .ok();
        }
        pool.extend(rendered);
        let end = pool.len() as u32;
        pool.extend(std::iter::repeat(0i16).take(PAD));

        shdr.extend_from_slice(&name20(&format!("slice{:02}", i)));
        shdr.extend_from_slice(&start.to_le_bytes());
        shdr.extend_from_slice(&end.to_le_bytes());
        shdr.extend_from_slice(&start.to_le_bytes());
        shdr.extend_from_slice(&end.to_le_bytes());
        shdr.extend_from_slice(&pcm.rate.to_le_bytes());
        shdr.push(keys[i] as u8);
        shdr.push(0);
        shdr.extend_from_slice(&0u16.to_le_bytes());
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

    // Zone mapping fix: instead of only mapping the explicit keys, we map
    // all 128 MIDI keys. Notes played above the slice range wrap around
    // (modulo n), matching FL Studio Slicex's overflow behavior.
    let zone_count = if n > 0 { 128u16 } else { 0u16 };

    let mut inst = Vec::new();
    inst.extend_from_slice(&name20(name));
    inst.extend_from_slice(&0u16.to_le_bytes());
    inst.extend_from_slice(&name20("EOI"));
    inst.extend_from_slice(&zone_count.to_le_bytes());

    let mut ibag = Vec::new();
    let mut igen = Vec::new();

    if n > 0 {
        let base_key = keys.first().copied().unwrap_or(60) as i32;
        let n_i32 = n as i32;

        for k in 0..128u16 {
            let slice_idx = ((k as i32 - base_key).rem_euclid(n_i32)) as usize;

            ibag.extend_from_slice(&(k * gens_per_zone).to_le_bytes());
            ibag.extend_from_slice(&0u16.to_le_bytes());

            let key = k;
            igen.extend_from_slice(&sf_gen(GEN_KEY_RANGE, key | (key << 8)));
            for g in &globals {
                igen.extend_from_slice(g);
            }
            igen.extend_from_slice(&sf_gen(GEN_SAMPLE_ID, slice_idx as u16));
        }
    }

    ibag.extend_from_slice(&(zone_count * gens_per_zone).to_le_bytes());
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
