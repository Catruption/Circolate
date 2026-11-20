//! Sample voice scheduling and rendering.

use super::*;
use crate::formats::halotext::parse::C5_HZ;
use crate::timing::AutomationLane;
use crate::player::project::Sf2VoiceEnvelope;
use crate::formats::flp::slicex::speed_mult;
use crate::formats::flp::slicex::envelope_state::EnvState as SlicexEnvState;
use crate::player::types::SlicexInstrument;
use dsp::{env_level_at, gains, interpolate, Biquad, EnvState};

pub(crate) struct SlicexVoiceRef {
    pub(crate) instrument: Arc<SlicexInstrument>,
    pub(crate) slice_idx: usize,
}

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
    pub(crate) part_path: String,
    pub(crate) source_row: f64,
    pub(crate) env: Option<Sf2VoiceEnvelope>,
    pub(crate) slicex: Option<SlicexVoiceRef>,
}

impl Scheduled {
    pub(crate) fn key(&self) -> (i64, u32, u32) {
        (self.start, self.line, self.seq)
    }
}

pub(crate) struct Active {
    pub(crate) v: Scheduled,
    pub(crate) raw_pos: f64,
    pub(crate) env_state: Option<EnvState>,
    pub(crate) slicex_env_state: Option<SlicexEnvState>,
    pub(crate) slicex_release_sec: f64,
    pub(crate) prev_vol: f64,
    pub(crate) filt_l: Biquad,
    pub(crate) filt_r: Biquad,
    pub(crate) filt_cooldown: u32,
}

impl Active {
    pub(crate) fn new(v: Scheduled, raw_pos: f64, env_state: Option<EnvState>) -> Self {
        let (slicex_env_state, slicex_release_sec) = v.slicex.as_ref()
        .and_then(|sx| sx.instrument.amp_env.as_ref().map(|env| (
            SlicexEnvState::new(env, sx.instrument.amp_sustain_index, sx.instrument.amp_release_index),
                                                                 SlicexEnvState::release_duration(env, sx.instrument.amp_release_index),
        )))
        .map_or((None, 0.020), |(state, release)| (Some(state), release));
        Self {
            v,
            raw_pos,
            env_state,
            slicex_env_state,
            slicex_release_sec,
            prev_vol: f64::NAN,
            filt_l: Biquad::new(),
            filt_r: Biquad::new(),
            filt_cooldown: 0,
        }
    }
}

impl Engine {
    pub(crate) fn raw_pos_at(&self, v: &Scheduled, to: i64) -> f64 {
        if to <= v.start {
            return v.offset;
        }
        let speed_env = v.slicex.as_ref().and_then(|s| s.instrument.speed_env.as_ref());
        let octaves = v.slicex.as_ref().map_or(0.0, |s| s.instrument.speed_octaves);
        let amt = v.slicex.as_ref().map_or(1.0, |s| s.instrument.speed_amt);

        let root_hz = match v.slicex.as_ref() {
            Some(sx) => {
                let sl = &sx.instrument.slices[sx.slice_idx];
                440.0 * 2f64.powf((sl.key as f64 - 69.0) / 12.0)
            }
            None => C5_HZ,
        };

        match (&v.pitch_lane, speed_env) {
            (None, None) => {
                v.offset + (to - v.start) as f64 * (v.sample.rate * (v.pitch_hz / root_hz) / self.sr)
            }
            _ => {
                let mut p = v.offset;
                for f in v.start..to {
                    let t = f as f64 / self.sr;
                    let t_rel = (f - v.start) as f64 / self.sr;
                    let pitch_m = match &v.pitch_lane {
                        Some(l) => l.value_at(t) / root_hz,
                        None => v.pitch_hz / root_hz,
                    };
                    let rate_m = match speed_env {
                        Some(e) => {
                            let y = e.eval_at(t_rel);
                            let m = speed_mult(y, octaves);
                            1.0 + (m - 1.0) * amt
                        }
                        None => 1.0,
                    };
                    p += v.sample.rate * pitch_m * rate_m / self.sr;
                }
                p
            }
        }
    }

    pub(crate) fn env_state_for(&self, v: &Scheduled) -> Option<EnvState> {
        let env = v.env?;
        let sr = self.sr;
        let note_dur_sec = ((v.end - v.start) as f64 / sr).max(0.0);

        let delay_sec = env.delay_sec.max(0.0) as f64;
        let attack_end = env.attack_sec as f64;
        let hold_end = attack_end + env.hold_sec as f64;
        let decay_end = hold_end + env.decay_sec as f64;

        let release_sec = (env.release_sec as f64).max(0.020);
        let release_start = note_dur_sec;
        let release_end = note_dur_sec + release_sec;

        let attack_inv = if env.attack_sec > 0.0 { 1.0 / env.attack_sec } else { 0.0 };
        let decay_inv = if env.decay_sec > 0.0 { 1.0 / env.decay_sec } else { 0.0 };
        let release_inv = (1.0 / release_sec) as f32;

        let peak = env.peak.clamp(0.0, 1.0);
        let sustain = peak * env.sustain_fraction.clamp(0.0, 1.0);

        Some(EnvState {
            delay_sec,
            attack_end,
            hold_end,
            decay_end,
            release_start,
            release_end,
            peak,
            sustain,
            attack_inv,
            decay_inv,
            release_inv,
            start_sample: v.start,
            release_level: 0.0,
        }
        .with_release_level())
    }

    pub(crate) fn render_sample_voices(
        &mut self,
        out_l: &mut [f32],
        out_r: &mut [f32],
        speed: i64,
        block_start: i64,
        block_end: i64,
    ) {
        let sr = self.sr;
        let sr_f32 = sr as f32;
        let block_len = out_l.len() as i64;
        let dir: i64 = if speed > 0 { 1 } else if speed < 0 { -1 } else { 0 };
        let dir_f = dir as f64;

        let disable_cut_env = std::env::var_os("SLICEX_NO_CUT_ENV").is_some();

        if speed != 0 {
            if speed > 0 {
                while self.pending.front().map_or(false, |v| v.start < block_end) {
                    let v = self.pending.pop_front().unwrap();
                    if !self.mixer.is_note_active(&v.part_path, v.line as usize, v.source_row as i64) {
                        continue;
                    }
                    let raw_pos = v.offset;
                    let env_state = self.env_state_for(&v);
                    self.active.push(Active::new(v, raw_pos, env_state));
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
                        if !self.mixer.is_note_active(&v.part_path, v.line as usize, v.source_row as i64) {
                            continue;
                        }
                        let root_hz = match v.slicex.as_ref() {
                            Some(sx) => {
                                let sl = &sx.instrument.slices[sx.slice_idx];
                                440.0 * 2f64.powf((sl.key as f64 - 69.0) / 12.0)
                            }
                            None => C5_HZ,
                        };
                        let step = v.sample.rate * (v.pitch_hz / root_hz) / sr;
                        let note_len = (v.end - v.start).max(0) as f64;
                        let raw_pos = v.offset + (note_len - 1.0).max(0.0) * step;
                        self.active.push(Active::new(v, raw_pos, None));
                    } else {
                        i += 1;
                    }
                }
            }
        }

        let song_volume = self.song_volume;
        let linear = self.linear;
        let pause_fade = self.pause_fade;

        let Self { active, shim_by_line, shim_scratch, mixer, .. } = self;
        active.retain_mut(|a| {
            if !mixer.is_note_active(&a.v.part_path, a.v.line as usize, a.v.source_row as i64) {
                return false;
            }
            let v = &a.v;
            let smp = &*v.sample;
            let len = smp.len() as f64;

            let root_hz = match v.slicex.as_ref() {
                Some(sx) => {
                    let sl = &sx.instrument.slices[sx.slice_idx];
                    440.0 * 2f64.powf((sl.key as f64 - 69.0) / 12.0)
                }
                None => C5_HZ,
            };
            let const_step = smp.rate * (v.pitch_hz / root_hz) / sr;

            let (play_lo, play_hi) = match v.slicex.as_ref() {
                Some(sx) => {
                    let sl = &sx.instrument.slices[sx.slice_idx];
                    (sl.frame_start as f64, sl.frame_end as f64)
                }
                None => (0.0, len),
            };

            let loop_f = if let Some(sx) = v.slicex.as_ref() {
                let sl = &sx.instrument.slices[sx.slice_idx];
                if sx.instrument.loop_flag && sl.frame_end > sl.frame_start {
                    Some((sl.frame_start as f64, sl.frame_end as f64))
                } else {
                    None
                }
            } else {
                match smp.loop_pts {
                    Some((s, e)) if e > s => Some((s as f64, e as f64)),
                          _ => None,
                }
            };

            let automated_gain = v.vol_lane.is_some() || v.pan_lane.is_some();

            let release_sec = if v.slicex.is_some() {
                a.slicex_release_sec
            } else {
                a.env_state.as_ref().map_or(0.020, |s| s.release_end - s.release_start)
            };
            let actual_end = v.end + (release_sec * sr) as i64;

            let (i_lo, i_hi) = if speed > 0 {
                let lo = ((v.start - block_start) as f64 / speed as f64)
                .ceil()
                .max(0.0) as i64;
                let hi = ((actual_end - block_start) as f64 / speed as f64)
                .ceil()
                .min(block_len as f64) as i64;
                (lo, hi)
            } else if speed < 0 {
                let lo = (((v.end - block_start) as f64 / speed as f64)
                .floor() as i64) + 1;
                let hi = (((v.start - block_start) as f64 / speed as f64)
                .floor() as i64) + 1;
                (lo.max(0), hi.min(block_len))
            } else {
                (0, block_len)
            };

            let first_shim: Option<u32> =
            shim_by_line.get(&v.line).and_then(|s| s.first().copied());

            let part_vol = mixer.volume(&v.part_path);
            let pv0 = if a.prev_vol.is_nan() { part_vol } else { a.prev_vol };
            let dpv = (part_vol - pv0) / block_len.max(1) as f64;

            let shim_buf = first_shim.and_then(|id| shim_scratch.get_mut(&id));
            let (dst_l, dst_r): (&mut [f32], &mut [f32]) = match shim_buf {
                Some(buf) if buf.0.len() >= block_len as usize => {
                    (&mut buf.0[..], &mut buf.1[..])
                }
                _ => (&mut out_l[..], &mut out_r[..]),
            };

            for i in i_lo..i_hi {
                let f = block_start + i * speed;
                let t = f as f64 / sr;
                let t_rel = (f - v.start) as f64 / sr;
                let pv = pv0 + dpv * i as f64;

                let slicex_amp_value = if let (Some(sx), Some(state)) =
                (v.slicex.as_ref(), a.slicex_env_state.as_mut()) {
                    if f >= v.end {
                        state.note_off(&sx.instrument.amp_env.as_ref().expect("state requires amp envelope"));
                    }
                    state.advance(&sx.instrument.amp_env.as_ref().expect("state requires amp envelope"), 1.0 / sr);
                    Some(state.value())
                } else {
                    None
                };

                let speed_m = match v.slicex.as_ref() {
                    Some(sx) => match sx.instrument.speed_env.as_ref() {
                        Some(env) => {
                            let y = env.eval_at(t_rel);
                            let m = speed_mult(y, sx.instrument.speed_octaves);
                            1.0 + (m - 1.0) * sx.instrument.speed_amt
                        }
                        None => 1.0,
                    },
                    None => 1.0,
                };

                let step = match &v.pitch_lane {
                    Some(l) => smp.rate * (l.value_at(t) / root_hz) / sr * speed_m,
                          None => const_step * speed_m,
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

                let gain_pair = || -> (f64, f64) {
                    let (gl, gr) = if automated_gain {
                        let vol = v.vol_lane.as_ref().map_or(v.vol, |l| l.value_at(t));
                        let pan = v.pan_lane.as_ref().map_or(v.pan, |l| l.value_at(t));
                        gains(vol, pan, song_volume, linear)
                    } else {
                        v.const_gain
                    };
                    (gl * pv, gr * pv)
                };

                let env_gain = || -> f32 {
                    let mut g = 1.0;
                    if speed == 0 {
                        g *= pause_fade;
                    }
                    if let Some(sx) = v.slicex.as_ref() {
                        if let Some(y) = slicex_amp_value {
                            let mix = 1.0 + (y - 1.0) * sx.instrument.amp_amt;
                            g *= mix as f32;
                            if f > v.end && sx.instrument.amp_release_index < 0 {
                                let over = ((f - v.end) as f64 / sr) as f32;
                                g *= (1.0_f32 - over / 0.020_f32).clamp(0.0, 1.0);
                            }
                        } else if f > v.end {
                            let over = ((f - v.end) as f64 / sr) as f32;
                            g *= (1.0_f32 - over / 0.020_f32).clamp(0.0, 1.0);
                        }
                        return g;
                    }
                    match &a.env_state {
                        Some(state) => {
                            let t_sec = (f - state.start_sample) as f64 / sr;
                            g * env_level_at(state, t_sec)
                        }
                        None => {
                            if f > v.end {
                                let over = ((f - v.end) as f64 / sr) as f32;
                                g * (1.0_f32 - over / 0.020_f32).clamp(0.0, 1.0)
                            } else {
                                g
                            }
                        }
                    }
                };

                if p >= play_lo && p < play_hi {
                    let (mut sl, mut sr_) = interpolate(smp, p, step, a.raw_pos);

                    if let Some(sx) = v.slicex.as_ref() {
                        let sl_slice = &sx.instrument.slices[sx.slice_idx];
                        let cutoff_raw = sl_slice.cutoff;
                        let has_cut_env = !disable_cut_env
                        && sx.instrument.cut_env.is_some()
                        && sx.instrument.cut_amt > 0.0;
                        // FIXED: Only filter if the cutoff is below max, or if there is an active cut envelope.
                        let needs_filter = cutoff_raw < 12799 || has_cut_env;
                        if needs_filter {
                            let base_norm = (cutoff_raw as f64 / 12800.0).clamp(0.0, 1.0);
                            // Changed mapping to be less aggressive than 100 * 200^x.
                            // Maps 0->20Hz, 12800->20000Hz.
                            let base_hz = (20.0 * 1000.0f64.powf(base_norm)) as f32;
                            let mod_hz = if has_cut_env {
                                let ce = sx.instrument.cut_env.as_ref().unwrap();
                                let y = ce.eval_at(t_rel).clamp(0.0, 1.0);
                                let amount = sx.instrument.cut_amt.clamp(0.0, 1.0);
                                let mult = 2f64.powf((y - 0.5) * 4.0 * amount);
                                (base_hz as f64 * mult).clamp(20.0, sr * 0.49) as f32
                            } else {
                                base_hz
                            };
                            if a.filt_cooldown == 0 {
                                a.filt_l.set_lowpass(mod_hz, 0.707, sr_f32);
                                a.filt_r.set_lowpass(mod_hz, 0.707, sr_f32);
                                a.filt_cooldown = 32;
                            } else {
                                a.filt_cooldown -= 1;
                            }
                            sl = a.filt_l.process(sl);
                            sr_ = a.filt_r.process(sr_);
                        }
                    }

                    let (gl, gr) = gain_pair();
                    let eg = env_gain();
                    let o = i as usize;
                    let cl = sl * gl as f32 * eg;
                    let cr = sr_ * gr as f32 * eg;
                    dst_l[o] += cl;
                    dst_r[o] += cr;
                } else if v.slicex.is_none() && p >= len && loop_f.is_none() && len > 0.0 {
                    let over = p - len;
                    let fade_len = HOLD_TAIL_SECS * sr;
                    if over < fade_len {
                        let fade = (1.0_f64 - (over / fade_len)).clamp(0.0, 1.0) as f32;
                        let last_i = (len as usize).saturating_sub(1);
                        let sl = smp.left[last_i];
                        let sr_ = if smp.stereo { smp.right[last_i] } else { sl };
                        let (gl, gr) = gain_pair();
                        let eg = env_gain();
                        let o = i as usize;
                        let cl = sl * gl as f32 * eg * fade;
                        let cr = sr_ * gr as f32 * eg * fade;
                        dst_l[o] += cl;
                        dst_r[o] += cr;
                    }
                }

                a.raw_pos += step * dir_f;
            }
            a.prev_vol = part_vol;

            if speed > 0 {
                actual_end > block_end
            } else if speed < 0 {
                block_end > v.start
            } else {
                true
            }
        });
    }
}
