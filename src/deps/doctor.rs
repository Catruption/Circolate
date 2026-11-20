//! `circolate doctor` — dependency and directory pre-flight.
//!
//! This is the "does this machine actually have what the project needs?"
//! command.  It is intentionally read-only: run `circolate install` first
//! if the writable roots are missing.  `--json` makes it script-friendly;
//! `--log-only` is used by the TUI so the report lands in the log overlay
//! instead of stdout.

use anyhow::{bail, Result};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::layout::DirEntry;

#[derive(Clone, Debug, Default, Serialize)]
pub struct PluginReport {
    pub vst3: usize,
    pub vst2: usize,
    pub lv2: usize,
    pub clap: usize,
    pub total: usize,
    pub roots: Vec<PluginRoot>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PluginRoot {
    pub path: PathBuf,
    pub exists: bool,
    pub vst3: usize,
    pub vst2: usize,
    pub lv2: usize,
    pub clap: usize,
    pub total: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct SoundfontRoot {
    pub path: PathBuf,
    pub exists: bool,
    pub files: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct FallbackStatus {
    pub source: String,
    pub target: String,
    pub provides: String,
    pub path: PathBuf,
    pub present: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConfigStatus {
    pub path: PathBuf,
    pub exists: bool,
    pub purpose: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub struct DoctorReport {
    pub dirs: Vec<DirEntry>,
    pub plugins: PluginReport,
    pub soundfonts: Vec<SoundfontRoot>,
    pub fallbacks: Vec<FallbackStatus>,
    pub config: Vec<ConfigStatus>,
    pub env: BTreeMap<String, String>,
    pub summary: DoctorSummary,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct DoctorSummary {
    pub missing_dirs: usize,
    pub plugins: usize,
    pub soundfonts: usize,
    pub fallbacks_total: usize,
    pub fallbacks_present: usize,
    pub problems: usize,
}

#[derive(Clone, Copy, Default)]
struct Sections {
    dirs: bool,
    plugins: bool,
    soundfonts: bool,
    fallbacks: bool,
    config: bool,
    json: bool,
    log_only: bool,
    strict: bool,
}

impl Sections {
    fn all() -> Self {
        Self { dirs: true, plugins: true, soundfonts: true, fallbacks: true, config: true, ..Default::default() }
    }

    fn any_specific(&self) -> bool {
        self.dirs || self.plugins || self.soundfonts || self.fallbacks || self.config
    }
}

pub fn run(argv: &[String]) -> Result<()> {
    let mut s = Sections::all();
    let mut explicit = false;
    for a in argv {
        match a.as_str() {
            "--dirs" | "--paths" => { s.dirs = true; explicit = true; }
            "--plugins" => { s.plugins = true; explicit = true; }
            "--soundfonts" | "--sf2" => { s.soundfonts = true; explicit = true; }
            "--fallbacks" => { s.fallbacks = true; explicit = true; }
            "--config" => { s.config = true; explicit = true; }
            "--json" | "-j" => s.json = true,
            "--log-only" => s.log_only = true,
            "--strict" => s.strict = true,
            "-h" | "--help" => { print_help(); return Ok(()); }
            other => bail!("doctor: unknown flag {other}"),
        }
    }
    if explicit {
        // When specific sections are requested, show only those.
        s.dirs = argv.iter().any(|a| matches!(a.as_str(), "--dirs" | "--paths"));
        s.plugins = argv.iter().any(|a| a == "--plugins");
        s.soundfonts = argv.iter().any(|a| matches!(a.as_str(), "--soundfonts" | "--sf2"));
        s.fallbacks = argv.iter().any(|a| a == "--fallbacks");
        s.config = argv.iter().any(|a| a == "--config");
    }

    crate::log_event!("deps.doctor.started");
    let report = collect();
    log_details(&report);
    crate::log_event!(
        "deps.doctor.summary",
        missing_dirs: report.summary.missing_dirs,
        plugins: report.summary.plugins,
        soundfonts: report.summary.soundfonts,
        fallbacks_present: report.summary.fallbacks_present,
        fallbacks_total: report.summary.fallbacks_total,
        problems: report.summary.problems,
    );

    if !s.log_only {
        if s.json {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            print_report(&report, &s);
        }
    }

    if s.strict && report.summary.problems > 0 {
        bail!(
            "doctor: {} problem(s) ({} missing dirs, {} plugin(s), {} soundfont(s), {}/{} fallback(s) present)",
            report.summary.problems,
            report.summary.missing_dirs,
            report.summary.plugins,
            report.summary.soundfonts,
            report.summary.fallbacks_present,
            report.summary.fallbacks_total,
        )
    }
    Ok(())
}

fn print_help() {
    println!("usage: circolate doctor [OPTIONS]");
    println!();
    println!("Report the directories, plugins, SoundFonts and fallback targets");
    println!("Circolate can see on this machine.  Read-only.");
    println!();
    println!("  --dirs, --paths   show the directory catalogue");
    println!("  --plugins         show plugin roots and counts");
    println!("  --soundfonts      show SoundFont roots and counts");
    println!("  --fallbacks       show configured fallback targets");
    println!("  --config          show config/registry file status");
    println!("  --json            emit one JSON object");
    println!("  --strict          exit non-zero if nothing usable was found");
    println!("  --log-only        emit log events only (used by the TUI)");
}

/// Build the full report.  Also used by the TUI menu and by tests.
pub fn collect() -> DoctorReport {
    let dirs = super::layout::all();
    let plugins = plugin_report();
    let soundfonts = soundfont_report();
    let fallbacks = fallback_report();
    let config = config_report();
    let env = env_report();

    let missing_dirs = dirs.iter().filter(|d| !d.exists && d.installable()).count();
    let soundfonts_total: usize = soundfonts.iter().map(|r| r.files).sum();
    let fallbacks_present = fallbacks.iter().filter(|f| f.present).count();
    let fallbacks_total = fallbacks.len();
    let problems = missing_dirs
        + usize::from(plugins.total == 0)
        + usize::from(soundfonts_total == 0)
        + fallbacks.iter().filter(|f| !f.present).count();

    DoctorReport {
        dirs,
        plugins,
        soundfonts,
        fallbacks,
        config,
        env,
        summary: DoctorSummary {
            missing_dirs,
            plugins: plugins_total(),
            soundfonts: soundfonts_total,
            fallbacks_total,
            fallbacks_present,
            problems,
        },
    }
}

fn plugins_total() -> usize {
    super::plugins::PluginTable::get().total()
}

fn plugin_report() -> PluginReport {
    let table = super::plugins::PluginTable::get();
    let roots = super::registry::paths::effective_candidates()
    .into_iter()
    .map(|path| {
        let contains = |list: &[PathBuf]| -> usize {
            list.iter().filter(|p| p.starts_with(&path)).count()
        };
        let vst3 = contains(&table.vst3);
        let vst2 = contains(&table.vst2);
        let lv2 = contains(&table.lv2);
        let clap = contains(&table.clap);
        PluginRoot {
            exists: path.is_dir(),
            vst3, vst2, lv2, clap,
            total: vst3 + vst2 + lv2 + clap,
            path,
        }
    })
    .collect();

    PluginReport {
        vst3: table.vst3.len(),
        vst2: table.vst2.len(),
        lv2: table.lv2.len(),
        clap: table.clap.len(),
        total: table.total(),
        roots,
    }
}

fn soundfont_report() -> Vec<SoundfontRoot> {
    crate::paths::soundfont_roots()
    .into_iter()
    .map(|path| {
        let files = std::fs::read_dir(&path)
        .map(|rd| {
            rd.filter_map(Result::ok)
            .filter(|e| {
                e.path().is_file()
                && e.path().extension().and_then(|s| s.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("sf2") || ext.eq_ignore_ascii_case("sf3"))
            })
            .count()
        })
        .unwrap_or(0);
        SoundfontRoot { exists: path.is_dir(), files, path }
    })
    .collect()
}

fn fallback_report() -> Vec<FallbackStatus> {
    let Ok(registry) = super::registry::RegistryTable::load() else {
        return Vec::new();
    };
    let plugins = super::plugins::PluginTable::get();
    let dest = crate::paths::user_data();
    registry.entries().map(|m| {
        let provides = m.fallback.acquire.as_ref()
        .map(|a| a.provides().to_string())
        .unwrap_or_else(|| m.fallback.path.clone());
        let fallback_name = m.fallback.name.clone();
        let provides_name = Path::new(&provides)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(&provides);
        // A fallback is "present" when Circolate can actually resolve it
        // by name from any configured root, not only from the download
        // directory.  Keep the download path as a secondary check.
        let resolved = [
            fallback_name.trim_end_matches(".vst3").trim_end_matches(".vst"),
            provides_name.trim_end_matches(".vst3").trim_end_matches(".vst"),
        ]
        .into_iter()
        .find_map(|name| plugins.find_any(name))
        .cloned();
        let download_path = if provides.is_empty() { PathBuf::new() } else { dest.join(&provides) };
        let present = resolved.is_some() || (!provides.is_empty() && download_path.exists());
        FallbackStatus {
            source: m.match_.primary().to_string(),
            target: fallback_name,
            provides,
            present,
            path: resolved.unwrap_or(download_path),
        }
    }).collect()
}

fn config_report() -> Vec<ConfigStatus> {
    let fb = crate::paths::layered_file("fallbacks.json")
    .unwrap_or_else(|| crate::paths::system_data().join("fallbacks.json"));
    vec![
        ConfigStatus {
            exists: fb.is_file(),
            path: fb,
            purpose: "effective fallbacks.json (user root > localshare > system)",
        },
        ConfigStatus {
            path: crate::paths::system_data().join(crate::paths::DIRECTORIES_FILENAME),
            exists: crate::paths::system_data().join(crate::paths::DIRECTORIES_FILENAME).is_file(),
            purpose: "shipped directory defaults",
        },
        ConfigStatus {
            path: crate::paths::user_config().join(crate::paths::DIRECTORIES_FILENAME),
            exists: crate::paths::user_config().join(crate::paths::DIRECTORIES_FILENAME).is_file(),
            purpose: "your user root setting (localshare)",
        },
        ConfigStatus {
            path: crate::paths::user_cache().join("plugins_inventory.json"),
            exists: crate::paths::user_cache().join("plugins_inventory.json").is_file(),
            purpose: "measured plugin inventory",
        },
    ]
}

fn env_report() -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for name in [
        "CIRCOLATE_DATA_DIR", "CIRCOLATE_USER_ROOT", "CIRCOLATE_PLUGIN_PATH", "VST3_PATH",
        "CIRCOLATE_SOUNDFONT_DIR", "XDG_CONFIG_HOME", "XDG_DATA_HOME",
        "XDG_DATA_DIRS", "XDG_CACHE_HOME", "HOME",
    ] {
        let value = std::env::var_os(name)
        .map(|v| PathBuf::from(v).to_string_lossy().into_owned())
        .unwrap_or_else(|| "(unset)".to_string());
        out.insert(name.to_string(), value);
    }
    out
}

fn print_report(r: &DoctorReport, s: &Sections) {
    if s.dirs {
        println!("Directories:");
        for d in &r.dirs {
            let mark = if d.exists { "ok     " } else if d.installable() { "MISSING" } else { "system " };
            let count = d.entries.map(|n| format!(" ({n} entries)")).unwrap_or_default();
            println!("  {mark} {:<22} {}{}", d.label, d.path.display(), count);
        }
        println!();
    }

    if s.plugins {
        println!("Plugins: {} VST3, {} VST2, {} LV2, {} CLAP ({} total)",
            r.plugins.vst3, r.plugins.vst2, r.plugins.lv2, r.plugins.clap, r.plugins.total);
        for root in &r.plugins.roots {
            let mark = if root.exists { "ok     " } else { "MISSING" };
            println!("  {mark} {:<52} {} plugin(s)", root.path.display(), root.total);
        }
        println!();
    }

    if s.soundfonts {
        let total: usize = r.soundfonts.iter().map(|x| x.files).sum();
        println!("SoundFonts: {} file(s) across {} root(s)", total, r.soundfonts.len());
        for root in &r.soundfonts {
            let mark = if root.exists { "ok     " } else { "MISSING" };
            println!("  {mark} {:<52} {} file(s)", root.path.display(), root.files);
        }
        println!();
    }

    if s.fallbacks {
        println!("Fallback targets: {}/{} present",
            r.summary.fallbacks_present, r.summary.fallbacks_total);
        for f in &r.fallbacks {
            let mark = if f.present { "ok     " } else { "MISSING" };
            println!("  {mark} {:<28} -> {}", f.source, f.path.display());
        }
        println!();
    }

    if s.config {
        println!("Configuration:");
        for c in &r.config {
            let mark = if c.exists { "ok     " } else { "MISSING" };
            println!("  {mark} {:<52} {}", c.path.display(), c.purpose);
        }
        println!();
        println!("Environment:");
        for (k, v) in &r.env {
            println!("  {:<26} {}", k, v);
        }
        println!();
    }

    println!("summary: {} problem(s) — {} missing writable dir(s), {} plugin(s), {} SoundFont(s), {}/{} fallback(s)",
        r.summary.problems, r.summary.missing_dirs, r.summary.plugins,
        r.summary.soundfonts, r.summary.fallbacks_present, r.summary.fallbacks_total);
}

/// Convenience for the TUI: one compact log line per missing/empty thing.
pub fn log_details(report: &DoctorReport) {
    for d in &report.dirs {
        if d.exists {
            crate::log_event!("deps.doctor.dir", path: d.path.display(), label: d.label, count: d.entries.unwrap_or(0));
        } else {
            crate::log_event!("deps.doctor.dir_missing", path: d.path.display(), label: d.label);
        }
    }
    for root in &report.plugins.roots {
        if root.exists {
            crate::log_event!("deps.doctor.plugin_root", path: root.path.display(), count: root.total);
        }
    }
}
