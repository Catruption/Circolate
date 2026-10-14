// src/sfl2/hi.rs
//! Higher-level SF2 synthesis.  Builds on the raw types in `lo`.
//!
//! Ported from SFLT's `lib.rs` `hi` module.  The only change from the
//! original is the `use` path fixups at the top, since `lo` and `scala`
//! are now siblings rather than `crate::`-rooted.

use std::{collections::{HashMap, HashSet}, f64::consts::PI, path::PathBuf, sync::Arc};

use super::scala::{KeyboardMapping, Tuning, default_freq_for_key};

use super::lo::{ControllerType, Gen, GenList, GenValues, GeneralControlPalette, Mod, ModList, ResampleMethod, Sample, Sf2Data, Transform};

const SEMITONE_RATIO: f64 = 1.05946309436;
const TIMECENT_BASE: f64 = 1.00057778951;

fn cents_to_hz_mult(cents: f64) -> f64 {
    SEMITONE_RATIO.powf(cents / 100.0)
}

fn centibels_to_gain(cb: f32) -> f32 {
    10.0f32.powf(cb / 200.0)
}

pub fn timecents_to_secs(timecents: f64) -> f64 {
    TIMECENT_BASE.powf(timecents)
}

pub fn millis_to_timecents(millis: f64) -> f64 {
    1200.0 * (millis / 1000.0).log2()
}

fn concave_curve(t: f64) -> f64 {
    if t <= 0.0 {
        return 0.0
    }
    if t >= 1.0 {
        return 1.0
    }

    let t = 1.0 - t;

    (-20.0 / 96.0) * (t * t).log10()
}

fn sf_curve(f: f64, t: ControllerType, clamp: bool) -> f64 {
    let result = match t {
        ControllerType::Linear => f,
        ControllerType::Concave => concave_curve(f),
        ControllerType::Convex => 1.0 - concave_curve(1.0 - f),
        ControllerType::Switch => if f >= 0.5 { 1.0 } else { 0.0 }
    };

    if clamp {
        result.clamp(0.0, 1.0)
    } else {
        result
    }
}

#[derive(Debug)]
pub struct Zone {
    pub mods: Vec<ModList>,
    pub gens: Vec<GenList>,
}

impl Zone {
    pub fn get_key_range_gen(&self) -> Option<&GenList> {
        if let key @ Some(GenList { oper: Gen::KeyRange, .. }) = self.gens.get(0) {
            key
        } else {
            None
        }
    }

    pub fn get_vel_range_gen(&self) -> Option<&GenList> {
        if let vel @ Some(GenList { oper: Gen::VelRange, .. }) = self.gens.get(0) {
            vel
        } else if let vel @ Some(GenList { oper: Gen::VelRange, .. }) = self.gens.get(1) {
            vel
        } else {
            None
        }
    }

    pub fn get_instrument_gen(&self) -> Option<&GenList> {
        if let inst @ Some(GenList { oper: Gen::Instrument, .. }) = self.gens.last() {
            inst
        } else {
            None
        }
    }

    pub fn get_sample_id_gen(&self) -> Option<&GenList> {
        if let smpl @ Some(GenList { oper: Gen::SampleID, .. }) = self.gens.last() {
            smpl
        } else {
            None
        }
    }

    pub fn is_active(&self, keynum: u8, vel: u8) -> bool {
        if let Some(key_range) = self.get_key_range_gen() {
            let range = key_range.amount.ranges();

            if keynum < range.lo || keynum > range.hi {
                return false
            }
        }

        if let Some(vel_range) = self.get_vel_range_gen() {
            let range = vel_range.amount.ranges();

            if vel < range.lo || vel > range.hi {
                return false
            }
        }

        true
    }
}

#[derive(Debug)]
pub struct Preset {
    pub name: String,
    pub preset: u16,
    pub bank: u16,

    pub zones: Vec<Arc<Zone>>,

    pub library: u32,
    pub genre: u32,
    pub morphology: u32,
}

#[derive(Debug)]
pub struct Instrument {
    pub name: String,

    pub zones: Vec<Arc<Zone>>,
}

#[derive(Default, Debug)]
pub struct LfoState {
    pub phase: f64,
    pub delay_progress: f64,
}

impl LfoState {
    pub fn value(&self) -> f64 {
        let state = self.phase % 1.0;

        if state >= 0.75 {
            (state - 0.75) * 4.0 - 1.0
        } else if state >= 0.25 {
            1.0 - (state - 0.25) * 4.0
        } else {
            state * 4.0
        }
    }

    fn update(&mut self, delay: f64, freq: f64, sample_rate: u32, samples: usize) {
        let elapsed = samples as f64 / sample_rate as f64;

        if self.delay_progress < 1.0 {
            self.delay_progress += elapsed / delay;
        } else {
            self.phase = (self.phase + elapsed * freq) % 1.0;
        }
    }
}

#[derive(Debug, Default)]
pub struct SynthOverrides {
    pub attack: i32,
    pub decay: i32,
    pub sustain: i32,
    pub release: i32,

    pub lfo_2_del: i32,
    pub lfo_2_amt: i32,
    pub lfo_2_spd: i32,

    pub filter_fc: i32,
    pub modulation: i32,

    pub filter_bypass: bool,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineTweak {
    MidiMappingFix = 0,
    SkipShortEnvPhases = 1,
    OverrideHoldWithDecay = 2,
    FilterFix = 3,
    FilterQCap = 4,
    EngineTweakMax = 5,
}

impl TryFrom<u8> for EngineTweak {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value <= EngineTweak::EngineTweakMax as u8 {
            Ok(unsafe { std::mem::transmute(value) })
        } else {
            Err(())
        }
    }
}

impl EngineTweak {
    pub fn newest_default(tweak: usize) -> bool {
        match tweak {
            _ => true,
        }
    }

    pub fn compat_default(tweak: usize) -> bool {
        match tweak {
            0 => true,
            _ => false,
        }
    }
}

#[derive(Debug)]
pub struct EngineSettings {
    pub tweaks: u64,
    pub env_phase_skip_threshold: i16,
    pub smart_pitching: bool,
    pub zone_remapping: bool,
}

impl EngineSettings {
    fn tweak(&self, tweak: EngineTweak) -> bool {
        (self.tweaks & (1 << tweak as usize)) != 0
    }
}

impl Default for EngineSettings {
    fn default() -> Self {
        Self {
            tweaks: 0,
            env_phase_skip_threshold: -12000,
            smart_pitching: false,
            zone_remapping: true,
        }
    }
}

#[derive(Debug)]
pub struct RenderContext {
    pub overrides: SynthOverrides,
    pub settings: EngineSettings,
    mods_filter: HashSet<ModList>,
    active_mods_pool: Vec<Vec<ModList>>,
    inst_voice_vec_pool: Vec<Vec<InstVoice>>,
}

impl RenderContext {
    pub fn new() -> Self {
        Self::with_poly(64)
    }

    pub fn with_poly(voices: usize) -> Self {
        Self::with_capacity(voices, 4, 32, 128)
    }

    pub fn with_capacity(
        voices: usize,
        inst_voices_per_voice: usize,
        mods_per_inst_voice: usize,
        mods_filter: usize,
    ) -> Self {
        let inst_voices = inst_voices_per_voice * voices;

        Self {
            overrides: SynthOverrides::default(),
            settings: EngineSettings::default(),

            mods_filter: HashSet::with_capacity(mods_filter),

            inst_voice_vec_pool: (0..voices).into_iter().map(|_| Vec::with_capacity(inst_voices_per_voice)).collect(),
            active_mods_pool: (0..inst_voices).into_iter().map(|_| Vec::with_capacity(mods_per_inst_voice)).collect(),
        }
    }

    fn get_mod_src_value_raw(&self, v: &Voice<impl HostData>, src: Mod) -> f64 {
        if src.is_general_palette_enabled() {
            match src.general_palette_value() {
                GeneralControlPalette::NoController => 1.0,

                GeneralControlPalette::NoteOnVelocity => v.note_on_velocity as f64 + v.vel_offset as f64,

                GeneralControlPalette::NoteOnKeyNumber => v.note_on_key_number as f64,

                GeneralControlPalette::ChannelPressure | GeneralControlPalette::PolyPressure => 0.0,

                _ => 1.0,
            }
        } else {
            let ctrl_src = src.controller_source();

            if self.overrides.modulation != -1 && let Ok(MidiCC::ModWheel) = ctrl_src.try_into() {
                return self.overrides.modulation as f64 / 127.0
            }

            let host_val = v.host_data.midi_cc_value(ctrl_src);

            if host_val != HOST_CC_NONE {
                if self.settings.tweak(EngineTweak::MidiMappingFix) {
                    if host_val > 64 {
                        0.5 + (host_val - 64) as f64 / 126.0
                    } else {
                        host_val as f64 / 128.0
                    }
                } else {
                    host_val as f64 / 128.0
                }
            } else {
                match ctrl_src.try_into() {
                    Ok(MidiCC::Balance | MidiCC::Pan) => 0.5,

                    Ok(MidiCC::Volume | MidiCC::Expression) => 1.0,

                    _ => 0.0
                }
            }
        }
    }

    pub(crate) fn get_mod_src_value(&self, v: &Voice<impl HostData>, src: Mod) -> f64 {
        let mut val = self.get_mod_src_value_raw(v, src);

        if src.direction_bit() {
            val = 1.0 - val;
        }

        if src.polarity_bit() {
            val -= 0.5;
            val *= 2.0;
        }

        sf_curve(val.abs(), src.controller_type(), true) * val.signum()
    }
}

#[derive(Copy, Clone, Default, Debug, PartialEq, Eq)]
pub enum EnvPhase {
    #[default]
    NotStarted,
    Delay,
    Attack,
    Hold,
    Decay,
    Sustain,
    Release,
    Stop
}

impl EnvPhase {
    fn next(self) -> EnvPhase {
        match self {
            EnvPhase::NotStarted => EnvPhase::Delay,
            EnvPhase::Delay => EnvPhase::Attack,
            EnvPhase::Attack => EnvPhase::Hold,
            EnvPhase::Hold => EnvPhase::Decay,
            EnvPhase::Decay => EnvPhase::Sustain,
            EnvPhase::Sustain => EnvPhase::Release,
            EnvPhase::Release => EnvPhase::Stop,
            EnvPhase::Stop => EnvPhase::Stop,
        }
    }
}

#[derive(Debug)]
pub struct EnvState {
    pub phase: EnvPhase,
    pub prev_phase: EnvPhase,

    pub amt_at_release: f64,
    pub state_progress: f64,

    pub delay_smpls: usize,

    pub time_cache: f64,
    pub env_prev_time: i16,

    pub is_mod: bool,
}

impl EnvState {
    pub fn volume() -> Self {
        EnvState {
            phase: EnvPhase::NotStarted,
            prev_phase: EnvPhase::NotStarted,

            amt_at_release: 0.0,
            state_progress: 0.0,
            delay_smpls: 0,

            time_cache: 0.0,
            env_prev_time: 0,

            is_mod: false,
        }
    }

    pub fn modulation() -> Self {
        EnvState {
            is_mod: true,
            ..Self::volume()
        }
    }

    pub fn process_fine(
        &mut self,
        del: i16,
        atk: i16,
        hold: i16,
        dec: i16,
        sus: f64,
        rel: i16,
        sample_rate: u32,
        out: &mut [f32],
        ctx: &mut RenderContext,
    ) {
        self.process(del, atk, hold, dec, sus, rel, sample_rate, 0, Some(out), ctx);
    }

    pub fn process_coarse(
        &mut self,
        del: i16,
        atk: i16,
        hold: i16,
        dec: i16,
        sus: f64,
        rel: i16,
        sample_rate: u32,
        samples: usize,
        ctx: &mut RenderContext,
    ) {
        self.process(del, atk, hold, dec, sus, rel, sample_rate, samples, None, ctx);
    }

    fn process(
        &mut self,
        del: i16,
        atk: i16,
        hold: i16,
        dec: i16,
        sus: f64,
        rel: i16,
        sample_rate: u32,
        mut len: usize,
        mut out: Option<&mut [f32]>,
        ctx: &mut RenderContext,
    ) {
        if let Some(out) = &out {
            len = out.len();
        }

        let mut i = 0;
        let delta = 1.0 / sample_rate as f64;

        while i < len {
            if self.state_progress >= 1.0 || self.phase == EnvPhase::NotStarted {
                self.state_progress = 0.0;
                self.phase = self.phase.next();
            }

            let time_sh = match self.phase {
                EnvPhase::Delay => del,
                EnvPhase::Attack => atk,
                EnvPhase::Hold => hold,
                EnvPhase::Decay => dec,
                EnvPhase::Release => rel.max(-9000),
                _ => 0,
            };

            if self.phase != self.prev_phase || time_sh != self.env_prev_time {
                if ctx.settings.tweak(EngineTweak::SkipShortEnvPhases) && time_sh <= ctx.settings.env_phase_skip_threshold {
                    self.state_progress = 1.0;
                    continue
                }

                self.time_cache = timecents_to_secs(time_sh as f64);

                if self.phase == EnvPhase::Release {
                    self.time_cache *= self.amt_at_release;
                }
                else if self.phase == EnvPhase::Decay {
                    self.time_cache *= 1.0 - sus;
                }

                if self.time_cache == 0.0 {
                    self.state_progress = 1.0;
                    continue
                }

                self.env_prev_time = time_sh;
                self.prev_phase = self.phase;
            }

            if self.phase == EnvPhase::Sustain {
                if let Some(out) = &mut out {
                    out[i..].fill(sus as f32);
                }

                break
            } else if self.phase == EnvPhase::Stop {
                break
            }

            while i < len && self.state_progress < 1.0 {
                self.state_progress += delta / self.time_cache;

                if let Some(out) = &mut out {
                    out[i] = self.value(sus) as f32;
                }

                if self.phase == EnvPhase::Delay {
                    self.delay_smpls += 1;
                }

                i += 1;
            }
        }
    }

    fn release(&mut self, sustain: f64) {
        if matches!(self.phase, EnvPhase::Release | EnvPhase::Stop) {
            return
        }

        self.amt_at_release = self.value(sustain);
        self.phase = EnvPhase::Release;
        self.state_progress = 0.0;
    }

    fn value(&self, sustain: f64) -> f64 {
        match self.phase {
            EnvPhase::NotStarted | EnvPhase::Delay | EnvPhase::Stop => 0.0,

            EnvPhase::Attack => {
                if self.is_mod {
                    self.state_progress.clamp(0.0, 1.0)
                } else {
                    if self.state_progress <= 0.0 {
                        return 0.0
                    }

                    let logprog = self.state_progress.min(1.0).ln();

                    (1.0 + (20.0 / 96.0) * logprog).clamp(0.0, 1.0)
                }
            }

            EnvPhase::Hold => 1.0,

            EnvPhase::Decay => 1.0 * (1.0 - self.state_progress) + sustain * self.state_progress,

            EnvPhase::Sustain => sustain,

            EnvPhase::Release => self.amt_at_release * (1.0 - self.state_progress),
        }
    }
}

#[derive(Default, Debug)]
pub struct FilterState {
    last_res_freq: f64,
    last_q: f64,
    last_sample_rate: u32,

    pub ic1eq: f32,
    pub ic2eq: f32,

    pub a1: f32,
    pub a2: f32,
    pub a3: f32,

    pub g: f32,
    pub k: f32,
    pub v0: f32,
    pub v1: f32,
    pub v2: f32,
    pub v3: f32
}

impl FilterState {
    pub fn calculate_coefficients(&mut self) {
        self.g = (PI * self.last_res_freq / (self.last_sample_rate as f64)).tan() as f32;
        self.k = 1.0 / self.last_q as f32;
        self.a1 = 1.0 / (1.0 + self.g * (self.g + self.k));
        self.a2 = self.g * self.a1;
        self.a3 = self.g * self.a2;
    }

    pub fn set_params(&mut self, mut res_freq: f64, q: f64, sample_rate: u32) {
        let mut stale = false;

        res_freq = res_freq.min(0.45 * sample_rate as f64).max(5.0);

        if res_freq != self.last_res_freq {
            self.last_res_freq = res_freq;
            stale = true;
        }

        if q != self.last_q {
            self.last_q = q;
            stale = true;
        }

        if sample_rate != self.last_sample_rate {
            self.last_sample_rate = sample_rate;
            stale = true;
        }

        if stale {
            self.calculate_coefficients();
        }
    }

    pub fn reset(&mut self) {
        self.ic1eq = 0.0;
        self.ic2eq = 0.0;
    }
}

#[derive(Debug)]
pub struct InstVoice {
    pub sf: Arc<SoundFont>,

    pub sample: Arc<Sample>,

    pub p_global_zone: Option<Arc<Zone>>,
    pub i_global_zone: Option<Arc<Zone>>,

    pub p_zone: Arc<Zone>,
    pub i_zone: Arc<Zone>,

    pub gen_values: GenValues,

    pub active_mods: Vec<ModList>,

    pub mod_env: EnvState,
    pub vol_env: EnvState,
    pub filter: FilterState,

    pub mod_lfo: LfoState,
    pub vib_lfo: LfoState,

    pub source_progress: f64,
    pub last_vol: (f32, f32)
}

pub const HOST_CC_NONE: u8 = 0xFF;

pub trait HostData {
    fn midi_cc_value(&self, _cc: u8) -> u8 { HOST_CC_NONE }
}

#[derive(Debug)]
pub struct Voice<H: HostData> {
    pub inst_voices: Option<Vec<InstVoice>>,

    pub init_pitch: f32,
    pub pitch_offset: f32,
    pub first_rendering: bool,
    pub released: bool,
    pub has_active_voices: bool,

    pub note_on_key_number: f32,
    pub note_on_velocity: f32,
    pub vel_offset: f32,

    pub host_data: H,
}

impl<H: HostData> Voice<H> {
    pub fn release_resources(&mut self, ctx: &mut RenderContext) {
        let Some(mut inst_voices) = self.inst_voices.take() else {
            return
        };

        for inst_voice in inst_voices.iter_mut() {
            let mut mods = std::mem::take(&mut inst_voice.active_mods);

            mods.clear();
            ctx.active_mods_pool.push(mods);
        }

        inst_voices.clear();
        ctx.inst_voice_vec_pool.push(inst_voices);
    }

    pub fn choke(&mut self) {
        let Some(inst_voices) = self.inst_voices.as_mut() else {
            return
        };

        for iv in inst_voices.iter_mut() {
            iv.vol_env.phase = EnvPhase::Stop;
            iv.mod_env.phase = EnvPhase::Stop;
        }
    }
}

pub static DEFAULT_MODS: &[ModList] = &[
    // 8.4.1 MIDI Note-On Velocity to Initial Attenuation
    ModList::new(
        0x0502,
        Gen::InitialAttenuation,
        960,
        0x0,
        Transform::Linear,
    ),

// 8.4.2 MIDI Note-On Velocity to Filter Cutoff
ModList::new(
    0x0102,
    Gen::InitialFilterFc,
    -2400,
    0x0,
    Transform::Linear,
),

// 8.4.4 MIDI Continuous Controller 1 (Modulation Wheel) to Vibrato LFO Pitch Depth
ModList::new(
    0x0081,
    Gen::VibLfoToPitch,
    50,
    0x0,
    Transform::Linear,
),

// 8.4.5 MIDI Continuous Controller 7 (Volume) to Initial Attenuation
ModList::new(
    0x0587,
    Gen::InitialAttenuation,
    960,
    0x0,
    Transform::Linear,
),

// 8.4.6 MIDI Continuous Controller 10 (Pan) to Pan Position
ModList::new(
    0x028A,
    Gen::Pan,
    500,
    0x0,
    Transform::Linear,
),

// 8.4.7 MIDI Continuous Controller 11 (Expression) to Initial Attenuation
ModList::new(
    0x058B,
    Gen::InitialAttenuation,
    960,
    0x0,
    Transform::Linear,
),
];

#[derive(Debug)]
pub struct SoundFont {
    pub path: PathBuf,
    pub name: Option<String>,
    pub presets: Vec<Preset>,
    pub instruments: Vec<Instrument>,
    pub sample_data: Vec<i16>,
    pub sample_data_24: Vec<u8>,
    pub sample_headers: Vec<Arc<Sample>>,

    pub preset_map: HashMap<(u8, u8), usize>,
}

#[repr(u8)]
#[derive(Copy, Clone, Debug)]
enum MidiCC {
    BankSelect = 0,
    ModWheel = 1,
    BreathControl = 2,
    FootPedal = 4,
    PortamentoTime = 5,
    DataEntry = 6,
    Volume = 7,
    Balance = 8,
    Pan = 10,
    Expression = 11,
}

impl TryFrom<u8> for MidiCC {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(MidiCC::BankSelect),
            1 => Ok(MidiCC::ModWheel),
            2 => Ok(MidiCC::BreathControl),
            4 => Ok(MidiCC::FootPedal),
            5 => Ok(MidiCC::PortamentoTime),
            6 => Ok(MidiCC::DataEntry),
            7 => Ok(MidiCC::Volume),
            8 => Ok(MidiCC::Balance),
            10 => Ok(MidiCC::Pan),
            11 => Ok(MidiCC::Expression),
            _ => Err(())
        }
    }
}

impl InstVoice {
    pub fn release(&mut self, ctx: &mut RenderContext) {
        self.vol_env.release(self.vol_env_sustain(ctx));
        self.mod_env.release(self.mod_env_sustain());
    }

    fn vol_env_sustain(&self, ctx: &mut RenderContext) -> f64 {
        if ctx.overrides.sustain != -1 {
            if ctx.overrides.sustain == 0 {
                return 0.0
            } else {
                return 1.0 + (20.0 / 96.0) * (ctx.overrides.sustain as f64 / 127.0).ln()
            }
        }

        let sustain = self.gen_values[Gen::SustainVolEnv].sh_amount();

        1.0 - sustain.clamp(0, 960) as f64 / 960.0
    }

    fn mod_env_sustain(&self) -> f64 {
        let sustain = self.gen_values[Gen::SustainModEnv].sh_amount();

        1.0 - sustain.clamp(0, 1000) as f64 / 1000.0
    }

    pub fn update(&mut self, v: &Voice<impl HostData>, ctx: &mut RenderContext) {
        self.gen_values.reset();

        if let Some(i_global_zone) = &self.i_global_zone {
            for generator in &i_global_zone.gens {
                self.gen_values.apply_gen_list(generator, true);
            }
        }

        for generator in &self.i_zone.gens {
            self.gen_values.apply_gen_list(generator, true);
        }

        if let Some(p_global_zone) = &self.p_global_zone {
            for generator in &p_global_zone.gens {
                if !self.p_zone.gens.contains(generator) {
                    self.gen_values.apply_gen_list(generator, false);
                }
            }
        }

        for generator in &self.p_zone.gens {
            self.gen_values.apply_gen_list(generator, false);
        }

        for modulator in &self.active_mods {
            let amt_val = ctx.get_mod_src_value(v, modulator.src_oper);
            let mod_amt_val = ctx.get_mod_src_value(v, modulator.mod_amt_src_oper);
            self.gen_values.apply_mod_list(&modulator, amt_val, mod_amt_val);
        }
    }

    pub fn render_mod_envelope(&mut self, samples: usize, sample_rate: u32, ctx: &mut RenderContext) {
        let del = self.gen_values[Gen::DelayModEnv].sh_amount();
        let atk = self.gen_values[Gen::AttackModEnv].sh_amount();
        let mut hold = self.gen_values[Gen::HoldModEnv].sh_amount();
        let mut dec = self.gen_values[Gen::DecayModEnv].sh_amount();
        let rel = self.gen_values[Gen::ReleaseModEnv].sh_amount();

        hold -= (self.gen_values[Gen::KeyNum].sh_amount() - 60) * self.gen_values[Gen::KeynumToModEnvHold].sh_amount();
        dec -= (self.gen_values[Gen::KeyNum].sh_amount() - 60) * self.gen_values[Gen::KeynumToModEnvDecay].sh_amount();

        let sus = self.mod_env_sustain();

        self.mod_env.process_coarse(
            del,
            atk,
            hold,
            dec,
            sus,
            rel,
            sample_rate,
            samples,
            ctx
        );
    }

    pub fn render_vol_envelope(&mut self, out: &mut [f32], sample_rate: u32, ctx: &mut RenderContext) {
        let del = self.gen_values[Gen::DelayVolEnv].sh_amount();
        let mut atk = self.gen_values[Gen::AttackVolEnv].sh_amount();
        let mut hold = self.gen_values[Gen::HoldVolEnv].sh_amount();
        let mut dec = self.gen_values[Gen::DecayVolEnv].sh_amount();
        let mut rel = self.gen_values[Gen::ReleaseVolEnv].sh_amount();

        hold -= (self.gen_values[Gen::KeyNum].sh_amount() - 60) * self.gen_values[Gen::KeynumToVolEnvHold].sh_amount();
        dec -= (self.gen_values[Gen::KeyNum].sh_amount() - 60) * self.gen_values[Gen::KeynumToVolEnvDecay].sh_amount();

        if ctx.overrides.attack != -1 {
            atk = millis_to_timecents(ctx.overrides.attack as f64).min(i16::MAX as f64) as i16;
        }

        if ctx.overrides.decay != -1 {
            dec = millis_to_timecents(ctx.overrides.decay as f64 * 4.0).min(i16::MAX as f64) as i16;

            if ctx.settings.tweak(EngineTweak::OverrideHoldWithDecay) {
                hold = -12000;
            }
        }

        if ctx.overrides.release != -1 {
            rel = millis_to_timecents(ctx.overrides.release as f64 * 4.0).min(i16::MAX as f64) as i16;
        }

        let sus = self.vol_env_sustain(ctx);
        let init_atten = (self.gen_values[Gen::InitialAttenuation].sh_amount() as f32).max(0.0);

        self.vol_env.process_fine(del, atk, hold, dec, sus, rel, sample_rate, out, ctx);

        for smp in out {
            if *smp == 0.0 {
                continue
            }

            let mut cb = -960.0 * (1.0 - *smp);

            cb -= init_atten;

            *smp = centibels_to_gain(cb);
        }
    }

    pub fn render(
        &mut self,
        v: &Voice<impl HostData>,
        out: &mut [f32],
        env: &[f32],
        sample_rate: u32,
        method: ResampleMethod,
        ctx: &mut RenderContext,
        scala: Option<&Tuning>,
        kb_map: Option<&KeyboardMapping>,
    ) -> bool {
        let smpl = &*self.sample;

        if smpl.is_rom() {
            return false
        }

        let sample_mode = self.gen_values[Gen::SampleModes].sh_amount();
        let not_looping = !Sample::should_loop(sample_mode, v.released);

        if not_looping && self.source_progress >= (smpl.get_end(&self.gen_values) - smpl.get_start(&self.gen_values) - 1) as f64 {
            self.vol_env.phase = EnvPhase::Stop;
            return false
        }

        let mod_env_val = self.mod_env.value(self.mod_env_sustain());
        let mod_lfo_val = self.mod_lfo.value();
        let vib_lfo_val = self.vib_lfo.value();

        let mut vib_amt = 0.0;

        let mut vib_lfo_to_pitch = self.gen_values[Gen::VibLfoToPitch].sh_amount() as f64 / 100.0;
        let mut vib_lfo_delay = timecents_to_secs(self.gen_values[Gen::DelayVibLFO].sh_amount() as f64);
        let mut vib_lfo_freq = 8.176 * cents_to_hz_mult(self.gen_values[Gen::FreqVibLFO].sh_amount() as f64);

        let mod_lfo_to_pitch = self.gen_values[Gen::ModLfoToPitch].sh_amount() as f64 / 100.0;
        let mod_lfo_delay = timecents_to_secs(self.gen_values[Gen::DelayModLFO].sh_amount() as f64);
        let mod_lfo_freq = 8.176 * cents_to_hz_mult(self.gen_values[Gen::FreqModLFO].sh_amount() as f64);

        let mod_env_to_pitch = self.gen_values[Gen::ModEnvToPitch].sh_amount() as f64 / 100.0;

        if ctx.overrides.lfo_2_del != -1 {
            vib_lfo_delay = ctx.overrides.lfo_2_del as f64 * 0.004;
        }

        if ctx.overrides.lfo_2_amt != -1 {
            vib_lfo_to_pitch = (ctx.overrides.lfo_2_amt - 129) as f64 / 12.7;
        }

        if ctx.overrides.lfo_2_spd != -1 {
            vib_lfo_freq = ctx.overrides.lfo_2_spd as f64 / 12.0;
        }

        vib_amt += vib_lfo_val * vib_lfo_to_pitch.abs();
        vib_amt += mod_lfo_val * mod_lfo_to_pitch.abs();
        vib_amt += mod_env_val * mod_env_to_pitch;

        let mut root_key = smpl.original_pitch as f64;
        let scale_tuning = self.gen_values[Gen::ScaleTuning].sh_amount() as f64 / 100.0;

        if self.gen_values[Gen::OverridingRootKey].sh_amount() != -1 {
            root_key = self.gen_values[Gen::OverridingRootKey].sh_amount() as f64;
        }

        let mut tuning = self.gen_values[Gen::CoarseTune].sh_amount() as f64;

        tuning += self.gen_values[Gen::FineTune].sh_amount() as f64 / 100.0;
        tuning += smpl.pitch_correction as f64 / 100.0;

        let mut micro_tuning = 0.0;

        if let Some(scala) = scala {
            let key = (v.note_on_key_number * 127.0) as u8;

            if let Some(retune) = scala.get_frequency_for_key(key, kb_map) {
                micro_tuning = 12.0 * (retune / default_freq_for_key(key)).log2();
            }
        }

        let sample_ratio = smpl.sample_rate as f64 / sample_rate as f64;
        let mut key_offset = (v.init_pitch as f64 - root_key + micro_tuning) * scale_tuning + tuning + vib_amt;

        if let Some(scala) = scala && ctx.settings.smart_pitching && v.pitch_offset != 0.0 {
            let key = (v.note_on_key_number * 127.0) + v.pitch_offset as f32;
            let lower = key.floor() as u8;
            let higher = key.ceil() as u8;
            let t = key.fract() as f64;
            let smart_offset;

            if let Some(lowertune) = scala.get_frequency_for_key(lower, kb_map)
                && let Some(highertune) = scala.get_frequency_for_key(higher, kb_map)
                && let Some(retune) = scala.get_frequency_for_key((v.note_on_key_number * 127.0) as u8, kb_map) {
                    smart_offset = 12.0 * ((lowertune * (1.0 - t) + highertune * t) / retune).log2();
                } else {
                    smart_offset = v.pitch_offset as f64;
                }

                key_offset += smart_offset;
        } else {
            key_offset += v.pitch_offset as f64;
        }
        let progress_ratio = SEMITONE_RATIO.powf(key_offset) * sample_ratio;
        let delay = self.vol_env.delay_smpls as usize;

        self.vol_env.delay_smpls = (delay as i64 - out.len() as i64).max(0) as usize;

        if delay < out.len() {
            let smpl_start = &self.sf.sample_data[smpl.start as usize..];

            let out = &mut out[delay..];
            let env = &env[delay..];

            if self.sf.sample_data_24.is_empty() {
                smpl.resample(
                    &mut self.source_progress,
                    progress_ratio,
                    method,
                    |i| unsafe { *smpl_start.get_unchecked(i) } as f32 / 32767.0,
                              out,
                              v.released,
                              &self.gen_values,
                );
            } else {
                let sm24_start = &self.sf.sample_data_24[smpl.start as usize..];

                smpl.resample(
                    &mut self.source_progress,
                    progress_ratio,
                    method,
                    |i| (((unsafe { *smpl_start.get_unchecked(i) } as i32) << 8) | unsafe { *sm24_start.get_unchecked(i) } as i32) as f32 / 8388607.0,
                              out,
                              v.released,
                              &self.gen_values,
                );
            }

            if !ctx.overrides.filter_bypass {
                let lp = &mut self.filter;
                let mut filter_q = self.gen_values[Gen::InitialFilterQ].sh_amount() as f64;
                let mut filter_fc = self.gen_values[Gen::InitialFilterFc].sh_amount() as f64;

                if ctx.settings.tweak(EngineTweak::FilterFix) {
                    filter_fc += mod_env_val * self.gen_values[Gen::ModEnvToFilterFc].sh_amount() as f64;
                    filter_fc += mod_lfo_val * self.gen_values[Gen::ModLfoToFilterFc].sh_amount() as f64;

                    if ctx.overrides.filter_fc != -1 {
                        let t = sf_curve(ctx.overrides.filter_fc as f64 / 127.0, ControllerType::Convex, true);
                        filter_fc = t * 13500.0 + (1.0 - t) * 1500.0;
                    }
                } else {
                    filter_fc += sf_curve(mod_env_val, ControllerType::Concave, true) * self.gen_values[Gen::ModEnvToFilterFc].sh_amount() as f64;
                    filter_fc += mod_lfo_val * self.gen_values[Gen::ModLfoToFilterFc].sh_amount() as f64;

                    if ctx.overrides.filter_fc != -1 {
                        filter_fc = ctx.overrides.filter_fc as f64 / 127.0 * 13500.0;
                    }
                }

                let filter_fc_hz = 8.176 * cents_to_hz_mult(filter_fc);

                if ctx.settings.tweak(EngineTweak::FilterQCap) {
                    filter_q = filter_q.min(240.0);
                }

                filter_q /= 10.0;
                filter_q -= 3.01;

                let linear_q = 10.0f64.powf(filter_q / 20.0);

                lp.set_params(filter_fc_hz, linear_q, sample_rate);

                if v.first_rendering {
                    lp.reset();
                }

                for s in out.iter_mut() {
                    lp.v0 = *s;

                    lp.v3 = lp.v0 - lp.ic2eq;
                    lp.v1 = lp.a1 * lp.ic1eq + lp.a2 * lp.v3;
                    lp.v2 = lp.ic2eq + lp.a2 * lp.ic1eq + lp.a3 * lp.v3;

                    lp.ic1eq = 2.0 * lp.v1 - lp.ic1eq;
                    lp.ic2eq = 2.0 * lp.v2 - lp.ic2eq;

                    if filter_fc < 13500.0 {
                        *s = lp.v2;
                    }
                }
            }

            let mut final_gain_db = 2.0;

            final_gain_db += mod_lfo_val * self.gen_values[Gen::ModLfoToVolume].sh_amount() as f64 / 10.0;

            let final_gain_linear = 10.0f64.powf(final_gain_db / 20.0) as f32;

            for (smp, env) in out.iter_mut().zip(env.iter().copied()) {
                *smp *= env * final_gain_linear;
            }

            self.vib_lfo.update(vib_lfo_delay, vib_lfo_freq, sample_rate, out.len());
            self.mod_lfo.update(mod_lfo_delay, mod_lfo_freq, sample_rate, out.len());
        }

        true
    }
}

impl SoundFont {
    pub fn from_sf2_data(sf: Sf2Data) -> Self {
        let mut result = Self {
            path: sf.path,
            name: sf.name,
            presets: vec![],
            instruments: vec![],
            sample_data: vec![],
            sample_data_24: sf.sm24,
            sample_headers: vec![],

            preset_map: HashMap::new(),
        };

        for i in 0..(sf.phdr.len() - 1) {
            let phdr = &sf.phdr[i];

            let mut zones = vec![];

            for j in phdr.preset_bag_idx..sf.phdr[i+1].preset_bag_idx {
                let j = j as usize;
                let pbag = &sf.pbag[j];

                let mut zone = Zone { gens: vec![], mods: vec![] };

                for k in pbag.gen_idx..sf.pbag[j+1].gen_idx {
                    zone.gens.push(sf.pgen[k as usize]);
                }

                for k in pbag.mod_idx..sf.pbag[j+1].mod_idx {
                    zone.mods.push(sf.pmod[k as usize]);
                }

                zones.push(Arc::new(zone));
            }

            if !result.preset_map.contains_key(&(phdr.bank as u8, phdr.preset as u8)) {
                result.preset_map.insert((phdr.bank as u8, phdr.preset as u8), i);
            }

            let end = phdr.name.iter().position(|i| *i == 0).unwrap_or(20);

            result.presets.push(Preset {
                name: String::from_utf8_lossy(&phdr.name[0..end]).into_owned(),

                                preset: phdr.preset,
                                bank: phdr.bank,
                                zones,

                                library: phdr.library,
                                genre: phdr.genre,
                                morphology: phdr.morphology
            });
        }

        for i in 0..(sf.inst.len() - 1) {
            let inst = &sf.inst[i];

            let mut zones = vec![];

            for j in inst.inst_bag_idx..sf.inst[i+1].inst_bag_idx {
                let j = j as usize;
                let ibag = &sf.ibag[j];

                let mut zone = Zone { gens: vec![], mods: vec![] };

                for k in ibag.gen_idx..sf.ibag[j+1].gen_idx {
                    zone.gens.push(sf.igen[k as usize]);
                }

                for k in ibag.mod_idx..sf.ibag[j+1].mod_idx {
                    zone.mods.push(sf.imod[k as usize]);
                }

                zones.push(Arc::new(zone));
            }

            let end = inst.name.iter().position(|i| *i == 0).unwrap_or(20);

            result.instruments.push(Instrument {
                name: String::from_utf8_lossy(&inst.name[0..end]).into_owned(),
                                    zones
            });
        }

        result.sample_data = sf.smpl;
        result.sample_headers = sf.shdr.into_iter().map(Arc::new).collect();

        result
    }


    pub fn get_preset_idx(&self, bank: u8, patch: u8) -> Option<usize> {
        self.preset_map.get(&(bank, patch)).copied()
    }

    pub fn spawn_dummy_voice<H: HostData>(host_data: H) -> Voice<H> {
        Voice {
            inst_voices: Some(vec![]),
            init_pitch: 0.0,
            pitch_offset: 0.0,
            first_rendering: true,
            released: false,
            has_active_voices: false,
            note_on_key_number: 60.0 / 127.0,
            note_on_velocity: 0.5,
            vel_offset: 0.0,
            host_data
        }
    }


    pub fn spawn_voice<H: HostData>(
        self: &Arc<Self>,
        host_data: H,
        preset_idx: usize,
        pitch: f32,
        vel: u8,
        tuning: Option<&Tuning>,
        kb_map: Option<&KeyboardMapping>,
        ctx: &mut RenderContext
    )
    -> Voice<H>
    {
        let preset = &self.presets[preset_idx];

        let mut p_global_zone = None;

        let mut keynum = pitch as u8;

        let mut voice = Voice {
            init_pitch: pitch,
            pitch_offset: 0.0,
            first_rendering: true,
            host_data,
            has_active_voices: false,
            inst_voices: None,
            released: false,
            note_on_key_number: pitch / 127.0,
            note_on_velocity: vel as f32 / 127.0,
            vel_offset: 0.0,
        };

        let mut inst_voices = ctx.inst_voice_vec_pool.pop().unwrap_or(vec![]);

        let vel = vel.min(127);

        if ctx.settings.zone_remapping && let Some(scala) = tuning {
            if let Some(retune) = scala.get_frequency_for_key(keynum, kb_map) {
                let micro_tuning = 12.0 * (retune / default_freq_for_key(keynum)).log2();
                keynum = (keynum as f64 + micro_tuning).clamp(0.0, 127.0) as u8;
            }
        }

        for zone in &preset.zones {
            let Some(inst_gen) = zone.get_instrument_gen() else {
                if zone.is_active(keynum, vel) {
                    p_global_zone = Some(zone.clone());
                    continue
                } else {
                    break
                }
            };

            if !zone.is_active(keynum, vel) {
                continue
            }

            let mut i_global_zone = None;

            let inst = &self.instruments[inst_gen.amount.w_amount() as usize];

            for izone in &inst.zones {
                let Some(sample_id_gen) = izone.get_sample_id_gen() else {
                    if izone.is_active(keynum, vel) {
                        i_global_zone = Some(izone.clone());
                        continue
                    } else {
                        break
                    }
                };

                if !izone.is_active(keynum, vel) {
                    continue
                }

                let mut gen_values = GenValues::new();

                gen_values[Gen::KeyNum].w_amount = keynum as u16;
                gen_values[Gen::Velocity].w_amount = vel as u16;

                let mut active_mods = ctx.active_mods_pool.pop().unwrap_or(vec![]);
                let mods_filter = &mut ctx.mods_filter;

                mods_filter.clear();

                for modulator in DEFAULT_MODS {
                    mods_filter.insert(*modulator);
                }

                if let Some(i_global_zone) = &i_global_zone {
                    for modulator in &i_global_zone.mods {
                        mods_filter.insert(*modulator);
                    }
                }

                for modulator in &izone.mods {
                    mods_filter.insert(*modulator);
                }

                active_mods.extend(mods_filter.iter());
                mods_filter.clear();

                if let Some(p_global_zone) = &p_global_zone {
                    for modulator in &p_global_zone.mods {
                        mods_filter.insert(*modulator);
                    }
                }

                for modulator in &zone.mods {
                    mods_filter.insert(*modulator);
                }

                active_mods.extend(mods_filter.iter());

                let mut iv = InstVoice {
                    sf: self.clone(),
                    sample: self.sample_headers[sample_id_gen.amount.w_amount() as usize].clone(),
                    p_global_zone: p_global_zone.clone(),
                    i_global_zone: i_global_zone.clone(),
                    p_zone: zone.clone(),
                    i_zone: izone.clone(),
                    gen_values,
                    active_mods,
                    filter: FilterState::default(),
                    mod_env: EnvState::modulation(),
                    vol_env: EnvState::volume(),
                    mod_lfo: LfoState::default(),
                    vib_lfo: LfoState::default(),
                    source_progress: 0.0,
                    last_vol: (0.0, 0.0)
                };

                iv.update(&voice, ctx);

                inst_voices.push(iv);
            }
        }

        voice.inst_voices = Some(inst_voices);

        voice
    }
}