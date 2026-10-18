//! Instrument sidecar parsing.
//!
//!     Instrument Foo
//!     {
//!         Kind: vst3;
//!         Plugin: 'Surge XT';
//!         Path: '/path/to/Surge XT.vst3';
//!         State: 'Foo.state';
//!         OriginalPlugin: 'ZENOLOGY';
//!         Replaces: 'ZENOLOGY';
//!         Fallback: true;
//!         Gain: 0.4;
//!         SourceChannel: 2;
//!     }
//!
//! Slicex sidecars may additionally contain Slicex-specific settings. These
//! are applied by the internal sample renderer, not baked into the generated
//! SoundFont. Existing sidecars remain compatible because every field is optional.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Instrument {
    pub name: String,
    pub kind: String,
    pub plugin: String,
    pub vst_path: String,
    pub state_path: PathBuf,
    pub sf2_path: PathBuf,
    pub sf2_bank: u32,
    pub sf2_preset: u32,
    pub fallback: bool,
    pub unresolved: bool,
    pub replaces: String,
    pub gain: f32,
    pub source_channel: u16,
    /// True only for generated FL Studio Slicex instruments.
    pub slicex: bool,
    /// Slicex channel pan, -1.0 (left) .. 1.0 (right).
    pub slicex_pan: f32,
    /// Linear channel-volume multiplier, 0.0 .. 1.0.
    pub slicex_gain: f32,
    /// Combined coarse/fine channel tuning in cents.
    pub slicex_tune_cents: i16,
    pub slicex_delay_sec: f32,
    pub slicex_attack_sec: f32,
    pub slicex_hold_sec: f32,
    pub slicex_decay_sec: f32,
    pub slicex_sustain_fraction: f32,
    pub slicex_release_sec: f32,
}

impl Instrument {
    pub fn is_sf2(&self) -> bool { self.kind == "sf2" }
    pub fn is_vst3(&self) -> bool { self.kind == "vst3" }
}

pub fn load(path: &Path) -> Result<Instrument> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let mut inst = parse(&text, &path.display().to_string())?;
    if !inst.state_path.as_os_str().is_empty() && inst.state_path.is_relative() {
        let dir = path.parent().unwrap_or(Path::new("."));
        inst.state_path = dir.join(&inst.state_path);
    }
    Ok(inst)
}

pub fn parse(text: &str, hint: &str) -> Result<Instrument> {
    let mut name = String::new();
    let mut kind = String::new();
    let mut plugin = String::new();
    let mut vst_path = String::new();
    let mut state = String::new();
    let mut sf2 = String::new();
    let mut sf2_bank: u32 = 0;
    let mut sf2_preset: u32 = 0;
    let mut sf2_bank_set = false;
    let mut fallback = false;
    let mut unresolved = false;
    let mut replaces = String::new();
    let mut gain: f32 = 1.0;
    let mut source_channel: u16 = 0;
    let mut slicex = false;
    let mut slicex_pan = 0.0f32;
    let mut slicex_gain = 1.0f32;
    let mut slicex_tune_cents = 0i16;
    let mut slicex_delay_sec = 0.0f32;
    let mut slicex_attack_sec = 0.0f32;
    let mut slicex_hold_sec = 0.0f32;
    let mut slicex_decay_sec = 0.0f32;
    let mut slicex_sustain_fraction = 1.0f32;
    let mut slicex_release_sec = 0.0f32;

    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("Instrument ") {
            name = line["Instrument ".len()..].trim().to_string();
            continue;
        }
        if line == "{" || line == "}" { continue; }
        let Some((key, rest)) = line.split_once(':') else { continue };
        let value = rest.trim().trim_end_matches(';').trim().trim_matches('\'').to_string();
        match key.trim() {
            "Kind" => kind = value,
            "Plugin" => plugin = value,
            "Path" => vst_path = value,
            "State" => state = value,
            "File" => sf2 = value,
            "Bank" => { sf2_bank = value.parse().unwrap_or(0); sf2_bank_set = true; }
            "Preset" => sf2_preset = value.parse().unwrap_or(0),
            "Replaces" => replaces = value,
            "Fallback" => fallback = value.eq_ignore_ascii_case("true"),
            "Unresolved" => unresolved = value.eq_ignore_ascii_case("true"),
            "Gain" => gain = value.parse().unwrap_or(1.0),
            "SourceChannel" => source_channel = value.parse().unwrap_or(0),
            "Slicex" => slicex = value.eq_ignore_ascii_case("true"),
            "SlicexPan" => slicex_pan = value.parse().unwrap_or(0.0),
            "SlicexGain" => slicex_gain = value.parse().unwrap_or(1.0),
            "SlicexTuneCents" => slicex_tune_cents = value.parse().unwrap_or(0),
            "SlicexDelayMs" => slicex_delay_sec = value.parse::<f32>().unwrap_or(0.0) / 1000.0,
            "SlicexAttackMs" => slicex_attack_sec = value.parse::<f32>().unwrap_or(0.0) / 1000.0,
            "SlicexHoldMs" => slicex_hold_sec = value.parse::<f32>().unwrap_or(0.0) / 1000.0,
            "SlicexDecayMs" => slicex_decay_sec = value.parse::<f32>().unwrap_or(0.0) / 1000.0,
            "SlicexSustain" => slicex_sustain_fraction = value.parse::<f32>().unwrap_or(1.0).clamp(0.0, 1.0),
            "SlicexReleaseMs" => slicex_release_sec = value.parse::<f32>().unwrap_or(0.0) / 1000.0,
            _ => {}
        }
    }

    // Legacy packed-preset decode: bank << 16 | preset << 8 | channel.
    if kind == "sf2" && !sf2_bank_set && sf2_preset > 255 {
        let word = sf2_preset;
        sf2_bank = (word >> 16) & 0xFF;
        sf2_preset = (word >> 8) & 0xFF;
        if std::env::var_os("SF2_DEBUG").is_some() {
            eprintln!("instruments: decoded packed Preset {:#x} -> bank {} preset {}", word, sf2_bank, sf2_preset);
        }
    }

    if kind.is_empty() { return Err(anyhow!("sidecar missing Kind: {}", hint)); }
    if kind == "vst3" && plugin.is_empty() { return Err(anyhow!("vst3 sidecar missing Plugin: {}", hint)); }
    if kind == "sf2" && sf2.is_empty() { return Err(anyhow!("sf2 sidecar missing File: {}", hint)); }

    Ok(Instrument {
        name, kind, plugin, vst_path,
        state_path: if state.is_empty() { PathBuf::new() } else { PathBuf::from(state) },
        sf2_path: if sf2.is_empty() { PathBuf::new() } else { PathBuf::from(&sf2) },
        sf2_bank, sf2_preset, fallback, unresolved, replaces, gain, source_channel,
        slicex, slicex_pan, slicex_gain, slicex_tune_cents, slicex_delay_sec,
        slicex_attack_sec, slicex_hold_sec, slicex_decay_sec,
        slicex_sustain_fraction, slicex_release_sec,
    })
}
