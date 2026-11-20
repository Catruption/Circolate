//! Search-path resolution.  Turns the `search_paths` section of a
//! registry into a flat `Vec<PathBuf>` that `PluginTable` can scan.
//!
//! This is the **only** place that knows where plugins live.  The
//! converter, player, archive writer and `plugins` command all go
//! through `PluginTable`, which in turn calls `effective()` here.
//!
//! Sources, in priority order:
//!   1. `CIRCOLATE_PLUGIN_PATH` / `VST3_PATH` (PATH-style overrides)
//!   2. layered `plugins/` roots: user root, localshare, system share
//!   3. `search_paths.system` from the registry
//!   4. `search_paths.user` from the registry
//!   5. `search_paths.relative` from the registry (resolved against
//!      the checkout/install root)
//!   6. standard per-user plugin directories (Unix and Windows)
//!   7. standard system plugin directories (Unix and Windows)
//!
//! Recorded absolute paths in project files are deliberately absent.
//! They are machine-specific and must never be treated as truth.

use serde::Deserialize;
use std::path::PathBuf;

use super::schema::SearchPaths;

const DEFAULT_SYSTEM_DIRS: &[&str] = &[
    "/usr/lib/vst3",
    "/usr/local/lib/vst3",
    "/usr/lib/vst",
    "/usr/local/lib/vst",
    "/usr/lib/lxvst",
    "/usr/local/lib/lxvst",
    "/usr/lib/lv2",
    "/usr/local/lib/lv2",
    "/usr/lib/clap",
    "/usr/local/lib/clap",
];

const DEFAULT_USER_DIRS: &[&str] = &[".vst3", ".vst", ".lxvst", ".lv2", ".clap"];

/// Source-checkout bundles.  These are relative to the repo root, so the
/// scan stays portable — no absolute machine path is ever recorded.
/// Only packaged layouts live here; local ignored stashes are deliberately
/// not built in.  Put those in `directories.json` or a plugin path override.
const DEFAULT_REPO_DIRS: &[&str] = &[
    "plugins",
    "plugins/vst3",
];

/// The directory where shipped native-effect artifacts live.  Sits
/// next to `share/registry.json` so the whole native-effect model set
/// travels with the binary.
pub fn native_fx_dir() -> Option<PathBuf> {
    Some(crate::paths::system_data().join("native_fx"))
}

/// `$XDG_CONFIG_HOME/circolate` or `~/.config/circolate`.
pub fn user_config_dir() -> Option<PathBuf> {
    Some(crate::paths::user_config())
}

/// Expand a leading `~` using `$HOME`.
pub fn expand_tilde(s: &str) -> PathBuf {
    crate::paths::expand_tilde(s)
}

/// The configured search paths, with defaults filled in.
///
/// Missing or malformed `fallbacks.json` is not fatal: the standard
/// directories still work, which keeps `circolate plugins` useful on a
/// bare system.
pub fn effective() -> Vec<PathBuf> {
    #[derive(Deserialize, Default)]
    struct SearchPathConfig {
        #[serde(default)]
        search_paths: SearchPaths,
    }

    let path = crate::paths::layered_file(crate::deps::fallbacks::FALLBACKS_FILENAME)
    .unwrap_or_else(|| crate::paths::system_data().join(crate::deps::fallbacks::FALLBACKS_FILENAME));
    let configured = std::fs::read_to_string(path)
    .ok()
    .and_then(|text| serde_json::from_str::<SearchPathConfig>(&text).ok())
    .map(|c| c.search_paths)
    .unwrap_or_default();

    resolve_all(&configured)
}

/// Every configured candidate, including directories that do not exist
/// yet.  Used by `install` and `doctor`; runtime discovery keeps using
/// [`effective`], which filters to real directories.
pub fn effective_candidates() -> Vec<PathBuf> {
    #[derive(Deserialize, Default)]
    struct SearchPathConfig {
        #[serde(default)]
        search_paths: SearchPaths,
    }

    let path = crate::paths::layered_file(crate::deps::fallbacks::FALLBACKS_FILENAME)
    .unwrap_or_else(|| crate::paths::system_data().join(crate::deps::fallbacks::FALLBACKS_FILENAME));
    let configured = std::fs::read_to_string(path)
    .ok()
    .and_then(|text| serde_json::from_str::<SearchPathConfig>(&text).ok())
    .map(|c| c.search_paths)
    .unwrap_or_default();

    resolve_all_inner(&configured, false)
}

/// Flatten the search paths into a de-duplicated, ordered list.
pub fn resolve_all(sp: &SearchPaths) -> Vec<PathBuf> {
    resolve_all_inner(sp, true)
}

fn resolve_all_inner(sp: &SearchPaths, existing_only: bool) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut push = |p: PathBuf| {
        if !out.iter().any(|q| q == &p) {
            out.push(p);
        }
    };

    // 1. Explicit environment override.  This is how a user points at a
    //    plugin in a non-standard location without editing JSON.
    for var in ["CIRCOLATE_PLUGIN_PATH", "VST3_PATH"] {
        if let Some(raw) = std::env::var_os(var) {
            for p in std::env::split_paths(&raw) {
                push(p);
            }
        }
    }

    // 2. The layered `plugins/` tree: optional user root, Circolate
    //    localshare, then shipped system data.  Old per-type roots are
    //    still read so existing configs keep working.
    for dir in crate::paths::layered_dirs("plugins") {
        push(dir);
    }
    for p in crate::paths::legacy_plugin_dirs() {
        push(p);
    }

    // 3. Explicit registry paths (system & user)
    for s in &sp.system {
        push(expand_tilde(s));
    }
    for s in &sp.user {
        push(expand_tilde(s));
    }

    // 4. Relative paths (resolved against the repo root in dev, or
    //    system_data in a packaged install).
    let root = crate::paths::repo_root().unwrap_or_else(crate::paths::system_data);
    for s in &sp.relative {
        push(root.join(s));
    }
    for s in DEFAULT_REPO_DIRS {
        push(root.join(s));
    }

    // 5. Optional per-user config directory.
    if sp.include_user_config {
        if let Some(dir) = user_config_dir() {
            push(dir.join("plugins"));
        }
    }

    // 6. Standard per-user directories.
    if let Some(home) = std::env::var_os("HOME") {
        for dir in DEFAULT_USER_DIRS {
            push(PathBuf::from(&home).join(dir));
        }
    }

    // 7. Standard Windows directories.  These are exported by the OS, so
    //    they are real "drop a plugin here" locations on Windows even when
    //    no Circolate config mentions them.
    for dir in windows_plugin_dirs() {
        push(dir);
    }

    // 8. Standard Unix system directories.
    for dir in DEFAULT_SYSTEM_DIRS {
        push(PathBuf::from(dir));
    }

    // A checkout without a registry still gets `<repo>/plugins`.
    if let Some(repo) = crate::paths::repo_root() {
        push(repo.join("plugins"));
    }

    if existing_only {
        out.into_iter().filter(|p| p.is_dir()).collect()
    } else {
        out
    }
}

/// Standard Windows plugin locations advertised by the OS.  `VST3_PATH`
/// and `CIRCOLATE_PLUGIN_PATH` are still checked first; these are the
/// common locations people expect a host to search without configuration.
fn windows_plugin_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut push = |base: &std::ffi::OsStr, sub: &str| {
        let path = PathBuf::from(base).join(sub);
        if !out.iter().any(|p: &PathBuf| p == &path) {
            out.push(path);
        }
    };

    for var in [
        "ProgramFiles",
        "ProgramFiles(x86)",
        "COMMONPROGRAMFILES",
        "COMMONPROGRAMFILES(X86)",
        "LOCALAPPDATA",
    ] {
        if let Some(root) = std::env::var_os(var) {
            for sub in ["VST3", "VST", "VST32"] {
                push(&root, sub);
            }
            if var == "LOCALAPPDATA" {
                push(&root, "Programs/VST3");
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_roots_never_include_ignored_stashes() {
        let candidates = resolve_all(&SearchPaths::default());
        assert!(candidates.iter().all(|p| !p.to_string_lossy().contains("system (ignore)")));
    }
}
