// src/convert/flp/resolve.rs
//! Plugin resolution: exact match, fallback, or unresolved.  Emits the
//! `.instrument` and `.state` sidecars that the player reads back.

use anyhow::Result;
use std::path::PathBuf;

use crate::convert::AuxFile;
use crate::fallbacks::FallbackTable;
use crate::plugins::PluginIndex;
use crate::sources::flp::{self as flp_src, Channel};

/// What resolution ended up choosing for one channel.
pub(super) enum Resolved {
    /// Exact plugin found installed.  Load original state.
    Exact { name: String, path: PathBuf },
    /// Fallback substituted.  Do not load original state.  `gain` is a
    /// linear trim applied to the plugin's *audio output* so a
    /// replacement synth's default patch doesn't drown the mix.  The
    /// player applies this after the plugin runs, since many synths
    /// ignore MIDI velocity entirely.
    Fallback { name: String, path: Option<PathBuf>, replaces: String, gain: f32 },
    /// Nothing found, no fallback.  Channel is dead.
    Unresolved { name: String },
}

pub(super) fn resolve_plugin(
    plugin: &flp_src::Plugin,
    index: &PluginIndex,
    fallbacks: Option<&FallbackTable>,
) -> Resolved {
    let display = if !plugin.display_name.is_empty() {
        plugin.display_name.clone()
    } else {
        plugin.internal_name.clone()
    };

    if let Some(path) = index.find_vst3(&display) {
        return Resolved::Exact { name: display, path: path.clone() };
    }

    if let Some(table) = fallbacks {
        if let Some((_m, target)) = table.resolve(&plugin.vst_path, &display) {
            let target_path = index.find_vst3(&target.name).cloned();
            return Resolved::Fallback {
                name: target.name.clone(),
                path: target_path,
                replaces: display,
                gain: target.gain,
            };
        }
    }

    if let Some(table) = fallbacks {
        let rule = table.default_rule();
        if let Some(target) = &rule.fallback {
            let target_path = index.find_vst3(&target.name).cloned();
            return Resolved::Fallback {
                name: target.name.clone(),
                path: target_path,
                replaces: display,
                gain: target.gain,
            };
        }
    }

    Resolved::Unresolved { name: display }
}

pub(super) fn vst_sidecar(
    base_name: &str,
    chan: &Channel,
    resolved: &Resolved,
) -> Result<Vec<AuxFile>> {
    let plugin = chan.plugin.as_ref().expect("caller checked plugin presence");

    let mut lines = vec![
        format!("Instrument {base_name}"),
            "{".to_string(),
            "    Kind: vst3;".to_string(),
    ];

    match resolved {
        Resolved::Exact { name, path } => {
            lines.push(format!("    Plugin: '{}';", name));
            lines.push(format!("    Path: '{}';", path.display()));
            if !plugin.state.is_empty() {
                lines.push(format!("    State: '{base_name}.state';"));
            }
        }
        Resolved::Fallback { name, path, replaces, gain } => {
            lines.push(format!("    Plugin: '{}';", name));
            if let Some(p) = path {
                lines.push(format!("    Path: '{}';", p.display()));
            } else {
                lines.push("    Unresolved: true;".to_string());
            }
            lines.push(format!("    Replaces: '{}';", replaces));
            lines.push("    Fallback: true;".to_string());
            // Post-plugin output trim.  Applied by the engine after the
            // VST has run, so it works even on patches that ignore
            // velocity or that set their own internal gain.
            lines.push(format!("    Gain: {};", gain));
        }
        Resolved::Unresolved { name } => {
            lines.push(format!("    Plugin: '{}';", name));
            lines.push("    Unresolved: true;".to_string());
        }
    }

    if !plugin.internal_name.is_empty() {
        lines.push(format!("    OriginalPlugin: '{}';", plugin.internal_name));
    }
    lines.push(format!("    SourceChannel: {};", chan.iid));
    lines.push("}".to_string());

    let inst_text = lines.join("\n") + "\n";
    let inst_rel = format!("instruments/{base_name}.instrument");

    let mut files = vec![AuxFile {
        rel_path: inst_rel,
        bytes: inst_text.into_bytes(),
    }];

    // Only write the raw state blob when we're loading the exact plugin
    // it came from.  State for one plugin fed to another corrupts the
    // load.
    if let Resolved::Exact { .. } = resolved {
        if !plugin.state.is_empty() {
            files.push(AuxFile {
                rel_path: format!("instruments/{base_name}.state"),
                       bytes: plugin.state.clone(),
            });
        }
    }

    Ok(files)
}