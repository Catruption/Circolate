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

static SYSTEM_DATA_WARN: Once = Once::new();

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
    SYSTEM_DATA_WARN.call_once(|| {
        eprintln!(
            "warning: system data dir {} does not exist; \
set CIRCOLATE_DATA_DIR or install the package data",
sys.display()
        );
    });
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

/// Where new trackscripts go.  Always returns a directory that exists on
/// disk — creating it if necessary — so a first-run export never silently
/// falls back to the current working directory.
pub fn export_dir() -> PathBuf {
    let dir = user_trackscripts();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!(
            "warning: could not create export dir {}: {}",
            dir.display(),
                  e
        );
    }
    dir
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

    // Search order: user exports shadow bundled demos, which shadow the
    // dev-checkout copy under <repo>/trackscripts/.  The dev dir is only
    // present when running from source, so it's a no-op after install.
    let mut roots = vec![user_trackscripts(), bundled_trackscripts()];
    if let Some(repo) = repo_root() {
        roots.push(repo.join("trackscripts"));
    }

    let mut searched: Vec<PathBuf> = Vec::new();
    for dir in roots {
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
/// - `$XDG_DATA_HOME/circolate/soundfonts/` — per-user, circolate-managed
/// - `$XDG_DATA_HOME/soundfonts/` — per-user, system-wide convention
/// - `~/.soundfonts/` — legacy user convention (old sf2 managers)
/// - `$XDG_DATA_DIRS/soundfonts/` — covers `/usr/local/share/soundfonts/`
///   and `/usr/share/soundfonts/`, which is what Debian, Fedora, Arch,
///   and NixOS all ship packaged soundfonts into
/// - `/usr/share/sounds/sf2/`, `/usr/share/sounds/sf3/` — Debian legacy
///   locations, still populated by older packages
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

    // 2. Circolate-managed user soundfonts
    push_unique(&mut roots, user_data().join("soundfonts"));

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

    // 7. Sibling of system_data() for custom prefixes
    //    (e.g. /opt/circolate/share/circolate -> /opt/circolate/share/soundfonts).
    //    Skipped when system_data() is a user override, since the
    //    sibling relation is meaningless there.
    let sys = system_data();
    if sys.file_name().map_or(false, |f| f == "circolate") {
        if let Some(prefix_share) = sys.parent() {
            push_unique(&mut roots, prefix_share.join("soundfonts"));
        }
    }

    // 8. Dev checkout
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
