// src/convert/it.rs
//! IT → Trackscript converter.
//!
//! Direct port of it2trackscript.py.  Runs the IT playback state machine
//! row by row, produces a list of voices with pitch/volume/pan automation,
//! then groups them into measure-sized parts and emits trackscript.
//!
//! Stages:
//!   1. Helpers, Voice, ChannelState, Converter core
//!   2. run / process_row / process_channel
//!   3. finish_voices / envelopes / fadeout / merge_steps
//!   4. Rendering + structural extraction
//!   5. emit / report / WAV / samples.json / semantics.json / convert() entry
//!
//! ── Assumptions about `crate::sources::it` (adjust in ONE place) ─────────
//!   * `Module` fields: title, message: String; gv, mv, sep, pwd, flags,
//!     special; speed, tempo: u8; linear, old_effects, use_instruments: bool;
//!     orders: Vec<u8|u16>; patterns: Vec<Vec<Vec<Option<ITNote>>>>;
//!     chnpan, chnvol: Vec<u8>; instruments: Vec<Instrument>;
//!     samples: Vec<Sample>.
//!   * `ITNote { note: Option<u8>, inst: Option<u8>, volc: Option<u8>,
//!     cmd: u8, par: u8 }`  (only `read_cell` below touches it).
//!   * `Sample`: name, vol, gvl, dfp, flg, cvt, c5, length, loop_beg,
//!     loop_end, sus_beg, sus_end, pcm: Vec<i16>, has_data, looped,
//!     pingpong, sus_loop, pingpong_sus, is16, stereo, compressed.
//!   * `Instrument`: name, nna, dct, dca, fadeout, gbv, dfp,
//!     keymap: Vec<(u8, u8)>, envelopes: HashMap<String, ITEnvelope>.
//!   * `ITEnvelope`: enabled, loop_enabled, sustain_enabled, loop_start,
//!     loop_end, sustain_start, sustain_end, points: Vec<(tick, value)>.
//!   * `PitchMode` is `Copy` with a `Names` variant.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use super::{AuxFile, ConvertOptions, Converted, PitchMode, SampleOut};
use crate::sources::it::{ITEnvelope, Instrument, Module, NoteTermination, PatternEvent};

const ROWS_PER_BEAT: u32 = 4;
const RPB: f64 = ROWS_PER_BEAT as f64;
const MEASURE_ROWS: f64 = RPB * 4.0;
const C5_HZ: f64 = 523.2511306;
const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];
const G_TABLE: [i32; 10] = [0, 1, 4, 8, 16, 32, 64, 96, 128, 255];
const VIB_PEAK_PER_DEPTH: f64 = 1.0 / 16.0;

const ORDER_SKIP: usize = 254;
const ORDER_END: usize = 255;

// ── helpers ────────────────────────────────────────────────────────────

/// Round half to even (Python's `round`); `f64::round_ties_even` needs 1.77.
fn round_even(x: f64) -> f64 {
    if (x - x.trunc()).abs() == 0.5 {
        2.0 * (x / 2.0).round()
    } else {
        x.round()
    }
}

/// Python-style `round(x, nd)`: round the exact binary value half-to-even.
/// (`x * 10^nd` then rounding is *not* the same: 1.615 -> 1.61 in Python.)
/// Rust's `{:.N}` formatting is also exact, so go through it.
fn pyround(x: f64, nd: i32) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{:.*}", nd.max(0) as usize, x).parse().unwrap_or(x)
}

/// Integer key for `round(x, 3)`, used to group equal positions/durations.
fn key3(x: f64) -> i64 {
    (pyround(x, 3) * 1000.0).round() as i64
}

/// Fixed decimals with trailing zeros trimmed ("1.50" -> "1.5", "-0" -> "0").
///
/// The Python version also stripped zeros from integers when nd == 0
/// ("50" -> "5"); this one only trims after a decimal point.
fn fmt(x: f64, nd: usize) -> String {
    let mut s = format!("{:.*}", nd, x);
    if s.contains('.') {
        s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    }
    if s.is_empty() || s == "-0" {
        "0".to_string()
    } else {
        s
    }
}

fn hz(semi: f64) -> f64 {
    C5_HZ * 2f64.powf((semi - 60.0) / 12.0)
}

/// Apply signed IT period movement; positive amount raises pitch.
fn slide_pitch_period(pitch: f64, amount: f64, c5_speed: f64, linear: bool) -> f64 {
    if linear {
        return (pitch + amount / 64.0).max(0.0);
    }
    let c5 = if c5_speed == 0.0 { 8363.0 } else { c5_speed };
    let c5 = c5.max(1.0);
    let period = (8363.0 * 1712.0) / (c5 * 2f64.powf((pitch - 60.0) / 12.0));
    let next_period = (period - amount).max(1.0);
    (pitch + 12.0 * (period / next_period).log2()).max(0.0)
}

/// Decode D/K/L slide parameter.  Returns (kind, amount).
fn slide_decode(p: u8) -> (&'static str, u8) {
    let x = p >> 4;
    let y = p & 15;
    if y == 15 && x != 0 {
        return ("fup", x);
    }
    if x == 15 && y != 0 {
        return ("fdown", y);
    }
    if y == 0 && x != 0 {
        return ("up", x);
    }
    if x == 0 && y != 0 {
        return ("down", y);
    }
    ("none", 0)
}

fn note_name(n: i64) -> String {
    format!("{}{}", NOTE_NAMES[n.rem_euclid(12) as usize], n.div_euclid(12))
}

fn safe_name(s: &str) -> String {
    let mut out = String::new();
    let mut in_bad = false;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-') {
            out.push(ch);
            in_bad = false;
        } else if !in_bad {
            out.push('_');
            in_bad = true;
        }
    }
    let t = out.trim_matches('_');
    if t.is_empty() {
        "sample".to_string()
    } else {
        t.to_string()
    }
}

/// Make a Trackscript identifier (readable names, not generated IDs).
fn identifier(s: &str) -> String {
    let s = safe_name(s);
    if s.chars().next().map_or(false, |c| c.is_ascii_digit()) {
        format!("_{}", s)
    } else {
        s
    }
}

/// Minimal Python `repr(str)` for the report.
fn py_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(quote);
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

// ── pattern cells ──────────────────────────────────────────────────────

#[derive(Clone, Copy)]
struct Cell {
    note: Option<u8>,
    inst: Option<u8>,
    volc: Option<u8>,
    cmd: u8,
    par: u8,
}

/// The only place that knows the shape of the parser's pattern cell.
fn read_cell(c: &Option<PatternEvent>) -> Option<Cell> {
    let p = c.as_ref()?;
    Some(Cell {
        note: p.note,
        inst: p.instrument,
        volc: p.volume,
        cmd: p.command.unwrap_or(0),
        par: p.parameter,
    })
}

fn term_kind(note: Option<u8>) -> Option<&'static str> {
    match note.and_then(|n| crate::sources::it::note_termination(n, false)) {
        Some(NoteTermination::Cut) => Some("cut"),
        Some(NoteTermination::KeyOff) => Some("off"),
        _ => None,
    }
}

fn note_name_from_it(note: Option<u8>) -> Option<String> {
    match note? {
        253 => Some("FADE".into()),
        254 => Some("CUT".into()),
        255 => Some("OFF".into()),
        n if n < 120 => Some(note_name(n as i64)),
        _ => None,
    }
}

// ── Voice ──────────────────────────────────────────────────────────────

struct Voice {
    ch: u8,
    /// Absolute row where the voice started.  f64 because of note delay.
    start: f64,
    /// Absolute row where the voice ends, or None if it hasn't ended yet.
    end: Option<f64>,

    sample: u32, // 1-based
    note: u8,
    #[allow(dead_code)]
    bpm: f64,
    /// 1-based index into `Module::instruments`; 0 = the fake instrument
    /// used when the module has `use_instruments == false`.
    instrument: usize,
    speed: u8,
    envelope: Option<ITEnvelope>,
    key_off_row: Option<f64>,
    /// (start_row, rows) of an instrument fadeout.
    fade: Option<(f64, f64)>,
    offset: u32,

    /// Initial pitch/vol/pan at note-on.
    init: HashMap<&'static str, f64>,
    /// Last value written per property, used to suppress duplicate points.
    last: HashMap<&'static str, f64>,

    /// (row, property, target_value, duration_rows)
    points: Vec<(f64, &'static str, f64, f64)>,

    /// Rows the sample would naturally end at, if the sample doesn't loop.
    natural_rows: Option<f64>,
}

impl Voice {
    #[allow(clippy::too_many_arguments)]
    fn new(
        ch: u8,
        start: f64,
        sample: u32,
        note: u8,
        bpm: f64,
        instrument: usize,
        speed: u8,
        envelope: Option<ITEnvelope>,
    ) -> Self {
        Voice {
            ch,
            start,
            end: None,
            sample,
            note,
            bpm,
            instrument,
            speed,
            envelope,
            key_off_row: None,
            fade: None,
            offset: 0,
            init: HashMap::new(),
            last: HashMap::new(),
            points: Vec::new(),
            natural_rows: None,
        }
    }
}

// ── ChannelState ───────────────────────────────────────────────────────

struct ChannelState {
    inst: u8,

    vol: u8,     // 0..64
    chanvol: u8, // 0..64

    /// 0..64, IT convention (32 = center).
    pan: f64,

    pitch: f64, // semitones (MIDI-number-as-f64)
    target: Option<f64>,

    /// Index into Converter::voices, or None.
    voice: Option<usize>,

    gscale: f64,

    mem_d: u8,
    mem_ef: u8,
    mem_g: u8,
    mem_o: u8,

    vib_spd: u8,
    /// f64: `Uxx` fine vibrato stores depth / 4.
    vib_dep: f64,
    vib_pos: f64,
    vib_on: bool,
}

impl ChannelState {
    fn new(pan: u8, vol: u8) -> Self {
        let p = (pan & 127) as i32;
        ChannelState {
            inst: 0,
            vol: 64,
            chanvol: vol.min(64),
            pan: if p <= 64 { p as f64 } else { 32.0 },
            pitch: 60.0,
            target: None,
            voice: None,
            gscale: 1.0,
            mem_d: 0,
            mem_ef: 0,
            mem_g: 0,
            mem_o: 0,
            vib_spd: 0,
            vib_dep: 0.0,
            vib_pos: 0.0,
            vib_on: false,
        }
    }
}

// ── Converter ──────────────────────────────────────────────────────────

/// A fake Instrument used when the module has `use_instruments == false`.
/// gbv=128 and dfp=0 match the Python's dummy.
fn fake_instrument() -> Instrument {
    let mut i = Instrument::default();
    i.gbv = 128;
    i.dfp = 0;
    i
}

struct Converter<'a> {
    m: &'a Module,
    pitch_mode: PitchMode,
    fake_ins: Instrument,

    /// Warning counts in first-seen order (Python's Counter + most_common).
    warn: Vec<(String, u32)>,
    #[allow(dead_code)]
    warn_detail: HashMap<String, String>,

    voices: Vec<Voice>,

    /// (absolute_row, order_idx, pattern_idx, pattern_row, bpm).
    rowlog: Vec<(f64, usize, usize, usize, f64)>,
    /// (row, per-tick BPM list) for tempo slides.
    tempo_ramps: Vec<(f64, Vec<f64>)>,
    executed_rows: Vec<ExecutedRow>,

    #[allow(dead_code)]
    comments: Vec<(f64, String)>,
    loop_info: Option<(usize, usize, f64)>,

    ch: Vec<ChannelState>,

    gvol: f64,
    speed: u8,
    tempo: u8,
    mem_w: u8,

    total_rows: f64,
    #[allow(dead_code)]
    gvol_row: f64,
}

#[derive(Debug, Clone, serde::Serialize)]
struct ExecutedRow {
    absolute_row: usize,
    order: usize,
    pattern: usize,
    pattern_row: usize,
}

/// Evaluate an automation lane at row `t`.  `events` are
/// (time, duration, target) in row units, in time order.
fn lane_eval<I>(initial: f64, start: f64, events: I, t: f64) -> f64
where
    I: Iterator<Item = (f64, f64, f64)>,
{
    let mut segment_start = start;
    let mut segment_end = start;
    let mut segment_value = initial;
    let mut segment_target = initial;

    for (event_t, duration, target) in events {
        if event_t > t {
            break;
        }
        let value = if segment_end > segment_start && event_t < segment_end {
            let fraction = ((event_t - segment_start) / (segment_end - segment_start)).max(0.0);
            segment_value + (segment_target - segment_value) * fraction
        } else {
            segment_target
        };
        segment_start = event_t;
        segment_value = value;
        segment_end = event_t + duration.max(0.0);
        segment_target = target;
        if duration <= 0.0 {
            segment_value = target;
            segment_end = event_t;
        }
    }

    if segment_end > segment_start && t < segment_end {
        let fraction = ((t - segment_start) / (segment_end - segment_start)).max(0.0);
        return segment_value + (segment_target - segment_value) * fraction;
    }
    segment_target
}

impl<'a> Converter<'a> {
    fn new(m: &'a Module, pitch_mode: PitchMode) -> Self {
        let ch = (0..64)
            .map(|i| {
                let pan = m.chnpan.get(i).copied().unwrap_or(32);
                let vol = m.chnvol.get(i).copied().unwrap_or(64);
                ChannelState::new(pan, vol)
            })
            .collect();

        Converter {
            m,
            pitch_mode,
            fake_ins: fake_instrument(),
            warn: Vec::new(),
            warn_detail: HashMap::new(),
            voices: Vec::new(),
            rowlog: Vec::new(),
            tempo_ramps: Vec::new(),
            executed_rows: Vec::new(),
            comments: Vec::new(),
            loop_info: None,
            ch,
            gvol: m.gv as f64,
            speed: if m.speed == 0 { 6 } else { m.speed },
            tempo: if m.tempo == 0 { 125 } else { m.tempo },
            mem_w: 0,
            total_rows: 0.0,
            gvol_row: 0.0,
        }
    }

    fn note_warn(&mut self, key: &str) {
        for (k, n) in self.warn.iter_mut() {
            if k == key {
                *n += 1;
                return;
            }
        }
        self.warn.push((key.to_string(), 1));
    }

    // ------------------------------------------------------------------
    // timing
    // ------------------------------------------------------------------

    fn bpm(&self) -> f64 {
        60.0 * self.tempo as f64 / (2.5 * self.speed as f64 * RPB)
    }

    // ------------------------------------------------------------------
    // note mapping
    // ------------------------------------------------------------------

    /// Instrument by the 1-based index stored in `Voice::instrument`
    /// (0 = fake instrument).
    fn ins(&self, idx: usize) -> &Instrument {
        if idx == 0 {
            &self.fake_ins
        } else {
            &self.m.instruments[idx - 1]
        }
    }

    /// Return (instrument_index, sample_index, mapped_note).
    ///
    /// When `use_instruments` is false, the fake instrument (index 0) is
    /// returned and the "instrument" number is treated as a sample index.
    fn map_note(&self, inst: u8, note: u8) -> Option<(usize, u32, u8)> {
        let m = self.m;
        if m.use_instruments {
            if inst < 1 || (inst as usize) > m.instruments.len() {
                return None;
            }
            let ins = &m.instruments[inst as usize - 1];
            let (n, s) = *ins.keymap.get(note as usize)?;
            if s == 0 || (s as usize) > m.samples.len() {
                return None;
            }
            Some((inst as usize, s as u32, n))
        } else {
            if inst < 1 || (inst as usize) > m.samples.len() {
                return None;
            }
            Some((0, inst as u32, note))
        }
    }

    fn eff_vol(&self, st: &ChannelState, vol: u8, gvol: f64) -> f64 {
        100.0 * (vol as f64 / 64.0) * (st.chanvol as f64 / 64.0) * (gvol / 128.0) * st.gscale
    }

    // ------------------------------------------------------------------
    // waypoint bookkeeping
    // ------------------------------------------------------------------

    fn point(&mut self, vi: usize, t: f64, prop: &'static str, value: f64, dur: f64) {
        let nd = match prop {
            "pitch" => 2,
            "vol" => 1,
            "pan" => 2,
            _ => 2,
        };
        let v = &mut self.voices[vi];
        if let Some(&last) = v.last.get(prop) {
            if pyround(last, nd) == pyround(value, nd) {
                return;
            }
        }
        v.points.push((t, prop, value, dur));
        v.last.insert(prop, value);
    }

    /// A voice lane's interpolated value at absolute row `t`.
    fn value_at(voice: &Voice, prop: &str, t: f64) -> f64 {
        let initial = voice.init.get(prop).copied().unwrap_or(0.0);
        lane_eval(
            initial,
            voice.start,
            voice
                .points
                .iter()
                .filter(|p| p.1 == prop)
                .map(|&(et, _, target, dur)| (et, dur, target)),
            t,
        )
    }

    fn end_voice(&mut self, ch: usize, t: f64) {
        if let Some(idx) = self.ch[ch].voice.take() {
            let v = &mut self.voices[idx];
            // A fading voice already has an end; a cut may shorten it but a
            // later event must never stretch it.
            v.end = Some(match v.end {
                Some(e) => e.min(t),
                None => t,
            });
        }
    }

    // ------------------------------------------------------------------
    // main walk
    // ------------------------------------------------------------------

    fn run(&mut self) {
        let m = self.m;

        let mut order_i = 0usize;
        let mut row = 0usize;
        let mut abs_row = 0usize;

        let mut visited: HashSet<(usize, usize)> = HashSet::new();

        loop {
            while order_i < m.orders.len() && m.orders[order_i] as usize == ORDER_SKIP {
                order_i += 1;
            }

            if order_i >= m.orders.len() || m.orders[order_i] as usize == ORDER_END {
                break;
            }

            let pat = m.orders[order_i] as usize;

            if pat >= m.patterns.len() {
                self.note_warn("order points at missing pattern");
                order_i += 1;
                continue;
            }

            let grid = &m.patterns[pat];

            self.executed_rows.push(ExecutedRow {
                absolute_row: abs_row,
                order: order_i,
                pattern: pat,
                pattern_row: row,
            });

            if row >= grid.len() {
                order_i += 1;
                row = 0;
                continue;
            }

            if visited.contains(&(order_i, row)) {
                self.loop_info = Some((order_i, row, abs_row as f64));
                break;
            }

            visited.insert((order_i, row));

            let (jump_o, brk) = self.process_row(abs_row as f64, order_i, pat, row);

            abs_row += 1;

            if let Some(j) = jump_o {
                order_i = j;
                row = brk.unwrap_or(0);
            } else if let Some(b) = brk {
                order_i += 1;
                row = b;
            } else {
                row += 1;
            }
        }

        self.total_rows = abs_row as f64;

        for ch in 0..self.ch.len() {
            self.end_voice(ch, abs_row as f64);
        }

        self.finish_voices();
    }

    fn process_row(
        &mut self,
        t: f64,
        order_i: usize,
        pat: usize,
        row: usize,
    ) -> (Option<usize>, Option<usize>) {
        let m = self.m;
        let cells = &m.patterns[pat][row];

        let mut jump_o: Option<usize> = None;
        let mut brk: Option<usize> = None;

        // --------------------------------------------------------------
        // global effects
        // --------------------------------------------------------------

        // IT's W is a memory effect: once a parameter is issued, the
        // slide continues every row until overridden. Seed it from mem_w
        // so rows without an explicit W command still carry the slide.
        let mut gslide: Option<(&'static str, u8)> = if self.mem_w != 0 {
            Some(slide_decode(self.mem_w))
        } else {
            None
        };
        let mut tslide: i32 = 0;
        let previous_gvol = self.gvol;

        for raw in cells.iter() {
            let Some(cell) = read_cell(raw) else { continue };
            if cell.cmd == 0 {
                continue;
            }
            let c = char::from_u32(64 + cell.cmd as u32).unwrap_or('\0');
            let p = cell.par;

            match c {
                'A' => {
                    if p != 0 {
                        self.speed = p;
                    }
                }
                'T' => {
                    if p >= 0x20 {
                        self.tempo = p;
                    } else if p >> 4 == 0 {
                        tslide = -((p & 15) as i32);
                    } else if p >> 4 == 1 {
                        tslide = (p & 15) as i32;
                    }
                }
                'V' => self.gvol = (p as f64).min(128.0),
                'W' => {
                    if p != 0 {
                        self.mem_w = p;
                    }
                    gslide = Some(slide_decode(self.mem_w));
                }
                'B' => jump_o = Some(p as usize),
                'C' => brk = Some((p >> 4) as usize * 10 + (p & 15) as usize),
                'S' if (p >> 4) == 0xE => self.note_warn("SEx pattern delay ignored"),
                _ => {}
            }
        }

        let speed = self.speed;
        let bpm = self.bpm();

        self.rowlog.push((t, order_i, pat, row, pyround(bpm, 2)));

        let mut gstart = self.gvol;
        let mut gend = gstart;

        if let Some((k, a)) = gslide {
            if k != "none" {
                let a = a as f64;
                // Fine slides (fup/fdown) move at half the numeric rate of
                // a coarse slide. IT's WF1 is -0.5 per row, not -1. Coarse
                // slides keep their per-row rate.
                match k {
                    "fup" => {
                        gstart = (self.gvol + a * 0.48).min(128.0);
                        gend = gstart;
                    }
                    "fdown" => {
                        gstart = (self.gvol - a * 0.48).max(0.0);
                        gend = gstart;
                    }
                    "up" => gend = (self.gvol + a * (speed as f64 - 1.0)).min(128.0),
                    "down" => gend = (self.gvol - a * (speed as f64 - 1.0)).max(0.0),
                    _ => {}
                }
            }
        }

        self.gvol_row = gstart;

        // Global volume commands and slides change the output level of every
        // voice that is already sounding, including detached NNA voices.
        // Note starts below use gstart directly; existing voices need an
        // explicit automation point to receive the same change.
        if previous_gvol > 0.0 && gstart != previous_gvol {
            let start_ratio = gstart / previous_gvol;
            let end_ratio = gend / previous_gvol;
            for vi in 0..self.voices.len() {
                if self.voices[vi].end.map_or(false, |e| e <= t) {
                    continue;
                }
                let current = Self::value_at(&self.voices[vi], "vol", t);
                self.point(vi, t, "vol", current * start_ratio, 0.0);
                if gend != gstart && gstart > 0.0 {
                    self.point(vi, t, "vol", current * end_ratio, 1.0);
                }
            }
        }

        for ch in 0..64 {
            let cell = cells.get(ch).and_then(read_cell);

            if let Some(vi) = self.ch[ch].voice {
                if self.voices[vi].end.map_or(false, |e| e <= t) {
                    self.ch[ch].voice = None;
                }
            }

            if cell.is_none() && self.ch[ch].voice.is_none() {
                continue;
            }

            if self.m.chnpan.get(ch).copied().unwrap_or(32) & 128 != 0 && cell.is_some() {
                self.note_warn("events on disabled channel ignored");
                continue;
            }

            self.process_channel(t, ch, cell, speed, bpm, gstart, gend);
        }

        self.gvol = gend;

        if tslide != 0 {
            // IT slides the tempo once per tick (ticks 1..speed-1), clamping
            // to 32..255, and each tick lasts as long as the tempo in force
            // when it starts.  Record the BPM of every tick so the tempo lane
            // can reproduce row timing exactly (a linear ramp is wrong when
            // the clamp is hit part-way through the row).
            let mut tick_bpms = vec![self.bpm()];
            for _tick in 1..speed {
                self.tempo = (self.tempo as i32 + tslide).clamp(32, 255) as u8;
                tick_bpms.push(self.bpm());
            }
            self.tempo_ramps.push((t, tick_bpms));
        }

        (jump_o, brk)
    }

    // ------------------------------------------------------------------
    // channel processing
    // ------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn process_channel(
        &mut self,
        t: f64,
        ch: usize,
        cell: Option<Cell>,
        speed: u8,
        bpm: f64,
        gstart: f64,
        gend: f64,
    ) {
        let m = self.m;

        let (note, inst_opt, volc, cmd, par) = match cell {
            Some(c) => (c.note, c.inst, c.volc, c.cmd, c.par),
            None => (None, None, None, 0u8, 0u8),
        };
        let inst = inst_opt.unwrap_or(0);
        let previous_channel_vol = self.ch[ch].vol;

        let c: Option<char> = if cmd != 0 {
            char::from_u32(64 + cmd as u32)
        } else {
            None
        };

        if inst != 0 {
            self.ch[ch].inst = inst;
        }

        // --------------------------------------------------------------
        // volume column
        // --------------------------------------------------------------

        let vcol: Option<(&'static str, i32)> = match volc {
            Some(v) if v <= 64 => Some(("set", v as i32)),
            Some(v) if v <= 74 => Some(("fup", v as i32 - 65)),
            Some(v) if v <= 84 => Some(("fdown", v as i32 - 75)),
            Some(v) if v <= 94 => Some(("up", v as i32 - 85)),
            Some(v) if v <= 104 => Some(("down", v as i32 - 95)),
            Some(v) if v <= 114 => Some(("pdown", v as i32 - 105)),
            Some(v) if v <= 124 => Some(("pup", v as i32 - 115)),
            Some(v) if (128..=192).contains(&v) => Some(("pan", v as i32 - 128)),
            Some(v) if (193..=202).contains(&v) => Some(("G", G_TABLE[(v - 193) as usize])),
            Some(v) if (203..=212).contains(&v) => Some(("H", v as i32 - 203)),
            _ => None,
        };

        let toneporta =
            matches!(c, Some('G') | Some('L')) || matches!(vcol, Some(("G", _)));

        let mut is_note = matches!(note, Some(n) if n < 120);

        let mut delay = 0.0;

        if c == Some('S') && (par >> 4) == 0xD && (par & 15) != 0 && is_note {
            if (par & 15) >= speed {
                self.note_warn("SDx delay >= speed: note never plays, dropped");
                is_note = false;
            } else {
                delay = (par & 15) as f64 / speed as f64;
            }
        }

        // --------------------------------------------------------------
        // note off / cut
        // --------------------------------------------------------------

        match term_kind(note) {
            Some("cut") => {
                self.end_voice(ch, t);
                self.ch[ch].target = None;
            }
            Some("off") => {
                let vidx = self.ch[ch].voice;
                let mut non_looping_env = false;
                let mut fade_branch = false;
                let mut looped_env = false;
                let mut fadeout = 0.0;
                let mut vspeed = 1.0;
                if let Some(vi) = vidx {
                    let v = &self.voices[vi];
                    let e = v.envelope.as_ref();
                    let enabled = e.map_or(false, |e| e.enabled);
                    non_looping_env = e.map_or(false, |e| {
                        e.enabled && !e.points.is_empty() && !e.loop_enabled && !e.sustain_enabled
                    });
                    looped_env = e.map_or(false, |e| e.enabled && (e.loop_enabled || e.sustain_enabled));
                    fadeout = self.ins(v.instrument).fadeout as f64;
                    vspeed = v.speed.max(1) as f64;
                    fade_branch = v.fade.is_none() && fadeout > 0.0 && !enabled;
                }

                if non_looping_env {
                    if let Some(vi) = vidx {
                        self.voices[vi].key_off_row = Some(t);
                    }
                } else if fade_branch {
                    // IT key-off starts instrument fadeout when there is no
                    // enabled volume envelope. OpenMPT reduces nFadeOutVol by
                    // 2 * fadeout per tick from 65536, so convert that tick
                    // count to rows using the voice's speed.
                    let fade_rows = 1024.0 / (fadeout * vspeed);
                    // Keep the fade out of the volume lane.  A ramp written
                    // into the lane is cancelled by the next channel/global
                    // volume point.  finish_voices() multiplies it back in.
                    if let Some(vi) = vidx {
                        self.voices[vi].fade = Some((t, fade_rows));
                        self.voices[vi].end = Some(t + fade_rows);
                    }
                    // The voice stays on the channel while it fades: a later
                    // note cut, or the next note's NNA, still applies to it.
                } else if looped_env {
                    self.end_voice(ch, t);
                    self.note_warn("looped/sustained volume envelope key-off approximated as cut");
                } else if let Some(vi) = vidx {
                    // A non-looping envelope keeps running after key-off. With
                    // no envelope and no fadeout, IT also keeps the sample
                    // playing.
                    self.voices[vi].key_off_row = Some(t);
                }
                self.ch[ch].target = None;
            }
            _ => {}
        }

        let mut trigger: Option<(usize, u32, u8)> = None;

        if is_note {
            let note_v = note.unwrap_or(0);
            if toneporta && self.ch[ch].voice.is_some() {
                let mapped = self.map_note(self.ch[ch].inst, note_v);

                self.ch[ch].target = Some(match mapped {
                    Some(mp) => mp.2 as f64,
                    None => note_v as f64,
                });

                // An instrument number next to a tone-portamento note does
                // not retrigger the sample, but IT still resets the channel
                // volume (and instrument default pan) to the sample/instrument
                // defaults.  Without this, volume slides (K/D) keep dragging
                // the level down across a run of Gxx notes.
                if inst != 0 {
                    if let Some(mp) = mapped {
                        let portasmp = &m.samples[mp.1 as usize - 1];
                        self.ch[ch].vol = portasmp.vol;
                        let dfp = self.ins(mp.0).dfp;
                        if m.use_instruments && (dfp & 128) != 0 {
                            self.ch[ch].pan = (dfp & 127).min(64) as f64;
                        }
                    }
                }
            } else if self.ch[ch].inst == 0 {
                self.note_warn("note with no instrument ignored");
            } else {
                let mapped = self.map_note(self.ch[ch].inst, note_v);

                match mapped {
                    Some(mp) if m.samples[mp.1 as usize - 1].has_data => trigger = Some(mp),
                    _ => {
                        self.note_warn("note maps to empty sample (silent)");
                        self.end_voice(ch, t);
                    }
                }
            }
        }

        // --------------------------------------------------------------
        // new-note defaults
        // --------------------------------------------------------------

        if let Some((ins_i, sidx, mnote)) = trigger {
            let smp = &m.samples[sidx as usize - 1];

            let new_start = t + delay;
            if let Some(ov) = self.ch[ch].voice {
                let nna = self.ins(self.voices[ov].instrument).nna;
                if nna == 1 {
                    // IT NNA: continue.  The old note moves to an independent
                    // voice. It remains in the score while the tracker
                    // channel starts the new note.
                    self.ch[ch].voice = None;
                } else {
                    if nna == 2 || nna == 3 {
                        self.note_warn("NNA note-off/fade approximated as cut");
                    }
                    self.end_voice(ch, new_start);
                }
            }

            let (dct, dca, dfp, gbv) = {
                let ins = self.ins(ins_i);
                (ins.dct, ins.dca, ins.dfp, ins.gbv)
            };
            if dct != 0 || dca != 0 {
                self.note_warn("instrument duplicate-check action not converted");
            }

            // IT resets volume/pan to the instrument/sample defaults only when
            // the cell has an instrument number; a bare note keeps them.
            if inst != 0 {
                self.ch[ch].vol = smp.vol;
            }

            if inst != 0 && m.use_instruments && (dfp & 128) != 0 {
                self.ch[ch].pan = (dfp & 127).min(64) as f64;
            }

            let st = &mut self.ch[ch];
            st.pitch = mnote as f64;
            st.target = None;

            st.vib_pos = 0.0;
            st.vib_on = false;

            st.gscale = (smp.gvl as f64 / 64.0)
                * if m.use_instruments {
                    gbv as f64 / 128.0
                } else {
                    1.0
                };
        }

        // --------------------------------------------------------------
        // instant changes
        // --------------------------------------------------------------

        if let Some((k, a)) = vcol {
            let st = &mut self.ch[ch];
            match k {
                "set" => st.vol = a as u8,
                "fup" => st.vol = (st.vol as i32 + a).min(64) as u8,
                "fdown" => st.vol = (st.vol as i32 - a).max(0) as u8,
                "pan" => st.pan = a as f64,
                _ => {}
            }
        }

        let mut cut_tick: Option<u8> = None;

        match c {
            Some('M') => {
                let old_chanvol = self.ch[ch].chanvol;
                // IT's Mxx is 00..40h (0..64), the same scale as channel volume.
                self.ch[ch].chanvol = par.min(64);
                let new_chanvol = self.ch[ch].chanvol;
                // IT channel-volume commands affect every playing voice on the
                // channel, including NNA voices that have been detached from
                // st.voice by later note-ons. Scale their current effective
                // level by the channel-volume ratio and emit the change on
                // each voice.
                if old_chanvol != 0 && old_chanvol != new_chanvol {
                    let ratio = new_chanvol as f64 / old_chanvol as f64;
                    for vi in 0..self.voices.len() {
                        let v = &self.voices[vi];
                        if v.ch as usize != ch || v.end.map_or(false, |e| e <= t) {
                            continue;
                        }
                        let current = Self::value_at(v, "vol", t);
                        self.point(vi, t, "vol", current * ratio, 0.0);
                    }
                }
            }
            Some('X') => self.ch[ch].pan = (par as f64 * 64.0 / 255.0).min(64.0),
            Some('S') => {
                let sub = par >> 4;
                let x = par & 15;
                match sub {
                    0x8 => self.ch[ch].pan = x as f64 * 64.0 / 15.0,
                    0xC => cut_tick = Some(x),
                    0xD => {}
                    0x9 => self.note_warn("S9x surround/sound control ignored"),
                    0x3 | 0x4 | 0x5 => self.note_warn("S3x/S4x/S5x waveform select ignored"),
                    0x7 | 0xB | 0xA | 0xF | 0x1 | 0x2 | 0x6 => {
                        self.note_warn(&format!("S{:X}x ignored", sub))
                    }
                    _ => {}
                }
            }
            Some('D') | Some('K') | Some('L') => {
                if par != 0 {
                    self.ch[ch].mem_d = par;
                }
            }
            Some('O') => {
                if par != 0 {
                    self.ch[ch].mem_o = par;
                }
            }
            Some('E') | Some('F') => {
                if par != 0 {
                    self.ch[ch].mem_ef = par;
                }
            }
            Some('G') => {
                if par != 0 {
                    self.ch[ch].mem_g = par;
                }
            }
            Some('Z') => self.z_comment(t, par),
            Some('H') | Some('U') => {}
            Some('B') | Some('C') | Some('A') | Some('T') | Some('V') | Some('W') => {}
            Some(other) => self.note_warn(&format!("effect {} not converted", other)),
            None => {}
        }

        // --------------------------------------------------------------
        // volume slide
        // --------------------------------------------------------------

        let mut dkind: Option<(&'static str, u8)> = None;

        if matches!(c, Some('D') | Some('K') | Some('L')) {
            let dk = slide_decode(self.ch[ch].mem_d);
            dkind = Some(dk);
            let st = &mut self.ch[ch];

            if dk.0 == "fup" {
                st.vol = (st.vol as i32 + dk.1 as i32).min(64) as u8;
            } else if dk.0 == "fdown" {
                st.vol = (st.vol as i32 - dk.1 as i32).max(0) as u8;
            }
        }

        // --------------------------------------------------------------
        // fine pitch slide
        // --------------------------------------------------------------

        let mut efs: Option<(&'static str, f64)> = None;

        if matches!(c, Some('E') | Some('F')) {
            let p = self.ch[ch].mem_ef;
            let sgn = if c == Some('E') { -1.0 } else { 1.0 };

            efs = Some(if p >= 0xF0 {
                ("fine", sgn * (p & 15) as f64 / 16.0)
            } else if p >= 0xE0 {
                ("fine", sgn * (p & 15) as f64 / 64.0)
            } else {
                ("ramp", sgn * p as f64 / 16.0)
            });
        } else if let Some((k @ ("pdown" | "pup"), a)) = vcol {
            let sgn = if k == "pdown" { -1.0 } else { 1.0 };
            efs = Some(("ramp", sgn * (a * 4) as f64 / 16.0));
        }

        if let Some(("fine", amount)) = efs {
            let c5 = match self.ch[ch].voice {
                Some(vi) => m.samples[self.voices[vi].sample as usize - 1].c5 as f64,
                None => 8363.0,
            };
            self.ch[ch].pitch = slide_pitch_period(self.ch[ch].pitch, amount * 64.0, c5, m.linear);
        }

        // --------------------------------------------------------------
        // create voice
        // --------------------------------------------------------------

        let mut vidx = self.ch[ch].voice;

        if let Some((ins_i, sidx, mnote)) = trigger {
            let smp = &m.samples[sidx as usize - 1];
            let envelope = self.ins(ins_i).envelopes.get("volume").cloned();

            let mut v = Voice::new(ch as u8, t + delay, sidx, mnote, bpm, ins_i, speed, envelope);

            let mut offset_warn = false;
            if c == Some('O') {
                v.offset = self.ch[ch].mem_o as u32 * 256;

                if v.offset >= smp.length {
                    v.offset = 0;
                    offset_warn = true;
                }
            }
            if offset_warn {
                self.note_warn("sample offset beyond sample end ignored");
            }

            let st = &self.ch[ch];
            v.init.insert("pitch", hz(st.pitch));
            v.init.insert("vol", self.eff_vol(st, st.vol, gstart));
            v.init.insert("pan", (st.pan - 32.0) / 32.0);
            v.last = v.init.clone();

            if !smp.r#loop {
                let c5 = if smp.c5 == 0 { 8363.0 } else { smp.c5 as f64 };
                let rate = (c5 * 2f64.powf((mnote as f64 - 60.0) / 12.0)).max(1.0);

                v.natural_rows = Some((smp.length as f64 / rate) * (bpm * RPB / 60.0));
            }

            let vi = self.voices.len();
            self.voices.push(v);
            self.ch[ch].voice = Some(vi);
            vidx = Some(vi);
        } else if let Some(vi) = vidx {
            let st = &self.ch[ch];
            let vol = self.eff_vol(st, st.vol, gstart);
            let pan = (st.pan - 32.0) / 32.0;
            let pitch = hz(st.pitch);

            self.point(vi, t, "vol", vol, 0.0);
            self.point(vi, t, "pan", pan, 0.0);

            if matches!(efs, Some(("fine", _))) {
                self.point(vi, t, "pitch", pitch, 0.0);
            }
        }

        let Some(vi) = vidx else { return };

        // --------------------------------------------------------------
        // continuous effects
        // --------------------------------------------------------------

        let ramp_dur = (speed as f64 - 1.0) / speed as f64;
        let ticks = speed as i32 - 1;

        // volume
        let mut vol_end = self.ch[ch].vol;
        let mut slide: Option<(&'static str, i32)> = None;

        if let Some((k @ ("up" | "down"), a)) = dkind {
            slide = Some((k, a as i32));
        } else if let Some((k @ ("up" | "down"), a)) = vcol {
            slide = Some((k, a));
        }

        if let Some((k, a)) = slide {
            let d = a * ticks;
            let cur = self.ch[ch].vol as i32;
            vol_end = if k == "up" {
                (cur + d).min(64) as u8
            } else {
                (cur - d).max(0) as u8
            };
        }

        // IT channel-volume changes apply to every voice still sounding on
        // the channel, including NNA voices detached from `st.voice`.
        // The attached voice is handled by eff0/eff1 below; update the rest
        // here so volume-column sets and slides do not leave old voices loud.
        let cur_vol = self.ch[ch].vol;
        if previous_channel_vol > 0 && (cur_vol != previous_channel_vol || vol_end != cur_vol) {
            let start_ratio = cur_vol as f64 / previous_channel_vol as f64;
            let end_ratio = vol_end as f64 / previous_channel_vol as f64;
            for oi in 0..self.voices.len() {
                let old = &self.voices[oi];
                if old.ch as usize != ch
                    || oi == vi
                    || old.start >= t
                    || old.end.map_or(false, |e| e <= t)
                {
                    continue;
                }
                let current = Self::value_at(old, "vol", t);
                self.point(oi, t, "vol", current * start_ratio, 0.0);
                if end_ratio != start_ratio {
                    self.point(oi, t, "vol", current * end_ratio, ramp_dur);
                }
            }
        }

        let eff0 = self.eff_vol(&self.ch[ch], cur_vol, gstart);
        let eff1 = self.eff_vol(&self.ch[ch], vol_end, gend);

        if (eff1 - eff0).abs() > 0.05 {
            self.point(vi, t, "vol", eff1, ramp_dur);
        }

        // pitch
        let base = self.ch[ch].pitch;
        let mut base_end = base;

        if let Some(("ramp", amount)) = efs {
            let sample = &m.samples[self.voices[vi].sample as usize - 1];
            if !m.linear {
                self.note_warn("non-linear IT period slide approximated by pitch ramp");
            }
            base_end = slide_pitch_period(base, amount * 64.0 * ticks as f64, sample.c5 as f64, m.linear);
        }

        let mut gp: Option<u8> = None;
        let mut stair: Vec<(i32, f64)> = Vec::new();

        if matches!(c, Some('G') | Some('L')) {
            gp = Some(self.ch[ch].mem_g);
        } else if let Some(("G", a)) = vcol {
            gp = Some(if a != 0 { a as u8 } else { self.ch[ch].mem_g });

            if a != 0 {
                self.ch[ch].mem_g = a as u8;
            }
        }

        if let (Some(gp), Some(target)) = (gp, self.ch[ch].target) {
            let sample = &m.samples[self.voices[vi].sample as usize - 1];
            if !m.linear {
                self.note_warn("non-linear IT tone portamento approximated by pitch ramp");
            }
            let direction = if target > base { 1.0 } else { -1.0 };

            // IT applies tone portamento once per tick (ticks 1..speed-1),
            // stepping gp*4 slide units toward the target and stopping on it.
            // A fast slide such as Gff therefore snaps to the new pitch on
            // tick 1 instead of gliding across the whole row.  Keep the
            // per-tick staircase so it can be emitted as steps.
            let mut p = base;
            for k in 1..=ticks {
                let mut nxt = slide_pitch_period(
                    p,
                    direction * gp as f64 * 4.0,
                    sample.c5 as f64,
                    m.linear,
                );
                if direction > 0.0 {
                    nxt = nxt.min(target);
                } else {
                    nxt = nxt.max(target);
                }
                if (nxt - p).abs() > 1e-9 {
                    stair.push((k, nxt));
                }
                p = nxt;
                if p == target {
                    break;
                }
            }

            base_end = p;

            if base_end == target {
                self.ch[ch].target = None;
            }
        }

        let is_vib_cmd = matches!(c, Some('H') | Some('K') | Some('U'))
            || matches!(vcol, Some(("H", _)));

        if (base_end - base).abs() > 1e-6 && !is_vib_cmd {
            if !stair.is_empty() {
                for &(k, val) in &stair {
                    self.point(vi, t + k as f64 / speed as f64, "pitch", hz(val), 0.0);
                }
            } else {
                self.point(vi, t, "pitch", hz(base_end), ramp_dur);
            }
        }

        // vibrato
        let mut vib = false;

        if is_vib_cmd {
            if matches!(c, Some('H') | Some('U')) {
                let x = par >> 4;
                let y = par & 15;

                if x != 0 {
                    self.ch[ch].vib_spd = x;
                }

                if y != 0 {
                    self.ch[ch].vib_dep = if c == Some('H') {
                        y as f64
                    } else {
                        y as f64 / 4.0
                    };
                }
            } else if let Some(("H", a)) = vcol {
                if a != 0 {
                    self.ch[ch].vib_dep = a as f64;
                }
            }

            vib = self.ch[ch].vib_spd > 0 && self.ch[ch].vib_dep > 0.0;
        }

        if vib {
            // libopenmpt/IT: vibrato delta is (sine * depth*4) >> 7, or >> 6
            // with "old effects" -- i.e. old effects are twice as deep.
            let peak = self.ch[ch].vib_dep
                * VIB_PEAK_PER_DEPTH
                * if m.old_effects { 1.0 } else { 0.5 };

            let step = self.ch[ch].vib_spd as f64 * 4.0;
            let mut pos = self.ch[ch].vib_pos;

            for tick in 0..speed {
                let p1 = pos + step;

                let mut k = (pos / 64.0 - 1e-9).ceil() as i64;

                while (k as f64) * 64.0 < p1 - 1e-9 {
                    let frac = ((k as f64) * 64.0 - pos) / step;

                    let tau = t + (tick as f64 + frac) / speed as f64;

                    let nxt = (k + 1).rem_euclid(4);

                    let off = [0.0, peak, 0.0, -peak][nxt as usize];

                    self.point(
                        vi,
                        tau,
                        "pitch",
                        hz(base_end + off),
                        (64.0 / step) / speed as f64,
                    );

                    k += 1;
                }

                pos = p1;
            }

            self.ch[ch].vib_pos = pos;
            self.ch[ch].vib_on = true;
        } else if self.ch[ch].vib_on {
            self.point(vi, t, "pitch", hz(base_end), 0.0);

            self.ch[ch].vib_on = false;
        }

        self.ch[ch].pitch = base_end;
        self.ch[ch].vol = vol_end;

        // sample cut
        if let Some(ct) = cut_tick {
            self.end_voice(ch, t + ct as f64 / speed as f64);
        }
    }

    // ------------------------------------------------------------------
    // comments
    // ------------------------------------------------------------------

    fn z_comment(&mut self, t: f64, p: u8) {
        if p < 0x80 {
            self.comments.push((
                t,
                format!("Z{:02X}: filter cutoff {}%", p, fmt(p as f64 / 127.0 * 100.0, 0)),
            ));
        } else {
            self.comments
                .push((t, format!("Z{:02X}: MIDI macro {:X}", p, p - 0x80)));
        }

        self.note_warn("Z filter/MIDI macros written as comments only");
    }

    // ------------------------------------------------------------------
    // post processing
    // ------------------------------------------------------------------

    fn finish_voices(&mut self) {
        for vi in 0..self.voices.len() {
            let total = self.total_rows;
            let mut end = self.voices[vi].end.unwrap_or(total);

            if let Some(nat) = self.voices[vi].natural_rows {
                end = end.min(self.voices[vi].start + nat.max(0.05));
            }

            let mut new_end = self.apply_volume_envelope(vi, end);

            // IT stops producing audio at the last played row. A non-looping
            // volume envelope may in theory release past that point, but the
            // reference render does not carry it, so cut the voice here.
            if new_end > total {
                new_end = total;
            }
            self.voices[vi].end = Some(new_end);

            // apply_volume_envelope may have emitted waypoints past the
            // clamped end when the envelope released beyond the song's last
            // row. Those cannot be heard, and if left in they cause the
            // player's automation lane to interpolate toward a value that is
            // only reached after the voice has stopped, so the render carries
            // audio into the tail where the source module is silent.
            self.voices[vi].points.retain(|p| p.0 <= new_end);

            self.apply_fadeout(vi);
            Self::merge_steps(&mut self.voices[vi]);
        }
    }

    fn apply_volume_envelope(&mut self, vi: usize, mut end: f64) -> f64 {
        let envelope = match &self.voices[vi].envelope {
            Some(e) if e.enabled && !e.points.is_empty() => e.clone(),
            _ => return end,
        };
        if envelope.loop_enabled || envelope.sustain_enabled {
            self.note_warn("looped/sustained volume envelope not converted");
            return end;
        }

        let voice = &mut self.voices[vi];

        let mut points: Vec<(f64, f64)> = envelope
            .points
            .iter()
            .map(|&(tick, value)| (tick as f64, value as f64))
            .collect();
        points.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let vspeed = voice.speed.max(1) as f64;
        let initial_volume = voice.init.get("vol").copied().unwrap_or(100.0);
        let (first_tick, first_value) = points[0];
        voice.init.insert("vol", initial_volume * first_value / 64.0);
        let last = points[points.len() - 1];
        let envelope_end = voice.start + (last.0 - first_tick).max(0.0) / vspeed;
        if last.1 <= 0.0 {
            // A key-off does not stop an IT voice with a non-looping volume
            // envelope. Likewise, voices still held when the order list ends
            // continue through their envelope tail. Keep natural sample ends
            // and explicit note cuts intact.
            if voice.key_off_row.is_some() || (voice.end.is_none() && voice.natural_rows.is_none()) {
                end = end.max(envelope_end);
            } else {
                end = end.min(envelope_end);
            }
        }

        // (time, duration, value) of the voice's volume lane.
        let volume_events: Vec<(f64, f64, f64)> = voice
            .points
            .iter()
            .filter(|p| p.1 == "vol")
            .map(|&(time, _, value, duration)| (time, duration, value))
            .collect();

        let vstart = voice.start;

        let envelope_value = |row: f64| -> f64 {
            let tick = first_tick.max((row - vstart) * vspeed + first_tick);
            for w in points.windows(2) {
                let (left_tick, left_value) = w[0];
                let (right_tick, right_value) = w[1];
                if tick <= right_tick {
                    if right_tick == left_tick {
                        return right_value;
                    }
                    let fraction = (tick - left_tick) / (right_tick - left_tick);
                    return left_value + (right_value - left_value) * fraction;
                }
            }
            last.1
        };

        // The Python builds an AutomationLane over a constant-tempo map;
        // with rows as the only unit that is the same lane evaluation.
        let base_value = |row: f64| -> f64 {
            lane_eval(initial_volume, vstart, volume_events.iter().copied(), row)
        };

        let mut breakpoints: Vec<f64> = vec![vstart, end];
        let mut instant_rows: Vec<f64> = Vec::new();
        for &(time, duration, _value) in &volume_events {
            if vstart < time && time < end {
                breakpoints.push(time);
            }
            let finish = time + duration.max(0.0);
            if vstart < finish && finish < end {
                breakpoints.push(finish);
            }
            if duration <= 0.0 && vstart < time && time <= end {
                instant_rows.push(time);
            }
        }
        for &(tick, _value) in &points {
            let row = vstart + (tick - first_tick).max(0.0) / vspeed;
            if vstart < row && row < end {
                breakpoints.push(row);
            }
        }

        let ordered = sort_dedup(breakpoints);
        let mut sampled = ordered.clone();
        sampled.extend(ordered.windows(2).map(|w| (w[0] + w[1]) / 2.0));
        let ordered = sort_dedup(sampled);

        let mut volume_points: Vec<(f64, &'static str, f64, f64)> = voice
            .points
            .iter()
            .filter(|p| p.1 != "vol")
            .copied()
            .collect();

        for w in ordered.windows(2) {
            let (left, right) = (w[0], w[1]);
            let epsilon = 1e-6f64.min((right - left) / 4.0);
            let sample_row = right - epsilon;
            let target = base_value(sample_row) * envelope_value(sample_row) / 64.0;
            volume_points.push((left, "vol", target, right - left));
            if instant_rows.iter().any(|&row| (right - row).abs() < 1e-9) {
                let target_after = base_value(right) * envelope_value(right) / 64.0;
                volume_points.push((right, "vol", target_after, 0.0));
            }
        }

        voice.points = volume_points;
        end
    }

    /// Multiply an instrument fadeout into the voice's volume lane.
    ///
    /// The lane holds channel and global volume only; the fade is a
    /// separate linear multiplier, so volume changes during the fade no
    /// longer cancel it.
    fn apply_fadeout(&mut self, vi: usize) {
        let voice = &self.voices[vi];
        let (Some((fade_start, fade_rows)), Some(vend)) = (voice.fade, voice.end) else {
            return;
        };
        let fade_end = fade_start + fade_rows;
        let start = voice.start;
        let end = vend;
        if end <= start {
            return;
        }

        let fade_factor = |row: f64| -> f64 {
            if row <= fade_start {
                1.0
            } else if row >= fade_end {
                0.0
            } else {
                1.0 - (row - fade_start) / fade_rows
            }
        };

        let value = |row: f64| -> f64 { Self::value_at(voice, "vol", row) * fade_factor(row) };

        let mut breakpoints: Vec<f64> = vec![start, end];
        let mut instant_rows: Vec<f64> = Vec::new();
        for &(time, prop, _value, duration) in &voice.points {
            if prop != "vol" {
                continue;
            }
            if start < time && time < end {
                breakpoints.push(time);
            }
            let finish = time + duration.max(0.0);
            if start < finish && finish < end {
                breakpoints.push(finish);
            }
            if duration <= 0.0 && start < time && time <= end {
                instant_rows.push(time);
            }
        }
        for row in [fade_start, fade_end] {
            if start < row && row < end {
                breakpoints.push(row);
            }
        }

        let ordered = sort_dedup(breakpoints);
        let mut sampled = ordered.clone();
        sampled.extend(ordered.windows(2).map(|w| (w[0] + w[1]) / 2.0));
        let ordered = sort_dedup(sampled);

        let mut new_points: Vec<(f64, &'static str, f64, f64)> =
            voice.points.iter().filter(|p| p.1 != "vol").copied().collect();
        for w in ordered.windows(2) {
            let (left, right) = (w[0], w[1]);
            let epsilon = 1e-6f64.min((right - left) / 4.0);
            new_points.push((left, "vol", value(right - epsilon), right - left));
            if instant_rows.iter().any(|&row| (right - row).abs() < 1e-9) {
                new_points.push((right, "vol", value(right), 0.0));
            }
        }
        self.voices[vi].points = new_points;
    }

    fn merge_steps(v: &mut Voice) {
        let mut out: Vec<(f64, &'static str, f64, f64)> = Vec::new();

        for prop in ["vol", "pan", "pitch"] {
            let pts: Vec<(f64, &'static str, f64, f64)> =
                v.points.iter().filter(|p| p.1 == prop).copied().collect();

            let mut i = 0;

            while i < pts.len() {
                let mut j = i;

                if pts[i].3 == 0.0 {
                    while j + 1 < pts.len()
                        && pts[j + 1].3 == 0.0
                        && (pts[j + 1].0 - pts[j].0 - 1.0).abs() < 1e-6
                        && (j + 1 == i + 1
                            || ((pts[j + 1].2 - pts[j].2) - (pts[i + 1].2 - pts[i].2)).abs() < 1e-6)
                    {
                        j += 1;
                    }
                }

                if j - i >= 2 {
                    out.push(pts[i]);
                    out.push((pts[i].0, prop, pts[j].2, pts[j].0 - pts[i].0));
                    i = j + 1;
                } else {
                    out.push(pts[i]);
                    i += 1;
                }
            }
        }

        // Stable, like Python's sort on (row, property).
        out.sort_by(|a, b| {
            a.0.partial_cmp(&b.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.1.cmp(b.1))
        });

        v.points = out;
    }
}

/// Sort ascending and drop exact duplicates (a Python `set` of floats).
fn sort_dedup(mut v: Vec<f64>) -> Vec<f64> {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v.dedup();
    v
}

// ── Trackscript representation ─────────────────────────────────────────

fn pos_str(r: f64) -> String {
    let mut b = (r / RPB).floor() as i64 + 1;

    let s = pyround(r - RPB * (b - 1) as f64 + 1.0, 2);

    if (s - round_even(s)).abs() < 1e-9 {
        let mut si = round_even(s) as i64;

        if si > ROWS_PER_BEAT as i64 {
            b += 1;
            si = 1;
        }

        return format!("§{}/{}", b, si);
    }

    format!("§{}/{}", b, fmt(s, 2))
}

/// Note name when the pitch is exactly a note (within 2 cents), otherwise Hz.
fn pitch_text(hz_value: f64, mode: PitchMode) -> String {
    if matches!(mode, PitchMode::Names) {
        let semis = 60.0 + 12.0 * (hz_value / C5_HZ).log2();
        let n = round_even(semis);
        if (semis - n).abs() < 0.02 {
            return note_name(n as i64);
        }
    }
    fmt(hz_value, 2)
}

fn pan_str(p: f64) -> String {
    fmt(p.clamp(-1.0, 1.0), 2)
}

/// Tempo events as (row, bpm, ramp_rows).
///
/// IT tempo slides change the tick rate inside the row that contains the
/// command. Preserve that as per-tick steps; direct tempo and speed changes
/// remain jumps at their row positions.
fn tempo_lane(conv: &Converter) -> Vec<(f64, f64, f64)> {
    let b: Vec<f64> = conv.rowlog.iter().map(|r| r.4).collect();
    if b.is_empty() {
        return Vec::new();
    }

    let mut wps: Vec<(f64, f64, f64)> = Vec::new();
    let mut slide_rows: HashSet<usize> = HashSet::new();
    for (row, tick_bpms) in &conv.tempo_ramps {
        // Tick 0 runs at the row's own BPM (already in `b`); every later
        // tick that changes the BPM becomes a step at its tick position.
        let n = tick_bpms.len();
        for k in 1..n {
            if (tick_bpms[k] - tick_bpms[k - 1]).abs() > 1e-9 {
                wps.push((row + k as f64 / n as f64, tick_bpms[k], 0.0));
            }
        }
        // The next row starts at the final tick's BPM, so a jump there is
        // redundant -- unless another command (e.g. Txx >= 0x20) changed the
        // tempo on that row, in which case it must still be emitted.
        let end_row = *row as usize + 1;
        if end_row < b.len() && (b[end_row] - pyround(tick_bpms[n - 1], 2)).abs() < 0.01 {
            slide_rows.insert(end_row);
        }
    }

    for row in 1..b.len() {
        if !slide_rows.contains(&row) && b[row] != b[row - 1] {
            wps.push((row as f64, b[row], 0.0));
        }
    }

    wps.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    wps
}

/// Global BPM runs as (start_row, end_row, bpm).  Kept for the report's
/// timing diagnostics.
fn build_sections(conv: &Converter) -> Vec<(f64, f64, f64)> {
    let mut runs: Vec<(f64, f64, f64)> = Vec::new();

    for &(t, _oi, _pat, _row, bpm) in &conv.rowlog {
        match runs.last_mut() {
            Some(last) if last.2 == bpm => last.1 = t + 1.0,
            _ => runs.push((t, t + 1.0, bpm)),
        }
    }

    runs
}

// ── Structural extraction ──────────────────────────────────────────────

/// One pattern span per measure.
///
/// A sounding voice may continue past a pattern boundary. Its note length
/// carries the sound into later measures; it must not force the converter to
/// merge measures into a song-sized pattern.
fn build_safe_spans(total_rows: f64) -> Vec<(f64, f64)> {
    if total_rows <= 0.0 {
        return Vec::new();
    }
    let total = total_rows as usize;
    let step = MEASURE_ROWS as usize;
    (0..total)
        .step_by(step)
        .map(|s| (s as f64, (s as f64 + MEASURE_ROWS).min(total_rows)))
        .collect()
}

struct PartDefinition {
    voices: Vec<usize>,
    #[allow(dead_code)]
    start: f64,
    #[allow(dead_code)]
    end: f64,
    sample_index: u32,
    name: String,
    rendered: String,
}

type Placement = (Vec<usize>, f64, f64);

impl<'a> Converter<'a> {
    /// Voices starting inside the span, grouped by source sample.
    ///
    /// Tracker channels are playback machinery, not part of Trackscript's
    /// musical model. Voices using the same sample in a measure belong to one
    /// candidate pattern even when the source module played them on different
    /// channels.
    fn group_span_voices(&self, start: f64, end: f64) -> BTreeMap<u32, Vec<usize>> {
        let mut groups: BTreeMap<u32, Vec<usize>> = BTreeMap::new();

        for (vi, v) in self.voices.iter().enumerate() {
            if start <= v.start && v.start < end {
                groups.entry(v.sample).or_default().push(vi);
            }
        }

        for voices in groups.values_mut() {
            voices.sort_by(|&a, &b| {
                let (va, vb) = (&self.voices[a], &self.voices[b]);
                va.start
                    .partial_cmp(&vb.start)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| {
                        va.end
                            .unwrap_or(1e18)
                            .partial_cmp(&vb.end.unwrap_or(1e18))
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
            });
        }

        groups
    }

    // ------------------------------------------------------------------
    // event rendering
    // ------------------------------------------------------------------

    fn render_voice_event(
        &self,
        v: &Voice,
        local_start: f64,
        sample_files: &HashMap<u32, String>,
    ) -> Option<String> {
        let mode = self.pitch_mode;
        let f = sample_files.get(&v.sample)?;
        if f.is_empty() {
            return None;
        }

        let dur_rows = (v.end.unwrap_or(v.start) - v.start).max(0.05);

        // ~ is length in BEATS. Convert from rows.
        let dur_beats = dur_rows / RPB;

        // § is local to the pattern's measure; BlockList supplies the measure.
        let mut fields = vec![
            pos_str(v.start - local_start),
            format!("#{}", pitch_text(v.init["pitch"], mode)),
            format!("~{}", fmt(dur_beats, 2)),
        ];

        if (v.init["vol"] - 100.0).abs() > 1e-9 {
            fields.push(format!("%{}", fmt(v.init["vol"], 1)));
        }

        if v.offset != 0 {
            fields.push(format!("ƒ'{}'({})", f, v.offset));
        }

        if v.init["pan"].abs() > 1e-9 {
            fields.push(format!("¶{}", pan_str(v.init["pan"])));
        }

        let ev = format!("<{}>", fields.join(","));

        // Group waypoints sharing (position, duration), in first-seen order.
        let mut order: Vec<(i64, i64)> = Vec::new();
        let mut groups: HashMap<(i64, i64), Vec<(&'static str, f64)>> = HashMap::new();

        for &(pt, prop, val, d) in &v.points {
            let key = (key3(pt), key3(d));
            groups
                .entry(key)
                .or_insert_with(|| {
                    order.push(key);
                    Vec::new()
                })
                .push((prop, val));
        }

        order.sort_by_key(|k| k.0); // stable

        let mut transitions = String::new();

        for key in order {
            let pt = key.0 as f64 / 1000.0;
            let d = key.1 as f64 / 1000.0;

            // Last value per property wins (Python's dict(props_list)).
            let (mut pitch, mut vol, mut pan) = (None, None, None);
            for &(prop, val) in &groups[&key] {
                match prop {
                    "pitch" => pitch = Some(val),
                    "vol" => vol = Some(val),
                    "pan" => pan = Some(val),
                    _ => {}
                }
            }

            // Like note duration, an attached waypoint may extend beyond this
            // measure's pattern. Its position remains relative to the
            // pattern's placement, so each instance carries its own later
            // automation.
            if pt < local_start {
                continue;
            }

            let mut fields = vec![pos_str(pt - local_start)];

            if let Some(p) = pitch {
                fields.push(format!("#{}", pitch_text(p, mode)));
            }
            if let Some(vv) = vol {
                fields.push(format!("%{}", fmt(vv, 1)));
            }
            if let Some(pp) = pan {
                fields.push(format!("¶{}", pan_str(pp)));
            }
            if d > 1e-9 {
                // Waypoint duration is in BEATS.
                fields.push(format!("~{}", fmt(d / RPB, 2)));
            }

            transitions.push_str(&format!("&<{}>", fields.join(",")));
        }

        Some(ev + &transitions)
    }

    /// Render a part independently of its original channel.
    ///
    /// Events sharing a start position are placed on one Trackscript line.
    fn render_part(
        &mut self,
        voices: &[usize],
        start: f64,
        sample_files: &HashMap<u32, String>,
    ) -> String {
        let mut events: Vec<(i64, String)> = Vec::new();
        let mut missing = 0;

        for &vi in voices {
            let v = &self.voices[vi];
            match self.render_voice_event(v, start, sample_files) {
                Some(ev) => {
                    let rel = key3(v.start - start);
                    events.push((rel, ev));
                }
                None => missing += 1,
            }
        }
        for _ in 0..missing {
            self.note_warn("voice references missing sample output");
        }

        events.sort_by_key(|e| e.0); // stable

        let all: Vec<String> = events.into_iter().map(|e| e.1).collect();
        render_event_groups(&all)
    }

    /// Build reusable one-measure Parts and their arrangement placements.
    ///
    /// Every measure is decomposed into sample-based musical layers. Tracker
    /// channels are ignored; identical layers are deduplicated globally, while
    /// each measure placement retains its calls.
    fn extract_structure(
        &mut self,
        sample_files: &HashMap<u32, String>,
    ) -> (Vec<PartDefinition>, Vec<Placement>) {
        let spans = build_safe_spans(self.total_rows);

        // Global rank per sample: the earliest row any voice using that sample
        // starts anywhere in the song.  Used to give every part a stable
        // column position across measures, instead of the position jumping
        // around based on which part happens to start first in a given span.
        let mut first_start: HashMap<u32, f64> = HashMap::new();
        for v in &self.voices {
            first_start
            .entry(v.sample)
            .and_modify(|e| *e = e.min(v.start))
            .or_insert(v.start);
        }

        let mut part_by_signature: HashMap<(Vec<u32>, String, i64), usize> = HashMap::new();
        let mut parts: Vec<PartDefinition> = Vec::new();
        let mut placements: Vec<Placement> = Vec::new();

        for (start, end) in spans {
            let groups = self.group_span_voices(start, end);

            let mut ordered: Vec<(u32, Vec<usize>)> = groups.into_iter().collect();
            let min_start = |vs: &Vec<usize>| {
                vs.iter()
                    .map(|&i| self.voices[i].start)
                    .fold(f64::INFINITY, f64::min)
            };
            ordered.sort_by(|a, b| {
                let ra = first_start.get(&a.0).copied().unwrap_or(f64::INFINITY);
                let rb = first_start.get(&b.0).copied().unwrap_or(f64::INFINITY);
                ra.partial_cmp(&rb)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
            });

            let mut span_parts: Vec<usize> = Vec::new();

            for (sample_index, voices) in ordered {
                // Signature: generated representation + relative extent.
                // Channel number is deliberately excluded.
                let rendered = self.render_part(&voices, start, sample_files);
                let last_end = voices
                    .iter()
                    .map(|&i| self.voices[i].end.unwrap_or(0.0) - start)
                    .fold(0.0f64, f64::max);
                let sig = (
                    vec![sample_index],
                    rendered,
                    key3(last_end),
                );

                let idx = if let Some(&idx) = part_by_signature.get(&sig) {
                    idx
                } else {
                    // (Python renders a second time here; keep that so the
                    // warning counts match.)
                    let rendered = self.render_part(&voices, start, sample_files);
                    parts.push(PartDefinition {
                        voices,
                        start,
                        end,
                        sample_index,
                        name: String::new(),
                        rendered,
                    });
                    part_by_signature.insert(sig, parts.len() - 1);
                    parts.len() - 1
                };

                span_parts.push(idx);
            }

            // Keep repeated references when identical musical material appears
            // on multiple layers: each call is a separate sounding instance.
            placements.push((span_parts, start, end));
        }

        let (parts, placements) = factor_repeated_event_pairs(parts, placements);
        merge_same_line_parts(parts, placements)
    }
}

/// Split a rendered measure into top-level event/connector groups.
fn split_rendered_events(rendered: &str) -> Vec<String> {
    let (Some(open), Some(close)) = (rendered.find('['), rendered.rfind(']')) else {
        return Vec::new();
    };
    if close <= open {
        return Vec::new();
    }
    let body = rendered[open + 1..close].trim();
    if body.is_empty() {
        return Vec::new();
    }
    let mut groups = Vec::new();
    let mut start = 0;
    let mut depth = 0i32;
    for (i, ch) in body.char_indices() {
        match ch {
            '<' => depth += 1,
            '>' => depth -= 1,
            ',' if depth == 0 => {
                groups.push(body[start..i].trim().to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    groups.push(body[start..].trim().to_string());
    groups.into_iter().filter(|g| !g.is_empty()).collect()
}

fn render_event_groups(groups: &[String]) -> String {
    let mut lines = vec!["@4/4[".to_string()];
    if !groups.is_empty() {
        lines.push(format!("    {}", groups.join(", ")));
    }
    lines.push("]".to_string());
    lines.join("\n")
}

/// Merge parts that share a sample_index AND appear on exactly the same
/// set of BlockList lines.
///
/// Such parts are musically one part: no line ever plays one without the
/// other.  Concatenating their events is audio-identical (same voices,
/// same schedule) and collapses two columns into one.
///
/// Parts that share a sample but appear on *different* lines (HARP05
/// paired with HARP06 on one line, HARP07 on another, etc.) are left
/// alone — merging them would make each line play the others' events.
fn merge_same_line_parts(
    parts: Vec<PartDefinition>,
    placements: Vec<Placement>,
) -> (Vec<PartDefinition>, Vec<Placement>) {
    use std::collections::{HashMap, HashSet};

    // Which placement indices does each part appear on?
    let mut lines_for: HashMap<usize, HashSet<usize>> = HashMap::new();
    for (li, (part_indices, _, _)) in placements.iter().enumerate() {
        for &pi in part_indices {
            lines_for.entry(pi).or_default().insert(li);
        }
    }

    // Cluster by (sample_index, sorted line-set).
    let mut clusters: HashMap<(u32, Vec<usize>), Vec<usize>> = HashMap::new();
    for (pi, part) in parts.iter().enumerate() {
        let mut lines: Vec<usize> = lines_for
        .get(&pi)
        .map(|s| s.iter().copied().collect())
        .unwrap_or_default();
        lines.sort();
        clusters
        .entry((part.sample_index, lines))
        .or_default()
        .push(pi);
    }

    let mut renames: HashMap<usize, usize> = HashMap::new();
    let mut merged_rendered: HashMap<usize, String> = HashMap::new();

    for ((_sample, lines), members) in clusters {
        if members.len() <= 1 || lines.is_empty() {
            continue;
        }

        let canonical = members[0];

        // Collect all event groups from every member, sorted by their
        // position within the measure.
        let mut groups: Vec<(f64, String)> = Vec::new();
        for &pi in &members {
            for g in split_rendered_events(&parts[pi].rendered) {
                groups.push((event_position(&g), g));
            }
        }
        groups.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        let ordered: Vec<String> = groups.into_iter().map(|(_, g)| g).collect();
        merged_rendered.insert(canonical, render_event_groups(&ordered));

        for &pi in &members[1..] {
            renames.insert(pi, canonical);
        }
    }

    if renames.is_empty() {
        return (parts, placements);
    }

    // Rewrite placements, and dedupe in case a line referenced two members.
    let new_placements: Vec<Placement> = placements
    .into_iter()
    .map(|(pis, s, e)| {
        let mut seen = HashSet::new();
        let new_pis: Vec<usize> = pis
        .into_iter()
        .map(|pi| *renames.get(&pi).unwrap_or(&pi))
        .filter(|pi| seen.insert(*pi))
        .collect();
        (new_pis, s, e)
    })
    .collect();

    // Drop merged-away parts and remap indices.
    let mut keep: Vec<bool> = vec![true; parts.len()];
    for pi in renames.keys() {
        keep[*pi] = false;
    }

    let mut remap: Vec<Option<usize>> = vec![None; parts.len()];
    let mut new_parts: Vec<PartDefinition> = Vec::with_capacity(parts.len());
    for (pi, mut part) in parts.into_iter().enumerate() {
        if !keep[pi] {
            continue;
        }
        if let Some(r) = merged_rendered.get(&pi) {
            part.rendered = r.clone();
        }
        remap[pi] = Some(new_parts.len());
        new_parts.push(part);
    }

    let final_placements: Vec<Placement> = new_placements
    .into_iter()
    .map(|(pis, s, e)| {
        let remapped: Vec<usize> = pis.into_iter().filter_map(|pi| remap[pi]).collect();
        (remapped, s, e)
    })
    .collect();

    (new_parts, final_placements)
}

/// Position key for an event group: (beat, sub) parsed from the leading
/// `§N/M`.  Groups without a `§` sort last.
fn event_position(group: &str) -> f64 {
    let bytes = group.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        // § is UTF-8 C2 A7.
        if bytes[i] == 0xC2 && bytes[i + 1] == 0xA7 {
            let rest = &group[i + 2..];
            let end = rest
            .find(|c: char| !c.is_ascii_digit() && c != '.' && c != '/')
            .unwrap_or(rest.len());
            let pos_str = &rest[..end];
            if let Some((a, b)) = pos_str.split_once('/') {
                if let (Ok(beat), Ok(sub)) = (a.parse::<f64>(), b.parse::<f64>()) {
                    return beat * 1000.0 + sub;
                }
            }
            return f64::INFINITY;
        }
        i += 1;
    }
    f64::INFINITY
}

/// Reuse repeated adjacent event pairs as direct pattern components.
fn factor_repeated_event_pairs(
    parts: Vec<PartDefinition>,
    placements: Vec<Placement>,
) -> (Vec<PartDefinition>, Vec<Placement>) {
    type PairKey = (u32, String, String);

    let event_groups: Vec<Vec<String>> =
        parts.iter().map(|p| split_rendered_events(&p.rendered)).collect();

    let mut pair_owners: HashMap<PairKey, HashSet<usize>> = HashMap::new();
    for (pi, groups) in event_groups.iter().enumerate() {
        for i in 0..groups.len().saturating_sub(1) {
            pair_owners
                .entry((parts[pi].sample_index, groups[i].clone(), groups[i + 1].clone()))
                .or_default()
                .insert(pi);
        }
    }

    let reusable: HashSet<PairKey> = pair_owners
        .into_iter()
        .filter(|(_, owners)| owners.len() >= 2)
        .map(|(k, _)| k)
        .collect();
    if reusable.is_empty() {
        return (parts, placements);
    }

    let mut new_parts: Vec<PartDefinition> = Vec::new();
    let mut replacement: Vec<Vec<usize>> = Vec::with_capacity(parts.len());
    let mut pair_parts: HashMap<PairKey, usize> = HashMap::new();

    for (pi, part) in parts.into_iter().enumerate() {
        let groups = &event_groups[pi];
        let sample_index = part.sample_index;

        // Prefer the earliest repeated pair. Each original part is factored
        // once, keeping the transformation deterministic and easy to inspect.
        let candidate = (0..groups.len().saturating_sub(1)).find(|&i| {
            reusable.contains(&(sample_index, groups[i].clone(), groups[i + 1].clone()))
        });

        let Some(pair_index) = candidate else {
            new_parts.push(part);
            replacement.push(vec![new_parts.len() - 1]);
            continue;
        };

        let key: PairKey = (
            sample_index,
            groups[pair_index].clone(),
            groups[pair_index + 1].clone(),
        );
        let pair_idx = match pair_parts.get(&key) {
            Some(&i) => i,
            None => {
                new_parts.push(PartDefinition {
                    voices: Vec::new(),
                    start: part.start,
                    end: part.end,
                    sample_index,
                    name: String::new(),
                    rendered: render_event_groups(&[key.1.clone(), key.2.clone()]),
                });
                pair_parts.insert(key, new_parts.len() - 1);
                new_parts.len() - 1
            }
        };

        let mut remainder: Vec<String> = groups[..pair_index].to_vec();
        remainder.extend_from_slice(&groups[pair_index + 2..]);

        let mut pieces = vec![pair_idx];
        if !remainder.is_empty() {
            new_parts.push(PartDefinition {
                voices: part.voices,
                start: part.start,
                end: part.end,
                sample_index,
                name: String::new(),
                rendered: render_event_groups(&remainder),
            });
            pieces.push(new_parts.len() - 1);
        }
        replacement.push(pieces);
    }

    let expanded = placements
        .into_iter()
        .map(|(ps, s, e)| {
            (
                ps.into_iter().flat_map(|p| replacement[p].clone()).collect(),
                s,
                e,
            )
        })
        .collect();

    (new_parts, expanded)
}

// ── naming ─────────────────────────────────────────────────────────────

fn sample_base_name(module: &Module, sample_index: u32) -> String {
    let s = &module.samples[sample_index as usize - 1];

    if !s.name.is_empty() {
        return identifier(&s.name);
    }

    format!("Sample{:02}", sample_index)
}

/// Name parts from their source instrument/sample when possible.
///
/// The names describe the source material without claiming that the
/// sample's name necessarily tells us its exact musical role.
fn assign_part_names(parts: &mut [PartDefinition], module: &Module) {
    let mut used: HashMap<String, u32> = HashMap::new();

    for part in parts.iter_mut() {
        let base = sample_base_name(module, part.sample_index);

        let n = used.entry(base.clone()).or_insert(0);
        *n += 1;

        part.name = if *n == 1 {
            base
        } else {
            format!("{}{:02}", base, *n)
        };
    }

    // A pathological case can still collide after sanitization.
    let mut seen: HashSet<String> = HashSet::new();

    for part in parts.iter_mut() {
        let original = part.name.clone();
        let mut candidate = original.clone();
        let mut n = 2;

        while seen.contains(&candidate) {
            candidate = format!("{}{:02}", original, n);
            n += 1;
        }

        part.name = candidate.clone();
        seen.insert(candidate);
    }
}

// ── Trackscript emitter ────────────────────────────────────────────────

fn emit(
    conv: &mut Converter,
    name: &str,
    sample_files: &HashMap<u32, String>,
    source_name: &str,
    with_message: bool,
) -> String {
    let m = conv.m;

    let (mut parts, placements) = conv.extract_structure(sample_files);
    assign_part_names(&mut parts, m);

    let mut out: Vec<String> = Vec::new();

    let runs = build_sections(conv);
    let first_bpm = runs.first().map_or(120.0, |r| r.2);

    out.push("Config".into());
    out.push("{".into());
    out.push("    TrackscriptVersion: 1.10;".into());
    out.push(format!("    BeatsPerMinute: {};", fmt(first_bpm, 2)));
    out.push("    TimeSignature: 4/4;".into());
    out.push(format!("    Subdivisions: {};", ROWS_PER_BEAT));
    out.push(format!("    SongVolume: {}%;", fmt(m.mv as f64 * 100.0 / 512.0, 2)));
    out.push("    PanningLaw: linear;".into());
    out.push("    Overlap: stack;".into());
    out.push("    PitchDefault: hz;".into());
    out.push("}".into());
    out.push(String::new());

    let wps = tempo_lane(conv);

    if !wps.is_empty() {
        out.push("Tempo".into());
        out.push("{".into());

        let measure_count = ((conv.total_rows / MEASURE_ROWS).ceil() as usize).max(1);

        let mut tempo_by_measure: HashMap<usize, Vec<String>> = HashMap::new();

        tempo_by_measure
            .entry(0)
            .or_default()
            .push(format!("<§1/1,#{}>", fmt(first_bpm, 2)));

        for &(row, bpm, d) in &wps {
            let measure = (row / MEASURE_ROWS).floor() as usize;
            let local = row - measure as f64 * MEASURE_ROWS;

            let mut ev = format!("&<{},#{}", pos_str(local), fmt(bpm, 2));
            if d != 0.0 {
                ev.push_str(&format!(",~{}", fmt(d / RPB, 2)));
            }
            ev.push('>');
            tempo_by_measure.entry(measure).or_default().push(ev);
        }

        for measure in 0..measure_count {
            let line = tempo_by_measure
                .get(&measure)
                .map(|v| v.join(" "))
                .unwrap_or_default();
            out.push(format!("    {}", line));
        }

        out.push("}".into());
        out.push(String::new());
    }

    // ------------------------------------------------------------------
    // BlockList
    // ------------------------------------------------------------------

    out.push("BlockList()".into());
    out.push("{".into());

    for (measure_parts, _start, _end) in &placements {
        let calls: Vec<String> = measure_parts
            .iter()
            .map(|&pi| format!("ƒ{};", parts[pi].name))
            .collect();

        if calls.is_empty() {
            out.push("    ;".into());
        } else {
            out.push(format!("    {}", calls.join(" ")));
        }
    }

    out.push("}".into());
    out.push(String::new());

    // ------------------------------------------------------------------
    // BlockArray
    // ------------------------------------------------------------------

    out.push("BlockArray()".into());
    out.push("{".into());

    // Pattern definitions are direct BlockList targets. Each generated
    // pattern contains one measure; repeated material is repeated in the list.
    for part in &parts {
        let file = sample_files
            .get(&part.sample_index)
            .map(String::as_str)
            .unwrap_or("");

        out.push(format!("    {} = ƒ'{}'():", part.name, file));
        out.push("    {".into());

        for line in part.rendered.lines() {
            out.push(format!("        {}", line));
        }

        out.push("    }".into());
        out.push(String::new());
    }

    out.push("}".into());

    // ------------------------------------------------------------------
    // Human-readable metadata
    // Keep commentary after the executable song structure.
    // ------------------------------------------------------------------

    out.push(String::new());
    out.push("// ------------------------------------------------------------".into());
    out.push(format!(
        "// {}",
        if m.title.is_empty() { name } else { &m.title }
    ));
    out.push(format!("// Converted from {} by it2trackscript", source_name));
    out.push("//".into());
    out.push(
        "// The generated structure is based on reusable musical parts rather than IT channels."
            .into(),
    );

    if !wps.is_empty() {
        out.push(String::new());
        out.push(
            "// Tempo: one tempo line per measure; § positions are local to that line.".into(),
        );
    }

    if with_message && !m.message.trim().is_empty() {
        out.push(String::new());
        out.push("// ---- original song message ----".into());

        for line in m.message.split('\n') {
            out.push(format!("// {}", line.trim_end()));
        }

        out.push("// ---- end song message ----".into());
    }

    if !sample_files.is_empty() {
        out.push(String::new());
        out.push("// Samples (WAV files are written at each sample's C5Speed):".into());

        for (idx, s) in m.samples.iter().enumerate() {
            let idx = idx as u32 + 1;
            if let Some(file) = sample_files.get(&idx) {
                let lp = if s.r#loop {
                    format!(", loop {}-{}", s.loop_beg, s.loop_end)
                } else {
                    String::new()
                };

                out.push(format!("//   {:02}  {}  (C5Speed {}{})", idx, file, s.c5, lp));
            }
        }
    }

    if let Some((oi, row, _)) = conv.loop_info {
        out.push(String::new());
        out.push(format!("// Original song loops back to order {:02} row {}", oi, row));
    }

    out.join("\n") + "\n"
}

fn make_report(
    conv: &mut Converter,
    source_name: &str,
    runs: &[(f64, f64, f64)],
    sample_files: &HashMap<u32, String>,
) -> String {
    let m = conv.m;

    let (parts, placements) = conv.extract_structure(sample_files);

    // These are only used for statistics in the report.
    let unique_part_count = parts.len();
    let pattern_placement_count: usize = placements.iter().map(|p| p.0.len()).sum();

    let mut r: Vec<String> = Vec::new();

    r.push(format!("it2trackscript report for {}", source_name));
    r.push(format!("title: {}", m.title));
    r.push(format!("source archive: source/{} (byte-exact)", source_name));
    r.push(format!(
        "orders: {}   patterns: {}   rows played: {}",
        m.orders.len(),
        m.patterns.len(),
        conv.total_rows
    ));
    r.push(format!(
        "instruments: {}   samples: {}",
        m.instruments.len(),
        m.samples.len()
    ));

    let shown: Vec<String> = runs
        .iter()
        .take(8)
        .map(|x| format!("{} BPM", fmt(x.2, 2)))
        .collect();
    r.push(format!(
        "timing sections: {} ({}{})",
        runs.len(),
        shown.join(", "),
        if runs.len() > 8 { " ..." } else { "" }
    ));

    r.push(format!(
        "notes converted: {}   waypoints: {}",
        conv.voices.len(),
        conv.voices.iter().map(|v| v.points.len()).sum::<usize>()
    ));

    r.push(String::new());

    r.push("Trackscript structure:".into());
    r.push(format!("  reusable pattern definitions: {}", unique_part_count));
    r.push(format!("  pattern placements in BlockList: {}", pattern_placement_count));

    if let Some((oi, row, _)) = conv.loop_info {
        r.push(format!("song loops back to order {} row {}", oi, row));
    }

    r.push(String::new());
    r.push("approximations / things not converted:".into());

    if conv.warn.is_empty() {
        r.push("  (none)".into());
    }

    // Counter.most_common(): count descending, ties in first-seen order.
    let mut warns = conv.warn.clone();
    warns.sort_by(|a, b| b.1.cmp(&a.1));
    for (k, n) in &warns {
        let extra = conv
            .warn_detail
            .get(k)
            .map(|d| format!("  -- {}", d))
            .unwrap_or_default();
        r.push(format!("  {:5} x {}{}", n, k, extra));
    }

    let mut envs: Vec<String> = Vec::new();

    for (i, ins) in m.instruments.iter().enumerate() {
        let used: Vec<&str> = ["volume", "pan", "pitch"]
            .into_iter()
            .filter(|n| ins.envelopes.get(*n).map_or(false, |e| e.enabled))
            .collect();

        if !used.is_empty() && ins.keymap.iter().any(|&(_, s)| s != 0) {
            let statuses: Vec<String> = used
                .iter()
                .map(|&name| {
                    let envelope = &ins.envelopes[name];
                    if name == "volume"
                        && !envelope.points.is_empty()
                        && !envelope.loop_enabled
                        && !envelope.sustain_enabled
                    {
                        "volume converted to approximate automation".to_string()
                    } else {
                        format!("{} playback not converted", name)
                    }
                })
                .collect();
            envs.push(format!(
                "  instrument {:02} {}: {}; source data retained in samples.json",
                i + 1,
                py_repr(&ins.name),
                statuses.join("; ")
            ));
        }
    }

    if !envs.is_empty() {
        r.push(String::new());
        r.push(
            "instrument envelope playback status (all source envelope data retained in samples.json):"
                .into(),
        );
        r.extend(envs);
    }

    r.join("\n") + "\n"
}

// ── WAV ────────────────────────────────────────────────────────────────

fn write_wav(s: &crate::sources::it::Sample) -> Vec<u8> {
    let rate: u32 = (if s.c5 == 0 { 8363 } else { s.c5 }).clamp(100, 192_000);

    let (data, bits): (Vec<u8>, u16) = if s.is16 {
        (
            s.pcm
                .iter()
                .flat_map(|&x| (x.clamp(i16::MIN as i32, i16::MAX as i32) as i16).to_le_bytes())
                .collect(),
            16,
        )
    } else {
        (
            s.pcm
                .iter()
                .map(|&x| ((x + 128).clamp(0, 255)) as u8)
                .collect(),
            8,
        )
    };

    let block = bits / 8;

    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(b"WAVE");

    body.extend_from_slice(b"fmt ");
    body.extend_from_slice(&16u32.to_le_bytes());
    body.extend_from_slice(&1u16.to_le_bytes());
    body.extend_from_slice(&1u16.to_le_bytes());
    body.extend_from_slice(&rate.to_le_bytes());
    body.extend_from_slice(&(rate * block as u32).to_le_bytes());
    body.extend_from_slice(&block.to_le_bytes());
    body.extend_from_slice(&bits.to_le_bytes());

    body.extend_from_slice(b"data");
    body.extend_from_slice(&(data.len() as u32).to_le_bytes());
    body.extend_from_slice(&data);
    if data.len() & 1 == 1 {
        body.push(0);
    }

    if s.r#loop && s.loop_end > s.loop_beg {
        let mut smpl: Vec<u8> = Vec::new();
        for v in [
            0u32,
            0,
            (1e9 / rate as f64) as u32,
            60,
            0,
            0,
            0,
            1,
            0,
        ] {
            smpl.extend_from_slice(&v.to_le_bytes());
        }
        for v in [
            0u32,
            if s.pingpong { 1 } else { 0 },
            s.loop_beg as u32,
            (s.loop_end - 1) as u32,
            0,
            0,
        ] {
            smpl.extend_from_slice(&v.to_le_bytes());
        }

        body.extend_from_slice(b"smpl");
        body.extend_from_slice(&(smpl.len() as u32).to_le_bytes());
        body.extend_from_slice(&smpl);
    }

    let mut file: Vec<u8> = Vec::with_capacity(body.len() + 8);
    file.extend_from_slice(b"RIFF");
    file.extend_from_slice(&(body.len() as u32).to_le_bytes());
    file.extend_from_slice(&body);

    file
}

// ── JSON side files ────────────────────────────────────────────────────

fn decoded_row_events(module: &Module) -> Vec<Value> {
    let mut out = Vec::new();
    for (pat_i, grid) in module.patterns.iter().enumerate() {
        for (row_i, cells) in grid.iter().enumerate() {
            let mut events = Vec::new();
            for (ch, raw) in cells.iter().enumerate() {
                let Some(c) = read_cell(raw) else { continue };
                if c.note.is_none() && c.inst.is_none() && c.volc.is_none() && c.cmd == 0 {
                    continue;
                }
                let effect = if (1..=26).contains(&c.cmd) {
                    Some(((64 + c.cmd) as char).to_string())
                } else {
                    None
                };
                events.push(json!({
                    "channel": ch,
                    "note_raw": c.note,
                    "note": note_name_from_it(c.note),
                    "instrument": c.inst,
                    "volume_column": c.volc,
                    "effect": effect,
                    "effect_raw": c.cmd,
                    "parameter": c.par,
                }));
            }
            if !events.is_empty() {
                out.push(json!({ "pattern": pat_i, "row": row_i, "events": events }));
            }
        }
    }
    out
}

fn voice_semantics(conv: &Converter) -> Vec<Value> {
    conv.voices
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let pitch_hz = v.init.get("pitch").copied();
            let pitch = pitch_hz
                .filter(|&h| h > 0.0)
                .map(|h| round_even(60.0 + 12.0 * (h / C5_HZ).log2()) as i64);
            json!({
                "voice": i,
                "channel": v.ch,
                "start_row": v.start,
                "end_row": v.end,
                "end_reason": if v.natural_rows.is_some() && v.end.is_some() { "natural_end" } else { "channel_end" },
                "sample": v.sample,
                "note": note_name_from_it(Some(v.note)),
                "pitch": pitch,
                "pitch_hz": pitch_hz,
                "volume": v.init.get("vol"),
                "pan": v.init.get("pan"),
                "offset": v.offset,
                "fade": v.fade.map(|(a, b)| vec![a, b]),
                "waypoints": v.points.iter().map(|&(pt, prop, val, dur)| json!({
                    "row": pt, "property": prop, "value": val, "duration": dur,
                })).collect::<Vec<_>>(),
            })
        })
        .collect()
}

fn channel_semantics(conv: &Converter) -> Vec<Value> {
    conv.ch
        .iter()
        .enumerate()
        .map(|(i, st)| {
            json!({
                "channel": i,
                "instrument": st.inst,
                "volume": st.vol,
                "channel_volume": st.chanvol,
                "pan": st.pan,
                "pitch": st.pitch,
                "global_scale": st.gscale,
                "sample_offset": st.mem_o,
                "vibrato_speed": st.vib_spd,
                "vibrato_depth": st.vib_dep,
                "active_voice": st.voice,
            })
        })
        .collect()
}

fn build_semantics_json(
    module: &Module,
    conv: &Converter,
    source_name: &str,
) -> Result<String> {
    let tempo_events: Vec<Value> = conv
        .rowlog
        .iter()
        .filter(|r| r.1 == 0 && r.2 == 0 && r.3 == 0)
        .map(|r| json!({ "row": r.0, "bpm": r.4 }))
        .collect();

    let data = json!({
        "format": "trackscript-semantics",
        "version": 1,
        "source": source_name,
        "converter": "it2trackscript",
        "source_archive": format!("source/{}", source_name),
        "module": {
            "title": module.title,
            "initial_speed": module.speed,
            "initial_tempo": module.tempo,
            "global_volume": module.gv,
            "mix_volume": module.mv,
            "stereo_separation": module.sep,
            "pitch_wheel_depth": module.pwd,
            "flags": module.flags,
            "special_flags": module.special,
            "linear_pitch": module.linear,
            "old_effects": module.old_effects,
            "orders_raw": module.orders,
            "channel_pan_raw": module.chnpan,
            "channel_volume_raw": module.chnvol,
            "message": module.message,
            "orders": module.orders.len(),
            "patterns": module.patterns.len(),
            "instruments": module.instruments.len(),
            "samples": module.samples.len(),
        },
        "it_decode": {
            "pattern_count": module.patterns.len(),
            "instrument_count": module.instruments.len(),
            "sample_count": module.samples.len(),
            "rows_with_events": decoded_row_events(module),
        },
        "simulation": {
            "total_rows": conv.total_rows,
            "rows_per_beat": ROWS_PER_BEAT,
            "rows_per_measure": ROWS_PER_BEAT * 4,
            "initial_bpm": conv.bpm(),
            "final_speed": conv.speed,
            "final_tempo": conv.tempo,
            "final_global_volume": conv.gvol,
            "tempo_events": tempo_events,
            "channels": channel_semantics(conv),
            "executed_rows": conv.executed_rows,
            "voices": voice_semantics(conv),
        },
        "effects": {
            "seen": {},
            "represented": {},
            "approximated": {},
            "dropped": {},
        },
        "warnings": conv.warn.iter().cloned().collect::<BTreeMap<String, u32>>(),
    });

    Ok(serde_json::to_string_pretty(&data)?)
}

/// samples.json for the Trackscript player.
///
/// The player reads c5Speed, loop points, and default volume from this file.
/// Source IT instrument/sample fields are retained for fidelity and future
/// player support, including currently unsupported envelope modes.
fn build_samples_json(
    module: &Module,
    sample_files: &HashMap<u32, String>,
) -> Result<String> {
    let mut samples: Vec<Value> = Vec::new();

    for (i, s) in module.samples.iter().enumerate() {
        let idx = i as u32 + 1;
        let mut entry = serde_json::Map::new();
        entry.insert("id".into(), json!(idx));
        entry.insert("name".into(), json!(s.name));
        entry.insert("length".into(), json!(s.length));
        if let Some(f) = sample_files.get(&idx) {
            entry.insert("file".into(), json!(f));
        }
        entry.insert("c5Speed".into(), json!(if s.c5 == 0 { 8363 } else { s.c5 }));

        let mut loops = serde_json::Map::new();
        if s.r#loop && s.loop_end > s.loop_beg {
            loops.insert(
                "main".into(),
                json!({ "start": s.loop_beg, "end": s.loop_end, "pingpong": s.pingpong }),
            );
        }
        if s.sus_loop && s.sus_end > s.sus_beg {
            loops.insert(
                "sustain".into(),
                json!({ "start": s.sus_beg, "end": s.sus_end, "pingpong": false }),
            );
        }
        if !loops.is_empty() {
            entry.insert("loops".into(), Value::Object(loops));
        }

        entry.insert(
            "volume".into(),
            json!({
                "default": s.vol as u32 * 4,
                "global": s.gvl,
                "panning": (s.dfp & 0x7F) as u32 * 4,
            }),
        );
        entry.insert(
            "source_it".into(),
            json!({
                "global_volume": s.gvl,
                "default_volume": s.vol,
                "flags": s.flg,
                "conversion_flags": s.cvt,
                "default_pan": s.dfp,
                "length_frames": s.length,
                "loop_start": s.loop_beg,
                "loop_end": s.loop_end,
                "c5_speed": s.c5,
                "sustain_loop_start": s.sus_beg,
                "sustain_loop_end": s.sus_end,
                "has_data": s.has_data,
                "is_16_bit": s.is16,
                "stereo": s.stereo,
                "compressed": s.compressed,
                "loop": s.r#loop,
                "pingpong_loop": s.pingpong,
                "sustain_loop": s.sus_loop,
                "pingpong_sustain": s.pingpong_sus,
            }),
        );
        samples.push(Value::Object(entry));
    }

    let mut instruments: Vec<Value> = Vec::new();

    for (i, ins) in module.instruments.iter().enumerate() {
        let mut envelopes = serde_json::Map::new();
        for name in ["volume", "pan", "pitch"] {
            if let Some(env) = ins.envelopes.get(name) {
                envelopes.insert(
                    name.into(),
                    json!({
                        "enabled": env.enabled,
                        "loop_enabled": env.loop_enabled,
                        "sustain_enabled": env.sustain_enabled,
                        "loop_start_index": env.loop_start,
                        "loop_end_index": env.loop_end,
                        "sustain_start_index": env.sustain_start,
                        "sustain_end_index": env.sustain_end,
                        "points": env.points.iter()
                            .map(|&(tick, value)| json!({ "tick": tick, "value": value }))
                            .collect::<Vec<_>>(),
                    }),
                );
            }
        }
        instruments.push(json!({
            "id": i + 1,
            "name": ins.name,
            "new_note_action": ins.nna,
            "duplicate_check_type": ins.dct,
            "duplicate_check_action": ins.dca,
            "fadeout": ins.fadeout,
            "global_volume": ins.gbv,
            "default_pan": ins.dfp,
            "keymap": ins.keymap.iter()
                .map(|&(note, sample)| json!({ "note": note, "sample": sample }))
                .collect::<Vec<_>>(),
            "envelopes": envelopes,
        }));
    }

    let out = json!({
        "format": "trackscript-sample-library",
        "version": 2,
        "samples": samples,
        // Keep IT instrument behavior beside the extracted samples even when
        // the current Trackscript player cannot execute every feature yet.
        "instruments": instruments,
    });

    Ok(serde_json::to_string_pretty(&out)?)
}

// ── entry point ────────────────────────────────────────────────────────

pub(super) fn convert(
    module: &Module,
    src_path: &Path,
    opts: &ConvertOptions,
) -> Result<Converted> {
    let base = src_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "song".into());
    let source_name = src_path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| base.clone());

    // Samples: decode PCM into WAV bytes, in memory.
    let mut sample_files: HashMap<u32, String> = HashMap::new();
    let mut sample_outs: Vec<SampleOut> = Vec::new();

    for (i, s) in module.samples.iter().enumerate() {
        let idx = i as u32 + 1;
        if !s.pcm.is_empty() {
            let fname = format!(
                "{:02}_{}.wav",
                idx,
                safe_name(if s.name.is_empty() { "sample" } else { &s.name })
            );
            let rel = format!("samples/{}", fname);
            sample_files.insert(idx, rel.clone());
            sample_outs.push(SampleOut {
                rel_path: rel,
                wav: write_wav(s),
            });
        }
    }

    let samples_json = build_samples_json(module, &sample_files)?;

    let mut conv = Converter::new(module, opts.pitch_mode);
    conv.run();

    let trackscript = emit(
        &mut conv,
        &base,
        &sample_files,
        &source_name,
        opts.with_message,
    );
    let runs = build_sections(&conv);
    let report = make_report(&mut conv, &source_name, &runs, &sample_files);
    let semantics = build_semantics_json(module, &conv, &source_name)?;

    // Byte-exact copy of the source, kept for unsupported/reserved data.
    let source_bytes = fs::read(src_path)
        .with_context(|| format!("reading {}", src_path.display()))?;
    let aux = vec![AuxFile {
        rel_path: format!("source/{}", source_name),
        bytes: source_bytes,
    }];

    Ok(Converted {
        trackscript,
        samples: sample_outs,
        samples_json,
        aux,
        report,
        semantics_json: Some(semantics),
    })
}
