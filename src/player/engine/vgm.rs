//! VGM playback inside the engine (goes in `src/player/engine/vgm.rs`).
//!
//! * The VGM command stream is parsed once into a flat, time-stamped event
//!   list, so seeking is just "reset the chips and replay register writes".
//! * SN76489 (PSG) is emulated natively below.
//! * YM2612 goes through the small `Ym2612` wrapper at the bottom of this
//!   file (ymfm via the `ymfm-sys` 0.1 crate). That wrapper is the ONLY place
//!   that touches `ymfm-sys`.
//!
//! The player is positioned in engine frames: `render()` is called with the
//! engine's `block_start`, and if that is not where the player left off
//! (seek, loop wrap, resume after rewind) it re-seeks first.
//!
//! Like the VST path, the player keeps a short ring buffer of recently
//! rendered audio so the tracker's braille scope can show a live envelope.
//! The ring is split into a PSG tap and a YM tap (the YM tap mixes all six
//! FM channels + DAC), which lets `VgmChip::from_source_path` route a part
//! to the right one from its `vgm_embedded/FM1`-style source string.

const VGM_RATE: f64 = 44100.0;

/// Output level of the PSG relative to full scale (before song/part volume).
const PSG_LEVEL: f32 = 0.15;

/// ymfm's OPN2 output is roughly 16-bit (+-32767). Scaled so a full Genesis FM mix
/// sits comfortably below clipping next to the PSG. Raise/lower to taste.
const YM_SCALE: f32 = 0.7 / 32768.0;

/// Ring buffer size for the live scope. Matches the VST scope so
/// `envelope::synth_envelope_live` bins the same amount of history for both.
const SCOPE_LEN: usize = 4096;

/// Which signal a scope consumer wants. The audio path always renders the
/// full mix; this is only for the visualiser.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VgmChip {
    All,
    Psg,
    Ym,
}

impl VgmChip {
    /// Pick a scope from a VGM part's source path (as emitted by the
    /// converter: `vgm_embedded/FM1`, `vgm_embedded/PSG2`, `vgm_embedded`, …).
    pub(crate) fn from_source_path(path: &str) -> Self {
        let lower = path.to_ascii_lowercase();
        let tail = lower.rsplit('/').next().unwrap_or(&lower);
        if tail == "dac" || tail.starts_with("fm") {
            VgmChip::Ym
        } else if tail == "noise" || tail.starts_with("psg") {
            VgmChip::Psg
        } else {
            VgmChip::All
        }
    }
}

#[derive(Clone, Copy)]
enum Op {
    Sn(u8),
    Ym { port: u8, reg: u8, val: u8 },
}

#[derive(Clone, Copy)]
struct Ev {
    /// Time in VGM samples (44100 Hz).
    t: u32,
    op: Op,
}

struct Parsed {
    events: Vec<Ev>,
    sn_clock: u32,
    sn_feedback: u16,
    sn_width: u8,
    ym_clock: u32,
}

fn rd16(b: &[u8], o: usize) -> u16 {
    if o + 2 <= b.len() { u16::from_le_bytes([b[o], b[o + 1]]) } else { 0 }
}

fn rd32(b: &[u8], o: usize) -> u32 {
    if o + 4 <= b.len() {
        u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
    } else {
        0
    }
}

fn parse(bytes: &[u8]) -> Option<Parsed> {
    if bytes.len() < 0x40 || &bytes[0..4] != b"Vgm " {
        return None;
    }
    let len = bytes.len();
    let version = rd32(bytes, 0x08);

    let sn_clock = rd32(bytes, 0x0C) & 0x3FFF_FFFF;
    let (sn_feedback, sn_width) = if version >= 0x110 {
        let fb = rd16(bytes, 0x28);
        let w = bytes[0x2A];
        (if fb == 0 { 0x0009 } else { fb }, if w == 0 { 16 } else { w })
    } else {
        (0x0009, 16)
    };
    let ym_clock = if version >= 0x110 { rd32(bytes, 0x2C) & 0x3FFF_FFFF } else { 0 };

    let data_start = if version >= 0x150 {
        let rel = rd32(bytes, 0x34);
        if rel == 0 { 0x40 } else { 0x34usize.saturating_add(rel as usize) }
    } else {
        0x40
    };
    if data_start >= len {
        return None;
    }

    let mut events: Vec<Ev> = Vec::new();
    let mut pcm: Vec<u8> = Vec::new();
    let mut pcm_ptr: usize = 0;
    let mut t: u32 = 0;
    let mut p = data_start;

    while p < len {
        let c = bytes[p];
        match c {
            0x50 => {
                if p + 1 < len {
                    events.push(Ev { t, op: Op::Sn(bytes[p + 1]) });
                }
                p += 2;
            }
            0x52 | 0x53 => {
                if p + 2 < len {
                    events.push(Ev {
                        t,
                        op: Op::Ym { port: c - 0x52, reg: bytes[p + 1], val: bytes[p + 2] },
                    });
                }
                p += 3;
            }
            0x61 => {
                t = t.saturating_add(rd16(bytes, p + 1) as u32);
                p += 3;
            }
            0x62 => {
                t = t.saturating_add(735);
                p += 1;
            }
            0x63 => {
                t = t.saturating_add(882);
                p += 1;
            }
            0x66 => break,
            0x67 => {
                // Data block: 0x67 0x66 tt ssssssss <data>
                if p + 7 > len {
                    break;
                }
                let tt = bytes[p + 2];
                let size = rd32(bytes, p + 3) as usize;
                let start = p + 7;
                let end = start.saturating_add(size).min(len);
                if tt == 0x00 {
                    // YM2612 PCM bank (used by 0x8n DAC writes).
                    pcm.extend_from_slice(&bytes[start..end]);
                }
                p = start.saturating_add(size);
            }
            0x70..=0x7F => {
                t = t.saturating_add((c & 0x0F) as u32 + 1);
                p += 1;
            }
            0x80..=0x8F => {
                // YM2612 DAC write from the PCM bank, then wait n samples.
                let b = pcm.get(pcm_ptr).copied().unwrap_or(0x80);
                pcm_ptr += 1;
                events.push(Ev { t, op: Op::Ym { port: 0, reg: 0x2A, val: b } });
                t = t.saturating_add((c & 0x0F) as u32);
                p += 1;
            }
            0xE0 => {
                pcm_ptr = rd32(bytes, p + 1) as usize;
                p += 5;
            }
            // Everything below is skipped by operand length (spec ranges).
            0x30..=0x3F => p += 2,
            0x40..=0x4E => p += 3,
            0x4F => p += 2,
            0x51 | 0x54..=0x5F => p += 3,
            0x64 => p += 4,
            0x68 => p += 12,
            0x90 | 0x91 | 0x95 => p += 5,
            0x92 => p += 6,
            0x93 => p += 11,
            0x94 => p += 2,
            0xA0..=0xBF => p += 3,
            0xC0..=0xDF => p += 4,
            0xE1..=0xFF => p += 5,
            _ => p += 1,
        }
    }

    Some(Parsed { events, sn_clock, sn_feedback, sn_width, ym_clock })
}

// ───────────────────────── SN76489 ─────────────────────────

struct Sn76489 {
    tone: [u16; 3],
    vol: [u8; 4],
    noise_ctl: u8,
    counter: [i32; 4],
    out: [bool; 3],
    noise_out: bool,
    lfsr: u32,
    fb_mask: u32,
    width: u32,
    latch_ch: usize,
    latch_vol: bool,
    /// Last per-voice level (tone 1-3, noise), centred on zero. Visualiser only.
    voice: [f32; 4],
}

impl Sn76489 {
    fn new(feedback: u16, width: u8) -> Self {
        let width = (width as u32).clamp(1, 31);
        Sn76489 {
            tone: [0; 3],
            vol: [15; 4],
            noise_ctl: 0,
            counter: [1; 4],
            out: [false; 3],
            noise_out: false,
            lfsr: 1 << (width - 1),
            fb_mask: feedback as u32,
            width,
            latch_ch: 0,
            latch_vol: false,
            voice: [0.0; 4],
        }
    }

    fn set_noise(&mut self, ctl: u8) {
        self.noise_ctl = ctl & 7;
        self.lfsr = 1 << (self.width - 1);
    }

    fn write(&mut self, b: u8) {
        if b & 0x80 != 0 {
            self.latch_ch = ((b >> 5) & 3) as usize;
            self.latch_vol = b & 0x10 != 0;
            let d = b & 0x0F;
            let ch = self.latch_ch;
            if self.latch_vol {
                self.vol[ch] = d;
            } else if ch < 3 {
                self.tone[ch] = (self.tone[ch] & 0x3F0) | d as u16;
            } else {
                self.set_noise(d);
            }
        } else {
            let d = b & 0x3F;
            let ch = self.latch_ch;
            if self.latch_vol {
                self.vol[ch] = d & 0x0F;
            } else if ch < 3 {
                self.tone[ch] = (self.tone[ch] & 0x00F) | ((d as u16) << 4);
            } else {
                self.set_noise(d & 7);
            }
        }
    }

    fn noise_period(&self) -> i32 {
        match self.noise_ctl & 3 {
            0 => 0x10,
            1 => 0x20,
            2 => 0x40,
            _ => self.tone[2].max(1) as i32,
        }
    }

    fn amp(v: u8) -> f32 {
        if v >= 15 { 0.0 } else { 10f32.powf(-0.1 * v as f32) }
    }

    /// One internal tick (clock / 16). Returns the mixed, positive-only level.
    fn tick(&mut self) -> f32 {
        for ch in 0..3 {
            self.counter[ch] -= 1;
            if self.counter[ch] <= 0 {
                self.counter[ch] = self.tone[ch].max(1) as i32;
                self.out[ch] = !self.out[ch];
            }
        }
        self.counter[3] -= 1;
        if self.counter[3] <= 0 {
            self.counter[3] = self.noise_period();
            self.noise_out = !self.noise_out;
            if self.noise_out {
                let fbit = if self.noise_ctl & 4 != 0 {
                    (self.lfsr & self.fb_mask).count_ones() & 1
                } else {
                    self.lfsr & 1
                };
                self.lfsr = (self.lfsr >> 1) | (fbit << (self.width - 1));
            }
        }

        let mut mix = 0.0f32;
        for ch in 0..3 {
            let a = Self::amp(self.vol[ch]);
            if self.out[ch] {
                mix += a;
            }
            self.voice[ch] = if self.out[ch] { 0.5 * a } else { -0.5 * a };
        }
        let na = Self::amp(self.vol[3]);
        if self.lfsr & 1 != 0 {
            mix += na;
        }
        self.voice[3] = if self.lfsr & 1 != 0 { 0.5 * na } else { -0.5 * na };
        mix
    }
}

// ───────────────────────── player ─────────────────────────

pub(crate) struct VgmPlayer {
    events: Vec<Ev>,
    sn_clock: u32,
    sn_feedback: u16,
    sn_width: u8,
    ym_clock: u32,

    sn: Option<Sn76489>,
    ym: Option<Ym2612>,

    /// Next event to apply.
    idx: usize,
    /// Current position in VGM samples (44100 Hz).
    vgm_time: f64,
    /// Engine frame the player is positioned at.
    next_frame: i64,
    sr: f64,

    // Resampling state.
    sn_acc: f64,
    sn_last: f32,
    dc_x: f32,
    dc_y: f32,
    ym_frac: f64,
    ym_prev: (f32, f32),
    ym_next: (f32, f32),

    // Live scope (visualiser only). `psg` is post-DC-filter PSG; `ym` is
    // the mono sum of the YM2612 (all six FM channels + DAC).
    scope_psg: Vec<f32>,
    scope_ym: Vec<f32>,
    scope_pos: usize,
    scope_filled: usize,

    // Per-block scratch for the global UI scope (see `player::scope`).
    scratch_psg: Vec<f32>,
    scratch_ym: Vec<f32>,
    scratch_voice: [Vec<f32>; 4],
    scratch_fm: [Vec<f32>; 6],

    // Latest mono samples from the native YM2612 per-channel tap.
    solo_last: [f32; 6],
    sn_last_voice: [f32; 4],
}

impl VgmPlayer {
    /// `bytes` must be an already-decompressed VGM (the converter inflates `.vgz`).
    pub(crate) fn new(bytes: &[u8], sample_rate: f64) -> Option<Self> {
        let parsed = parse(bytes)?;
        let mut p = VgmPlayer {
            events: parsed.events,
            sn_clock: parsed.sn_clock,
            sn_feedback: parsed.sn_feedback,
            sn_width: parsed.sn_width,
            ym_clock: parsed.ym_clock,
            sn: None,
            ym: None,
            idx: 0,
            vgm_time: 0.0,
            next_frame: 0,
            sr: sample_rate.max(1.0),
            sn_acc: 0.0,
            sn_last: 0.0,
            dc_x: 0.0,
            dc_y: 0.0,
            ym_frac: 0.0,
            ym_prev: (0.0, 0.0),
            ym_next: (0.0, 0.0),
            scope_psg: vec![0.0; SCOPE_LEN],
            scope_ym: vec![0.0; SCOPE_LEN],
            scope_pos: 0,
            scope_filled: 0,
            scratch_psg: Vec::new(),
            scratch_ym: Vec::new(),
            scratch_voice: Default::default(),
            scratch_fm: Default::default(),
            solo_last: [0.0; 6],
            sn_last_voice: [0.0; 4],
        };
        p.reset();
        Some(p)
    }

    fn reset(&mut self) {
        self.sn = if self.sn_clock > 0 {
            Some(Sn76489::new(self.sn_feedback, self.sn_width))
        } else {
            None
        };
        if self.ym_clock > 0 {
            match self.ym.as_mut() {
                Some(ym) => ym.reset(),
                None => self.ym = Some(Ym2612::new(self.ym_clock)),
            }
        } else {
            self.ym = None;
        }
        self.solo_last = [0.0; 6];
        self.sn_last_voice = [0.0; 4];
        self.idx = 0;
        self.vgm_time = 0.0;
        self.next_frame = 0;
        self.sn_acc = 0.0;
        self.sn_last = 0.0;
        self.dc_x = 0.0;
        self.dc_y = 0.0;
        self.ym_frac = 0.0;
        self.ym_prev = (0.0, 0.0);
        self.ym_next = (0.0, 0.0);
        // Scope: clear on seek so the visualiser doesn't show audio from
        // before the scrub target. It refills within ~85 ms.
        self.scope_pos = 0;
        self.scope_filled = 0;
    }

    fn apply(&mut self, op: Op) {
        match op {
            Op::Sn(b) => {
                if let Some(sn) = self.sn.as_mut() {
                    sn.write(b);
                }
            }
            Op::Ym { port, reg, val } => {
                if let Some(ym) = self.ym.as_mut() {
                    ym.write(port, reg, val);
                }
            }
        }
    }

    /// Reposition to engine frame `frame`: reset the chips and replay every
    /// register write before that time without generating audio.
    fn seek(&mut self, frame: i64) {
        self.reset();
        let frame = frame.max(0);
        let target = frame as f64 * VGM_RATE / self.sr;
        while self.idx < self.events.len() && (self.events[self.idx].t as f64) < target {
            let op = self.events[self.idx].op;
            self.apply(op);
            self.idx += 1;
        }
        self.vgm_time = target;
        self.next_frame = frame;
    }

    fn push_scope(&mut self, psg: f32, ym: f32) {
        let len = self.scope_psg.len();
        let p = self.scope_pos;
        self.scope_psg[p] = psg;
        self.scope_ym[p] = ym;
        self.scope_pos = (p + 1) % len;
        if self.scope_filled < len {
            self.scope_filled += 1;
        }
    }

    /// Oldest→newest samples of the requested scope tap, for the braille
    /// waveform widget. Empty until `render()` has produced some audio.
    pub(crate) fn scope_snapshot(&self, chip: VgmChip) -> Vec<f32> {
        let len = self.scope_psg.len();
        let n = self.scope_filled;
        let start = if n < len { 0 } else { self.scope_pos };
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let idx = (start + i) % len;
            let s = match chip {
                VgmChip::Psg => self.scope_psg[idx],
                VgmChip::Ym => self.scope_ym[idx],
                VgmChip::All => self.scope_psg[idx] + self.scope_ym[idx],
            };
            out.push(s);
        }
        out
    }

    /// Add this VGM's audio for engine frames `block_start..block_start+len`
    /// into `out_l` / `out_r`, scaled by `gain`.
    pub(crate) fn render(
        &mut self,
        out_l: &mut [f32],
        out_r: &mut [f32],
        block_start: i64,
        gain: f32,
    ) {
        if block_start != self.next_frame {
            self.seek(block_start);
        }

        let n = out_l.len().min(out_r.len());
        self.scratch_psg.clear();
        self.scratch_ym.clear();
        self.scratch_psg.reserve(n);
        self.scratch_ym.reserve(n);
        for b in self.scratch_voice.iter_mut().chain(self.scratch_fm.iter_mut()) {
            b.clear();
            b.reserve(n);
        }
        let step = VGM_RATE / self.sr;
        let sn_ratio = self.sn_clock as f64 / 16.0 / self.sr;
        let ym_ratio = self.ym.as_ref().map_or(0.0, |y| y.native_rate()) / self.sr;

        for i in 0..n {
            while self.idx < self.events.len()
                && (self.events[self.idx].t as f64) <= self.vgm_time
                {
                    let op = self.events[self.idx].op;
                    self.apply(op);
                    self.idx += 1;
                }

                let mut psg_mono = 0.0f32;
            let mut ym_l = 0.0f32;
            let mut ym_r = 0.0f32;

            if let Some(sn) = self.sn.as_mut() {
                self.sn_acc += sn_ratio;
                let k = self.sn_acc as u32;
                self.sn_acc -= k as f64;
                if k > 0 {
                    let mut s = 0.0f32;
                    let mut vs = [0.0f32; 4];
                    for _ in 0..k {
                        s += sn.tick();
                        for v in 0..4 {
                            vs[v] += sn.voice[v];
                        }
                    }
                    self.sn_last = s / k as f32;
                    for v in 0..4 {
                        self.sn_last_voice[v] = vs[v] / k as f32 * PSG_LEVEL;
                    }
                }
                // The PSG output is positive-only; strip the DC offset.
                let x = self.sn_last;
                let y = x - self.dc_x + 0.999 * self.dc_y;
                self.dc_x = x;
                self.dc_y = y;
                psg_mono = y * PSG_LEVEL;
            }

            if let Some(ym) = self.ym.as_mut() {
                self.ym_frac += ym_ratio;
                while self.ym_frac >= 1.0 {
                    self.ym_frac -= 1.0;
                    self.ym_prev = self.ym_next;
                    self.ym_next = ym.next_sample_with_voices(&mut self.solo_last);
                }
                let f = self.ym_frac as f32;
                ym_l = self.ym_prev.0 + (self.ym_next.0 - self.ym_prev.0) * f;
                ym_r = self.ym_prev.1 + (self.ym_next.1 - self.ym_prev.1) * f;
            }

            // Scope taps sit *before* `gain` so `auto_scale` sees the raw
            // chip level (a song/part volume of 0.1 shouldn't flatten it).
            self.push_scope(psg_mono, (ym_l + ym_r) * 0.5);
            self.scratch_psg.push(psg_mono);
            self.scratch_ym.push((ym_l + ym_r) * 0.5);
            if self.sn.is_some() {
                for v in 0..4 {
                    self.scratch_voice[v].push(self.sn_last_voice[v]);
                }
            }
            if self.ym.is_some() {
                for k in 0..6 {
                    self.scratch_fm[k].push(self.solo_last[k]);
                }
            }

            out_l[i] += (psg_mono + ym_l) * gain;
            out_r[i] += (psg_mono + ym_r) * gain;
            self.vgm_time += step;
        }

        self.next_frame = block_start + n as i64;

        // Publish the block to the global scope the TUI reads (same ring the
        // VST parts use), pre-gain so `auto_scale` sees the raw chip level.
        use crate::player::scope::{
            scope_push_block, VGM_SLOT_FM0, VGM_SLOT_NOISE, VGM_SLOT_PSG, VGM_SLOT_PSG0,
            VGM_SLOT_YM, VGM_SLOT_DAC,
        };
        scope_push_block(VGM_SLOT_PSG, &self.scratch_psg, &self.scratch_psg, 1.0);
        scope_push_block(VGM_SLOT_YM, &self.scratch_ym, &self.scratch_ym, 1.0);
        for k in 0..3 {
            let b = &self.scratch_voice[k];
            scope_push_block(VGM_SLOT_PSG0 + k, b, b, 1.0);
        }
        let nb = &self.scratch_voice[3];
        scope_push_block(VGM_SLOT_NOISE, nb, nb, 1.0);
        for k in 0..6 {
            let b = &self.scratch_fm[k];
            scope_push_block(VGM_SLOT_FM0 + k, b, b, 1.0);
        }
        let dac = &self.scratch_fm[5];
        scope_push_block(VGM_SLOT_DAC, dac, dac, 1.0);
    }
}

// ═════════════ YM2612 — the only ymfm-sys-dependent code ═════════════
//
// Contract (the rest of the file relies only on this):
//   new(clock)            create + reset a YM2612 at `clock` Hz
//   reset()               back to power-on state (used by seek / loop wrap)
//   native_rate()         output rate of next_sample(), i.e. clock / 144
//   write(port, reg, val) port 0 = 0x52 command, port 1 = 0x53 command
//   next_sample()         one stereo sample, scaled to roughly -1.0..1.0

use ymfm_sys::ffi::{self, ChipType};
use ymfm_sys::ChipPtr;

pub(crate) struct Ym2612 {
    chip: ChipPtr,
    channels: usize,
    rate: f64,
    buf: Vec<i32>,
    voice_buf: [i32; 12],
}

// SAFETY: the chip is exclusively owned by this wrapper, has no thread
// affinity, and its (default) callbacks hold no shared state. It is only ever
// used from the one thread that owns the VgmPlayer (the audio thread).
unsafe impl Send for Ym2612 {}

impl Ym2612 {
    fn new(clock: u32) -> Self {
        let chip = ffi::create_chip(ChipType::Ym2612, clock);
        let channels = (chip.channels() as usize).max(1);
        let rate = (chip.sample_rate() as f64).max(1.0);
        Ym2612 { chip, channels, rate, buf: vec![0; channels], voice_buf: [0; 12] }
    }

    fn reset(&mut self) {
        self.chip.pin_mut().reset();
    }

    fn native_rate(&self) -> f64 {
        self.rate
    }

    /// ymfm ports: 0 = address (bank 0), 1 = data (bank 0), 2 = address (bank 1), 3 = data (bank 1).
    fn write(&mut self, port: u8, reg: u8, val: u8) {
        let base = if port == 0 { 0 } else { 2 };
        self.chip.pin_mut().write(base, reg);
        self.chip.pin_mut().write(base + 1, val);
    }

    fn next_sample(&mut self) -> (f32, f32) {
        self.chip.pin_mut().generate(&mut self.buf);
        let l = self.buf[0] as f32 * YM_SCALE;
        let r = if self.channels > 1 { self.buf[1] as f32 * YM_SCALE } else { l };
        (l, r)
    }

    /// Generate the normal stereo mix and six isolated channel samples in one
    /// YM2612 step. This avoids maintaining six extra emulators for scopes.
    fn next_sample_with_voices(&mut self, voices: &mut [f32; 6]) -> (f32, f32) {
        if !self.chip.pin_mut().generate_voice_outputs(&mut self.voice_buf) {
            voices.fill(0.0);
            return self.next_sample();
        }

        let mut sum_l = 0i32;
        let mut sum_r = 0i32;
        let voice_scale = YM_SCALE * (128.0 * 64.0 / (6.0 * 65.0));
        for (idx, voice) in voices.iter_mut().enumerate() {
            let left = self.voice_buf[idx * 2];
            let right = self.voice_buf[idx * 2 + 1];
            sum_l += left;
            sum_r += right;
            *voice = (left + right) as f32 * 0.5 * voice_scale;
        }
        let left = (((sum_l * 128) * 64 / (6 * 65)) as f32) * YM_SCALE;
        let right = (((sum_r * 128) * 64 / (6 * 65)) as f32) * YM_SCALE;
        (left, right)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny_vgm() -> Vec<u8> {
        let mut b = vec![0u8; 0x40];
        b[0..4].copy_from_slice(b"Vgm ");
        b[0x08..0x0C].copy_from_slice(&0x150u32.to_le_bytes());
        b[0x0C..0x10].copy_from_slice(&3_579_545u32.to_le_bytes());
        b[0x34..0x38].copy_from_slice(&0x0Cu32.to_le_bytes());
        // Tone 0 freq = 0x0FE, volume 0, then wait 882 samples, end.
        b.extend_from_slice(&[0x50, 0x8E, 0x50, 0x0F, 0x50, 0x90, 0x63, 0x66]);
        b
    }

    #[test]
    fn parses_and_renders_psg() {
        let mut p = VgmPlayer::new(&tiny_vgm(), 48000.0).expect("parse");
        assert_eq!(p.events.len(), 3);
        let mut l = vec![0.0f32; 2048];
        let mut r = vec![0.0f32; 2048];
        p.render(&mut l, &mut r, 0, 1.0);
        assert!(l.iter().any(|s| s.abs() > 1e-4));
    }

    #[test]
    fn seek_matches_register_state() {
        let mut p = VgmPlayer::new(&tiny_vgm(), 48000.0).expect("parse");
        let mut l = vec![0.0f32; 256];
        let mut r = vec![0.0f32; 256];
        p.render(&mut l, &mut r, 1000, 1.0);
        assert_eq!(p.next_frame, 1256);
    }

    #[test]
    fn scope_snapshot_tracks_render() {
        let mut p = VgmPlayer::new(&tiny_vgm(), 48000.0).expect("parse");
        assert!(p.scope_snapshot(VgmChip::All).is_empty());

        let mut l = vec![0.0f32; 512];
        let mut r = vec![0.0f32; 512];
        p.render(&mut l, &mut r, 0, 1.0);

        let all = p.scope_snapshot(VgmChip::All);
        let psg = p.scope_snapshot(VgmChip::Psg);
        let ym = p.scope_snapshot(VgmChip::Ym);
        assert_eq!(all.len(), 512);
        assert_eq!(psg.len(), 512);
        assert_eq!(ym.len(), 512);
        assert!(psg.iter().any(|s| s.abs() > 1e-6), "PSG scope should have signal");
        // tiny_vgm has no YM2612 (ym_clock == 0), so the YM tap stays silent.
        assert!(ym.iter().all(|s| s.abs() < 1e-9));
        // Full mix equals PSG when YM is absent.
        for (a, b) in all.iter().zip(psg.iter()) {
            assert!((a - b).abs() < 1e-9);
        }
    }

    #[test]
    fn scope_clears_on_seek() {
        let mut p = VgmPlayer::new(&tiny_vgm(), 48000.0).expect("parse");
        let mut l = vec![0.0f32; 256];
        let mut r = vec![0.0f32; 256];
        p.render(&mut l, &mut r, 0, 1.0);
        assert_eq!(p.scope_snapshot(VgmChip::All).len(), 256);

        // Moving to a different frame forces a seek -> reset -> empty scope.
        p.render(&mut l, &mut r, 500, 1.0);
        // `render` re-fills from scratch, so after this call we have 256
        // fresh samples, not 512.
        assert_eq!(p.scope_snapshot(VgmChip::All).len(), 256);
    }

    #[test]
    fn chip_routing() {
        assert_eq!(VgmChip::from_source_path("vgm_embedded/FM1"), VgmChip::Ym);
        assert_eq!(VgmChip::from_source_path("vgm_embedded/FM6"), VgmChip::Ym);
        assert_eq!(VgmChip::from_source_path("vgm_embedded/DAC"), VgmChip::Ym);
        assert_eq!(VgmChip::from_source_path("vgm_embedded/PSG3"), VgmChip::Psg);
        assert_eq!(VgmChip::from_source_path("vgm_embedded/NOISE"), VgmChip::Psg);
        assert_eq!(VgmChip::from_source_path("vgm_embedded"), VgmChip::All);
        // Case-insensitive and tolerant of `./`-prefixes.
        assert_eq!(VgmChip::from_source_path("./VGM_EMBEDDED/fm2"), VgmChip::Ym);
    }
}
