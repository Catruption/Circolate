//! Runtime data types shared across the player submodules.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::sf2::{attenuation_cb_to_linear, timecents_to_secs, Sf2File};
use crate::sources::trackscript::C5_HZ;

// ── samples ────────────────────────────────────────────────────────────

pub(crate) struct Sample {
    pub(crate) left: Vec<f32>,
    pub(crate) right: Vec<f32>,
    pub(crate) stereo: bool,
    pub(crate) rate: f64,
    pub(crate) loop_pts: Option<(i64, i64)>,
}

impl Sample {
    pub(crate) fn len(&self) -> usize {
        self.left.len()
    }
}

// ── VST instruments ────────────────────────────────────────────────────

pub(crate) struct VstInstrument {
    pub(crate) iid: u16,
    pub(crate) plugin_path: PathBuf,
    pub(crate) state_bytes: Vec<u8>,
    pub(crate) fallback: bool,
    pub(crate) gain: f32,
}

// ── SF2 instruments ────────────────────────────────────────────────────

#[derive(Clone, Copy)]
pub(crate) struct Sf2VoiceEnvelope {
    pub peak: f32,
    pub attack_sec: f32,
    pub hold_sec: f32,
    pub decay_sec: f32,
    pub sustain_fraction: f32,
    pub release_sec: f32,
}

impl Default for Sf2VoiceEnvelope {
    fn default() -> Self {
        Sf2VoiceEnvelope {
            peak: 1.0,
            attack_sec: 0.0,
            hold_sec: 0.0,
            decay_sec: 0.0,
            sustain_fraction: 1.0,
            release_sec: 0.0,
        }
    }
}

pub(crate) struct Sf2VoiceSpec {
    pub sample_id: u32,
    pub pitch_hz: f64,
    pub env: Sf2VoiceEnvelope,
}

struct Sf2ZoneEntry {
    key_lo: u8,
    key_hi: u8,
    vel_lo: u8,
    vel_hi: u8,
    sample_id: u32,
    root_key: u8,
    tune_cents: i16,
    env: Sf2VoiceEnvelope,
}

pub(crate) struct Sf2Instrument {
    zones: Vec<Sf2ZoneEntry>,
}

impl Sf2Instrument {
    pub(crate) fn zone_count_hint(&self) -> u32 {
        self.zones.len() as u32
    }

    pub(crate) fn resolve(&self, key: u8, vel: u8) -> Option<Sf2VoiceSpec> {
        let key_dist = |z: &Sf2ZoneEntry| -> i32 {
            if key < z.key_lo {
                (z.key_lo - key) as i32
            } else if key > z.key_hi {
                (key - z.key_hi) as i32
            } else {
                0
            }
        };

        let mut best: Option<(&Sf2ZoneEntry, i32)> = None;
        for z in self.zones.iter().rev() {
            if vel < z.vel_lo || vel > z.vel_hi {
                continue;
            }
            let d = key_dist(z);
            if d == 0 {
                best = Some((z, 0));
                break;
            }
            if best.map_or(true, |(_, bd)| d < bd) {
                best = Some((z, d));
            }
        }
        if best.is_none() {
            for z in self.zones.iter().rev() {
                let d = key_dist(z);
                if best.map_or(true, |(_, bd)| d < bd) {
                    best = Some((z, d));
                }
            }
        }
        let (z, _) = best?;
        let semitones =
            (key as i32 - z.root_key as i32) as f64 + z.tune_cents as f64 / 100.0;
        let pitch_hz = C5_HZ * 2f64.powf(semitones / 12.0);

        let vel_cb = ((127 - vel) as f32) * 960.0 / 127.0;
        let vel_gain = (10f32).powf(-vel_cb / 200.0);
        let peak = (z.env.peak * vel_gain).clamp(0.0, 1.0);

        let mut env = z.env;
        env.peak = peak;

        Some(Sf2VoiceSpec {
            sample_id: z.sample_id,
            pitch_hz,
            env,
        })
    }
}

pub(crate) fn build_sf2_instrument(
    bytes: &[u8],
    bank: u16,
    preset: u16,
    base_id: u32,
    samples: &mut HashMap<u32, Arc<Sample>>,
) -> anyhow::Result<Sf2Instrument> {
    let sf2 = match Sf2File::load_preset_bytes(bytes, bank, preset) {
        Ok(f) => f,
        Err(e) => {
            if std::env::var_os("FLP_DEBUG").is_some() {
                eprintln!(
                    "sf2: bank {} preset {} not found, falling back to all zones ({:#})",
                    bank, preset, e
                );
            }
            Sf2File::load_bytes(bytes)?
        }
    };
    let mut zones: Vec<Sf2ZoneEntry> = Vec::with_capacity(sf2.zones.len());

    for (zi, z) in sf2.zones.iter().enumerate() {
        let sm = &sf2.samples[z.sample_idx];
        let start = (sm.start as i64 + z.start_offset as i64).clamp(0, sm.end as i64);
        let end = (sm.end as i64 + z.end_offset as i64)
            .clamp(start, sf2.pcm.len() as i64);
        let ls = (sm.loop_start as i64 + z.loop_start_offset as i64).clamp(start, end);
        let le = (sm.loop_end as i64 + z.loop_end_offset as i64).clamp(ls, end);

        let mono: Vec<f32> = sf2.pcm[start as usize..end as usize]
            .iter()
            .map(|&s| s as f32 / 32768.0)
            .collect();

        let loop_pts = if sm.loop_mode != 0 && le > ls {
            Some(((ls - start) as i64, (le - start) as i64))
        } else {
            None
        };

        let arc = Arc::new(Sample {
            left: mono,
            right: Vec::new(),
            stereo: false,
            rate: sm.rate as f64,
            loop_pts,
        });
        let sid = base_id + zi as u32;
        samples.insert(sid, arc);

        let root_key = z.root_override.unwrap_or(sm.root_key);

        let mut attack_sec = timecents_to_secs(z.attack_timecents);
        let mut release_sec = timecents_to_secs(z.release_timecents);
        if attack_sec < 0.003 {
            attack_sec = 0.003;
        }
        if release_sec < 0.05 {
            release_sec = 0.05;
        }
        let env = Sf2VoiceEnvelope {
            peak: attenuation_cb_to_linear(z.initial_attenuation_cb),
            attack_sec,
            hold_sec: timecents_to_secs(z.hold_timecents),
            decay_sec: timecents_to_secs(z.decay_timecents),
            sustain_fraction: attenuation_cb_to_linear(z.sustain_level_cb),
            release_sec,
        };

        zones.push(Sf2ZoneEntry {
            key_lo: z.key_lo,
            key_hi: z.key_hi,
            vel_lo: z.vel_lo,
            vel_hi: z.vel_hi,
            sample_id: sid,
            root_key,
            tune_cents: z.tune_cents,
            env,
        });
    }

    Ok(Sf2Instrument { zones })
}

// ── multi-sample instruments ───────────────────────────────────────────

struct MultiSampleZone {
    key_lo: u8,
    key_hi: u8,
    sample_id: u32,
    root_key: u8,
}

pub(crate) struct MultiSampleInstrument {
    zones: Vec<MultiSampleZone>,
}

impl MultiSampleInstrument {
    pub(crate) fn new(
        files: &[String],
        root_keys: &[Option<i32>],
        path_to_id: &HashMap<String, u32>,
    ) -> Option<Self> {
        if files.is_empty() {
            return None;
        }
        let n = files.len() as u32;
        let mut zones: Vec<MultiSampleZone> = Vec::with_capacity(files.len());
        for (zi, file) in files.iter().enumerate() {
            let Some(&sid) = path_to_id.get(file) else { continue };
            let root = root_keys
                .get(zi)
                .copied()
                .flatten()
                .unwrap_or(60)
                .clamp(0, 127) as u8;
            let lo = ((zi as u32) * 128 / n) as u8;
            let hi = (((zi as u32 + 1) * 128 / n).saturating_sub(1)).min(127) as u8;
            zones.push(MultiSampleZone {
                key_lo: lo,
                key_hi: hi,
                sample_id: sid,
                root_key: root,
            });
        }
        if zones.is_empty() {
            None
        } else {
            Some(MultiSampleInstrument { zones })
        }
    }

    pub(crate) fn resolve(&self, key: u8) -> Option<(u32, f64)> {
        let mut best: Option<(&MultiSampleZone, i32)> = None;
        for z in &self.zones {
            if key >= z.key_lo && key <= z.key_hi {
                let semitones = (key as i32 - z.root_key as i32) as f64;
                let pitch_hz = C5_HZ * 2f64.powf(semitones / 12.0);
                return Some((z.sample_id, pitch_hz));
            }
            let d = if key < z.key_lo {
                z.key_lo as i32 - key as i32
            } else {
                key as i32 - z.key_hi as i32
            };
            if best.map_or(true, |(_, bd)| d < bd) {
                best = Some((z, d));
            }
        }
        let (z, _) = best?;
        let semitones = (key as i32 - z.root_key as i32) as f64;
        let pitch_hz = C5_HZ * 2f64.powf(semitones / 12.0);
        Some((z.sample_id, pitch_hz))
    }
}