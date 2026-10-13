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
//!     Instrument Bar
//!     {
//!         Kind: sf2;
//!         File: 'samples/18_sf2_SGM.sf2';
//!         Bank: 0;
//!         Preset: 25;
//!         SourceChannel: 18;
//!     }
//!
//! `parse` reads from a string (used by archive sources); `load` reads
//! from disk and resolves `state_path` relative to the sidecar's dir.

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
}

impl Instrument {
    pub fn is_sf2(&self) -> bool {
        self.kind == "sf2"
    }
    pub fn is_vst3(&self) -> bool {
        self.kind == "vst3"
    }
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

pub fn parse(text: &str, _hint: &str) -> Result<Instrument> {
    let mut name = String::new();
    let mut kind = String::new();
    let mut plugin = String::new();
    let mut vst_path = String::new();
    let mut state = String::new();
    let mut sf2 = String::new();
    let mut sf2_bank: u32 = 0;
    let mut sf2_preset: u32 = 0;
    let mut fallback = false;
    let mut unresolved = false;
    let mut replaces = String::new();
    let mut gain: f32 = 1.0;
    let mut source_channel: u16 = 0;

    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("Instrument ") {
            name = line["Instrument ".len()..].trim().to_string();
            continue;
        }
        if line == "{" || line == "}" {
            continue;
        }
        let Some((key, rest)) = line.split_once(':') else { continue };
        let value = rest
            .trim()
            .trim_end_matches(';')
            .trim()
            .trim_matches('\'')
            .to_string();
        match key.trim() {
            "Kind" => kind = value,
            "Plugin" => plugin = value,
            "Path" => vst_path = value,
            "State" => state = value,
            "File" => sf2 = value,
            "Bank" => sf2_bank = value.parse().unwrap_or(0),
            "Preset" => sf2_preset = value.parse().unwrap_or(0),
            "Replaces" => replaces = value,
            "Fallback" => fallback = value.eq_ignore_ascii_case("true"),
            "Unresolved" => unresolved = value.eq_ignore_ascii_case("true"),
            "Gain" => gain = value.parse().unwrap_or(1.0),
            "SourceChannel" => source_channel = value.parse().unwrap_or(0),
            _ => {}
        }
    }

    if kind.is_empty() {
        return Err(anyhow!("sidecar missing Kind: {}", _hint));
    }
    if kind == "vst3" && plugin.is_empty() {
        return Err(anyhow!("vst3 sidecar missing Plugin: {}", _hint));
    }
    if kind == "sf2" && sf2.is_empty() {
        return Err(anyhow!("sf2 sidecar missing File: {}", _hint));
    }

    Ok(Instrument {
        name,
        kind,
        plugin,
        vst_path,
        state_path: if state.is_empty() {
            PathBuf::new()
        } else {
            PathBuf::from(state)
        },
        sf2_path: if sf2.is_empty() {
            PathBuf::new()
        } else {
            PathBuf::from(&sf2)
        },
        sf2_bank,
        sf2_preset,
        fallback,
        unresolved,
        replaces,
        gain,
        source_channel,
    })
}