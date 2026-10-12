//! Voice scheduling, mixer, and DSP primitives.

use std::collections::{BTreeMap, VecDeque};
use std::f64::consts::{FRAC_PI_4, PI};
use std::sync::{Arc, OnceLock};

use crate::effects::EffectChain;
use crate::sources::trackscript::C5_HZ;
use crate::timing::AutomationLane;
use crate::vst_host_ffi::VstHost;

use super::project::{Project, Sample};

// ── scheduled sample voice ─────────────────────────────────────────────

pub(crate) struct Scheduled {
    pub(crate) start: i64,
    pub(crate) end: i64,
    pub(crate) line: u32,
    pub(crate) seq: u32,
    pub(crate) sample: Arc<Sample>,
    pub(crate) pitch_hz: f64,
    pub(crate) offset: f64,
    pub(crate) vol: f64,
    pub(crate) pan: f64,
    pub(crate) pitch_lane: Option<AutomationLane>,
    pub(crate) vol_lane: Option<AutomationLane>,
    pub(crate) pan_lane: Option<AutomationLane>,
    pub(crate) const_gain: (f64, f64),
}

impl Scheduled {
    fn key(&self) -> (i64, u32, u32) {
        (self.start, self.line, self.seq)
    }
}

// ── scheduled VST note ─────────────────────────────────────────────────

/// A single MIDI note destined for one channel's VST host.
#[derive(Clone, Debug)]
pub(crate) struct VstNote {
    pub(crate) start: i64,
    pub(crate) end: i64,
    pub(crate) iid: u16,
    pub(crate) note: u8,
    pub(crate) velocity: u8,
    pub(crate) line: u32,
    pub(crate) seq: u32,
}

impl VstNote {
    fn key(&self) -> (i64, u32, u32) {
        (self.start, self.line, self.seq)
    }
}

// ── mixing maths ───────────────────────────────────────────────────────

#[inline]
pub(crate) fn gains(vol_pct: f64, pan: f64, song_volume: f64, linear: bool) -> (f64, f64) {
    let v = vol_pct * song_volume / 100.0;
    let pan = pan.clamp(-1.0, 1.0);
    if linear {
        (v * (1.0 - pan) * 0.5, v * (1.0 + pan) * 0.5)
    } else {
        let theta = (pan + 1.0) * FRAC_PI_4;
        (v * theta.cos(), v * theta.sin())
    }
}

// ── sinc interpolation ─────────────────────────────────────────────────

const SINC_PHASES: usize = 512;
const SINC_TAPS: usize = 8;
const SINC_TAPS_BEFORE: i64 = 3;

static SINC_UP: OnceLock<[[f64; SINC_TAPS]; SINC_PHASES]> = OnceLock::new();

#[inline]
fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 {
        1.0
    } else {
        let px = PI * x;
        px.sin() / px
    }
}

fn up_table() -> &'static [[f64; SINC_TAPS]; SINC_PHASES] {
    SINC_UP.get_or_init(|| {
        let mut t = [[0.0f64; SINC_TAPS]; SINC_PHASES];
        for p in 0..SINC_PHASES {
            let frac = p as f64 / SINC_PHASES as f64;
            let mut sum = 0.0;
            for k in 0..SINC_TAPS {
                let d = frac - (k as i64 - SINC_TAPS_BEFORE) as f64;
                let win = if d.abs() < 4.0 {
                    0.5 + 0.5 * (PI * d / 4.0).cos()
                } else {
                    0.0
                };
                t[p][k] = sinc(d) * win;
                sum += t[p][k];
            }
            if sum.abs() >= 1e-12 {
                for k in 0..SINC_TAPS {
                    t[p][k] /= sum;
                }
            }
        }
        t
    })
}

fn interpolate(smp: &Sample, pos: f64, step: f64, raw_pos: f64) -> (f32, f32) {
    let n = smp.len() as i64;
    let center = pos.floor() as i64;

    if step <= 1.0 {
        let frac = pos - center as f64;
        let phase = ((frac * SINC_PHASES as f64) as usize) & (SINC_PHASES - 1);
        let w = &up_table()[phase];

        let mut l = 0.0f64;
        let mut r = 0.0f64;
        for k in 0..SINC_TAPS {
            let mut i = center + (k as i64 - SINC_TAPS_BEFORE);
            if let Some((ls, le)) = smp.loop_pts {
                let ll = le - ls;
                if ll > 0 {
                    let wrap = i >= le || (raw_pos >= le as f64 && i < ls);
                    if wrap {
                        i = ls + (i - ls).rem_euclid(ll);
                    }
                }
            }
            let i = i.clamp(0, n - 1) as usize;
            l += smp.left[i] as f64 * w[k];
            if smp.stereo {
                r += smp.right[i] as f64 * w[k];
            }
        }
        if !smp.stereo {
            r = l;
        }
        return (l as f32, r as f32);
    }

    let mut i0 = center;
    let mut i1 = center + 1;
    if let Some((ls, le)) = smp.loop_pts {
        let ll = le - ls;
        if ll > 0 {
            let wrap0 = i0 >= le || (raw_pos >= le as f64 && i0 < ls);
            let wrap1 = i1 >= le || (raw_pos >= le as f64 && i1 < ls);
            if wrap0 {
                i0 = ls + (i0 - ls).rem_euclid(ll);
            }
            if wrap1 {
                i1 = ls + (i1 - ls).rem_euclid(ll);
            }
        }
    }
    let i0 = i0.clamp(0, n - 1) as usize;
    let i1 = i1.clamp(0, n - 1) as usize;
    let frac = pos - center as f64;

    let l0 = smp.left[i0] as f64;
    let l1 = smp.left[i1] as f64;
    let l = l0 + (l1 - l0) * frac;

    let r = if smp.stereo {
        let r0 = smp.right[i0] as f64;
        let r1 = smp.right[i1] as f64;
        r0 + (r1 - r0) * frac
    } else {
        l
    };

    (l as f32, r as f32)
}

// ── engine ─────────────────────────────────────────────────────────────

struct Active {
    v: Scheduled,
    raw_pos: f64,
}

pub(crate) enum Msg {
    Batch { epoch: u64, samples: Vec<Scheduled>, vsts: Vec<VstNote> },
    Seek  { epoch: u64, pos: i64, samples: Vec<Scheduled>, vsts: Vec<VstNote> },
}

/// Sample chunk used when driving VST hosts.  Notes are dispatched at
/// sample offsets within the chunk; the shim supports per-event offsets,
/// so this value controls the minimum scheduling granularity, not an
/// audible quantization.
const VST_BLOCK: i64 = 512;

/// Seconds of VST output retained for reverse playback.  Ten seconds of
/// 48 kHz stereo is about 3.8 MB.
const VST_HISTORY_SECS: f64 = 10.0;

pub(crate) struct Engine {
    sr: f64,
    song_volume: f64,
    linear: bool,
    total: i64,
    pub(crate) pos: i64,
    pub(crate) ready: bool,
    epoch: u64,
    pending: VecDeque<Scheduled>,
    active: Vec<Active>,

    // VST hosting — one instance per channel, keyed by channel iid.
    // BTreeMap (not HashMap) so iteration is sorted by iid and therefore
    // deterministic across calls.
    hosts: BTreeMap<u16, VstHost>,
    /// Per-host scratch, preallocated so the audio thread never allocates.
    host_scratch: BTreeMap<u16, (Vec<f32>, Vec<f32>)>,
    vst_pending: VecDeque<VstNote>,
    vst_active: Vec<VstNote>,
    vst_scratch_l: Vec<f32>,
    vst_scratch_r: Vec<f32>,

    /// Rolling stereo history of the summed VST output, indexed by
    /// absolute song sample position modulo the ring length.  Used to
    /// play plugin audio backward, since we can't run a plugin in
    /// reverse.
    vst_hist_l: Vec<f32>,
    vst_hist_r: Vec<f32>,
    /// The highest song position whose VST output has been recorded.
    /// `i64::MIN` means nothing has been recorded yet.
    vst_hist_head: i64,

    // Master bus effects — one chain, run on the fully-summed stereo mix.
    master_fx: Option<EffectChain>,
}

impl Engine {
    pub(crate) fn new(p: &Project) -> Self {
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
            hosts: BTreeMap::new(),
            host_scratch: BTreeMap::new(),
            vst_pending: VecDeque::new(),
            vst_active: Vec::new(),
            vst_scratch_l: vec![0.0; VST_BLOCK as usize],
            vst_scratch_r: vec![0.0; VST_BLOCK as usize],
            vst_hist_l: vec![0.0; hist_len],
            vst_hist_r: vec![0.0; hist_len],
            vst_hist_head: i64::MIN,
            master_fx: None,
        }
    }

    /// Install a live VST host for one channel.  Called by the loader
    /// after resolving the channel's `.instrument` sidecar.
    pub(crate) fn install_host(&mut self, iid: u16, host: VstHost) {
        self.hosts.insert(iid, host);
        self.host_scratch.insert(
            iid,
            (
                vec![0.0f32; VST_BLOCK as usize],
                vec![0.0f32; VST_BLOCK as usize],
            ),
        );
    }

    pub(crate) fn host_count(&self) -> usize {
        self.hosts.len()
    }

    /// Attach a master-bus effect chain.  Runs after all voices are summed.
    pub(crate) fn install_master_fx(&mut self, chain: EffectChain) {
        self.master_fx = Some(chain);
    }

    pub(crate) fn master_fx_count(&self) -> usize {
        self.master_fx.as_ref().map_or(0, |c| c.len())
    }

    pub(crate) fn sample_rate(&self) -> f64 { self.sr }

    fn raw_pos_at(&self, v: &Scheduled, to: i64) -> f64 {
        if to <= v.start {
            return v.offset;
        }
        match &v.pitch_lane {
            None => {
                v.offset
                + (to - v.start) as f64
                * (v.sample.rate * (v.pitch_hz / C5_HZ) / self.sr)
            }
            Some(lane) => {
                let mut p = v.offset;
                for f in v.start..to {
                    p += v.sample.rate
                    * (lane.value_at(f as f64 / self.sr) / C5_HZ)
                    / self.sr;
                }
                p
            }
        }
    }

    pub(crate) fn ingest(&mut self, mut voices: Vec<Scheduled>) {
        voices.sort_by_key(Scheduled::key);
        let mut future = Vec::new();
        for v in voices {
            if v.end <= self.pos {
                continue;
            }
            if v.start < self.pos {
                let raw_pos = self.raw_pos_at(&v, self.pos);
                self.active.push(Active { v, raw_pos });
            } else {
                future.push(v);
            }
        }
        if !future.is_empty() {
            let mut all: Vec<Scheduled> = self.pending.drain(..).collect();
            all.extend(future);
            all.sort_by_key(Scheduled::key);
            self.pending = all.into();
        }
    }

    /// Queue VST notes.  Notes that start before `pos` are pinned to `pos`
    /// so their `note_on` fires at seek time and the plugin plays from the
    /// seek point.
    pub(crate) fn ingest_vst(&mut self, mut notes: Vec<VstNote>) {
        notes.sort_by_key(VstNote::key);
        let mut future = Vec::new();
        for mut n in notes {
            if n.end <= self.pos {
                continue;
            }
            if n.start < self.pos {
                n.start = self.pos;
                self.vst_active.push(n);
            } else {
                future.push(n);
            }
        }
        if !future.is_empty() {
            let mut all: Vec<VstNote> = self.vst_pending.drain(..).collect();
            all.extend(future);
            all.sort_by_key(VstNote::key);
            self.vst_pending = all.into();
        }
    }

    pub(crate) fn apply_seek(&mut self, to: i64, voices: Vec<Scheduled>) {
        self.pos = to.clamp(0, self.total);
        self.active.clear();
        self.pending.clear();
        self.vst_active.clear();
        self.vst_pending.clear();
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
        out_l.fill(0.0);
        out_r.fill(0.0);

        if speed == 0 {
            return;
        }

        let block_len = out_l.len() as i64;
        let block_start = self.pos;
        let block_end = block_start + block_len * speed;
        let sr = self.sr;
        let dir: i64 = if speed > 0 { 1 } else { -1 };
        let dir_f = dir as f64;

        // ── Voice activation ─────────────────────────────────────────
        //
        // Forward: pop pending voices whose start we've crossed as we
        // move up through time.
        // Reverse: pop pending voices whose *end* we've crossed as we
        // move down through time.
        if speed > 0 {
            while self.pending.front().map_or(false, |v| v.start < block_end) {
                let v = self.pending.pop_front().unwrap();
                let raw_pos = v.offset;
                self.active.push(Active { v, raw_pos });
            }
        } else {
            let mut i = 0;
            while i < self.pending.len() {
                let activate = {
                    let v = &self.pending[i];
                    v.end <= block_start && v.end > block_end
                };
                if activate {
                    let v = self.pending.remove(i).unwrap();
                    let step = v.sample.rate * (v.pitch_hz / C5_HZ) / sr;
                    let note_len = (v.end - v.start).max(0) as f64;
                    let raw_pos = v.offset + (note_len - 1.0).max(0.0) * step;
                    self.active.push(Active { v, raw_pos });
                } else {
                    i += 1;
                }
            }
        }

        // ── Sample voices ────────────────────────────────────────────
        let song_volume = self.song_volume;
        let linear = self.linear;

        self.active.retain_mut(|a| {
            let v = &a.v;
            let smp = &*v.sample;
            let len = smp.len() as f64;
            let const_step = smp.rate * (v.pitch_hz / C5_HZ) / sr;
            let loop_f = match smp.loop_pts {
                Some((s, e)) if e > s => Some((s as f64, e as f64)),
                               _ => None,
            };
            let automated_gain = v.vol_lane.is_some() || v.pan_lane.is_some();

            // Output-sample range where this voice is active.
            let (i_lo, i_hi) = if speed > 0 {
                let lo = ((v.start - block_start) as f64 / speed as f64)
                    .ceil()
                    .max(0.0) as i64;
                let hi = ((v.end - block_start) as f64 / speed as f64)
                    .ceil()
                    .min(block_len as f64) as i64;
                (lo, hi)
            } else {
                let lo = (((v.end - block_start) as f64 / speed as f64)
                    .floor() as i64)
                    + 1;
                let hi = (((v.start - block_start) as f64 / speed as f64)
                    .floor() as i64)
                    + 1;
                (lo.max(0), hi.min(block_len))
            };

            for i in i_lo..i_hi {
                let f = block_start + i * speed;
                let t = f as f64 / sr;
                let step = match &v.pitch_lane {
                    Some(l) => smp.rate * (l.value_at(t) / C5_HZ) / sr,
                               None => const_step,
                };

                let mut p = a.raw_pos;
                if let Some((ls, le)) = loop_f {
                    let ll = le - ls;
                    if ll > 0.0 {
                        if p >= le {
                            p = ls + (p - le).rem_euclid(ll);
                        } else if p < ls {
                            p = le - (ls - p).rem_euclid(ll);
                        }
                    }
                }

                if p >= 0.0 && p < len {
                    let (sl, sr_) = interpolate(smp, p, step, a.raw_pos);
                    let (gl, gr) = if automated_gain {
                        let vol = v.vol_lane.as_ref().map_or(v.vol, |l| l.value_at(t));
                        let pan = v.pan_lane.as_ref().map_or(v.pan, |l| l.value_at(t));
                        gains(vol, pan, song_volume, linear)
                    } else {
                        v.const_gain
                    };
                    let o = i as usize;
                    out_l[o] += sl * gl as f32;
                    out_r[o] += sr_ * gr as f32;
                }

                a.raw_pos += step * dir_f;
            }

            // Retain if we haven't yet crossed the voice's far boundary
            // in the current traversal direction.
            if speed > 0 {
                v.end > block_end
            } else {
                block_end > v.start
            }
        });

        // ── VST voices ───────────────────────────────────────────────
        //
        // Forward: run the plugins and record their output into a
        // rolling history ring.  Reverse: read the ring backward, so
        // plugin audio plays in reverse too.  Notes still get a
        // note-off when we enter reverse so a plugin isn't left
        // sustaining on stale input.
        if !self.hosts.is_empty() {
            if speed > 0 {
                self.render_vsts(out_l, out_r, block_start);
                self.vst_hist_head = self.vst_hist_head.max(block_end - 1);
            } else {
                self.render_vsts_reverse(out_l, out_r, block_start, speed);
            }
        }

        // ── Master bus effects ───────────────────────────────────────
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

        // Clamp into the song's time range.  Going backward stops at 0;
        // forward stops at `total` and the caller sees `finished()`.
        self.pos = block_end.clamp(0, self.total);
    }

    /// Drive every installed VST host for this block.  Chunked because the
    /// shim was created with a fixed max block size; notes are dispatched
    /// with sample offsets so timing stays exact within the chunk.
    fn render_vsts(&mut self, out_l: &mut [f32], out_r: &mut [f32], block_start: i64) {
        let block_len = out_l.len() as i64;
        let mut chunk_start = block_start;
        let block_end = block_start + block_len;

        // Destructure `self` so the borrow checker can see that
        // `hosts`, `host_scratch`, `vst_scratch_*`, `vst_hist_*`,
        // `vst_pending` and `vst_active` are disjoint.
        let Self {
            hosts,
            host_scratch,
            vst_pending,
            vst_active,
            vst_scratch_l,
            vst_scratch_r,
            vst_hist_l,
            vst_hist_r,
            ..
        } = self;

        while chunk_start < block_end {
            let chunk_len =
            ((block_end - chunk_start) as usize).min(VST_BLOCK as usize);
            let chunk_end = chunk_start + chunk_len as i64;

            // Note-ons whose start falls in this chunk.
            while vst_pending
                .front()
                .map_or(false, |n| n.start < chunk_end)
                {
                    let n = vst_pending.pop_front().unwrap();
                    if let Some(host) = hosts.get_mut(&n.iid) {
                        let offset = (n.start - chunk_start).max(0) as i32;
                        host.note_on_at(n.note, n.velocity, offset);
                    }
                    vst_active.push(n);
                }

                // Note-offs for anything ending in this chunk.
                let mut i = 0;
                while i < vst_active.len() {
                    if vst_active[i].end <= chunk_end {
                        let n = vst_active.remove(i);
                        if let Some(host) = hosts.get_mut(&n.iid) {
                            let offset = (n.end - chunk_start).max(0) as i32;
                            host.note_off_at(n.note, offset);
                        }
                    } else {
                        i += 1;
                    }
                }

                // Zero the summed scratch for this chunk.
                for s in vst_scratch_l[..chunk_len].iter_mut() {
                    *s = 0.0;
                }
                for s in vst_scratch_r[..chunk_len].iter_mut() {
                    *s = 0.0;
                }

                // Process each host into its own scratch, record its
                // output for reverse playback, push the mono mix to the
                // TUI scope, then sum into the shared scratch.
                for (iid, host) in hosts.iter_mut() {
                    let Some((tl, tr)) = host_scratch.get_mut(iid) else { continue };
                    for s in tl[..chunk_len].iter_mut() { *s = 0.0; }
                    for s in tr[..chunk_len].iter_mut() { *s = 0.0; }
                    host.process(&mut tl[..chunk_len], &mut tr[..chunk_len]);

                    let slot = *iid as usize;
                    for j in 0..chunk_len {
                        let mono = (tl[j] + tr[j]) * 0.5;
                        crate::tui::scope_push(slot, mono);
                    }
                    for j in 0..chunk_len {
                        vst_scratch_l[j] += tl[j];
                        vst_scratch_r[j] += tr[j];
                    }
                }

                // Record the fully-summed VST mix for reverse playback.
                // Done after all hosts have been summed so the ring holds
                // the mix, not whichever host happened to go last.
                if !vst_hist_l.is_empty() {
                    for j in 0..chunk_len {
                        let pos = chunk_start + j as i64;
                        let idx = pos.rem_euclid(vst_hist_l.len() as i64) as usize;
                        vst_hist_l[idx] = vst_scratch_l[j];
                        vst_hist_r[idx] = vst_scratch_r[j];
                    }
                }

                // Sum scratch into the output at the right offset.
                let out_off = (chunk_start - block_start) as usize;
                for j in 0..chunk_len {
                    out_l[out_off + j] += vst_scratch_l[j];
                    out_r[out_off + j] += vst_scratch_r[j];
                }

                chunk_start = chunk_end;
        }
    }

    /// Play the recorded VST history backward.  Used when the transport
    /// speed is negative.  Each output sample corresponds to song
    /// position `block_start + i * speed`; if that position has been
    /// recorded, its stereo value is summed into the output, otherwise
    /// the sample is silent.
    fn render_vsts_reverse(
        &mut self,
        out_l: &mut [f32],
        out_r: &mut [f32],
        block_start: i64,
        speed: i64,
    ) {
        let head = self.vst_hist_head;
        if head == i64::MIN {
            return;
        }
        let len = self.vst_hist_l.len() as i64;
        if len == 0 {
            return;
        }
        let frames = out_l.len() as i64;
        for i in 0..frames {
            let pos = block_start + i * speed;
            // Valid history range: [head - len + 1, head].
            if pos > head || pos <= head - len {
                continue;
            }
            let idx = pos.rem_euclid(len) as usize;
            out_l[i as usize] += self.vst_hist_l[idx];
            out_r[i as usize] += self.vst_hist_r[idx];
        }
    }
}