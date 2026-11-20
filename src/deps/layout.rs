//! The canonical directory catalogue.
//!
//! `install` creates every writable directory; `doctor` reports every
//! directory (including ones that are expected to be supplied by the
//! distribution and therefore are never created here).
//!
//! Nothing in this module scans plugins.  Plugin roots still come from
//! `deps::registry::paths::effective()`; this is only the human-facing
//! map of what each directory is for.

use anyhow::{Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// Owned by the user; safe to create and edit.
    User,
    /// Owned by the package manager; reported, never created here.
    System,
    /// Present only in a source checkout; safe to create.
    Checkout,
    /// A user-typed root.  Reported, but never created automatically: a
    /// typo must stay visible instead of becoming a real directory.
    Configured,
    /// Disposable runtime state; safe to create.
    Cache,
}

#[derive(Clone, Debug, Serialize)]
pub struct DirEntry {
    pub key: &'static str,
    pub label: &'static str,
    pub path: PathBuf,
    pub scope: Scope,
    pub purpose: &'static str,
    pub exists: bool,
    pub entries: Option<usize>,
}

impl DirEntry {
    fn new(
        key: &'static str,
        label: &'static str,
        path: PathBuf,
        scope: Scope,
        purpose: &'static str,
    ) -> Self {
        let exists = path.is_dir();
        let entries = exists.then(|| {
            std::fs::read_dir(&path)
            .map(|rd| rd.filter_map(Result::ok).count())
            .unwrap_or(0)
        });
        Self { key, label, path, scope, purpose, exists, entries }
    }

    /// Whether `install` should create this directory.
    pub fn installable(&self) -> bool {
        matches!(self.scope, Scope::User | Scope::Cache | Scope::Checkout)
    }
}

/// A built-in search root that is checked even when it is not listed in
/// `directories.json`.  These are valid drop locations: files placed here
/// work without editing configuration or copying them into a Circolate
/// directory.
#[derive(Clone, Debug, Serialize)]
pub struct SearchRoot {
    pub kind: &'static str,
    pub path: PathBuf,
    pub note: &'static str,
    pub exists: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct InstallReport {
    pub created: Vec<PathBuf>,
    pub existing: Vec<PathBuf>,
    pub failed: Vec<(PathBuf, String)>,
    pub files_written: Vec<PathBuf>,
}

impl InstallReport {
    pub fn failures(&self) -> usize {
        self.failed.len()
    }
}

/// Whether a path is inside the source checkout.  The scanner still uses
/// these roots in development, but they are not user-facing portable
/// locations and must not be advertised as "always checked".
fn in_checkout(path: &Path) -> bool {
    crate::paths::repo_root()
    .is_some_and(|root| path.starts_with(root))
}

/// Roots the resolvers always check outside the editable user root.
///
/// The plugin list comes from `effective_candidates()` and the SoundFont
/// list from `soundfont_roots()`, so this display cannot drift from what
/// the player actually scans.  Anything inside the user root is filtered
/// out; the TUI and `directories` command show that root as one editable
/// setting instead.  Checkout-local roots are excluded too: they are dev
/// conveniences whose absolute paths are meaningless outside this machine.
pub fn always_checked_roots() -> Vec<SearchRoot> {
    let configured = crate::paths::configured_paths();
    let mut roots: Vec<SearchRoot> = Vec::new();

    let mut push = |kind: &'static str, path: PathBuf, note: &'static str| {
        if path.as_os_str().is_empty() { return; }
        if configured.iter().any(|p| path.starts_with(p)) { return; }
        if in_checkout(&path) { return; }
        if roots.iter().any(|r: &SearchRoot| r.path == path) { return; }
        roots.push(SearchRoot { kind, exists: path.is_dir(), path, note });
    };

    for path in crate::deps::registry::paths::effective_candidates() {
        push("plugin", path, "always scanned by the plugin resolver");
    }
    for path in crate::paths::soundfont_roots() {
        push("sf2", path, "always scanned by the SoundFont resolver");
    }
    push("halotext", crate::paths::user_halotexts(), "always checked for halotexts");
    push("halotext", crate::paths::bundled_halotexts(), "always checked for halotexts");
    push("sample", crate::paths::user_samples(), "always checked for loose samples");
    push("patch", crate::paths::user_patches(), "always checked for chip patches");

    roots
}

fn root_label(kind: &'static str) -> &'static str {
    match kind {
        "plugin" => "Always checked: plugins",
        "sf2" => "Always checked: SoundFonts",
        "halotext" => "Always checked: halotexts",
        "sample" => "Always checked: samples",
        "patch" => "Always checked: chip patches",
        _ => "Always checked",
    }
}

/// All directories Circolate knows about, in display order.
pub fn all() -> Vec<DirEntry> {
    let mut entries = vec![
        // User-owned content roots.
        dir("user_data", "Circolate localshare", crate::paths::user_data(), Scope::User,
            "settings plus plugins/, soundfonts/, halotexts/, samples/, patches/, covers/"),
        dir("halotexts", "My halotexts", crate::paths::user_halotexts(), Scope::User,
            "exported and downloaded .halo.txt / .halo.zip files"),
        dir("plugins", "Plugins", crate::paths::user_plugins(), Scope::User,
            "user-installed VST3/LV2/CLAP bundles"),
        dir("soundfonts", "SoundFonts", crate::paths::user_soundfonts(), Scope::User,
            "user-installed .sf2/.sf3 files"),
        dir("instruments", "Instruments", crate::paths::user_instruments(), Scope::User,
            ".instrument sidecars copied out of archives"),
        dir("samples", "Samples", crate::paths::user_samples(), Scope::User,
            "loose sample libraries referenced by halotext"),
        dir("patches", "Chip patches", crate::paths::user_patches(), Scope::User,
            "user-installed .patch files for chip voices"),
        dir("covers", "Covers", crate::paths::user_covers(), Scope::User,
            "extracted cover images from halotext archives"),
        // Disposable state.
        dir("cache", "Cache", crate::paths::user_cache(), Scope::Cache,
            "decoded samples and extracted archives"),
        dir("downloads", "Downloads", crate::paths::user_downloads(), Scope::Cache,
            "staging area for `circolate fetch`"),
        dir("logs", "Logs", crate::paths::user_logs(), Scope::Cache,
            "rotating headless/TUI log files"),
        // Shipped, read-only data.
        dir("system_data", "System data", crate::paths::system_data(), Scope::System,
            "packaged fallbacks.json, chip files, themes, demos"),
        dir("bundled_halotexts", "Bundled halotexts", crate::paths::bundled_halotexts(), Scope::System,
            "read-only demo tracks shipped with Circolate"),
        dir("native_fx", "Native FX", crate::paths::system_data().join("native_fx"), Scope::System,
            "packaged native-effect model artifacts"),
        dir("chip_defs", "Chip definitions", crate::paths::system_data().join("chip"), Scope::System,
            "shipped read-only .chip.txt hardware definitions"),
        // Source-checkout conveniences.
        dir("repo_plugins", "Checkout plugins", crate::paths::repo_plugins().unwrap_or_default(), Scope::Checkout,
            "portable plugin bundles beside the source tree"),
    ];

    // The one editable user root, plus old per-type roots still honoured
    // for compatibility.  These are never created automatically: a typo in
    // a config file should be visible, not silently turned into a directory.
    if let Some(root) = crate::paths::user_root() {
        entries.push(DirEntry::new(
            "configured",
            "Config: user root",
            root,
            Scope::Configured,
            "plugins/, soundfonts/, halotexts/, ... mirror localshare",
        ));
    }
    for value in crate::paths::legacy_dirs() {
        entries.push(DirEntry::new(
            "configured",
            "Config: legacy root",
            value,
            Scope::Configured,
            "old per-type root; move it under user_root",
        ));
    }

    // Built-in roots are resolution truth too.  Report candidates including
    // missing ones so users can see that an absent directory is still a
    // valid place to put files.
    for root in always_checked_roots() {
        entries.push(DirEntry::new(
            "always_checked",
            root_label(root.kind),
            root.path,
            Scope::System,
            root.note,
        ));
    }

    // De-duplicate by path, keeping the friendlier first label.
    let mut seen: Vec<PathBuf> = Vec::new();
    entries.into_iter().filter(|e| {
        if e.path.as_os_str().is_empty() {
            return false;
        }
        if seen.iter().any(|p| p == &e.path) {
            return false;
        }
        seen.push(e.path.clone());
        true
    }).collect()
}

fn dir(
    key: &'static str,
    label: &'static str,
    path: PathBuf,
    scope: Scope,
    purpose: &'static str,
) -> DirEntry {
    DirEntry::new(key, label, path, scope, purpose)
}

/// Create every installable directory and write first-run helper files.
/// Individual failures are collected instead of aborting the whole pass:
/// on a read-only home the user still wants to know which roots worked.
pub fn install() -> InstallReport {
    let mut report = InstallReport::default();

    for entry in all() {
        if !entry.installable() {
            continue;
        }
        match crate::paths::ensure_dir(&entry.path) {
            Ok(()) => {
                if entry.exists {
                    report.existing.push(entry.path);
                } else {
                    report.created.push(entry.path);
                }
            }
            Err(e) => report.failed.push((entry.path, e.to_string())),
        }
    }

    write_helper_files(&mut report);
    report
}

fn write_helper_files(report: &mut InstallReport) {
    let readme = crate::paths::user_data().join("README.md");
    let body = concat!(
        "Circolate user data\n",
        "===================\n\n",
        "halotexts/   exported and downloaded tracks\n",
        "plugins/     VST3, LV2 and CLAP bundles, resolved by name\n",
        "soundfonts/  .sf2 / .sf3 files, resolved by name\n",
        "instruments/ .instrument sidecars\n",
        "samples/     loose sample libraries\n",
        "patches/     chip patches (hardware definitions ship read-only in share/chip/)\n",
        "covers/      extracted cover images\n",
        "directories.json and player_settings.json live at the root of this tree\n\n",
        "Set one optional user root in directories.json; its folders mirror this tree.\n",
        "Recorded absolute paths in project files are never used for loading.\n",
    );
    if let Err(e) = std::fs::write(&readme, body)
    .with_context(|| format!("writing {}", readme.display()))
    {
        report.failed.push((readme, format!("{e:#}")));
    } else {
        report.files_written.push(readme);
    }

    let config = crate::paths::user_config().join(crate::paths::DIRECTORIES_FILENAME);
    if !config.exists() {
        let text = serde_json::to_string_pretty(&crate::paths::DirectoriesConfig::default())
        .unwrap_or_else(|_| "{}".to_string());
        if let Err(e) = std::fs::write(&config, format!("{text}\n"))
        .with_context(|| format!("writing {}", config.display()))
        {
            report.failed.push((config, format!("{e:#}")));
        } else {
            report.files_written.push(config);
        }
    }
}

/// Human-readable install summary, used by both the CLI and the TUI log.
pub fn summarize(report: &InstallReport) -> String {
    format!(
        "install: {} created, {} already present, {} file(s) written, {} failed",
        report.created.len(),
        report.existing.len(),
        report.files_written.len(),
        report.failures(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn always_checked_roots_cover_plugins_and_soundfonts() {
        let roots = always_checked_roots();
        assert!(roots.iter().any(|r| r.kind == "plugin"));
        assert!(roots.iter().any(|r| r.kind == "sf2"));
        assert!(roots.iter().all(|r| !r.path.as_os_str().is_empty()));

        // Dev-checkout paths are machine-specific; they must never be
        // advertised as portable "always checked" locations.
        if let Some(repo) = crate::paths::repo_root() {
            assert!(roots.iter().all(|r| !r.path.starts_with(&repo)));
        }

        // The same path must not appear twice just because two resolvers
        // happen to include it.
        for (i, root) in roots.iter().enumerate() {
            assert!(!roots[..i].iter().any(|other| other.path == root.path));
        }
    }
}
