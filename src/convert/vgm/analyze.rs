//! VGM command stream → per-channel note lists.
//!
//! Uses `soundlog::VgmCallbackStream` for key-on/key-off detection (the
//! crate's `Ym2612State` / `Sn76489State` trackers handle port mapping,
//! channel selectors, and DAC suppression). Our own register snapshots
//! feed the timbre of each note's patch.
//!
//! Both `on_write` callbacks share state through `Rc<RefCell<_>>` because
//! Rust can't borrow the same `&mut` from two closures simultaneously.
//!
//! Beyond soundlog's key-on/key-off, this file also:
//!   * preserves held-note pitch-register changes as pitch events,
//!   * keeps events in chronological order within a sample (an off+on written
//!     at the same sample used to sort On-before-Off and lose the new note),
//!   * tracks `0x2B` (DAC enable) so FM6 key-on/key-off can be suppressed
//!     while the DAC channel is active (reference: sub.ym2612.cpp
//!     `if (ch==5&&dacOn) return;`),
//!   * builds a plain-text `report` (written next to the patches as
//!     `vgm_report.txt`) that lists every command the converter does not
//!     handle, and counts of every place a note or write is dropped.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::rc::Rc;

use anyhow::{Context, Result};

use soundlog::vgm::VgmCallbackStream;
use soundlog::vgm::command::Instance;
use soundlog::vgm::stream::VgmStream;
use soundlog::chip::event::StateEvent;
use soundlog::chip::state::{Ym2612State, Sn76489State};
use soundlog::chip::{Ym2612Spec, PsgSpec};

use super::{CH_DAC, CH_FM, CH_NOISE, CH_PSG, DAC_MIDI, LABELS, N_CH, VGM_RATE};
use super::psg::PsgState;
use super::regs::{classify_op_register, TimbreSnapshot, YmChannelRegs};
use super::tracker::{Note, PitchPoint, VolumePoint};
use super::util::rd32;

pub(crate) struct Analysis {
    pub(crate) notes: Vec<Vec<Note>>,
    pub(crate) total: u32,
    pub(crate) report: String,
}

const PITCH_POINT_SEMITONES: f64 = 0.2;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind { On, Pitch, Volume, Off }

struct TrackedEvent {
    sample: u32,
    channel_idx: usize,
    kind: Kind,
    pitch: f64,
    vol: f64,
    timbre: TimbreSnapshot,
}

struct OpenNote {
    start: u32,
    pitch: f64,
    vol: f64,
    timbre: TimbreSnapshot,
    pitch_points: Vec<PitchPoint>,
    vol_points: Vec<VolumePoint>,
}

#[derive(Default, Clone)]
struct ChStats {
    key_on: u32,
    key_off: u32,
    pitch_points: u32,
    volume_points: u32,
    zero_len: u32,
    orphan_off: u32,
    tl_midnote: u32,
    param_midnote: u32,
    pitch_disagree: u32,
    notes_out: u32,
}

struct Stats {
    ch: Vec<ChStats>,
    dac_writes: u32,
    dac_enable_writes: u32,
    lfo_writes: u32,
    ch3_mode_writes: u32,
}

impl Stats {
    fn new() -> Self {
        Stats {
            ch: vec![ChStats::default(); N_CH],
            dac_writes: 0,
            dac_enable_writes: 0,
            lfo_writes: 0,
            ch3_mode_writes: 0,
        }
    }
}

fn hz_to_semis(hz: f64) -> Option<f64> {
    if hz.is_finite() && hz > 0.0 {
        Some(69.0 + 12.0 * (hz / 440.0).log2())
    } else {
        None
    }
}

fn fm_hz(r: &YmChannelRegs, clock: u32) -> Option<f64> {
    let fnum = (((r.fnum_hi & 7) as u32) << 8) | r.fnum_lo as u32;
    if fnum == 0 {
        return None;
    }
    let block = ((r.fnum_hi >> 3) & 7) as i32;
    Some(fnum as f64 * (clock as f64 / 144.0) / 2f64.powi(21 - block))
}

fn psg_hz(tone: u16, clock: u32) -> Option<f64> {
    Some(clock as f64 / (32.0 * tone.max(1) as f64))
}

fn channel_reg_channel(reg: u8, port: u8) -> Option<usize> {
    let in_port = (reg & 3) as usize;
    if in_port > 2 { return None; }
    match reg & 0xF0 {
        0xA0 | 0xA4 | 0xB0 | 0xB4 => Some(if port == 0 { in_port } else { in_port + 3 }),
        _ => None,
    }
}

#[derive(Default)]
struct PsgTrack {
    open: [Option<f64>; 3],
    open_vol: [Option<u8>; 3],
    dirty_at: Option<u32>,
}

fn psg_volume_percent(att: u8) -> f64 {
    ((15u8.saturating_sub(att.min(15))) as f64 / 15.0) * 100.0
}

fn psg_flush(
    p: &mut PsgState,
    tr: &mut PsgTrack,
    buf: &mut Vec<TrackedEvent>,
    st: &mut Stats,
    clock: u32,
    now: Option<u32>,
) {
    let Some(at) = tr.dirty_at else { return };
    if let Some(now) = now {
        if now <= at { return; }
    }
    tr.dirty_at = None;
    for ch in 0..3usize {
        let Some(cur) = tr.open[ch] else { continue };
        let TimbreSnapshot::Psg(r) = p.snapshot(ch) else { continue };
        let Some(m) = psg_hz(r.tone, clock).and_then(hz_to_semis) else { continue };
        let idx = CH_PSG + ch;
        if (m - cur).abs() >= PITCH_POINT_SEMITONES {
            buf.push(TrackedEvent {
                sample: at, channel_idx: idx, kind: Kind::Pitch,
                pitch: m, vol: 0.0, timbre: TimbreSnapshot::Psg(r.clone()),
            });
            tr.open[ch] = Some(m);
            st.ch[idx].pitch_points += 1;
        }
        if tr.open_vol[ch] != Some(r.vol) {
            let att = r.vol;
            buf.push(TrackedEvent {
                sample: at, channel_idx: idx, kind: Kind::Volume,
                pitch: 0.0, vol: psg_volume_percent(r.vol),
                timbre: TimbreSnapshot::Psg(r),
            });
            tr.open_vol[ch] = Some(att);
            st.ch[idx].volume_points += 1;
        }
    }
}

fn cmd_name(c: u8) -> String {
    let s = match c {
        0x50 => "SN76489 write",
        0x51 => "YM2413 write",
        0x52 => "YM2612 port0 write",
        0x53 => "YM2612 port1 write",
        0x54 => "YM2151 write",
        0x55 => "YM2203 write",
        0x56 | 0x57 => "YM2608 write",
        0x58 | 0x59 => "YM2610 write",
        0x5A => "YM3812 write",
        0x5B => "YM3526 write",
        0x5C => "Y8950 write",
        0x5D => "YMZ280B write",
        0x5E | 0x5F => "YMF262 write",
        0x61 => "wait n",
        0x62 => "wait 735",
        0x63 => "wait 882",
        0x66 => "end of data",
        0x67 => "data block",
        0x68 => "PCM RAM write",
        0x70..=0x7F => "wait 1-16",
        0x80..=0x8F => "YM2612 DAC write + wait",
        0x90 => "DAC stream setup",
        0x91 => "DAC stream data",
        0x92 => "DAC stream frequency",
        0x93 => "DAC stream start",
        0x94 => "DAC stream stop",
        0x95 => "DAC stream fast start",
        0xA0 => "AY8910 write",
        0xA2 | 0xA3 => "YM2612 #2 write",
        0xA1..=0xAF => "second-chip write",
        0xB0..=0xBF => "other chip write",
        0xC0..=0xDF => "other chip write (24-bit)",
        0xE0 => "PCM bank seek",
        0x4F => "Game Gear stereo",
        0x30..=0x3F => "second SN76489 write",
        _ => "",
    };
    if s.is_empty() { format!("opcode 0x{:02X}", c) } else { s.to_string() }
}

fn cmd_converted(c: u8) -> bool {
    matches!(c, 0x50 | 0x52 | 0x53 | 0x61 | 0x62 | 0x63 | 0x66 | 0x67 | 0x70..=0x7F)
}

fn scan_commands(bytes: &[u8], data_start: usize) -> BTreeMap<u8, u32> {
    let mut hist: BTreeMap<u8, u32> = BTreeMap::new();
    let len = bytes.len();
    let mut p = data_start;
    while p < len {
        let c = bytes[p];
        *hist.entry(if (0x70..=0x7F).contains(&c) { 0x70 }
        else if (0x80..=0x8F).contains(&c) { 0x80 }
        else { c }).or_insert(0) += 1;
        p += match c {
            0x66 => break,
            0x67 => {
                if p + 7 > len { break; }
                7usize.saturating_add(rd32(bytes, p + 3) as usize)
            }
            0x4F | 0x50 => 2,
            0x30..=0x3F => 2,
            0x40..=0x4E => 3,
            0x51..=0x5F => 3,
            0x61 => 3,
            0x62 | 0x63 => 1,
            0x64 => 4,
            0x68 => 12,
            0x70..=0x8F => 1,
            0x90 | 0x91 | 0x95 => 5,
            0x92 => 6,
            0x93 => 11,
            0x94 => 2,
            0xA0..=0xBF => 3,
            0xC0..=0xDF => 4,
            0xE0 => 5,
            // 0xE1-0xFF are reserved opcodes; per VGM spec they occupy 1 byte
            // each (see dacopt.c fallthrough and vgm2txt behavior).
            0xE1..=0xFF => 1,
            _ => 1,
        };
    }
    hist
}

pub(crate) fn analyze(bytes: &[u8]) -> Result<Option<Analysis>> {
    if bytes.len() < 0x40 || &bytes[0..4] != b"Vgm " {
        return Ok(None);
    }

    let version = rd32(bytes, 0x08);
    let header_total = rd32(bytes, 0x18);
    let sn_clock = rd32(bytes, 0x0C) & 0x3FFF_FFFF;
    let ym_clock = if version >= 0x110 { rd32(bytes, 0x2C) & 0x3FFF_FFFF } else { 0 };

    let inner = VgmStream::from_vgm(bytes.to_vec())
    .context("parsing VGM command stream")?;
    let mut stream = VgmCallbackStream::new(inner);
    if ym_clock > 0 {
        stream.track_state::<Ym2612State>(Instance::Primary, ym_clock as f32);
    }
    if sn_clock > 0 {
        stream.track_state::<Sn76489State>(Instance::Primary, sn_clock as f32);
    }

    let ym_regs = Rc::new(RefCell::new([YmChannelRegs::default(); 6]));
    let ym_events = Rc::new(RefCell::new(Vec::<TrackedEvent>::new()));
    let ym_open: Rc<RefCell<[Option<f64>; 6]>> = Rc::new(RefCell::new([None; 6]));
    // DAC enable (register 0x2B bit 7). While set, FM6 is muted on hardware.
    // Reference: sub.ym2612.cpp `write()` case 0x2b + `if (ch==5&&dacOn) return;`.
    let dac_on: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));
    let psg = Rc::new(RefCell::new(PsgState::new()));
    let psg_events = Rc::new(RefCell::new(Vec::<TrackedEvent>::new()));
    let psg_track = Rc::new(RefCell::new(PsgTrack::default()));
    let last_sample = Rc::new(RefCell::new(0u32));
    let stats = Rc::new(RefCell::new(Stats::new()));

    if ym_clock > 0 {
        let ym_regs = Rc::clone(&ym_regs);
        let ym_events = Rc::clone(&ym_events);
        let ym_open = Rc::clone(&ym_open);
        let dac_on = Rc::clone(&dac_on);
        let last_sample = Rc::clone(&last_sample);
        let stats = Rc::clone(&stats);
        stream.on_write(move |_inst: Instance, spec: Ym2612Spec, sample, events: Option<Vec<StateEvent>>| {
            let s = sample as u32;
            {
                let mut ls = last_sample.borrow_mut();
                *ls = (*ls).max(s);
            }

            let port = spec.port;
            let reg = spec.register;
            let val = spec.value;

            {
                let mut st = stats.borrow_mut();
                if port == 0 {
                    match reg {
                        0x22 => st.lfo_writes += 1,
                        0x27 => st.ch3_mode_writes += 1,
                        0x2A => st.dac_writes += 1,
                        0x2B => st.dac_enable_writes += 1,
                        _ => {}
                    }
                }
            }

            // Track DAC enable bit (0x2B bit 7). FM6 must be silenced while
            // this is set (per YM2612 hardware; reference: sub.ym2612.cpp).
            if port == 0 && reg == 0x2B {
                *dac_on.borrow_mut() = val & 0x80 != 0;
            }

            {
                let mut regs = ym_regs.borrow_mut();
                if let Some((ch, _op)) = classify_op_register(reg, port) {
                    regs[ch].apply(reg, val, port);
                } else if let Some(ch) = channel_reg_channel(reg, port) {
                    regs[ch].apply(reg, val, port);
                }
            }

            if let Some((ch, _op)) = classify_op_register(reg, port) {
                if ym_open.borrow()[ch].is_some() {
                    let mut st = stats.borrow_mut();
                    if reg & 0xF0 == 0x40 {
                        st.ch[CH_FM + ch].tl_midnote += 1;
                    } else {
                        st.ch[CH_FM + ch].param_midnote += 1;
                    }
                }
            }
            if matches!(reg, 0xA0..=0xA2 | 0xA4..=0xA6) {
                let ch = (reg & 3) as usize + if port == 0 { 0 } else { 3 };
                // FM6 pitch register writes while DAC is on still don't affect
                // the audible output — skip the split logic for ch5.
                if ch != 5 || !*dac_on.borrow() {
                    let cur = ym_open.borrow()[ch];
                    if let Some(cur) = cur {
                        let regs = ym_regs.borrow();
                        if let Some(m) = fm_hz(&regs[ch], ym_clock).and_then(hz_to_semis) {
                            if (m - cur).abs() >= PITCH_POINT_SEMITONES {
                                let mut buf = ym_events.borrow_mut();
                                buf.push(TrackedEvent {
                                    sample: s, channel_idx: CH_FM + ch, kind: Kind::Pitch,
                                    pitch: m, vol: 0.0,
                                    timbre: TimbreSnapshot::Ym(regs[ch].clone()),
                                });
                                ym_open.borrow_mut()[ch] = Some(m);
                                stats.borrow_mut().ch[CH_FM + ch].pitch_points += 1;
                            }
                        }
                    }
                }
            }

            let Some(evs) = events else { return };
            let regs = ym_regs.borrow();
            let mut buf = ym_events.borrow_mut();
            for ev in evs {
                match ev {
                    StateEvent::KeyOn { channel, tone } => {
                        let ch = channel as usize;
                        if ch >= 6 { continue; }
                        // Hardware: key-on to FM6 while DAC enabled is a no-op.
                        // Reference: sub.ym2612.cpp `if (ch==5&&dacOn) return;`.
                        if ch == 5 && *dac_on.borrow() { continue; }
                        let own = fm_hz(&regs[ch], ym_clock).and_then(hz_to_semis);
                        let theirs = tone.freq_hz.and_then(|hz| hz_to_semis(hz as f64));
                        let chosen = own.or(theirs);
                        {
                            let mut st = stats.borrow_mut();
                            st.ch[CH_FM + ch].key_on += 1;
                            if let (Some(a), Some(b)) = (own, theirs) {
                                if (a - b).abs() >= 1.0 {
                                    st.ch[CH_FM + ch].pitch_disagree += 1;
                                }
                            }
                        }
                        ym_open.borrow_mut()[ch] = Some(chosen.unwrap_or(60.0));
                        buf.push(TrackedEvent {
                            sample: s,
                            channel_idx: CH_FM + ch,
                            kind: Kind::On,
                            pitch: chosen.unwrap_or(60.0),
                                 vol: 100.0,
                                 timbre: TimbreSnapshot::Ym(regs[ch].clone()),
                        });
                    }
                    StateEvent::KeyOff { channel } => {
                        let ch = channel as usize;
                        if ch >= 6 { continue; }
                        // Same as KeyOn: while DAC on, FM6 writes are ignored.
                        if ch == 5 && *dac_on.borrow() { continue; }
                        stats.borrow_mut().ch[CH_FM + ch].key_off += 1;
                        ym_open.borrow_mut()[ch] = None;
                        buf.push(TrackedEvent {
                            sample: s,
                            channel_idx: CH_FM + ch,
                            kind: Kind::Off,
                            pitch: 0.0,
                            vol: 0.0,
                            timbre: TimbreSnapshot::Ym(Default::default()),
                        });
                    }
                    StateEvent::ToneChange { .. } => {}
                }
            }
        });
    }

    if sn_clock > 0 {
        let psg = Rc::clone(&psg);
        let psg_events = Rc::clone(&psg_events);
        let psg_track = Rc::clone(&psg_track);
        let last_sample = Rc::clone(&last_sample);
        let stats = Rc::clone(&stats);
        stream.on_write(move |_inst: Instance, spec: PsgSpec, sample, events: Option<Vec<StateEvent>>| {
            let s = sample as u32;
            {
                let mut ls = last_sample.borrow_mut();
                *ls = (*ls).max(s);
            }

            {
                let mut p = psg.borrow_mut();
                let mut tr = psg_track.borrow_mut();
                let mut buf = psg_events.borrow_mut();
                let mut st = stats.borrow_mut();
                psg_flush(&mut p, &mut tr, &mut buf, &mut st, sn_clock, Some(s));
                p.write(spec.value);
                tr.dirty_at = Some(s);
            }

            let Some(evs) = events else { return };
            let p = psg.borrow_mut();
            let mut tr = psg_track.borrow_mut();
            let mut buf = psg_events.borrow_mut();
            let mut st = stats.borrow_mut();
            for ev in evs {
                match ev {
                    StateEvent::KeyOn { channel, tone } => {
                        let ch = channel as usize;
                        if ch > 3 { continue; }
                        let timbre = p.snapshot(ch);
                        let idx = if ch == 3 { CH_NOISE } else { CH_PSG + ch };
                        let theirs = tone.freq_hz.and_then(|hz| hz_to_semis(hz as f64));
                        let own = match (&timbre, ch < 3) {
                            (TimbreSnapshot::Psg(r), true) => psg_hz(r.tone, sn_clock).and_then(hz_to_semis),
                        _ => None,
                        };
                        let chosen = own.or(theirs);
                        st.ch[idx].key_on += 1;
                        if let (Some(a), Some(b)) = (own, theirs) {
                            if (a - b).abs() >= 1.0 {
                                st.ch[idx].pitch_disagree += 1;
                            }
                        }
                        if ch < 3 {
                            tr.open[ch] = Some(chosen.unwrap_or(60.0));
                            tr.open_vol[ch] = match &timbre {
                                TimbreSnapshot::Psg(r) => Some(r.vol),
                                _ => None,
                            };
                        }
                        let vol = match &timbre {
                            TimbreSnapshot::Psg(r) if ch < 3 => psg_volume_percent(r.vol),
                            _ => 100.0,
                        };
                        buf.push(TrackedEvent {
                            sample: s,
                            channel_idx: idx,
                            kind: Kind::On,
                            pitch: chosen.unwrap_or(60.0),
                                 vol,
                                 timbre,
                        });
                    }
                    StateEvent::KeyOff { channel } => {
                        let ch = channel as usize;
                        if ch > 3 { continue; }
                        let timbre = p.snapshot(ch);
                        let idx = if ch == 3 { CH_NOISE } else { CH_PSG + ch };
                        st.ch[idx].key_off += 1;
                        if ch < 3 {
                            tr.open[ch] = None;
                            tr.open_vol[ch] = None;
                        }
                        buf.push(TrackedEvent {
                            sample: s,
                            channel_idx: idx,
                            kind: Kind::Off,
                            pitch: 0.0,
                            vol: 0.0,
                            timbre,
                        });
                    }
                    StateEvent::ToneChange { .. } => {}
                }
            }
        });
    }

    while let Some(item) = stream.next() {
        let _ = item.context("decoding VGM command stream")?;
    }
    drop(stream);

    if sn_clock > 0 {
        let mut p = psg.borrow_mut();
        let mut tr = psg_track.borrow_mut();
        let mut buf = psg_events.borrow_mut();
        let mut st = stats.borrow_mut();
        psg_flush(&mut p, &mut tr, &mut buf, &mut st, sn_clock, None);
    }

    let last = *last_sample.borrow();
    let total = if last > 0 {
        last.max(header_total.min(last.saturating_mul(2)))
    } else {
        header_total
    };

    let ym_evs = std::mem::take(&mut *ym_events.borrow_mut());
    let psg_evs = std::mem::take(&mut *psg_events.borrow_mut());
    let mut all_events: Vec<TrackedEvent> = ym_evs.into_iter()
    .chain(psg_evs.into_iter())
    .collect();
    all_events.sort_by_key(|e| (e.sample, e.channel_idx));

    let mut st = stats.borrow_mut();
    let mut notes: Vec<Vec<Note>> = (0..N_CH).map(|_| Vec::new()).collect();
    let mut open: Vec<Option<OpenNote>> = (0..N_CH).map(|_| None).collect();

    for ev in all_events {
        let ch = ev.channel_idx;
        if ch >= N_CH { continue; }
        match ev.kind {
            Kind::On => {
                if let Some(prev) = open[ch].take() {
                    if ev.sample > prev.start {
                        notes[ch].push(Note {
                            start: prev.start, end: ev.sample, pitch: prev.pitch,
                            pitch_points: prev.pitch_points, vol_points: prev.vol_points, vol: prev.vol,
                            timbre_hash: prev.timbre.hash(), timbre: prev.timbre,
                        });
                    } else {
                        st.ch[ch].zero_len += 1;
                    }
                }
                open[ch] = Some(OpenNote {
                    start: ev.sample, pitch: ev.pitch, vol: ev.vol,
                    timbre: ev.timbre, pitch_points: Vec::new(), vol_points: Vec::new(),
                });
            }
            Kind::Pitch => {
                if let Some(n) = open[ch].as_mut() {
                    if ev.sample > n.start {
                        if let Some(last) = n.pitch_points.last_mut() {
                            if last.sample == ev.sample {
                                last.pitch = ev.pitch;
                                continue;
                            }
                        }
                        n.pitch_points.push(PitchPoint { sample: ev.sample, pitch: ev.pitch });
                    }
                }
            }
            Kind::Volume => {
                if let Some(n) = open[ch].as_mut() {
                    if ev.sample > n.start {
                        if let Some(last) = n.vol_points.last_mut() {
                            if last.sample == ev.sample {
                                last.volume = ev.vol;
                                continue;
                            }
                        }
                        n.vol_points.push(VolumePoint { sample: ev.sample, volume: ev.vol });
                    }
                }
            }
            Kind::Off => {
                if let Some(n) = open[ch].take() {
                    let final_timbre = n.timbre;
                    if ev.sample > n.start {
                        notes[ch].push(Note {
                            start: n.start, end: ev.sample, pitch: n.pitch,
                            pitch_points: n.pitch_points, vol_points: n.vol_points, vol: n.vol,
                            timbre_hash: final_timbre.hash(), timbre: final_timbre,
                        });
                    } else {
                        st.ch[ch].zero_len += 1;
                    }
                } else {
                    st.ch[ch].orphan_off += 1;
                }
            }
        }
    }

    for (ch, slot) in open.iter_mut().enumerate() {
        if let Some(n) = slot.take() {
            if total > n.start {
                notes[ch].push(Note {
                    start: n.start, end: total, pitch: n.pitch,
                    pitch_points: n.pitch_points, vol_points: n.vol_points, vol: n.vol,
                    timbre_hash: n.timbre.hash(), timbre: n.timbre,
                });
            }
        }
    }
    // DAC: one note per PCM hit (hosting/chip/dac.rs matches the nearest hit
    // to each note's start time at playback). soundlog never reports the DAC
    // as a keyed channel, so these come from our own scan of the raw stream.
    if let Some(scan) = super::dac::extract(bytes) {
        for h in scan.hits {
            if h.end > h.start {
                notes[CH_DAC].push(Note {
                    start: h.start,
                    end: h.end,
                    pitch: DAC_MIDI as f64,
                    pitch_points: Vec::new(),
                    vol_points: Vec::new(),
                    vol: 100.0,
                    timbre_hash: TimbreSnapshot::Dac.hash(),
                                   timbre: TimbreSnapshot::Dac,
                });
            }
        }
    }
    for ch in 0..N_CH {
        st.ch[ch].notes_out = notes[ch].len() as u32;
    }

    let data_start = if version >= 0x150 {
        let rel = rd32(bytes, 0x34);
        if rel == 0 { 0x40 } else { 0x34usize.saturating_add(rel as usize) }
    } else {
        0x40
    };
    let hist = scan_commands(bytes, data_start.min(bytes.len()));

    let mut r = String::new();
    let _ = writeln!(r, "VGM conversion report");
    let _ = writeln!(r, "=====================");
    let _ = writeln!(r, "version 0x{:03X}, {} samples ({:.2}s), SN76489 clock {} Hz, YM2612 clock {} Hz",
                     version, total, total as f64 / VGM_RATE, sn_clock, ym_clock);

    let other_clocks: [(usize, &str); 10] = [
        (0x10, "YM2413"), (0x30, "SegaPCM"), (0x44, "YM2203"), (0x48, "YM2608"),
        (0x4C, "YM2610"), (0x50, "YM3812"), (0x54, "YM3526"), (0x58, "Y8950"),
        (0x5C, "YMF262"), (0x74, "AY8910"),
    ];
    let mut present: Vec<String> = Vec::new();
    for (off, name) in other_clocks {
        if off + 4 <= data_start.min(bytes.len()) {
            let c = rd32(bytes, off) & 0x3FFF_FFFF;
            if c > 0 { present.push(format!("{name} ({c} Hz)")); }
        }
    }
    if rd32(bytes, 0x2C) & 0x4000_0000 != 0 { present.push("dual YM2612".to_string()); }
    if rd32(bytes, 0x0C) & 0x4000_0000 != 0 { present.push("dual SN76489".to_string()); }
    if present.is_empty() {
        let _ = writeln!(r, "other chips in header: none");
    } else {
        let _ = writeln!(r, "other chips in header (NOT converted): {}", present.join(", "));
    }

    let _ = writeln!(r, "\nCommands in stream  (count, name, status)");
    for (c, n) in &hist {
        let status = if cmd_converted(*c) { "converted" } else { "NOT CONVERTED" };
        let _ = writeln!(r, "  {:>8}  {:<28} {}", n, cmd_name(*c), status);
    }

    let _ = writeln!(r, "\nYM2612 registers the converter does not model");
    let _ = writeln!(r, "  DAC data writes (0x2A):        {}", st.dac_writes);
    let _ = writeln!(r, "  DAC enable writes (0x2B):      {}", st.dac_enable_writes);
    let _ = writeln!(r, "  LFO writes (0x22):             {}", st.lfo_writes);
    let _ = writeln!(r, "  ch3 mode writes (0x27):        {}", st.ch3_mode_writes);
    if st.dac_writes > 0 || hist.contains_key(&0x80) || hist.contains_key(&0x93) || hist.contains_key(&0x95) {
        let _ = writeln!(r, "  -> DAC hits are emitted as DAC channel notes (played by hosting/chip/dac.rs).");
    }

    let _ = writeln!(r, "\nPer channel");
    let _ = writeln!(r, "  {:<6} {:>6} {:>6} {:>7} {:>7} {:>6} {:>8} {:>9} {:>10} {:>10} {:>9}",
                     "chan", "key_on", "key_off", "pitch_pts", "volume_pts", "notes", "zero_len", "orphan_off", "tl_midnote", "par_midnote", "pitch_dis");
    for ch in 0..N_CH {
        let c = &st.ch[ch];
        if c.key_on == 0 && c.key_off == 0 && c.notes_out == 0 && c.tl_midnote == 0 && c.param_midnote == 0 {
            continue;
        }
        let _ = writeln!(r, "  {:<6} {:>6} {:>6} {:>7} {:>7} {:>6} {:>8} {:>9} {:>10} {:>10} {:>9}",
                         LABELS[ch], c.key_on, c.key_off, c.pitch_points, c.volume_points, c.notes_out, c.zero_len,
                         c.orphan_off, c.tl_midnote, c.param_midnote, c.pitch_disagree);
    }
    let _ = writeln!(r, "\n  pitch_pts      held-note pitch changes preserved as waypoints");
    let _ = writeln!(r, "  volume_pts     PSG attenuation changes preserved as waypoints");
    let _ = writeln!(r, "  zero_len       notes dropped: on and off on the same sample");
    let _ = writeln!(r, "  orphan_off     key-offs with no open note");
    let _ = writeln!(r, "  tl_midnote     FM volume (TL) writes while keyed on: volume automation NOT in output");
    let _ = writeln!(r, "  par_midnote    other FM operator writes while keyed on: modulation NOT in output");
    let _ = writeln!(r, "  pitch_dis      key-ons where soundlog's pitch and ours differ by >= 1 semitone");

    if std::env::var_os("CIRCOLATE_VGM_DEBUG_TIMING").is_some() {
        eprintln!("VGM total: {} samples ({:.2}s)", total, total as f64 / VGM_RATE);
        for (ch, chn) in notes.iter().enumerate() {
            if chn.is_empty() { continue; }
            let first = chn.iter().map(|n| n.start).min().unwrap();
            let last  = chn.iter().map(|n| n.end).max().unwrap();
            eprintln!("{}: {} notes, {:.2}s–{:.2}s",
                      LABELS[ch], chn.len(),
                      first as f64 / VGM_RATE, last as f64 / VGM_RATE);
        }
        eprintln!("{r}");
    }

    drop(st);
    Ok(Some(Analysis { notes, total, report: r }))
}
