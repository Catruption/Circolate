// src/convert/flp/resolve.rs
//! Plugin resolution: exact match, fallback, or unresolved.  Emits the
//! `.instrument` and `.state` sidecars that the player reads back.

use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::conversion::AuxFile;
use crate::deps::plugins::PluginTable;
use crate::deps::registry::{RegistryTable, Resolution};
use crate::formats::flp::model::{self as flp_src, Channel};

pub(super) enum Resolved {
    Exact { name: String, path: PathBuf },
    Fallback {
        name: String,
        path: Option<PathBuf>,
        replaces: String,
        gain: f32,
        via_default: bool,
        state: Option<PathBuf>, // <--- NEW
    },
    Unresolved { name: String },
}

pub(super) fn canonical_name(plugin: &flp_src::Plugin) -> String {
    if !plugin.display_name.is_empty() {
        plugin.display_name.clone()
    } else {
        plugin.internal_name.clone()
    }
}

fn name_candidates(plugin: &flp_src::Plugin) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: &str| {
        let s = s.trim();
        if !s.is_empty() && !out.iter().any(|x| x == s) {
            out.push(s.to_string());
        }
    };

    push(&plugin.display_name);
    push(&plugin.internal_name);

    if !plugin.vst_path.is_empty() {
        let p = Path::new(&plugin.vst_path);
        if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
            push(stem);
            for suffix in ["_x64", "_64", "x64", "64", "_vst3", "_vst"] {
                if let Some(stripped) = stem.strip_suffix(suffix) {
                    push(stripped);
                }
            }
        }
        if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
            push(name);
        }
    }
    out
}

fn find_loose(wanted: &str, index: &PluginTable) -> Option<PathBuf> {
    if let Some(p) = index.find_any(wanted) {
        return Some(p.clone());
    }

    let no_space: String = wanted.chars().filter(|c| !c.is_whitespace()).collect();
    if no_space != wanted {
        if let Some(p) = index.find_any(&no_space) {
            return Some(p.clone());
        }
    }

    let no_underscore: String = wanted.chars().filter(|c| *c != '_').collect();
    if no_underscore != wanted && no_underscore != no_space {
        if let Some(p) = index.find_any(&no_underscore) {
            return Some(p.clone());
        }
    }

    if !wanted.contains(' ') {
        let mut spaced = String::with_capacity(wanted.len() + 8);
        for (i, c) in wanted.chars().enumerate() {
            if i > 0 && c.is_ascii_uppercase() {
                spaced.push(' ');
            }
            spaced.push(c);
        }
        if spaced != wanted {
            if let Some(p) = index.find_any(&spaced) {
                return Some(p.clone());
            }
        }
    }

    None
}

pub(super) fn resolve_plugin(
    plugin: &flp_src::Plugin,
    index: &PluginTable,
    registry: Option<&RegistryTable>,
) -> Resolved {
    let canonical = canonical_name(plugin);

    for candidate in name_candidates(plugin) {
        if let Some(path) = index.find_any(&candidate) {
            return Resolved::Exact { name: candidate, path: path.clone() };
        }
    }

    if let Some(table) = registry {
        return match table.resolve_instrument(index, &canonical) {
            Resolution::Exact { name, path } => Resolved::Exact { name, path },
            Resolution::Fallback { name, path, replaces, gain, via_default, state } => {
                Resolved::Fallback {
                    name,
                    path: Some(path),
                    replaces,
                    gain,
                    via_default,
                    state,
                }
            }
            Resolution::FallbackMissing { name, replaces } => {
                if let Some(p) = find_loose(&name, index) {
                    Resolved::Fallback {
                        name,
                        path: Some(p),
                        replaces,
                        gain: 1.0,
                        via_default: false,
                        state: None,
                    }
                } else {
                    Resolved::Fallback {
                        name,
                        path: None,
                        replaces,
                        gain: 1.0,
                        via_default: false,
                        state: None,
                    }
                }
            }
            Resolution::NativeArtifact { name, path, replaces, gain } => {
                Resolved::Fallback {
                    name,
                    path: Some(path),
                    replaces,
                    gain,
                    via_default: false,
                    state: None,
                }
            }
            Resolution::NativeMissing { replaces, .. } => {
                Resolved::Unresolved {
                    name: format!("{} (native model not profiled)", replaces),
                }
            }
            Resolution::Skipped { note } => {
                Resolved::Unresolved { name: format!("{} ({})", canonical, note) }
            }
            Resolution::NoMatch => Resolved::Unresolved { name: canonical },
        };
    }

    Resolved::Unresolved { name: canonical }
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
        Resolved::Exact { name, path: _ } => {
            // No `Path:` field.  The player resolves `Plugin:` by name
            // through the shared plugin index, so the sidecar stays
            // portable and never leaks the machine it was made on.
            lines.push(format!("    Plugin: '{}';", name));
            if !plugin.state.is_empty() {
                lines.push(format!("    State: '{base_name}.state';"));
            }
        }
        Resolved::Fallback { name, path, replaces, gain, via_default, state } => {
            lines.push(format!("    Plugin: '{}';", name));
            if path.is_none() {
                lines.push("    Unresolved: true;".to_string());
                lines.push("    UnresolvedReason: 'fallback plugin not installed';".to_string());
            }
            lines.push(format!("    Replaces: '{}';", replaces));
            lines.push("    Fallback: true;".to_string());
            lines.push(format!("    Gain: {};", gain));
            if *via_default {
                lines.push("    ViaDefault: true;".to_string());
            }
            if let Some(st) = state {
                // State files travel with the archive.  Only a path
                // relative to a Circolate root may be recorded; an
                // absolute path from the converting machine is dropped.
                if let Some(rel) = crate::deps::plugins::clean_relative(st)
                    .or_else(|| PluginTable::get().relative_path(st))
                {
                    lines.push(format!("    State: '{}';", rel.display()));
                }
            }
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
