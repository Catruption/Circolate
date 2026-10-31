//! Voice scheduling, mixer, and DSP primitives.
//!
//! # Chip players
//!
//! This engine has four voice sources: samples, SF2, VST, and chips.
//! Chip players (`crate::hosting::chip::ChipPlayer`) render a *patch*
//! through a data-driven backend (`.chip` file). They replace the old
//! VGM player, which replayed register writes through ymfm-sys.
//!
//! Each chip instance is its own mixer channel (`iid` -> `part_path`).
//! Notes are dispatched at block boundaries (~10 ms at 48 kHz); sample-
//! accurate dispatch is a future upgrade.

mod dsp;
pub(crate) mod perf;
mod voices;
mod vst;
mod shims;

pub(crate) use dsp::gains;
use dsp::{init_tables, sanitize};
use perf::PERF;

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;

use crate::effects::EffectChain;
use crate::hosting::chip::{ChipNote, ChipPlayer};
use crate::hosting::sf2::engine::Sf2Engine;
use crate::hosting::vst::ffi::VstHost;
use crate::player::project::Project;
use crate::player::inserts;

pub(crate) use voices::{Scheduled, Active};
pub(crate) use vst::HostSlot;
pub(crate) use shims::ShimHost;
pub(crate) use crate::player::types::Sample;

#[derive(Clone, Debug)]
pub(crate) struct VstNote {
    pub(crate) start: i64,
    pub(crate) end: i64,
    pub(crate) iid: u16,
    pub(crate) note: u8,
    pub(crate) velocity: u8,
    pub(crate) line: u32,
    pub(crate) seq: u32,
    pub(crate) part_path: Arc<str>,
    pub(crate) source_row: f64,
}

impl VstNote {
    pub(crate) fn key(&self) -> (i64, u32, u32) {
        (self.start, self.line, self.seq)
    }
}

pub(crate) enum Msg {
    Batch {
        epoch: u64,
        samples: Vec<Scheduled>,
        vsts: Vec<VstNote>,
        chips: Vec<ChipNote>,
    },
    Seek {
        epoch: u64,
        pos: i64,
        samples: Vec<Scheduled>,
        vsts: Vec<VstNote>,
        chips: Vec<ChipNote>,
    },
}

pub(crate) const VST_BLOCK: i64 = 512;
pub(crate) const VST_HISTORY_SECS: f64 = 10.0;
pub(crate) const HOLD_TAIL_SECS: f64 = 0.020;

pub(crate) const VST_TAIL_BLOCKS: u32 = 20;

pub(crate) struct Engine {
    pub(crate) sr: f64,
    pub(crate) song_volume: f64,
    pub(crate) linear: bool,
    pub(crate) total: i64,
    pub(crate) pos: i64,
    pub(crate) ready: bool,
    pub(crate) epoch: u64,
    pub(crate) pending: VecDeque<Scheduled>,
    pub(crate) active: Vec<Active>,

    pub(crate) insert_chains: BTreeMap<u8, inserts::InsertChain>,
    pub(crate) channel_to_insert: BTreeMap<u16, u8>,
    pub(crate) sf2_scratch: BTreeMap<u16, (Vec<f32>, Vec<f32>)>,

    pub(crate) hosts: Vec<HostSlot>,
    pub(crate) iid_to_host_idx: BTreeMap<u16, usize>,
    pub(crate) vst_gains: BTreeMap<u16, f32>,
    pub(crate) iid_to_part_path: BTreeMap<u16, String>,
    pub(crate) sf2_engines: BTreeMap<u16, Sf2Engine>,
    pub(crate) vst_pending: VecDeque<VstNote>,
    pub(crate) vst_active: Vec<VstNote>,
    pub(crate) vst_log: Vec<VstNote>,
    pub(crate) vst_held: BTreeMap<u16, u128>,
    pub(crate) vst_tail: BTreeMap<u16, u32>,
    pub(crate) vst_synced: bool,
    pub(crate) vst_scratch_l: Vec<f32>,
    pub(crate) vst_scratch_r: Vec<f32>,
    pub(crate) vst_hist_l: Vec<f32>,
    pub(crate) vst_hist_r: Vec<f32>,
    pub(crate) vst_hist_head: i64,
    pub(crate) vst_events_buf: Vec<(i32, u8, u16, u8, u8)>,
    pub(crate) render_flags: Vec<bool>,

    pub(crate) shims: BTreeMap<u32, ShimHost>,
    pub(crate) shim_by_line: BTreeMap<u32, Vec<u32>>,
    pub(crate) shim_scratch: BTreeMap<u32, (Vec<f32>, Vec<f32>)>,
    pub(crate) shim_dry_l: Vec<f32>,
    pub(crate) shim_dry_r: Vec<f32>,
    pub(crate) shim_chains_cache: Vec<Vec<u32>>,
    pub(crate) shim_processed: Vec<u32>,
    pub(crate) shim_link_l: Vec<f32>,
    pub(crate) shim_link_r: Vec<f32>,

    pub(crate) master_fx: Option<EffectChain>,
    pub(crate) pause_fade: f32,
    pub(crate) tail_frames: i64,

    /// One chip player per chip instance. Keyed by `iid`; the mapping
    /// `iid -> part_path` lives in `iid_to_part_path` (shared with VSTs
    /// and SF2s).
    pub(crate) chip_players: BTreeMap<u16, ChipPlayer>,
    pub(crate) chip_pending: VecDeque<ChipNote>,
    pub(crate) chip_active: Vec<ChipNote>,
    chip_ended: Vec<ChipNote>,
    chip_gain_scratch: Vec<(u16, f32)>,
    /// All chip notes ever ingested, for the tracker's history view.
    /// Mirrors `vst_log`.
    pub(crate) chip_log: Vec<ChipNote>,
}

impl Engine {
    pub(crate) fn new(p: &Project) -> Self {
        init_tables();
        vst::init_audio_pool();
        let hist_len = (VST_HISTORY_SECS * p.sample_rate as f64).max(1.0) as usize;
        Engine {
            sr: p.sample_rate as f64,
            song_volume: p.song_volume,
            linear: p.linear_pan,
            total: p.total_samples,
            pos: 0,
            ready: false,
            epoch: 0,
            pending: VecDeque::new(),
            active: Vec::new(),
            hosts: Vec::new(),
            iid_to_host_idx: BTreeMap::new(),
            vst_gains: BTreeMap::new(),
            iid_to_part_path: BTreeMap::new(),
            sf2_engines: BTreeMap::new(),
            vst_pending: VecDeque::new(),
            vst_active: Vec::new(),
            vst_log: Vec::new(),
            vst_held: BTreeMap::new(),
            vst_tail: BTreeMap::new(),
            vst_synced: true,
            vst_scratch_l: vec![0.0; VST_BLOCK as usize],
            vst_scratch_r: vec![0.0; VST_BLOCK as usize],
            vst_hist_l: vec![0.0; hist_len],
            vst_hist_r: vec![0.0; hist_len],
            vst_hist_head: i64::MIN,
            insert_chains: BTreeMap::new(),
            channel_to_insert: BTreeMap::new(),
            sf2_scratch: BTreeMap::new(),
            master_fx: None,
            pause_fade: 1.0,
            tail_frames: 0,
            shims: BTreeMap::new(),
            shim_by_line: BTreeMap::new(),
            shim_scratch: BTreeMap::new(),
            shim_dry_l: Vec::new(),
            shim_dry_r: Vec::new(),
            shim_chains_cache: Vec::new(),
            shim_processed: Vec::with_capacity(64),
            shim_link_l: vec![0.0; VST_BLOCK as usize],
            shim_link_r: vec![0.0; VST_BLOCK as usize],
            vst_events_buf: Vec::with_capacity(128),
            render_flags: Vec::new(),
            chip_players: BTreeMap::new(),
            chip_pending: VecDeque::new(),
            chip_active: Vec::new(),
            chip_ended: Vec::with_capacity(256),
            chip_gain_scratch: Vec::with_capacity(8),
            chip_log: Vec::new(),
        }
    }

    pub(crate) fn install_insert_chain(&mut self, insert: u8, chain: inserts::InsertChain) {
        self.insert_chains.insert(insert, chain);
    }

    pub(crate) fn set_channel_insert(&mut self, iid: u16, insert: u8) {
        self.channel_to_insert.insert(iid, insert);
    }

    pub(crate) fn install_host(&mut self, iid: u16, host: VstHost) {
        self.install_host_with_gain(iid, host, 1.0);
    }

    pub(crate) fn install_host_with_gain(&mut self, iid: u16, host: VstHost, gain: f32) {
        let idx = self.hosts.len();
        self.hosts.push(HostSlot {
            iid,
            host,
            scratch_l: vec![0.0f32; VST_BLOCK as usize],
            scratch_r: vec![0.0f32; VST_BLOCK as usize],
        });
        self.render_flags.push(false);
        self.iid_to_host_idx.insert(iid, idx);
        self.vst_gains.insert(iid, gain);
        self.vst_held.entry(iid).or_insert(0);
    }

    pub(crate) fn register_part_path(&mut self, iid: u16, part_path: String) {
        self.iid_to_part_path.insert(iid, part_path);
    }

    pub(crate) fn install_sf2_engine(&mut self, iid: u16, engine: Sf2Engine) {
        self.sf2_engines.insert(iid, engine);
        self.sf2_scratch.insert(iid, (vec![0.0; VST_BLOCK as usize], vec![0.0; VST_BLOCK as usize]));
        self.vst_held.entry(iid).or_insert(0);
    }

    /// Register a fully-constructed chip player under `iid`. The player
    /// owns its backend, patch table, and internal voice allocation. The
    /// engine will dispatch notes to it and call `render` each block.
    pub(crate) fn install_chip(&mut self, iid: u16, part_path: String, player: ChipPlayer) {
        self.register_part_path(iid, part_path);
        self.chip_players.insert(iid, player);
    }

    pub(crate) fn host_count(&self) -> usize { self.hosts.len() }
    pub(crate) fn sf2_engine_count(&self) -> usize { self.sf2_engines.len() }
    pub(crate) fn chip_count(&self) -> usize { self.chip_players.len() }

    pub(crate) fn install_master_fx(&mut self, chain: EffectChain) {
        self.master_fx = Some(chain);
    }

    pub(crate) fn master_fx_count(&self) -> usize {
        self.master_fx.as_ref().map_or(0, |c| c.len())
    }

    pub(crate) fn sample_rate(&self) -> f64 { self.sr }

    // ─────────────── chip gain ───────────────

    fn chip_gain(&self, iid: u16) -> f32 {
        // Two chip players: 100 = sn76489, 101 = ym2612. The mixer's
        // mute/volume registry doesn't track these paths, so use
        // song_volume directly instead of zeroing out.
        if iid == 100 || iid == 101 {
            return self.song_volume as f32;
        }
        let path = match self.iid_to_part_path.get(&iid) {
            Some(p) => p.as_str(),
            None => return 0.0,
        };
        if !crate::mixer::mute::is_note_active(path, 0, 0) {
            return 0.0;
        }
        let v = crate::mixer::part_volume::get(path);
        (self.song_volume * v) as f32
    }

    // ─────────────── chip ingest ───────────────

    pub(crate) fn ingest_chip(&mut self, mut notes: Vec<ChipNote>) {
        notes.sort_unstable_by_key(|n| (n.start, n.line, n.seq));

        if self.chip_pending.capacity() < self.chip_pending.len() + notes.len() + 64 {
            self.chip_pending.reserve(notes.len() + 64);
        }
        if self.chip_log.capacity() < self.chip_log.len() + notes.len() + 64 {
            self.chip_log.reserve(notes.len() + 64);
        }
        self.chip_active.reserve(notes.len());
        self.chip_ended.reserve(notes.len());

        const CHIP_LOG_MAX: usize = 10_000;
        if self.chip_log.len() + notes.len() > CHIP_LOG_MAX {
            let drop = ((self.chip_log.len() + notes.len()) - CHIP_LOG_MAX)
            .min(self.chip_log.len());
            self.chip_log.drain(..drop);
        }

        for mut n in notes {
            self.chip_log.push(n.clone());
            if n.end <= self.pos {
                continue;
            }
            // Chip parts (iids 100/101) don't participate in the mixer's mute
            // registry — those paths are chip patch files, not sample/VST paths.
            // Skip the check for them so notes actually reach `render_chips`.
            if n.iid != 100 && n.iid != 101 {
                if !crate::mixer::mute::is_note_active(&n.part_path, n.line as usize, n.source_row as i64) {
                    continue;
                }
            }
            if n.start < self.pos {
                // Note was already sounding when we entered this block:
                // begin it now so the tail is audible.
                n.start = self.pos;
            }
            let key = (n.start, n.line, n.seq);
            if self.chip_pending.back().map_or(true, |b| (b.start, b.line, b.seq) <= key) {
                self.chip_pending.push_back(n);
            } else {
                let idx = self
                .chip_pending
                .binary_search_by_key(&key, |p| (p.start, p.line, p.seq))
                .unwrap_or_else(|i| i);
                self.chip_pending.insert(idx, n);
            }
        }

    }

    // ─────────────── chip render ───────────────

    /// Dispatch due note-ons and note-offs into chip players, then render
    /// each player's block. Block-boundary dispatch (not sample-accurate);
    /// upgrade path is to pass absolute sample time into the player.
    fn render_chips(&mut self, out_l: &mut [f32], out_r: &mut [f32], block_start: i64) {
        if self.chip_players.is_empty() {
            return;
        }
        let block_end = block_start + out_l.len() as i64;

        self.chip_ended.clear();
        {
            let (active, ended) = (&mut self.chip_active, &mut self.chip_ended);
            active.retain(|n| {
                if n.end <= block_start {
                    ended.push(n.clone());
                    false
                } else {
                    true
                }
            });
        }
        for n in &self.chip_ended {
            if let Some(p) = self.chip_players.get_mut(&n.iid) {
                p.note_off(n);
            }
        }

        while let Some(n) = self.chip_pending.front() {
            if n.start >= block_end { break; }
            let note = self.chip_pending.pop_front().unwrap();
            if let Some(p) = self.chip_players.get_mut(&note.iid) {
                p.note_on(&note);
            }
            self.chip_active.push(note);
        }

        self.chip_gain_scratch.clear();
        for iid in self.chip_players.keys().copied() {
            let gain = self.chip_gain(iid);
            self.chip_gain_scratch.push((iid, gain));
        }
        for idx in 0..self.chip_gain_scratch.len() {
            let (iid, gain) = self.chip_gain_scratch[idx];
            if gain <= 0.0 { continue; }
            if let Some(player) = self.chip_players.get_mut(&iid) {
                player.render(out_l, out_r, block_start, gain);
            }
        }

    }

    // ─────────────── sample voices ───────────────

    pub(crate) fn ingest(&mut self, mut voices: Vec<Scheduled>) {
        voices.sort_unstable_by_key(Scheduled::key);

        if self.pending.capacity() < self.pending.len() + voices.len() + 64 {
            self.pending.reserve(voices.len() + 64);
        }
        if self.active.capacity() < self.active.len() + voices.len() + 64 {
            self.active.reserve(voices.len() + 64);
        }

        for v in voices {
            if v.end <= self.pos {
                continue;
            }
            if v.start < self.pos {
                if !crate::mixer::mute::is_note_active(&v.part_path, v.line as usize, v.source_row as i64) {
                    continue;
                }
                let raw_pos = self.raw_pos_at(&v, self.pos);
                let env_state = self.env_state_for(&v);
                self.active.push(Active { v, raw_pos, env_state, prev_vol: f64::NAN });
            } else {
                let key = v.key();
                if self.pending.back().map_or(true, |b| b.key() <= key) {
                    self.pending.push_back(v);
                } else {
                    let idx = self
                    .pending
                    .binary_search_by_key(&key, |p| p.key())
                    .unwrap_or_else(|i| i);
                    self.pending.insert(idx, v);
                }
            }
        }
    }

    pub(crate) fn ingest_vst(&mut self, mut notes: Vec<VstNote>) {
        notes.sort_unstable_by_key(VstNote::key);

        if self.vst_pending.capacity() < self.vst_pending.len() + notes.len() + 64 {
            self.vst_pending.reserve(notes.len() + 64);
        }
        if self.vst_log.capacity() < self.vst_log.len() + notes.len() + 64 {
            self.vst_log.reserve(notes.len() + 64);
        }

        const VST_LOG_MAX: usize = 10_000;
        if self.vst_log.len() + notes.len() > VST_LOG_MAX {
            let drop = ((self.vst_log.len() + notes.len()) - VST_LOG_MAX)
            .min(self.vst_log.len());
            self.vst_log.drain(..drop);
        }

        for mut n in notes {
            self.vst_log.push(n.clone());
            if n.end <= self.pos {
                continue;
            }
            if n.start < self.pos {
                n.start = self.pos;
            }
            let key = n.key();
            if self.vst_pending.back().map_or(true, |b| b.key() <= key) {
                self.vst_pending.push_back(n);
            } else {
                let idx = self
                .vst_pending
                .binary_search_by_key(&key, |p| p.key())
                .unwrap_or_else(|i| i);
                self.vst_pending.insert(idx, n);
            }
        }
    }

    // ─────────────── seek ───────────────

    pub(crate) fn apply_seek(&mut self, to: i64, voices: Vec<Scheduled>) {
        self.pos = to.clamp(0, self.total);
        self.active.clear();
        self.pending.clear();
        self.panic_hosts();
        self.vst_tail.clear();
        self.vst_pending.clear();
        self.vst_log.clear();
        self.vst_synced = true;
        self.vst_hist_head = i64::MIN;
        // Chip state: reset voices, drop pending/active notes, and reset
        // the chip players to a silent state. Seeking is a full re-entry;
        // any note that would have been mid-flight is restarted by the
        // loader's seek batch.
        self.chip_pending.clear();
        self.chip_active.clear();
        self.chip_log.clear();
        for p in self.chip_players.values_mut() {
            p.seek(to);
        }
        self.ready = true;
        self.ingest(voices);
    }

    pub(crate) fn handle(&mut self, m: Msg) {
        match m {
            Msg::Batch { epoch, samples, vsts, chips } => {
                if epoch == self.epoch {
                    self.ingest(samples);
                    self.ingest_vst(vsts);
                    self.ingest_chip(chips);
                }
            }
            Msg::Seek { epoch, pos, samples, vsts, chips } => {
                self.epoch = epoch;
                self.apply_seek(pos, samples);
                self.ingest_vst(vsts);
                self.ingest_chip(chips);
            }
        }
    }

    pub(crate) fn finished(&self) -> bool {
        self.ready && self.pos >= self.total
    }

    pub(crate) fn active_count(&self) -> usize {
        self.active.len() + self.chip_active.len()
    }

    pub(crate) fn render(&mut self, out_l: &mut [f32], out_r: &mut [f32], speed: i64) {
        vst::enable_ftz_daz();
        vst::set_realtime_priority();

        out_l.fill(0.0);
        out_r.fill(0.0);

        if !self.shims.is_empty() {
            let need = out_l.len();
            for buf in self.shim_scratch.values_mut() {
                if buf.0.len() < need {
                    buf.0.resize(need, 0.0);
                    buf.1.resize(need, 0.0);
                }
                buf.0[..need].fill(0.0);
                buf.1[..need].fill(0.0);
            }
        }

        if speed != 0 {
            self.pause_fade = 1.0;
            self.tail_frames = (self.sr * 1.0) as i64;
        } else if self.tail_frames <= 0 {
            return;
        }

        let block_len = out_l.len() as i64;
        let block_start = self.pos;
        let block_end = if speed != 0 { block_start + block_len * speed } else { block_start };

        let t_start = Instant::now();
        let mut lap_t = t_start;

        // 1. Sample Voices
        self.render_sample_voices(out_l, out_r, speed, block_start, block_end);
        perf::lap(&mut lap_t, &PERF.voices_ns);

        // 2. VSTs and SF2s
        if !self.hosts.is_empty() || !self.sf2_engines.is_empty() {
            if speed == 1 {
                if !self.vst_synced {
                    self.resync_vsts(block_start);
                }
                self.render_vsts(out_l, out_r, block_start);
                self.vst_hist_head = self.vst_hist_head.max(block_end - 1);
            } else if speed == 0 {
                self.vst_synced = false;
                self.render_vsts(out_l, out_r, block_start);
            } else {
                if self.vst_synced {
                    self.panic_hosts();
                    self.vst_synced = false;
                }
                self.render_vsts_history(out_l, out_r, block_start, speed);
            }
        }
        perf::lap(&mut lap_t, &PERF.vst_ns);

        // 3. Chip players (replaces the old VGM path).
        //    Same speed==1 constraint: chip players don't time-stretch.
        if !self.chip_players.is_empty() && speed == 1 {
            self.render_chips(out_l, out_r, block_start);
        }

        // 4. Shims
        if !self.shims.is_empty() {
            self.process_shims(out_l, out_r);
        }
        perf::lap(&mut lap_t, &PERF.shim_ns);

        // 5. Master FX
        sanitize(out_l);
        sanitize(out_r);

        if let Some(chain) = self.master_fx.as_mut() {
            if !chain.is_empty() {
                let mut i = 0;
                while i < out_l.len() {
                    let n = (out_l.len() - i).min(VST_BLOCK as usize);
                    chain.process(&mut out_l[i..i + n], &mut out_r[i..i + n]);
                    i += n;
                }
            }
        }

        sanitize(out_l);
        sanitize(out_r);
        perf::lap(&mut lap_t, &PERF.fx_ns);

        if speed != 1 && speed != 0 {
            let scale = 1.0 / (speed.unsigned_abs() as f32);
            for s in out_l.iter_mut() { *s *= scale; }
            for s in out_r.iter_mut() { *s *= scale; }
        }

        PERF.record_block(t_start.elapsed(), (block_len as f64 / self.sr * 1e9) as u64);

        if speed == 0 {
            self.tail_frames -= block_len;
            self.pause_fade = (self.pause_fade - (block_len as f32 / (0.1 * self.sr) as f32)).max(0.0);
            let fade_frames = (self.sr * 1.0) as i64;
            if self.tail_frames < fade_frames {
                let fade = (self.tail_frames as f32 / fade_frames as f32).clamp(0.0, 1.0);
                for i in 0..out_l.len() {
                    out_l[i] *= fade;
                    out_r[i] *= fade;
                }
            }
        } else {
            self.pos = block_end.clamp(0, self.total);
        }
    }
}
