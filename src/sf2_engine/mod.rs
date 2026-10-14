// src/sf2_engine/mod.rs
//! Native SF2 playback engine.
//!
//! Drives `sfl2`'s voice renderer directly.  No VST3, no plugin host,
//! no state blobs.  Load an SF2, set a preset, send notes, get audio.
//!
//! sfl2 renders a single `InstVoice` into a mono buffer per call.  This
//! module owns voice storage, applies the SF2 pan generator as a stereo
//! pan law, routes per-voice reverb/chorus send generators into two
//! shared FX units, and sums dry + wet to the output — matching what
//! FluidSynth does with its default on-by-default reverb and chorus.

mod fx;
mod host;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicU32;

use crate::sfl2::hi::{
    EngineSettings, EngineTweak, EnvPhase, RenderContext, SoundFont,
    SynthOverrides, Voice,
};
use crate::sfl2::lo::{Gen, ResampleMethod, Sf2Data};

use fx::{ChorusMode, ChorusUnit, ReverbMode, ReverbUnit};

pub use host::Sf2HostData;

const MAX_POLY: usize = 64;

/// Post-voice gain.  sfl2 applies `final_gain_db = 2.0` (≈ 1.26×)
/// inside every InstVoice, and stereo SoundFont zones spawn two voices
/// per note, so the raw sum runs hot.  Tuned so the dry output level
/// matches a FluidSynth session (which applies `synth.gain = 0.2`).
pub const DEFAULT_GAIN: f32 = 0.22;

pub struct Sf2Engine {
    sf: Option<Arc<SoundFont>>,
    bank: u8,
    patch: u8,

    ctx: RenderContext,
    voices: Vec<VoiceSlot>,

    reverb: ReverbUnit,
    chorus: ChorusUnit,

    resample: ResampleMethod,
    sample_rate: u32,
    gain: f32,
}

struct VoiceSlot {
    key: u8,
    voice: Voice<Sf2HostData>,
    released: bool,
}

impl Sf2Engine {
    pub fn new(sample_rate: u32) -> Self {
        // `SynthOverrides::default()` gives 0, but -1 means "use the
        // SF2's own value" throughout sfl2.  0s would collapse every
        // envelope and filter cutoff to their minimum.
        let overrides = SynthOverrides {
            attack: -1,
            decay: -1,
            sustain: -1,
            release: -1,
            lfo_2_del: -1,
            lfo_2_amt: -1,
            lfo_2_spd: -1,
            filter_fc: -1,
            modulation: -1,
            filter_bypass: false,
        };

        // `EngineSettings::default()` has every tweak off.  SFLT's own
        // default is all tweaks on (`newest_default(i) == true`).
        let mut tweaks: u64 = 0;
        for i in 0..EngineTweak::EngineTweakMax as usize {
            if EngineTweak::newest_default(i) {
                tweaks |= 1 << i;
            }
        }

        let mut ctx = RenderContext::with_poly(MAX_POLY);
        ctx.settings = EngineSettings {
            tweaks,
            env_phase_skip_threshold: -12000,
            smart_pitching: false,
            zone_remapping: true,
        };
        ctx.overrides = overrides;

        // FluidSynth enables both units by default.  Per-voice wet
        // level comes from the SF2's own send generators (16 = reverb,
        // 15 = chorus), not from the units being on/off.
        // `SF2_FX=0` disables them entirely, `SF2_FX=1` (default) keeps
        // FluidSynth's behaviour.
        let fx_on = std::env::var("SF2_FX")
            .ok()
            .map(|v| v != "0")
            .unwrap_or(true);

        let mut reverb = ReverbUnit::new();
        reverb.set_mode(
            if fx_on { ReverbMode::Enabled } else { ReverbMode::Disabled },
            sample_rate,
        );

        let mut chorus = ChorusUnit::new();
        chorus.set_mode(
            if fx_on { ChorusMode::Enabled } else { ChorusMode::Disabled },
            sample_rate,
        );

        Sf2Engine {
            sf: None,
            bank: 0,
            patch: 0,
            ctx,
            voices: Vec::new(),
            reverb,
            chorus,
            resample: ResampleMethod::Hermite,
            sample_rate,
            gain: DEFAULT_GAIN,
        }
    }

    pub fn load(&mut self, path: &Path) -> std::io::Result<()> {
        let progress = Arc::new(AtomicU32::new(0));
        let data = Sf2Data::load_from_file(path.to_path_buf(), true, progress)?;
        self.sf = Some(Arc::new(SoundFont::from_sf2_data(data)));
        Ok(())
    }

    pub fn is_loaded(&self) -> bool { self.sf.is_some() }
    pub fn preset(&self) -> (u8, u8) { (self.bank, self.patch) }

    pub fn set_preset(&mut self, bank: u8, patch: u8) {
        self.bank = bank;
        self.patch = patch;
    }

    pub fn set_gain(&mut self, gain: f32) { self.gain = gain; }
    pub fn set_resample(&mut self, m: ResampleMethod) { self.resample = m; }

    pub fn list_presets(&self) -> Vec<(u16, u16, String)> {
        let Some(sf) = &self.sf else { return Vec::new() };
        sf.presets
            .iter()
            .map(|p| (p.bank, p.preset, p.name.clone()))
            .collect()
    }

    pub fn note_on(&mut self, key: u8, velocity: u8) {
        let Some(sf) = self.sf.clone() else { return };
        let Some(preset_idx) = sf.get_preset_idx(self.bank, self.patch) else {
            return;
        };

        if self.voices.len() >= MAX_POLY {
            if let Some(old) = self.voices.first_mut() {
                old.voice.release_resources(&mut self.ctx);
            }
            self.voices.remove(0);
        }

        let host_data = Sf2HostData::new();
        let voice = sf.spawn_voice(
            host_data,
            preset_idx,
            key as f32,
            velocity,
            None,
            None,
            &mut self.ctx,
        );

        self.voices.push(VoiceSlot {
            key,
            voice,
            released: false,
        });
    }

    pub fn note_off(&mut self, key: u8) {
        for slot in self.voices.iter_mut() {
            if slot.key == key && !slot.released {
                if let Some(ivs) = slot.voice.inst_voices.as_mut() {
                    for iv in ivs.iter_mut() {
                        iv.release(&mut self.ctx);
                    }
                }
                slot.voice.released = true;
                slot.released = true;
            }
        }
    }

    pub fn all_notes_off(&mut self) {
        for slot in self.voices.iter_mut() {
            if let Some(ivs) = slot.voice.inst_voices.as_mut() {
                for iv in ivs.iter_mut() {
                    iv.release(&mut self.ctx);
                }
            }
            slot.voice.released = true;
            slot.released = true;
        }
    }

    pub fn active_voices(&self) -> usize { self.voices.len() }

    /// Render `out_l.len()` samples and add them to the buffers.
    pub fn process(&mut self, out_l: &mut [f32], out_r: &mut [f32]) {
        let n = out_l.len().min(out_r.len());
        if n == 0 { return; }

        // Dry master (everything the voices produce, gain already
        // applied) and the two send buses.
        let mut dry_l = vec![0.0f32; n];
        let mut dry_r = vec![0.0f32; n];

        let mut rev_l = vec![0.0f32; n];
        let mut rev_r = vec![0.0f32; n];
        let mut cho_l = vec![0.0f32; n];
        let mut cho_r = vec![0.0f32; n];

        for slot in self.voices.iter_mut() {
            slot.render(
                &mut dry_l, &mut dry_r,
                &mut rev_l, &mut rev_r,
                &mut cho_l, &mut cho_r,
                self.sample_rate,
                self.resample,
                &mut self.ctx,
                self.gain,
            );
        }

        // Sum the dry mix.
        for i in 0..n {
            out_l[i] += dry_l[i];
            out_r[i] += dry_r[i];
        }

        // Reverb: fill its input with the send bus, process, add wet.
        if !rev_l.is_empty() {
            self.reverb.input[0].resize(n, 0.0);
            self.reverb.input[1].resize(n, 0.0);
            self.reverb.input[0][..n].copy_from_slice(&rev_l);
            self.reverb.input[1][..n].copy_from_slice(&rev_r);

            let mut wet_l = vec![0.0f32; n];
            let mut wet_r = vec![0.0f32; n];
            {
                let mut target: [&mut [f32]; 2] = [&mut wet_l[..n], &mut wet_r[..n]];
                self.reverb.process(&mut target[..], [1.0, 1.0]);
            }
            for i in 0..n {
                out_l[i] += wet_l[i];
                out_r[i] += wet_r[i];
            }
        }

        // Chorus: same pattern.
        if !cho_l.is_empty() {
            self.chorus.input[0].resize(n, 0.0);
            self.chorus.input[1].resize(n, 0.0);
            self.chorus.input[0][..n].copy_from_slice(&cho_l);
            self.chorus.input[1][..n].copy_from_slice(&cho_r);

            let mut wet_l = vec![0.0f32; n];
            let mut wet_r = vec![0.0f32; n];
            {
                let mut target: [&mut [f32]; 2] = [&mut wet_l[..n], &mut wet_r[..n]];
                self.chorus.process(&mut target[..], [1.0, 1.0]);
            }
            for i in 0..n {
                out_l[i] += wet_l[i];
                out_r[i] += wet_r[i];
            }
        }

        self.voices
            .retain(|slot| !slot.released || slot.voice.has_active_voices);
    }
}

impl VoiceSlot {
    #[allow(clippy::too_many_arguments)]
    fn render(
        &mut self,
        dry_l: &mut [f32],
        dry_r: &mut [f32],
        rev_l: &mut [f32],
        rev_r: &mut [f32],
        cho_l: &mut [f32],
        cho_r: &mut [f32],
        sample_rate: u32,
        resample: ResampleMethod,
        ctx: &mut RenderContext,
        gain: f32,
    ) {
        let Some(mut ivs) = self.voice.inst_voices.take() else {
            return;
        };

        let n = dry_l.len();
        let mut iv_buf = vec![0.0f32; n];
        let mut env_buf = vec![0.0f32; n];
        let mut any_active = false;

        for iv in ivs.iter_mut() {
            for x in iv_buf.iter_mut() { *x = 0.0; }
            for x in env_buf.iter_mut() { *x = 0.0; }

            iv.update(&self.voice, ctx);
            iv.render_vol_envelope(&mut env_buf, sample_rate, ctx);

            let ok = iv.render(
                &self.voice,
                &mut iv_buf,
                &env_buf,
                sample_rate,
                resample,
                ctx,
                None,
                None,
            );

            if ok {
                // SF2 pan generator: -500 hard left, 0 centre, +500
                // hard right.  Equal-power pan law.
                let pan_raw = iv.gen_values[Gen::Pan].sh_amount() as f32;
                let pan = (pan_raw / 500.0).clamp(-1.0, 1.0);
                let theta = (pan + 1.0) * std::f32::consts::FRAC_PI_4;
                let gl = theta.cos();
                let gr = theta.sin();

                // SF2 send generators: 0..1000, in 0.1% steps.  These
                // are what determines whether a note gets reverb or
                // chorus at all, regardless of whether the units are
                // running — same as FluidSynth.
                let rev_send =
                    (iv.gen_values[Gen::ReverbEffectsSend].w_amount() as f32 / 1000.0)
                        .clamp(0.0, 1.0);
                let cho_send =
                    (iv.gen_values[Gen::ChorusEffectsSend].w_amount() as f32 / 1000.0)
                        .clamp(0.0, 1.0);

                let master_g = gain;
                let rev_g = gain * rev_send;
                let cho_g = gain * cho_send;

                for i in 0..n {
                    let s = iv_buf[i];
                    dry_l[i] += s * gl * master_g;
                    dry_r[i] += s * gr * master_g;
                    rev_l[i] += s * gl * rev_g;
                    rev_r[i] += s * gr * rev_g;
                    cho_l[i] += s * gl * cho_g;
                    cho_r[i] += s * gr * cho_g;
                }
            }

            iv.render_mod_envelope(n, sample_rate, ctx);

            if iv.vol_env.phase != EnvPhase::Stop {
                any_active = true;
            }
        }

        self.voice.inst_voices = Some(ivs);
        self.voice.has_active_voices = any_active;
        self.voice.first_rendering = false;
    }
}