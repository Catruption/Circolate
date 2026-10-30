//! Sample voice scheduling and rendering.

use super::*;
use crate::formats::trackscript::C5_HZ;
use crate::mixer::mute;
use crate::timing::AutomationLane;
use crate::player::project::Sf2VoiceEnvelope;
use dsp::{env_level_at, gains, interpolate, EnvState};

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
    pub(crate) prev_vol: f64,
}

impl Engine {
    pub(crate) fn raw_pos_at(&self, v: &Scheduled, to: i64) -> f64 {
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
        let block_len = out_l.len() as i64;
        let dir: i64 = if speed > 0 { 1 } else if speed < 0 { -1 } else { 0 };
        let dir_f = dir as f64;

        if speed != 0 {
            if speed > 0 {
                while self.pending.front().map_or(false, |v| v.start < block_end) {
                    let v = self.pending.pop_front().unwrap();
                    if !mute::is_note_active(&v.part_path, v.line as usize, v.source_row as i64) {
                        continue;
                    }
                    let raw_pos = v.offset;
                    let env_state = self.env_state_for(&v);
                    self.active.push(Active { v, raw_pos, env_state, prev_vol: f64::NAN });
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
                        if !mute::is_note_active(&v.part_path, v.line as usize, v.source_row as i64) {
                            continue;
                        }
                        let step = v.sample.rate * (v.pitch_hz / C5_HZ) / sr;
                        let note_len = (v.end - v.start).max(0) as f64;
                        let raw_pos = v.offset + (note_len - 1.0).max(0.0) * step;
                        self.active.push(Active { v, raw_pos, env_state: None, prev_vol: f64::NAN });
                    } else {
                        i += 1;
                    }
                }
            }
        }

        let song_volume = self.song_volume;
        let linear = self.linear;
        let pause_fade = self.pause_fade;

        let Self { active, shim_by_line, shim_scratch, .. } = self;
        active.retain_mut(|a| {
            if !mute::is_note_active(&a.v.part_path, a.v.line as usize, a.v.source_row as i64) {
                return false;
            }
            let v = &a.v;
            let smp = &*v.sample;
            let len = smp.len() as f64;
            let const_step = smp.rate * (v.pitch_hz / C5_HZ) / sr;
            let loop_f = match smp.loop_pts {
                Some((s, e)) if e > s => Some((s as f64, e as f64)),
                _ => None,
            };
            let automated_gain = v.vol_lane.is_some() || v.pan_lane.is_some();

            let release_sec = a.env_state.as_ref().map_or(0.020, |s| s.release_end - s.release_start);
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

            let part_vol = crate::mixer::part_volume::get(&v.part_path);
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
                let pv = pv0 + dpv * i as f64;
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
                    match &a.env_state {
                        Some(state) => {
                            let t_sec = (f - state.start_sample) as f64 / sr;
                            g * env_level_at(state, t_sec)
                        }
                        None => {
                            if f > v.end {
                                let over = ((f - v.end) as f64 / sr) as f32;
                                g * (1.0 - over / 0.020).clamp(0.0, 1.0)
                            } else {
                                g
                            }
                        }
                    }
                };

                if p >= 0.0 && p < len {
                    let (sl, sr_) = interpolate(smp, p, step, a.raw_pos);
                    let (gl, gr) = gain_pair();
                    let eg = env_gain();
                    let o = i as usize;
                    let cl = sl * gl as f32 * eg;
                    let cr = sr_ * gr as f32 * eg;
                    dst_l[o] += cl;
                    dst_r[o] += cr;
                } else if p >= len && loop_f.is_none() && len > 0.0 {
                    let over = p - len;
                    let fade_len = HOLD_TAIL_SECS * sr;
                    if over < fade_len {
                        let fade = (1.0 - (over / fade_len)).clamp(0.0, 1.0) as f32;
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