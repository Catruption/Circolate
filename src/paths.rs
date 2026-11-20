//! Unified filesystem path resolution.
//!
//! Handles the transition from "running inside a git checkout" to
//! "installed as a system package". When installed, system files live
//! in /usr/share/circolate, user config in $XDG_CONFIG_HOME, and
//! downloaded plugins in $XDG_DATA_HOME.
//!
//! Halotexts are split into two roots:
//!
//! - `bundled_halotexts()` — package data, read-only after install
//!   (`/usr/share/circolate/halotexts/`)
//! - `user_halotexts()` — user exports, always writable
//!   (`~/.local/share/circolate/halotexts/`)
//!
//! `resolve_track(name)` searches both, user first, so a user export
//! shadows the bundled demo of the same name.
//!
//! Plugins are resolved **by name only**.  Any path recorded in a
//! halotext, `.instrument` sidecar, or `plugins_inventory.json` is
//! ignored — those were written on whatever machine did the
//! conversion, are almost certainly stale, and may leak the author's
//! home directory.  `deps::plugins::PluginTable::get()` is the only
//! sanctioned way to locate a plugin at runtime.
//!
//! Soundfonts follow the same rule: they are resolved **by name**
//! against a set of standard system roots (see `soundfont_roots()`),
//! never by the absolute path recorded in a project file.  That covers
//! the Debian legacy layout (`/usr/share/soundfonts/`,
//! `/usr/share/sounds/sf2/`), the XDG layout
//! (`$XDG_DATA_HOME/soundfonts/`, `$XDG_DATA_DIRS/soundfonts/`), and
//! the dev-checkout layout (`<repo>/share/soundfonts/`).  Paths
//! authored on Windows (`C:\...\x.sf2`) are reduced to their basename
//! before lookup, which is the only portable interpretation.

use std::path::{Path, PathBuf};
use std::sync::Once;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

static SYSTEM_DATA_WARN: Once = Once::new();

/// Circolate-owned settings and the optional user root live in this file.
/// Shipped defaults live next to `fallbacks.json`; the user copy is what the
/// TUI and `directories` command edit.
pub const DIRECTORIES_FILENAME: &str = "directories.json";

/// Optional environment override for the user root.  This is checked before
/// the saved config so a machine can point at a different tree per session.
pub const USER_ROOT_ENV: &str = "CIRCOLATE_USER_ROOT";

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct DirectoriesConfig {
    /// One user-owned root.  Its subdirectories mirror the managed
    /// `~/.local/share/circolate` layout exactly.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_root: Option<String>,

    // Deprecated per-type roots.  Still read so old installs keep working,
    // but never written again: one user root replaces all five arrays.
    #[serde(rename = "plugins", default, skip_serializing)]
    pub legacy_plugins: Vec<String>,
    #[serde(rename = "soundfonts", default, skip_serializing)]
    pub legacy_soundfonts: Vec<String>,
    #[serde(rename = "halotexts", default, skip_serializing)]
    pub legacy_halotexts: Vec<String>,
    #[serde(rename = "samples", default, skip_serializing)]
    pub legacy_samples: Vec<String>,
    #[serde(rename = "patches", default, skip_serializing)]
    pub legacy_patches: Vec<String>,
}

impl DirectoriesConfig {
    fn merge(&mut self, next: Self) {
        if next.user_root.is_some() {
            self.user_root = next.user_root;
        }
        extend_unique(&mut self.legacy_plugins, next.legacy_plugins);
        extend_unique(&mut self.legacy_soundfonts, next.legacy_soundfonts);
        extend_unique(&mut self.legacy_halotexts, next.legacy_halotexts);
        extend_unique(&mut self.legacy_samples, next.legacy_samples);
        extend_unique(&mut self.legacy_patches, next.legacy_patches);
    }

    /// Deprecated roots, still honoured as extra search paths until the
    /// user migrates them into `user_root`.
    pub fn legacy_paths(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for group in [
            &self.legacy_plugins,
            &self.legacy_soundfonts,
            &self.legacy_halotexts,
            &self.legacy_samples,
            &self.legacy_patches,
        ] {
            for raw in group {
                let path = expand_tilde(raw);
                if !path.as_os_str().is_empty() && !out.contains(&path) {
                    out.push(path);
                }
            }
        }
        out
    }
}

fn extend_unique(dst: &mut Vec<String>, src: Vec<String>) {
    for s in src {
        if !dst.iter().any(|x| x == &s) {
            dst.push(s);
        }
    }
}

/// Expand a leading `~` using `$HOME`; leaves everything else untouched.
pub fn expand_tilde(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(s)
}

fn read_directories(path: &Path) -> Option<DirectoriesConfig> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Shipped defaults, then the old XDG config location, then localshare.
/// A malformed or absent file is not fatal: built-in roots still work.
pub fn load_directories() -> DirectoriesConfig {
    let mut cfg = DirectoriesConfig::default();
    if let Some(next) = read_directories(&system_data().join(DIRECTORIES_FILENAME)) {
        cfg.merge(next);
    }
    if let Some(next) = read_directories(&legacy_user_config().join(DIRECTORIES_FILENAME)) {
        cfg.merge(next);
    }
    if let Some(next) = read_directories(&user_config().join(DIRECTORIES_FILENAME)) {
        cfg.merge(next);
    }
    cfg
}

/// Ensure `path` is a usable directory, following symlinks whose target does
/// not exist yet.  `create_dir_all` reports `AlreadyExists` for that case,
/// which breaks the common setup where `~/.local/share/circolate` is a link
/// into a checkout that has not been created (or has been moved).
pub fn ensure_dir(path: &Path) -> Result<()> {
    let mut current = path.to_path_buf();
    for _ in 0..16 {
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                let target = std::fs::read_link(&current)?;
                current = if target.is_absolute() {
                    target
                } else {
                    current.parent().unwrap_or_else(|| Path::new(".")).join(target)
                };
            }
            Ok(meta) if meta.is_dir() => return Ok(()),
            Ok(_) => bail!("{} is not a directory", current.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if let Some(parent) = current.parent().filter(|p| !p.as_os_str().is_empty()) {
                    ensure_dir(parent)?;
                }
                std::fs::create_dir_all(&current)?;
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        }
    }
    bail!("too many symlinks while resolving {}", path.display())
}

/// Persist only the new one-root config to Circolate's localshare tree.
pub fn save_user_directories(cfg: &DirectoriesConfig) -> Result<()> {
    let dir = user_config();
    ensure_dir(&dir)?;
    let path = dir.join(DIRECTORIES_FILENAME);
    let saved = DirectoriesConfig {
        user_root: cfg.user_root.clone(),
        ..DirectoriesConfig::default()
    };
    let text = serde_json::to_string_pretty(&saved)?;
    std::fs::write(&path, format!("{text}\n"))?;
    Ok(())
}

/// The effective user root: environment override first, then saved config.
pub fn user_root() -> Option<PathBuf> {
    if let Some(raw) = std::env::var_os(USER_ROOT_ENV) {
        let path = PathBuf::from(raw);
        if !path.as_os_str().is_empty() {
            return Some(path);
        }
    }
    load_directories()
    .user_root
    .as_deref()
    .map(expand_tilde)
    .filter(|p| !p.as_os_str().is_empty())
}

/// The same subdirectory under each managed layer, in priority order.
pub fn layered_dirs(name: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(root) = user_root() {
        out.push(root.join(name));
    }
    out.push(user_data().join(name));
    out.push(system_data().join(name));
    out
}

/// First existing file of `name` in the same layered tree.  Lets a user
/// override shipped JSON (fallbacks, registry settings) by copying it into
/// their root, instead of editing files under `/usr/share`.
pub fn layered_file(name: &str) -> Option<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(root) = user_root() {
        dirs.push(root);
    }
    dirs.push(user_data());
    dirs.push(system_data());
    dirs.into_iter()
    .map(|dir| dir.join(name))
    .find(|p| p.is_file())
}

/// Explicitly configured locations, used only to avoid listing a path twice
/// in diagnostics.  The user root matches as a prefix because its
/// subdirectories are shown as part of that single editable setting.
pub fn configured_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(root) = user_root() {
        out.push(root);
    }
    out.extend(legacy_dirs());
    out
}

/// Legacy per-type roots, kept as read-only search paths for migration.
pub fn legacy_plugin_dirs() -> Vec<PathBuf> {
    load_directories().legacy_plugins.iter().map(|s| expand_tilde(s)).collect()
}

pub fn legacy_soundfont_dirs() -> Vec<PathBuf> {
    load_directories().legacy_soundfonts.iter().map(|s| expand_tilde(s)).collect()
}

pub fn legacy_halotext_dirs() -> Vec<PathBuf> {
    load_directories().legacy_halotexts.iter().map(|s| expand_tilde(s)).collect()
}

/// All deprecated roots, for diagnostics and the always-checked catalogue.
pub fn legacy_dirs() -> Vec<PathBuf> {
    load_directories().legacy_paths()
}

/// Where static, read-only assets live (fallbacks.json, player_settings.json).
pub fn system_data() -> PathBuf {
    if let Ok(p) = std::env::var("CIRCOLATE_DATA_DIR") {
        return PathBuf::from(p);
    }
    let sys = Path::new("/usr/share/circolate");
    if sys.is_dir() {
        return sys.to_path_buf();
    }
    // Source checkout: `share/` is the canonical data root.  Accept the
    // older `share/circolate/` layout too so existing checkouts still work.
    if let Some(repo) = repo_root() {
        let share = repo.join("share");
        if share.join("fallbacks.json").is_file() {
            return share;
        }
        let nested = share.join("circolate");
        if nested.is_dir() {
            return nested;
        }
    }
    let cwd_share = Path::new("share");
    if cwd_share.join("fallbacks.json").is_file() {
        return cwd_share.to_path_buf();
    }
    let cwd = cwd_share.join("circolate");
    if cwd.is_dir() {
        return cwd.to_path_buf();
    }
    SYSTEM_DATA_WARN.call_once(|| {
        eprintln!(
            "warning: system data dir {} does not exist; \
set CIRCOLATE_DATA_DIR or install the package data",
sys.display()
        );
    });
    sys.to_path_buf()
}

/// Circolate-owned settings live beside its data in localshare.  One tree
/// keeps settings easy to find, back up, and save from the TUI.
pub fn user_config() -> PathBuf {
    user_data()
}

/// Old XDG config location.  Read as a fallback so existing installs keep
/// their settings; nothing is written here any more.
pub fn legacy_user_config() -> PathBuf {
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

/// Fish's autoload directory for functions.  Fish uses
/// `$XDG_CONFIG_HOME/fish/functions`, falling back to `~/.config/fish/functions`.
pub fn fish_functions_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("fish").join("functions");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".config").join("fish").join("functions");
    }
    PathBuf::from(".config").join("fish").join("functions")
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

/// Where user-installed VST3/LV2/CLAP bundles go.
pub fn user_plugins() -> PathBuf {
    user_data().join("plugins")
}

/// Where user-installed SoundFonts go.
pub fn user_soundfonts() -> PathBuf {
    user_data().join("soundfonts")
}

/// Where `.instrument` sidecars go.
pub fn user_instruments() -> PathBuf {
    user_data().join("instruments")
}

/// Where loose sample libraries go.
pub fn user_samples() -> PathBuf {
    user_data().join("samples")
}

/// Where chip patches go.
pub fn user_patches() -> PathBuf {
    user_data().join("patches")
}

/// Where extracted cover images go.  Covers are Circolate-owned assets, so
/// they live in the localshare tree rather than a separate config folder.
pub fn user_covers() -> PathBuf {
    user_data().join("covers")
}

/// Where Circolate's own log files go.
pub fn user_logs() -> PathBuf {
    user_cache().join("logs")
}

/// Where downloaded archives are staged before extraction.
pub fn user_downloads() -> PathBuf {
    user_cache().join("downloads")
}

/// The checkout-local plugin bundle, when running from source.
pub fn repo_plugins() -> Option<PathBuf> {
    repo_root().map(|p| p.join("plugins"))
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

/// Standard XDG data directories, split out for use by `soundfont_roots`.
///
/// Per the XDG spec, `$XDG_DATA_DIRS` defaults to `/usr/local/share:/usr/share`
/// when unset or empty.  We implement that default so every common layout
/// (Debian, Fedora, Arch, NixOS with the env var set) works without per-distro
/// special-casing.
fn xdg_data_dirs() -> Vec<PathBuf> {
    let raw = std::env::var("XDG_DATA_DIRS")
    .unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());
    raw.split(':')
    .filter(|s| !s.is_empty())
    .map(PathBuf::from)
    .collect()
}

/// SoundFont folders exported by Windows.  There is no single universal
/// variable, so check the ones installers and sample libraries actually use.
fn windows_soundfont_roots() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut push = |base: &std::ffi::OsStr, name: &str| {
        let path = PathBuf::from(base).join(name);
        if !out.iter().any(|p: &PathBuf| p == &path) {
            out.push(path);
        }
    };

    for var in ["ProgramData", "APPDATA", "LOCALAPPDATA", "USERPROFILE"] {
        if let Some(root) = std::env::var_os(var) {
            push(&root, "Soundfonts");
            push(&root, "soundfonts");
            if var == "USERPROFILE" {
                push(&root, "Documents/Soundfonts");
            }
        }
    }
    out
}

// ── halotexts ──────────────────────────────────────────────────────

pub fn user_halotexts() -> PathBuf {
    user_data().join("halotexts")
}

pub fn bundled_halotexts() -> PathBuf {
    system_data().join("halotexts")
}

pub fn halotexts_dir() -> PathBuf {
    let user = user_halotexts();
    if user.is_dir() {
        return user;
    }
    let bundled = bundled_halotexts();
    if bundled.is_dir() {
        return bundled;
    }
    if let Some(repo) = repo_root() {
        let dev = repo.join("halotexts");
        if dev.is_dir() {
            return dev;
        }
    }
    user
}

/// Where new halotexts go.  Always returns a directory that exists on
/// disk — creating it if necessary — so a first-run export never silently
/// falls back to the current working directory.
pub fn export_dir() -> PathBuf {
    let dir = user_halotexts();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!(
            "warning: could not create export dir {}: {}",
            dir.display(),
                  e
        );
    }
    dir
}

/// Resolve a user-typed name to a concrete halotext path.  See the
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

    // Search order: the optional user root, localshare, and system share
    // are the same layered tree.  Legacy roots are read for migration,
    // then the dev-checkout copy under <repo>/halotexts/ is a no-op after
    // install.
    let mut roots = layered_dirs("halotexts");
    roots.extend(legacy_halotext_dirs());
    if let Some(repo) = repo_root() {
        roots.push(repo.join("halotexts"));
    }

    let mut searched: Vec<PathBuf> = Vec::new();
    for dir in roots {
        for ext in ["", ".halo.txt", ".halo.zip", ".flp", ".it", ".mid", ".midi", ".vgm", ".vgz"] {
            let candidate = dir.join(format!("{}{}", name, ext));
            searched.push(candidate.clone());
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
        let dir_form = dir.join(name).join("halotext");
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

// ── soundfonts ────────────────────────────────────────────────────────

/// Every directory that might contain an `.sf2`, in priority order.
///
/// User-specific roots come first so a user-installed soundfont shadows
/// a system one with the same name.  Roots are returned whether or not
/// they exist — callers should filter with `.is_dir()` (which
/// `find_soundfont` does internally).
///
/// Covered layouts:
///
/// - `$CIRCOLATE_SOUNDFONT_DIR` — explicit override, always wins
/// - `<user root>/soundfonts/` — optional manually chosen root
/// - `$XDG_DATA_HOME/circolate/soundfonts/` — circolate-managed localshare
/// - `/usr/share/circolate/soundfonts/` — shipped system share
/// - `$XDG_DATA_HOME/soundfonts/` — per-user, system-wide convention
/// - `~/.soundfonts/` — legacy user convention (old sf2 managers)
/// - `$XDG_DATA_DIRS/soundfonts/` — covers `/usr/local/share/soundfonts/`
///   and `/usr/share/soundfonts/`, which is what Debian, Fedora, Arch,
///   and NixOS all ship packaged soundfonts into
/// - `/usr/share/sounds/sf2/`, `/usr/share/sounds/sf3/` — Debian legacy
///   locations, still populated by older packages
/// - Windows `%ProgramData%`, `%APPDATA%`, `%LOCALAPPDATA%` and
///   `%USERPROFILE%` SoundFont folders
/// - `<prefix>/share/soundfonts/` — sibling of `system_data()` for
///   non-standard prefixes; skipped when `system_data()` is a user
///   override (`CIRCOLATE_DATA_DIR`) since the sibling relation no
///   longer holds
/// - `<repo>/share/soundfonts/` — dev checkout
pub fn soundfont_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();

    let push_unique = |v: &mut Vec<PathBuf>, p: PathBuf| {
        if !v.contains(&p) {
            v.push(p);
        }
    };

    // 1. Explicit override
    if let Ok(p) = std::env::var("CIRCOLATE_SOUNDFONT_DIR") {
        push_unique(&mut roots, PathBuf::from(p));
    }

    // 2. Layered SoundFont trees: optional user root, localshare, then
    //    shipped system data.  Legacy per-type roots remain readable.
    for dir in layered_dirs("soundfonts") {
        push_unique(&mut roots, dir);
    }
    for p in legacy_soundfont_dirs() {
        push_unique(&mut roots, p);
    }

    // 3. Standard per-user soundfont dir
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        push_unique(&mut roots, PathBuf::from(xdg).join("soundfonts"));
    } else if let Ok(home) = std::env::var("HOME") {
        push_unique(
            &mut roots,
            PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("soundfonts"),
        );
    }

    // 4. Legacy user convention
    if let Ok(home) = std::env::var("HOME") {
        push_unique(&mut roots, PathBuf::from(home).join(".soundfonts"));
    }

    // 5. Standard system dirs from XDG_DATA_DIRS
    for dir in xdg_data_dirs() {
        push_unique(&mut roots, dir.join("soundfonts"));
    }

    // 6. Debian legacy paths — still populated by older packages
    push_unique(&mut roots, PathBuf::from("/usr/share/sounds/sf2"));
    push_unique(&mut roots, PathBuf::from("/usr/share/sounds/sf3"));

    // 7. Standard Windows locations exported by the OS
    for dir in windows_soundfont_roots() {
        push_unique(&mut roots, dir);
    }

    // 8. Sibling of system_data() for custom prefixes
    //    (e.g. /opt/circolate/share/circolate -> /opt/circolate/share/soundfonts).
    //    Skipped when system_data() is a user override, since the
    //    sibling relation is meaningless there.
    let sys = system_data();
    if sys.file_name().map_or(false, |f| f == "circolate") {
        if let Some(prefix_share) = sys.parent() {
            push_unique(&mut roots, prefix_share.join("soundfonts"));
        }
    }

    // 9. Dev checkout
    if let Some(repo) = repo_root() {
        push_unique(&mut roots, repo.join("share").join("soundfonts"));
    }

    roots
}

/// Extract the final path component from a path that may have been
/// written on Windows (`C:\...\x.sf2`) or Unix (`/a/b/x.sf2`).
///
/// Because `Path::file_name` on Linux treats backslashes as ordinary
/// characters, a Windows-recorded path cannot be split with the stdlib
/// alone.  This helper handles both conventions so callers can pass
/// recorded paths directly to `find_soundfont` without preprocessing.
pub fn soundfont_basename(recorded: &str) -> &str {
    let after_backslash = recorded.rsplit('\\').next().unwrap_or(recorded);
    after_backslash.rsplit('/').next().unwrap_or(after_backslash)
}

/// Find a soundfont by name.
///
/// Accepts:
///
/// - An absolute path that exists on disk (CLI override) — returned as-is
/// - A bare basename, with or without `.sf2` (`undertale`, `undertale.sf2`)
/// - A recorded path from a project file (Windows or Unix) — the basename
///   is extracted and resolved against `soundfont_roots()`, following the
///   module's "recorded paths are untrusted" rule
///
/// Matching is case-insensitive and ignores the extension in the final
/// comparison, so `UNDERTALE.SF2`, `undertale`, and `undertale.sf2` all
/// find the same file.  Returns `None` if nothing matches.
///
/// Cost is bounded by (number of roots × number of files per root) —
/// cheap at the scales soundfont libraries actually operate at (~100
/// files).  Callers that need to look up many soundfonts in a tight
/// loop should call `soundfont_roots()` once and cache.
pub fn find_soundfont(name: &str) -> Option<PathBuf> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }

    // 1. Existing absolute path — user typed it, respect it.
    let as_path = Path::new(name);
    if as_path.is_absolute() && as_path.is_file() {
        return Some(as_path.to_path_buf());
    }

    // 2. Normalize Windows-style paths to their basename.  This is
    //    what makes a project file recorded on Windows resolve here.
    let basename = soundfont_basename(name);

    // 3. Strip any existing extension so `undertale` and `undertale.sf2`
    //    both match a file called `undertale.sf2`.
    let want_stem = basename
    .rsplit_once('.')
    .map(|(stem, _)| stem)
    .unwrap_or(basename);

    for root in soundfont_roots() {
        if !root.is_dir() {
            continue;
        }

        // 3a. Direct hit: basename as given, or with `.sf2` appended.
        for candidate in [basename.to_string(), format!("{}.sf2", want_stem)] {
            let p = root.join(&candidate);
            if p.is_file() {
                return Some(p);
            }
        }

        // 3b. Directory scan for a case-insensitive stem match, so
        //     casing differences (FluidR3_GM vs fluidr3_gm) and
        //     extension differences (.SF2 vs .sf2) both resolve.
        if let Ok(entries) = std::fs::read_dir(&root) {
            for entry in entries.flatten() {
                let Some(fname) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                let Some((stem, _ext)) = fname.rsplit_once('.') else {
                    continue;
                };
                if stem.eq_ignore_ascii_case(want_stem) {
                    return Some(entry.path());
                }
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_root_config_serializes_only_user_root() {
        let cfg = DirectoriesConfig {
            user_root: Some("~/my-circolate".into()),
            ..DirectoriesConfig::default()
        };
        let json = serde_json::to_string(&cfg).unwrap();
        assert_eq!(json, r#"{"user_root":"~/my-circolate"}"#);

        let back: DirectoriesConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.user_root.as_deref(), Some("~/my-circolate"));
    }

    #[test]
    fn legacy_per_type_roots_are_read_but_never_written() {
        let old = r#"{"plugins":["/tmp/vst"],"soundfonts":["/tmp/sf2"]}"#;
        let cfg: DirectoriesConfig = serde_json::from_str(old).unwrap();
        assert_eq!(cfg.legacy_plugins, vec!["/tmp/vst"]);
        assert_eq!(cfg.legacy_soundfonts, vec!["/tmp/sf2"]);

        let json = serde_json::to_string(&cfg).unwrap();
        assert_eq!(json, "{}");
    }

    #[cfg(unix)]
    #[test]
    fn ensure_dir_creates_dangling_symlink_target() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir().join(format!(
            "circolate-ensure-dir-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        let link = base.join("link");
        let target = base.join("target");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        ensure_dir(&link).unwrap();
        assert!(target.is_dir());

        std::fs::remove_dir_all(&base).ok();
    }
}
