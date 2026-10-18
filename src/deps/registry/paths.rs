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

/// Walk up from the current executable looking for a directory that
/// contains a `share/` subfolder.  That's the "install root" — where
/// `share/registry.json` lives.  Returns `None` if we never find one,
/// which is fine; relative paths are simply skipped.
pub fn install_root() -> Option<PathBuf> {
    let mut dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    for _ in 0..4 {
        if dir.join("share").is_dir() {
            return Some(dir);
        }
        dir = dir.parent()?.to_path_buf();
    }
    None
}

/// `$XDG_CONFIG_HOME/circolate` or `~/.config/circolate`.
pub fn user_config_dir() -> Option<PathBuf> {
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(x).join("circolate"));
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".config").join("circolate"))
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

    for s in &sp.system {
        push(expand_tilde(s));
    }
    for s in &sp.user {
        push(expand_tilde(s));
    }
    if let Some(root) = install_root() {
        for s in &sp.relative {
            push(root.join(s));
        }
    }
    // Filter to directories that actually exist.  No need to scan
    // paths that will just fail.
    out.into_iter().filter(|p| p.is_dir()).collect()
}
/// The directory where shipped native-effect artifacts live.  Sits
/// next to `share/registry.json` so the whole native-effect model set
/// travels with the binary.
pub fn native_fx_dir() -> Option<PathBuf> {
    install_root().map(|r| r.join("share").join("native_fx"))
}
