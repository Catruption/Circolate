//! Search-path resolution.  Turns the `search_paths` section of a
//! registry into a flat `Vec<PathBuf>` that `PluginIndex` can scan.
//!
//! Three categories:
//!   * `system`   — absolute system locations (`/usr/lib/vst3`, ...)
//!   * `user`     — `~`-prefixed per-user locations
//!   * `relative` — resolved against the install root (the directory
//!                  containing `share/`), so a `plugins/` folder next
//!                  to the shipped binary is found automatically.

use std::path::PathBuf;

use super::schema::SearchPaths;

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
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(s)
}

/// Flatten the search paths into a de-duplicated, ordered list.
pub fn resolve_all(sp: &SearchPaths) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut push = |p: PathBuf| {
        if !out.iter().any(|q| q == &p) {
            out.push(p);
        }
    };

    // 1. User data (where `circolate fetch` downloads plugins)
    push(crate::paths::user_data().join("plugins"));

    // 2. Explicit registry paths (system & user)
    for s in &sp.system {
        push(expand_tilde(s));
    }
    for s in &sp.user {
        push(expand_tilde(s));
    }

    // 3. Relative paths (resolved against the repo root in dev, or
    //    system_data in a packaged install).
    let root = crate::paths::repo_root().unwrap_or_else(crate::paths::system_data);
    for s in &sp.relative {
        push(root.join(s));
    }

    // Filter to directories that actually exist.
    out.into_iter().filter(|p| p.is_dir()).collect()
}
