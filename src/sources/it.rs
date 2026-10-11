// src/sources/it.rs
//! Impulse Tracker (.it) decoder — Rust port of it_parser.py.
//!
//! Owns the binary IT parsing/decompression logic.  Consumers should use
//! `parse_file` / `parse` and `unpack_pattern` rather than re-implementing
//! the format.  Behaviour is kept identical to the Python original,
//! including the odd corners of the IT compression bit reader.

use anyhow::{anyhow, bail, Result};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

// ── constants ──────────────────────────────────────────────────────────

// Raw special note values in packed patterns.
pub const ITNOTE_MPT_FADE: u8 = 0xFD;
pub const ITNOTE_FADE: u8 = 0xFD;
pub const ITNOTE_CUT: u8 = 0xFE;
pub const ITNOTE_KEY_OFF: u8 = 0xFF;

// Non-pattern order-list entries.
pub const ITORDER_SKIP: u8 = 0xFE;
pub const ITORDER_END: u8 = 0xFF;

// Sample header flag bits (flg).
pub const ITSF_DATA_PRESENT: u8 = 0x01;
pub const ITSF_BIT_16: u8 = 0x02;
pub const ITSF_STEREO: u8 = 0x04;
pub const ITSF_COMPRESSED: u8 = 0x08;
pub const ITSF_LOOP: u8 = 0x10;
pub const ITSF_SUSTAIN_LOOP: u8 = 0x20;
pub const ITSF_PINGPONG_LOOP: u8 = 0x40;
pub const ITSF_PINGPONG_SUSTAIN: u8 = 0x80;

// Sample conversion flag bits (cvt).
pub const ITCVT_SIGNED: u8 = 0x01;
pub const ITCVT_BIG_ENDIAN: u8 = 0x02;
pub const ITCVT_DELTA: u8 = 0x04;
pub const ITCVT_PTM_8_TO_16: u8 = 0x08;

/// IT ADPCM marker in the cvt byte.  Exact value, not a bit flag.
pub const ITCVT_ADPCM: u8 = 0xFF;

// ── note termination ───────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteTermination {
    Fade,
    Cut,
    KeyOff,
}

/// Classify a raw termination note.
///
/// OpenMPT treats 0xFD as fade only in MPTM; in standard IT it is not a
/// note.  Matches the Python `note_termination`.
pub fn note_termination(note: u8, mptm: bool) -> Option<NoteTermination> {
    match note {
        ITNOTE_MPT_FADE => {
            if mptm {
                Some(NoteTermination::Fade)
            } else {
                None
            }
        }
        ITNOTE_CUT => Some(NoteTermination::Cut),
        ITNOTE_KEY_OFF => Some(NoteTermination::KeyOff),
        _ => None,
    }
}

// ── data model ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default, Serialize)]
pub struct ITEnvelope {
    pub enabled: bool,
    pub loop_enabled: bool,
    pub sustain_enabled: bool,
    pub loop_start: u8,
    pub loop_end: u8,
    pub sustain_start: u8,
    pub sustain_end: u8,
    /// (tick, value) pairs.  Value is signed (-128..127).
    pub points: Vec<(u16, i8)>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Instrument {
    pub name: String,
    pub nna: u8,
    pub dct: u8,
    pub dca: u8,
    pub fadeout: u16,
    pub gbv: u8,
    pub dfp: u8,
    /// (note, sample) pairs.  Note 0..119, sample 0..255 (0 = none).
    pub keymap: Vec<(u8, u8)>,
    /// Envelope enabled flags, keyed by "volume" / "pan" / "pitch".
    pub env: BTreeMap<String, bool>,
    pub envelopes: BTreeMap<String, ITEnvelope>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Sample {
    pub name: String,
    pub gvl: u8,
    pub flg: u8,
    pub vol: u8,
    pub cvt: u8,
    pub dfp: u8,
    pub length: u32,
    pub loop_beg: u32,
    pub loop_end: u32,
    pub c5: u32,
    pub sus_beg: u32,
    pub sus_end: u32,
    pub ptr: u32,
    pub has_data: bool,
    pub is16: bool,
    pub stereo: bool,
    pub compressed: bool,
    pub r#loop: bool,
    pub pingpong: bool,
    pub sus_loop: bool,
    pub pingpong_sus: bool,
    /// Decoded PCM, signed.  `i32` because 16-bit samples need the range.
    #[serde(skip)]
    pub pcm: Vec<i32>,
    /// Filled in by the converter; None during parse.
    pub file: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PatternEvent {
    pub note: Option<u8>,
    pub instrument: Option<u8>,
    pub volume: Option<u8>,
    pub command: Option<u8>,
    pub parameter: u8,
}

impl PatternEvent {
    /// True when the cell encodes nothing (used by the converter's row log).
    pub fn is_empty(&self) -> bool {
        self.note.is_none()
        && self.instrument.is_none()
        && self.volume.is_none()
        && self.command.is_none()
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Module {
    pub title: String,
    pub cwtv: u16,
    pub cmwt: u16,
    pub highlight_minor: u8,
    pub highlight_major: u8,
    pub gv: u8,
    pub mv: u8,
    pub speed: u8,
    pub tempo: u8,
    pub sep: u8,
    pub pwd: u8,
    pub flags: u16,
    pub special: u16,
    pub stereo: bool,
    pub use_instruments: bool,
    pub linear: bool,
    pub old_effects: bool,
    pub chnpan: Vec<u8>,
    pub chnvol: Vec<u8>,
    pub orders: Vec<u8>,
    pub message: String,
    pub instruments: Vec<Instrument>,
    pub samples: Vec<Sample>,
    pub patterns: Vec<Vec<Vec<Option<PatternEvent>>>>,
}

// ── byte helpers ───────────────────────────────────────────────────────

#[inline]
fn u16_at(data: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([data[off], data[off + 1]])
}

#[inline]
fn u32_at(data: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
}

/// Latin1 (ISO-8859-1) byte slice to String: each byte is its own code point.
fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

/// Split on NUL, take the first piece, latin1-decode, trim.
/// Equivalent of Python's `cstr`.
fn cstr(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    latin1(&b[..end]).trim().to_string()
}

// ── envelope decoding ──────────────────────────────────────────────────

pub fn parse_envelope(data: &[u8], offset: usize) -> ITEnvelope {
    let flags = data[offset];
    let count = data[offset + 1];
    let loop_start = data[offset + 2];
    let loop_end = data[offset + 3];
    let sustain_start = data[offset + 4];
    let sustain_end = data[offset + 5];

    let n = (count as usize).min(25);
    let mut points = Vec::with_capacity(n);
    for i in 0..n {
        let po = offset + 6 + i * 3;
        let mut value = data[po] as i32;
        if value >= 128 {
            value -= 256;
        }
        let tick = u16_at(data, po + 1);
        points.push((tick, value as i8));
    }

    ITEnvelope {
        enabled: flags & 1 != 0,
        loop_enabled: flags & 2 != 0,
        sustain_enabled: flags & 4 != 0,
        loop_start,
        loop_end,
        sustain_start,
        sustain_end,
        points,
    }
}

// ── top-level parse ────────────────────────────────────────────────────

pub fn parse(data: &[u8]) -> Result<Module> {
    if data.len() < 4 || &data[0..4] != b"IMPM" {
        bail!("not an IT module");
    }
    if data.len() < 0xC0 {
        bail!("IT header truncated");
    }

    let mut m = Module::default();
    m.title = cstr(&data[4..30]);
    m.highlight_minor = if data[0x1E] != 0 { data[0x1E] } else { 4 };
    m.highlight_major = if data[0x1F] != 0 {
        data[0x1F]
    } else {
        m.highlight_minor.saturating_mul(4)
    };

    let ordnum = u16_at(data, 0x20) as usize;
    let insnum = u16_at(data, 0x22) as usize;
    let smpnum = u16_at(data, 0x24) as usize;
    let patnum = u16_at(data, 0x26) as usize;
    m.cwtv = u16_at(data, 0x28);
    m.cmwt = u16_at(data, 0x2A);
    m.flags = u16_at(data, 0x2C);
    m.special = u16_at(data, 0x2E);

    m.gv = data[0x30];
    m.mv = data[0x31];
    m.speed = data[0x32];
    m.tempo = data[0x33];
    m.sep = data[0x34];
    m.pwd = data[0x35];
    let msglen = u16_at(data, 0x36) as usize;
    let msgoff = u32_at(data, 0x38) as usize;

    m.stereo = m.flags & 1 != 0;
    m.use_instruments = m.flags & 4 != 0;
    m.linear = m.flags & 8 != 0;
    m.old_effects = m.flags & 16 != 0;

    m.chnpan = data[0x40..0x80].to_vec();
    m.chnvol = data[0x80..0xC0].to_vec();
    m.orders = data[0xC0..0xC0 + ordnum].to_vec();

    let mut p = 0xC0 + ordnum;
    let mut ins_ptr = Vec::with_capacity(insnum);
    for i in 0..insnum {
        ins_ptr.push(u32_at(data, p + i * 4) as usize);
    }
    p += 4 * insnum;
    let mut smp_ptr = Vec::with_capacity(smpnum);
    for i in 0..smpnum {
        smp_ptr.push(u32_at(data, p + i * 4) as usize);
    }
    p += 4 * smpnum;
    let mut pat_ptr = Vec::with_capacity(patnum);
    for i in 0..patnum {
        pat_ptr.push(u32_at(data, p + i * 4) as usize);
    }

    // Message: only present when special bit 0 is set and the offsets fit.
    if m.special & 1 != 0 && msglen > 0 && msgoff + msglen <= data.len() {
        let raw = &data[msgoff..msgoff + msglen];
        let nul = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        m.message = latin1(&raw[..nul]).replace('\r', "\n");
    }

    // ── instruments ──
    for &o in &ins_ptr {
        if o + 0x3A > data.len() {
            bail!("instrument offset out of range");
        }
        let mut i = Instrument::default();
        i.name = cstr(&data[o + 0x20..o + 0x3A]);
        i.nna = data[o + 0x11];
        i.dct = data[o + 0x12];
        i.dca = data[o + 0x13];
        i.fadeout = u16_at(data, o + 0x14);
        i.gbv = data[o + 0x18];
        i.dfp = data[o + 0x19];

        let mut keymap = Vec::with_capacity(120);
        for n in 0..120 {
            let note = data[o + 0x40 + 2 * n];
            let sample = data[o + 0x41 + 2 * n];
            keymap.push((note, sample));
        }
        i.keymap = keymap;

        for (name, off) in [
            ("volume", 0x130usize),
            ("pan", 0x182usize),
            ("pitch", 0x1D4usize),
        ] {
            let env = parse_envelope(data, o + off);
            i.env.insert(name.to_string(), env.enabled);
            i.envelopes.insert(name.to_string(), env);
        }
        m.instruments.push(i);
    }

    // ── samples ──
    for &o in &smp_ptr {
        if o + 0x50 > data.len() {
            bail!("sample offset out of range");
        }
        let mut s = Sample::default();
        s.name = cstr(&data[o + 0x14..o + 0x2E]);
        s.gvl = data[o + 0x11];
        s.flg = data[o + 0x12];
        s.vol = data[o + 0x13];
        s.cvt = data[o + 0x2E];
        s.dfp = data[o + 0x2F];
        s.length = u32_at(data, o + 0x30);
        s.loop_beg = u32_at(data, o + 0x34);
        s.loop_end = u32_at(data, o + 0x38);
        s.c5 = u32_at(data, o + 0x3C);
        s.sus_beg = u32_at(data, o + 0x40);
        s.sus_end = u32_at(data, o + 0x44);
        s.ptr = u32_at(data, o + 0x48);

        s.has_data = (s.flg & ITSF_DATA_PRESENT) != 0 && s.length > 0;
        s.is16 = (s.flg & ITSF_BIT_16) != 0;
        s.stereo = (s.flg & ITSF_STEREO) != 0;
        s.compressed = (s.flg & ITSF_COMPRESSED) != 0;
        s.r#loop = (s.flg & ITSF_LOOP) != 0;
        s.pingpong = (s.flg & ITSF_PINGPONG_LOOP) != 0;
        s.sus_loop = (s.flg & ITSF_SUSTAIN_LOOP) != 0;
        s.pingpong_sus = (s.flg & ITSF_PINGPONG_SUSTAIN) != 0;
        m.samples.push(s);
    }
    // Decode PCM in a second pass so the borrow of the freshly pushed sample
    // doesn't fight the &data slice.
    for i in 0..m.samples.len() {
        if m.samples[i].has_data {
            let s = m.samples[i].clone();
            m.samples[i].pcm = decode_sample(data, &s);
        }
    }

    // ── patterns ──
    for &off in &pat_ptr {
        m.patterns.push(unpack_pattern(data, off));
    }

    Ok(m)
}

pub fn parse_file(path: &Path) -> Result<Module> {
    let data = std::fs::read(path)
    .map_err(|e| anyhow!("reading {}: {}", path.display(), e))?;
    parse(&data)
}

// ── pattern unpacking ──────────────────────────────────────────────────

pub fn unpack_pattern(data: &[u8], off: usize) -> Vec<Vec<Option<PatternEvent>>> {
    if off == 0 {
        return (0..64)
        .map(|_| (0..64).map(|_| None).collect())
        .collect();
    }
    if off + 8 > data.len() {
        return (0..64)
        .map(|_| (0..64).map(|_| None).collect())
        .collect();
    }

    let length = u16_at(data, off) as usize;
    let nrows = u16_at(data, off + 2) as usize;
    let mut p = off + 8;
    let end = (off + 8 + length).min(data.len());

    let mut grid: Vec<Vec<Option<PatternEvent>>> = (0..nrows)
    .map(|_| (0..64).map(|_| None).collect())
    .collect();
    let mut lmask = [0u8; 64];
    let mut lnote = [0u8; 64];
    let mut linst = [0u8; 64];
    let mut lvol = [0u8; 64];
    let mut lcmd: [(u8, u8); 64] = [(0, 0); 64];
    let mut r = 0usize;

    while r < nrows && p < end {
        let cv = data[p];
        p += 1;
        if cv == 0 {
            r += 1;
            continue;
        }
        let mut ch = (cv & 0x7F) as usize;
        if ch != 0 {
            ch -= 1;
        }
        if ch >= 64 {
            break;
        }

        if cv & 128 != 0 {
            if p >= end {
                break;
            }
            lmask[ch] = data[p];
            p += 1;
        }
        let mk = lmask[ch];

        let mut note: Option<u8> = None;
        let mut inst: Option<u8> = None;
        let mut vol: Option<u8> = None;
        let mut cmd: Option<u8> = None;
        let mut par: u8 = 0;

        // The Python version catches IndexError mid-record and breaks the
        // outer loop.  Match that: check bounds before each byte read.
        macro_rules! byte_or_break {
            () => {{
                if p >= data.len() {
                    break;
                }
                let b = data[p];
                p += 1;
                b
            }};
        }

        if mk & 1 != 0 {
            let b = byte_or_break!();
            note = Some(b);
            lnote[ch] = b;
        } else if mk & 16 != 0 {
            note = Some(lnote[ch]);
        }
        if mk & 2 != 0 {
            let b = byte_or_break!();
            inst = Some(b);
            linst[ch] = b;
        } else if mk & 32 != 0 {
            inst = Some(linst[ch]);
        }
        if mk & 4 != 0 {
            let b = byte_or_break!();
            vol = Some(b);
            lvol[ch] = b;
        } else if mk & 64 != 0 {
            vol = Some(lvol[ch]);
        }
        if mk & 8 != 0 {
            if p + 1 >= data.len() {
                break;
            }
            cmd = Some(data[p]);
            par = data[p + 1];
            lcmd[ch] = (data[p], data[p + 1]);
            p += 2;
        } else if mk & 128 != 0 {
            cmd = Some(lcmd[ch].0);
            par = lcmd[ch].1;
        }

        if note.is_some() || inst.is_some() || vol.is_some() || cmd.is_some() {
            grid[r][ch] = Some(PatternEvent {
                note,
                // Python: inst or None -- 0 becomes None.
                instrument: inst.filter(|&v| v != 0),
                               volume: vol,
                               // Python: cmd or None.
                               command: cmd.filter(|&v| v != 0),
                               parameter: par,
            });
        }
    }

    grid
}

// ── IT compression (the width-escape bit reader) ───────────────────────

struct BitReader<'a> {
    blk: &'a [u8],
    bi: usize,
    acc: u32,
    nb: u32,
}

impl<'a> BitReader<'a> {
    fn new(blk: &'a [u8]) -> Self {
        BitReader { blk, bi: 0, acc: 0, nb: 0 }
    }

    /// Read `n` bits LSB-first.  Past the end of the block, reads zeroes,
    /// matching the Python reader's auto-padding.
    fn read(&mut self, n: u32) -> u32 {
        while self.nb < n {
            let b = if self.bi < self.blk.len() {
                self.blk[self.bi] as u32
            } else {
                0
            };
            self.acc |= b << self.nb;
            self.bi += 1;
            self.nb += 8;
        }
        let mask = if n == 0 { 0 } else { (1u32 << n) - 1 };
        let v = self.acc & mask;
        self.acc >>= n;
        self.nb -= n;
        v
    }
}

/// Python `_change_it_width`.  Kept as a separate function because it is
/// the whole point of the escape coding.
#[inline]
fn change_it_width(current_width: u32, encoded_width: u32) -> u32 {
    let mut new_width = encoded_width + 1;
    if new_width >= current_width {
        new_width += 1;
    }
    new_width
}

/// One channel of packed IT sample data.
///
/// Ported 1:1 from the Python, including the `width < 1 || width >
/// initial_width` early break (which mirrors OpenMPT's behaviour when a
/// malformed block pushes width out of the legal range).
fn it_decompress_channel(
    src: &[u8],
    mut pos: usize,
    length: usize,
    is16: bool,
    it215: bool,
) -> (Vec<i32>, usize) {
    let bitsize: u32 = if is16 { 16 } else { 8 };
    let initial_width: u32 = bitsize + 1;
    let maxblock: usize = if is16 { 0x4000 } else { 0x8000 };
    let half: i64 = 1i64 << (bitsize - 1);
    let mask: i64 = (1i64 << bitsize) - 1;
    let esc_bits: u32 = if is16 { 4 } else { 3 };
    let lower_b: i64 = if is16 { -8 } else { -4 };
    let upper_b: i64 = if is16 { 7 } else { 3 };

    let mut out: Vec<i32> = Vec::new();

    while out.len() < length && pos + 2 <= src.len() {
        let blen = src[pos] as usize | ((src[pos + 1] as usize) << 8);
        pos += 2;
        if blen == 0 || pos + blen > src.len() {
            break;
        }
        let mut br = BitReader::new(&src[pos..pos + blen]);
        pos += blen;

        let mut n = maxblock.min(length - out.len());
        let mut width = initial_width;
        let mut d1: i64 = 0;
        let mut d2: i64 = 0;

        while n > 0 {
            if width < 1 || width > initial_width {
                break;
            }

            let value = br.read(width) as i64;
            let top_bit: i64 = 1i64 << (width - 1);

            if width <= 6 {
                if value == top_bit {
                    width = change_it_width(width, br.read(esc_bits));
                    continue;
                }
            } else if width < initial_width {
                let lower_border = top_bit + lower_b;
                let upper_border = top_bit + upper_b;
                if lower_border <= value && value <= upper_border {
                    width = change_it_width(width, (value - lower_border) as u32);
                    continue;
                }
            } else if value & top_bit != 0 {
                width = ((value ^ top_bit) + 1) as u32;
                continue;
            }

            let dw: i64 = if width < bitsize {
                let shift = bitsize - width;
                let mut w = (value << shift) & mask;
                if w >= half {
                    w -= 1i64 << bitsize;
                }
                // Arithmetic shift-right of a possibly-negative i64.
                w >> shift
            } else {
                let mut w = value & mask;
                if w >= half {
                    w -= 1i64 << bitsize;
                }
                w
            };

            d1 += dw;
            d2 += d1;
            let v = if it215 { d2 } else { d1 };
            let sample = ((v + half) & mask) - half;
            out.push(sample as i32);

            n -= 1;
        }
    }

    (out, pos)
}

/// Port of `it_decompress`.  `channels` is 1 (mono) or 2 (stereo); for
/// stereo, planes are decompressed sequentially and interleaved.
pub fn it_decompress(
    src: &[u8],
    length: usize,
    is16: bool,
    it215: bool,
    channels: usize,
) -> Vec<i32> {
    if channels < 1 {
        return Vec::new();
    }
    let mut planes: Vec<Vec<i32>> = Vec::with_capacity(channels);
    let mut pos = 0usize;
    for _ in 0..channels {
        let (plane, new_pos) = it_decompress_channel(src, pos, length, is16, it215);
        planes.push(plane);
        pos = new_pos;
    }
    if channels == 1 {
        return planes.into_iter().next().unwrap_or_default();
    }
    // Interleave: L0 R0 L1 R1 ...
    let shortest = planes.iter().map(|p| p.len()).min().unwrap_or(0);
    let mut out = Vec::with_capacity(shortest * channels);
    for i in 0..shortest {
        for plane in &planes {
            out.push(plane[i]);
        }
    }
    out
}

// ── sample decoding ────────────────────────────────────────────────────

/// Decode a single sample's PCM.  Returns signed i32; caller scales.
pub fn decode_sample(data: &[u8], s: &Sample) -> Vec<i32> {
    if !s.has_data || s.ptr == 0 || s.ptr as usize >= data.len() {
        return Vec::new();
    }
    let ptr = s.ptr as usize;
    let cvt = s.cvt;
    let channels = if s.stereo { 2 } else { 1 };

    if s.compressed {
        let it215 = (cvt & ITCVT_DELTA) != 0;
        return it_decompress(
            &data[ptr..],
            s.length as usize,
            s.is16,
            it215,
            channels,
        );
    }

    if !s.is16 && cvt == ITCVT_ADPCM {
        if ptr + 16 > data.len() {
            return Vec::new();
        }
        let mut lut = [0i32; 16];
        for i in 0..16 {
            lut[i] = data[ptr + i] as i8 as i32;
        }
        let start = ptr + 16;
        let n = (((s.length as usize) + 1) / 2).min(data.len().saturating_sub(start));
        let mut out = Vec::with_capacity(n * 2);
        let mut delta: i32 = 0;
        for i in 0..n {
            let b = data[start + i];
            delta = (delta + lut[(b & 0x0F) as usize]) & 0xFF;
            if delta >= 128 {
                delta -= 256;
            }
            out.push(delta);
            delta = (delta + lut[((b >> 4) & 0x0F) as usize]) & 0xFF;
            if delta >= 128 {
                delta -= 256;
            }
            out.push(delta);
        }
        out.truncate(s.length as usize);
        return out;
    }

    let signed = (cvt & ITCVT_SIGNED) != 0;
    let big_endian = (cvt & ITCVT_BIG_ENDIAN) != 0;
    let delta = (cvt & ITCVT_DELTA) != 0;

    if s.is16 {
        let mut needed = s.length as usize * channels * 2;
        if ptr + needed > data.len() {
            needed = data.len().saturating_sub(ptr);
            if needed < 2 {
                return Vec::new();
            }
        }
        let n = needed / 2;
        let mut vals: Vec<i32> = Vec::with_capacity(n);
        for i in 0..n {
            let off = ptr + i * 2;
            let word = if big_endian {
                u16::from_be_bytes([data[off], data[off + 1]])
            } else {
                u16::from_le_bytes([data[off], data[off + 1]])
            };
            let v: i32 = if signed {
                word as i16 as i32
            } else {
                word as i32 - 32768
            };
            vals.push(v);
        }
        if delta {
            let mut acc: i32 = 0;
            for v in vals.iter_mut() {
                acc = (acc + *v) & 0xFFFF;
                if acc >= 32768 {
                    acc -= 65536;
                }
                *v = acc;
            }
        }
        return vals;
    }

    // 8-bit
    let mut needed = s.length as usize * channels;
    if ptr + needed > data.len() {
        needed = data.len().saturating_sub(ptr);
    }
    let raw = &data[ptr..ptr + needed];
    let mut vals: Vec<i32> = Vec::with_capacity(raw.len());
    if signed {
        for &b in raw {
            vals.push(b as i8 as i32);
        }
    } else {
        for &b in raw {
            vals.push(b as i32 - 128);
        }
    }
    if delta {
        let mut acc: i32 = 0;
        for v in vals.iter_mut() {
            acc = (acc + *v) & 0xFF;
            if acc >= 128 {
                acc -= 256;
            }
            *v = acc;
        }
    }
    vals
}
