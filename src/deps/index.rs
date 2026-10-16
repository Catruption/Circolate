//! Plugin discovery.  `plugins` subcommand prints what was found; the
//! converter uses `PluginIndex` to resolve names to bundle paths.

use anyhow::Result;
use std::path::{Path, PathBuf};

pub struct PluginIndex {
    pub vst3: Vec<PathBuf>,
    pub lv2:  Vec<PathBuf>,
    pub clap: Vec<PathBuf>,
    pub vst2: Vec<PathBuf>,
}

impl PluginIndex {
    pub fn scan() -> Self {
        let (mut vst3, mut lv2, mut clap, mut vst2) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for root in search_paths() {
            scan(&root, &mut vst3, &mut lv2, &mut clap, &mut vst2);
        }

        for v in [&mut vst3, &mut lv2, &mut clap, &mut vst2] {
            for p in v.iter_mut() {
                if let Ok(c) = p.canonicalize() {
                    *p = c;
                }
            }
            v.sort();
            v.dedup();
        }

        PluginIndex { vst3, lv2, clap, vst2 }
    }

    /// Match a plugin by file stem, ignoring extension and case.
    /// Searches VST3, then VST2, then LV2, then CLAP.
    ///
    /// The FLP only carries a Windows path like
    /// `C:\...\magical8bitPlug3.dll`; the actual binary on this machine
    /// could be a `.vst3` bundle, a `.so`, or anything else.  All we can
    /// trust is the stem, so that is what we match on.
    pub fn find_any(&self, name: &str) -> Option<&PathBuf> {
        find_in(&self.vst3, name)
        .or_else(|| find_in(&self.vst2, name))
        .or_else(|| find_in(&self.lv2, name))
        .or_else(|| find_in(&self.clap, name))
    }

    /// Kept for callers that specifically want a VST3.
    pub fn find_vst3(&self, name: &str) -> Option<&PathBuf> {
        find_in(&self.vst3, name)
    }

    pub fn total(&self) -> usize {
        self.vst3.len() + self.lv2.len() + self.clap.len() + self.vst2.len()
    }
}

/// Exact stem match (case-insensitive) first, then normalized comparison
/// (strip " #N", lowercase, drop non-alphanumerics).
fn find_in<'a>(list: &'a [PathBuf], name: &str) -> Option<&'a PathBuf> {
    if name.is_empty() {
        return None;
    }

    if let Some(p) = list.iter().find(|p| {
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        stem.eq_ignore_ascii_case(name)
    }) {
        return Some(p);
    }

    let want = normalize_plugin_key(name);
    if want.len() < 2 {
        return None;
    }
    list.iter().find(|p| {
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        normalize_plugin_key(stem) == want
    })
}

fn normalize_plugin_key(s: &str) -> String {
    let base = if let Some(idx) = s.rfind(" #") {
        let tail = &s[idx + 2..];
        if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) {
            &s[..idx]
        } else {
            s
        }
    } else {
        s
    };
    base.chars()
    .filter(|c| c.is_ascii_alphanumeric())
    .map(|c| c.to_ascii_lowercase())
    .collect()
}

pub fn run(argv: &[String]) -> Result<()> {
    let verbose = argv.iter().any(|a| a == "--paths" || a == "-v");

    if verbose {
        eprintln!("search paths:");
        for p in &search_paths() {
            eprintln!("  {}", p.display());
        }
    }

    let idx = PluginIndex::scan();

    print_group("VST3", &idx.vst3);
    print_group("VST2", &idx.vst2);
    print_group("LV2",  &idx.lv2);
    print_group("CLAP", &idx.clap);

    println!("\n{} plugin(s) found", idx.total());
    Ok(())
}

fn print_group(label: &str, items: &[PathBuf]) {
    if items.is_empty() {
        return;
    }
    println!("{label} ({}):", items.len());
    for p in items {
        println!("  {}", p.display());
    }
}

fn search_paths() -> Vec<PathBuf> {
    let mut v = Vec::new();

    v.push(PathBuf::from("plugins"));
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            v.push(dir.join("plugins"));
            v.push(dir.join("../../plugins"));
        }
    }

    if let Ok(home) = std::env::var("HOME") {
        let home = PathBuf::from(home);
        v.push(home.join(".vst3"));
        v.push(home.join(".vst"));
        v.push(home.join(".lv2"));
        v.push(home.join(".clap"));
    }

    for p in [
        "/usr/lib/vst3", "/usr/local/lib/vst3",
        "/usr/lib/vst",  "/usr/local/lib/vst",
        "/usr/lib/lv2",  "/usr/local/lib/lv2",
        "/usr/lib/clap", "/usr/local/lib/clap",
    ] {
        v.push(PathBuf::from(p));
    }

    v.into_iter().filter(|p| p.exists()).collect()
}

fn scan(
    root: &Path,
    vst3: &mut Vec<PathBuf>,
    lv2:  &mut Vec<PathBuf>,
    clap: &mut Vec<PathBuf>,
    vst2: &mut Vec<PathBuf>,
) {
    scan_depth(root, 0, 4, vst3, lv2, clap, vst2);
}

/// True when `dir` (the parent directory of a candidate VST2 binary)
/// sits inside a `vst` or `.vst` tree.  Guards against collecting every
/// `.so` on the system as a plugin.
fn in_vst_tree(dir: &Path) -> bool {
    dir.components().any(|c| {
        let s = c.as_os_str().to_string_lossy();
        s.eq_ignore_ascii_case("vst") || s.eq_ignore_ascii_case(".vst")
    })
}

fn scan_depth(
    dir: &Path,
    depth: usize,
    max: usize,
    vst3: &mut Vec<PathBuf>,
    lv2:  &mut Vec<PathBuf>,
    clap: &mut Vec<PathBuf>,
    vst2: &mut Vec<PathBuf>,
) {
    if depth > max {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };

    for e in entries.flatten() {
        let p = e.path();
        let is_dir = p.is_dir();
        let ext = p
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

        match (ext.as_str(), is_dir) {
            ("vst3", true)  => { vst3.push(p); continue; }
            ("vst3", false) => { vst3.push(p); continue; }
            ("lv2", true)   => { lv2.push(p);  continue; }
            ("clap", false) => { clap.push(p); continue; }
            // VST2: `.so` on Linux, `.dll` on Windows.  Only accept
            // them under a `vst` / `.vst` path component so a random
            // shared library elsewhere on the scan root isn't
            // mistaken for a plugin.
            ("so", false) | ("dll", false) => {
                if in_vst_tree(dir) {
                    vst2.push(p);
                }
                continue;
            }
            _ => {}
        }

        if is_dir {
            scan_depth(&p, depth + 1, max, vst3, lv2, clap, vst2);
        }
    }
}
