//! Unified filesystem path resolution.
//!
//! Handles the transition from "running inside a git checkout" to
//! "installed as a system package". When installed, system files live
//! in /usr/share/circolate, user config in $XDG_CONFIG_HOME, and
//! downloaded plugins in $XDG_DATA_HOME.
//!
//! Trackscripts are split into two roots:
//!
//! - `bundled_trackscripts()` — package data, read-only after install
//!   (`/usr/share/circolate/trackscripts/`)
//! - `user_trackscripts()` — user exports, always writable
//!   (`~/.local/share/circolate/trackscripts/`)
//!
//! `resolve_track(name)` searches both, user first, so a user export
//! shadows the bundled demo of the same name.
//!
//! Plugins are resolved **by name only**.  Any path recorded in a
//! trackscript, `.instrument` sidecar, or `plugins_inventory.json` is
//! ignored — those were written on whatever machine did the
//! conversion, are almost certainly stale, and may leak the author's
//! home directory.  `find_plugin()` and `find_plugin_vst3()` are the
//! only sanctioned ways to locate a plugin at runtime.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

/// Where static, read-only assets live (fallbacks.json, player_settings.json).
pub fn system_data() -> PathBuf {
    if let Ok(p) = std::env::var("CIRCOLATE_DATA_DIR") {
        return PathBuf::from(p);
    }
    let sys = Path::new("/usr/share/circolate");
    if sys.is_dir() {
        return sys.to_path_buf();
    }
    if let Some(repo) = repo_root() {
        let share = repo.join("share").join("circolate");
        if share.is_dir() {
            return share;
        }
    }
    let cwd = Path::new("share").join("circolate");
    if cwd.is_dir() {
        return cwd.to_path_buf();
    }
    sys.to_path_buf()
}

/// Where user-writable config lives (player_settings.json overrides).
pub fn user_config() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("circolate");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".config").join("circolate");
    }
    PathBuf::from(".config").join("circolate")
}

/// Where user-writable data lives (downloaded plugins, soundfonts, exports).
pub fn user_data() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        return PathBuf::from(xdg).join("circolate");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home)
        .join(".local")
        .join("share")
        .join("circolate");
    }
    PathBuf::from(".local").join("share").join("circolate")
}

/// Where cache files live.
pub fn user_cache() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
        return PathBuf::from(xdg).join("circolate");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".cache").join("circolate");
    }
    std::env::temp_dir().join("circolate-cache")
}

/// Find the root of the git checkout, if running from source.
pub fn repo_root() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        let mut d = exe.parent();
        while let Some(p) = d {
            if p.join("Cargo.toml").is_file() {
                return Some(p.to_path_buf());
            }
            d = p.parent();
        }
    }
    if Path::new("Cargo.toml").is_file() {
        return std::env::current_dir().ok();
    }
    None
}

// ── trackscripts ──────────────────────────────────────────────────────

pub fn user_trackscripts() -> PathBuf {
    user_data().join("trackscripts")
}

pub fn bundled_trackscripts() -> PathBuf {
    system_data().join("trackscripts")
}

pub fn trackscripts_dir() -> PathBuf {
    let user = user_trackscripts();
    if user.is_dir() {
        return user;
    }
    let bundled = bundled_trackscripts();
    if bundled.is_dir() {
        return bundled;
    }
    if let Some(repo) = repo_root() {
        let dev = repo.join("trackscripts");
        if dev.is_dir() {
            return dev;
        }
    }
    user
}

/// Resolve a user-typed name to a concrete trackscript path.  See the
/// module docs for search order.
pub fn resolve_track(name: &str) -> Result<PathBuf> {
    let name = name.trim();
    if name.is_empty() {
        bail!("empty song name");
    }
    let as_path = Path::new(name);
    if as_path.is_file() {
        return Ok(as_path.to_path_buf());
    }

    let mut searched: Vec<PathBuf> = Vec::new();
    for dir in [user_trackscripts(), bundled_trackscripts()] {
        for ext in ["", ".tsz", ".trackscript", ".flp", ".it"] {
            let candidate = dir.join(format!("{}{}", name, ext));
            searched.push(candidate.clone());
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
        let dir_form = dir.join(name).join("trackscript");
        searched.push(dir_form.clone());
        if dir_form.is_file() {
            return Ok(dir_form);
        }
    }

    bail!(
        "no track named {:?}\nsearched:\n{}",
        name,
        searched
        .iter()
        .map(|p| format!("  {}", p.display()))
        .collect::<Vec<_>>()
        .join("\n")
    )
}

// ── plugins ───────────────────────────────────────────────────────────

/// Find any plugin (VST3, VST2, LV2, CLAP) by name.
///
/// This is the only sanctioned way to locate a plugin at runtime.
/// Callers must **not** consult a recorded `Path:` first — those are
/// stale and doxxing.  `PluginIndex::find_any` handles case
/// normalization, `" #N"` suffixes, and stem-vs-filename differences.
///
/// A fresh scan runs on every call.  If callers are hot (inside a
/// loader loop), cache the index at the outer scope instead.
pub fn find_plugin(name: &str) -> Option<PathBuf> {
    crate::deps::index::PluginIndex::scan()
    .find_any(name)
    .cloned()
}

/// Same as `find_plugin`, restricted to VST3 bundles.  Use when the
/// consumer cannot host VST2/LV2/CLAP (e.g. `VstHost::load`).
pub fn find_plugin_vst3(name: &str) -> Option<PathBuf> {
    crate::deps::index::PluginIndex::scan()
    .find_vst3(name)
    .cloned()
}
