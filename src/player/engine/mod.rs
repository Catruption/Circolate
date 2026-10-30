//! Voice scheduling, mixer, and DSP primitives.

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
use crate::hosting::sf2::engine::Sf2Engine;
use crate::hosting::vst::ffi::VstHost;
use crate::player::project::{Project, Sample};
use crate::player::inserts;

// Re-exports so the rest of the app doesn't break
pub(crate) use voices::{Scheduled, Active};
pub(crate) use vst::HostSlot;
pub(crate) use shims::ShimHost;

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
    Batch { epoch: u64, samples: Vec<Scheduled>, vsts: Vec<VstNote> },
    Seek  { epoch: u64, pos: i64, samples: Vec<Scheduled>, vsts: Vec<VstNote> },
}

pub(crate) const VST_BLOCK: i64 = 512;
pub(crate) const VST_HISTORY_SECS: f64 = 10.0;
pub(crate) const HOLD_TAIL_SECS: f64 = 0.020;

/// How many blocks to keep processing a VST after its last note event.
/// 20 blocks × 512 samples @ 48kHz ≈ 213ms — covers most release
/// envelopes and short reverb tails, but skips hosts that have been
/// silent for a quarter second.  Larger values waste CPU on silent
/// plugins in dense projects; smaller values clip audible tails.
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

    pub(crate) fn host_count(&self) -> usize {
        self.hosts.len()
    }

    pub(crate) fn sf2_engine_count(&self) -> usize {
        self.sf2_engines.len()
    }

    pub(crate) fn install_master_fx(&mut self, chain: EffectChain) {
        self.master_fx = Some(chain);
    }

    pub(crate) fn master_fx_count(&self) -> usize {
        self.master_fx.as_ref().map_or(0, |c| c.len())
    }

    pub(crate) fn sample_rate(&self) -> f64 { self.sr }

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
        self.ready = true;
        self.ingest(voices);
    }

    pub(crate) fn handle(&mut self, m: Msg) {
        match m {
            Msg::Batch { epoch, samples, vsts } => {
                if epoch == self.epoch {
                    self.ingest(samples);
                    self.ingest_vst(vsts);
                }
            }
            Msg::Seek { epoch, pos, samples, vsts } => {
                self.epoch = epoch;
                self.apply_seek(pos, samples);
                self.ingest_vst(vsts);
            }
        }
    }

    pub(crate) fn finished(&self) -> bool {
        self.ready && self.pos >= self.total
    }

    pub(crate) fn active_count(&self) -> usize {
        self.active.len()
    }

    pub(crate) fn render(&mut self, out_l: &mut [f32], out_r: &mut [f32], speed: i64) {
        // Denormals make reverb/filter tails many times slower; this flag
        // is per-thread, so set it on whichever thread runs the callback.
        vst::enable_ftz_daz();
        // Also try to escalate this thread to real-time priority.  The
        // function is idempotent (only tries once per thread) so it's
        // safe to call on every render block.
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

        // 3. Shims
        if !self.shims.is_empty() {
            self.process_shims(out_l, out_r);
        }
        perf::lap(&mut lap_t, &PERF.shim_ns);

        // 4. Master FX
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

        // Tail logic
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
