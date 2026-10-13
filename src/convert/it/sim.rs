// src/convert/it/sim.rs
//! IT playback simulation: Voice, ChannelState, Converter, and the
//! row-by-row state machine.  Everything that reads/writes module state
//! lives here.  The rendering submodule only reads the resulting voices.

use std::collections::{HashMap, HashSet};

use crate::sources::it::{
    note_termination, Instrument, ITEnvelope, Module, NoteTermination, PatternEvent,
};

use super::helpers::*;

// ── pattern cells ──────────────────────────────────────────────────────

#[derive(Clone, Copy)]
pub(super) struct Cell {
    pub(super) note: Option<u8>,
    pub(super) inst: Option<u8>,
    pub(super) volc: Option<u8>,
    pub(super) cmd: u8,
    pub(super) par: u8,
}

/// The only place that knows the shape of the parser's pattern cell.
pub(super) fn read_cell(c: &Option<PatternEvent>) -> Option<Cell> {
    let p = c.as_ref()?;
    Some(Cell {
        note: p.note,
        inst: p.instrument,
        volc: p.volume,
        cmd: p.command.unwrap_or(0),
        par: p.parameter,
    })
}

pub(super) fn term_kind(note: Option<u8>) -> Option<&'static str> {
    match note.and_then(|n| note_termination(n, false)) {
        Some(NoteTermination::Cut) => Some("cut"),
        Some(NoteTermination::KeyOff) => Some("off"),
        _ => None,
    }
}

pub(super) fn note_name_from_it(note: Option<u8>) -> Option<String> {
    match note? {
        253 => Some("FADE".into()),
        254 => Some("CUT".into()),
        255 => Some("OFF".into()),
        n if n < 120 => Some(note_name(n as i64)),
        _ => None,
    }
}

// ── Voice ──────────────────────────────────────────────────────────────

pub(super) struct Voice {
    pub(super) ch: u8,
    pub(super) start: f64,
    pub(super) end: Option<f64>,

    pub(super) sample: u32, // 1-based
    pub(super) note: u8,
    #[allow(dead_code)]
    pub(super) bpm: f64,
    pub(super) instrument: usize,
    pub(super) speed: u8,
    pub(super) envelope: Option<ITEnvelope>,
    pub(super) key_off_row: Option<f64>,
    pub(super) fade: Option<(f64, f64)>,
    pub(super) offset: u32,

    pub(super) init: HashMap<&'static str, f64>,
    pub(super) last: HashMap<&'static str, f64>,

    /// (row, property, target_value, duration_rows)
    pub(super) points: Vec<(f64, &'static str, f64, f64)>,

    pub(super) natural_rows: Option<f64>,
}

impl Voice {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
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

pub(super) struct ChannelState {
    pub(super) inst: u8,
    pub(super) vol: u8,
    pub(super) chanvol: u8,
    /// 0..64, IT convention (32 = center).
    pub(super) pan: f64,
    pub(super) pitch: f64,
    pub(super) target: Option<f64>,
    pub(super) voice: Option<usize>,
    pub(super) gscale: f64,
    pub(super) mem_d: u8,
    pub(super) mem_ef: u8,
    pub(super) mem_g: u8,
    pub(super) mem_o: u8,
    pub(super) vib_spd: u8,
    pub(super) vib_dep: f64,
    pub(super) vib_pos: f64,
    pub(super) vib_on: bool,
}

impl ChannelState {
    pub(super) fn new(pan: u8, vol: u8) -> Self {
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

/// A fake Instrument used when the module has `use_instruments == false`.
/// gbv=128 and dfp=0 match the Python's dummy.
pub(super) fn fake_instrument() -> Instrument {
    let mut i = Instrument::default();
    i.gbv = 128;
    i.dfp = 0;
    i
}

// ── Converter ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize)]
pub(super) struct ExecutedRow {
    pub(super) absolute_row: usize,
    pub(super) order: usize,
    pub(super) pattern: usize,
    pub(super) pattern_row: usize,
}

pub(super) struct Converter<'a> {
    pub(super) m: &'a Module,
    pub(super) pitch_mode: crate::convert::PitchMode,
    pub(super) fake_ins: Instrument,

    pub(super) warn: Vec<(String, u32)>,
    #[allow(dead_code)]
    pub(super) warn_detail: HashMap<String, String>,

    pub(super) voices: Vec<Voice>,

    pub(super) rowlog: Vec<(f64, usize, usize, usize, f64)>,
    pub(super) tempo_ramps: Vec<(f64, Vec<f64>)>,
    pub(super) executed_rows: Vec<ExecutedRow>,

    #[allow(dead_code)]
    pub(super) comments: Vec<(f64, String)>,
    pub(super) loop_info: Option<(usize, usize, f64)>,

    pub(super) ch: Vec<ChannelState>,

    pub(super) gvol: f64,
    pub(super) speed: u8,
    pub(super) tempo: u8,
    pub(super) mem_w: u8,

    pub(super) total_rows: f64,
    #[allow(dead_code)]
    pub(super) gvol_row: f64,
}

impl<'a> Converter<'a> {
    pub(super) fn new(m: &'a Module, pitch_mode: crate::convert::PitchMode) -> Self {
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

    pub(super) fn note_warn(&mut self, key: &str) {
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

    pub(super) fn bpm(&self) -> f64 {
        60.0 * self.tempo as f64 / (2.5 * self.speed as f64 * RPB)
    }

    // ------------------------------------------------------------------
    // note mapping
    // ------------------------------------------------------------------

    pub(super) fn ins(&self, idx: usize) -> &Instrument {
        if idx == 0 {
            &self.fake_ins
        } else {
            &self.m.instruments[idx - 1]
        }
    }

    pub(super) fn map_note(&self, inst: u8, note: u8) -> Option<(usize, u32, u8)> {
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

    pub(super) fn eff_vol(&self, st: &ChannelState, vol: u8, gvol: f64) -> f64 {
        100.0 * (vol as f64 / 64.0) * (st.chanvol as f64 / 64.0) * (gvol / 128.0) * st.gscale
    }

    // ------------------------------------------------------------------
    // waypoint bookkeeping
    // ------------------------------------------------------------------

    pub(super) fn point(&mut self, vi: usize, t: f64, prop: &'static str, value: f64, dur: f64) {
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

    pub(super) fn value_at(voice: &Voice, prop: &str, t: f64) -> f64 {
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

    pub(super) fn end_voice(&mut self, ch: usize, t: f64) {
        if let Some(idx) = self.ch[ch].voice.take() {
            let v = &mut self.voices[idx];
            v.end = Some(match v.end {
                Some(e) => e.min(t),
                None => t,
            });
        }
    }

    // ------------------------------------------------------------------
    // main walk
    // ------------------------------------------------------------------

    pub(super) fn run(&mut self) {
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
            let mut tick_bpms = vec![self.bpm()];
            for _tick in 1..speed {
                self.tempo = (self.tempo as i32 + tslide).clamp(32, 255) as u8;
                tick_bpms.push(self.bpm());
            }
            self.tempo_ramps.push((t, tick_bpms));
        }

        (jump_o, brk)
    }

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
                    looped_env = e.map_or(false, |e| {
                        e.enabled && (e.loop_enabled || e.sustain_enabled)
                    });
                    fadeout = self.ins(v.instrument).fadeout as f64;
                    vspeed = v.speed.max(1) as f64;
                    fade_branch = v.fade.is_none() && fadeout > 0.0 && !enabled;
                }

                if non_looping_env {
                    if let Some(vi) = vidx {
                        self.voices[vi].key_off_row = Some(t);
                    }
                } else if fade_branch {
                    let fade_rows = 1024.0 / (fadeout * vspeed);
                    if let Some(vi) = vidx {
                        self.voices[vi].fade = Some((t, fade_rows));
                        self.voices[vi].end = Some(t + fade_rows);
                    }
                } else if looped_env {
                    self.end_voice(ch, t);
                    self.note_warn("looped/sustained volume envelope key-off approximated as cut");
                } else if let Some(vi) = vidx {
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

        if let Some((ins_i, sidx, mnote)) = trigger {
            let smp = &m.samples[sidx as usize - 1];

            let new_start = t + delay;
            if let Some(ov) = self.ch[ch].voice {
                let nna = self.ins(self.voices[ov].instrument).nna;
                if nna == 1 {
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
                self.ch[ch].chanvol = par.min(64);
                let new_chanvol = self.ch[ch].chanvol;
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

        let mut vidx = self.ch[ch].voice;

        if let Some((ins_i, sidx, mnote)) = trigger {
            let smp = &m.samples[sidx as usize - 1];
            let envelope = self.ins(ins_i).envelopes.get("volume").cloned();

            let mut v = Voice::new(
                ch as u8,
                t + delay,
                sidx,
                mnote,
                bpm,
                ins_i,
                speed,
                envelope,
            );

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

        let ramp_dur = (speed as f64 - 1.0) / speed as f64;
        let ticks = speed as i32 - 1;

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

        let base = self.ch[ch].pitch;
        let mut base_end = base;

        if let Some(("ramp", amount)) = efs {
            let sample = &m.samples[self.voices[vi].sample as usize - 1];
            if !m.linear {
                self.note_warn("non-linear IT period slide approximated by pitch ramp");
            }
            base_end =
                slide_pitch_period(base, amount * 64.0 * ticks as f64, sample.c5 as f64, m.linear);
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

            let mut p = base;
            for k in 1..=ticks {
                let mut nxt =
                    slide_pitch_period(p, direction * gp as f64 * 4.0, sample.c5 as f64, m.linear);
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

        if let Some(ct) = cut_tick {
            self.end_voice(ch, t + ct as f64 / speed as f64);
        }
    }

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

    fn finish_voices(&mut self) {
        for vi in 0..self.voices.len() {
            let total = self.total_rows;
            let mut end = self.voices[vi].end.unwrap_or(total);

            if let Some(nat) = self.voices[vi].natural_rows {
                end = end.min(self.voices[vi].start + nat.max(0.05));
            }

            let mut new_end = self.apply_volume_envelope(vi, end);

            if new_end > total {
                new_end = total;
            }
            self.voices[vi].end = Some(new_end);

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
            if voice.key_off_row.is_some() || (voice.end.is_none() && voice.natural_rows.is_none()) {
                end = end.max(envelope_end);
            } else {
                end = end.min(envelope_end);
            }
        }

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
                            || ((pts[j + 1].2 - pts[j].2) - (pts[i + 1].2 - pts[i].2)).abs()
                                < 1e-6)
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

        out.sort_by(|a, b| {
            a.0.partial_cmp(&b.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.1.cmp(b.1))
        });

        v.points = out;
    }
}