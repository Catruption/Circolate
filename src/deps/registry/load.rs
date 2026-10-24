//! Loads the shipped registry from `share/registry.json`, then overlays
//! the user's registry from `~/.config/circolate/registry.json` if one
//! exists.  Merge rules:
//!
//!   * instruments / effects  — user entries prepend; a `match` is only
//!                              taken from the first table that declares it
//!   * master                 — user replaces shipped if non-empty
//!   * search_paths           — extends; user paths append to shipped
//!   * on_missing             — user wins if set to non-default
//!   * defaults               — user wins if non-`None`

use anyhow::{Context, Result};
use std::collections::HashSet;

use super::schema::*;

pub fn load_shipped() -> Result<Registry> {
    // Use the centralized system data path (e.g., /usr/share/circolate)
    let path = crate::paths::system_data().join("registry.json");
    let text = std::fs::read_to_string(&path)
    .with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text)
    .with_context(|| format!("parsing {}", path.display()))
}

pub fn load_user() -> Option<Registry> {
    // Use the centralized user config path (e.g., ~/.config/circolate)
    let path = crate::paths::user_config().join("registry.json");
    if !path.is_file() {
        return None;
    }
    let text = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn load() -> Result<Registry> {
    let shipped = load_shipped()?;
    match load_user() {
        Some(user) => Ok(merge(shipped, user)),
        None => Ok(shipped),
    }
}

fn merge(shipped: Registry, user: Registry) -> Registry {
    let instruments = merge_mappings(&user.instruments, &shipped.instruments);
    let effects     = merge_mappings(&user.effects,     &shipped.effects);

    let master = if user.master.is_empty() { shipped.master } else { user.master };

    let search_paths = SearchPaths {
        system:   [shipped.search_paths.system,   user.search_paths.system].concat(),
        user:     [shipped.search_paths.user,     user.search_paths.user].concat(),
        relative: [shipped.search_paths.relative, user.search_paths.relative].concat(),
        include_user_config: shipped.search_paths.include_user_config
        || user.search_paths.include_user_config,
    };

    Registry {
        version: user.version.max(shipped.version),
        instruments,
        effects,
        default_instrument: user.default_instrument.or(shipped.default_instrument),
            default_effect:     user.default_effect.or(shipped.default_effect),
                master,
                search_paths,
                on_missing: user.on_missing,
    }
}

fn merge_mappings(user: &[Mapping], shipped: &[Mapping]) -> Vec<Mapping> {
    let mut out: Vec<Mapping> = Vec::with_capacity(user.len() + shipped.len());
    let mut seen: HashSet<String> = HashSet::new();
    for m in user.iter().chain(shipped.iter()) {
        let key = m.match_.primary().to_ascii_lowercase();
        if seen.insert(key) {
            out.push(m.clone());
        }
    }
    out
}
