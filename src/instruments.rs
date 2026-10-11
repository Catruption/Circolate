//! Instrument sidecar parsing.
//!
//!     Instrument Foo
//!     {
//!         Kind: vst3;
//!         Plugin: 'ZENOLOGY';
//!         Path: 'ZENOLOGY.vst3';
//!         State: 'Foo.state';
//!         SourceChannel: 2;
//!     }
//!
//!     Instrument Bar
//!     {
//!         Kind: sf2;
//!         File: 'samples/18_sf2_SGM.sf2';
//!         SourceChannel: 18;
//!     }

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
}

impl Instrument {
    pub fn is_sf2(&self) -> bool {
        self.kind == "sf2"
    }
}

pub fn load(path: &Path) -> Result<Instrument> {
    let text = std::fs::read_to_string(path)
    .with_context(|| format!("reading {}", path.display()))?;
    let mut name = String::new();
    let mut kind = String::new();
    let mut plugin = String::new();
    let mut vst_path = String::new();
    let mut state = String::new();
    let mut sf2 = String::new();

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
            _ => {}
        }
    }

    if kind.is_empty() {
        return Err(anyhow!("sidecar missing Kind: {}", path.display()));
    }
    if kind == "vst3" && plugin.is_empty() {
        return Err(anyhow!("vst3 sidecar missing Plugin: {}", path.display()));
    }
    if kind == "sf2" && sf2.is_empty() {
        return Err(anyhow!("sf2 sidecar missing File: {}", path.display()));
    }

    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    Ok(Instrument {
        name,
       kind,
       plugin,
       vst_path,
       state_path: if state.is_empty() {
           PathBuf::new()
       } else {
           dir.join(state)
       },
       sf2_path: if sf2.is_empty() {
           PathBuf::new()
       } else {
           PathBuf::from(&sf2)    // relative to project root, not the sidecar
       },
    })
}
