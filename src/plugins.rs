//! `trackscript-player plugins` — scan search paths for VST3 / LV2 / CLAP
//! plugins and print what was found.
//!
//! No loading, no hosting.  Just discovery, so we can confirm the local
//! `plugins/` dir is visible before wiring it into the VST host.

use anyhow::Result;
use std::path::{Path, PathBuf};

pub fn run(argv: &[String]) -> Result<()> {
    let verbose = argv.iter().any(|a| a == "--paths" || a == "-v");

    let paths = search_paths();
    if verbose {
        eprintln!("search paths:");
        for p in &paths {
            eprintln!("  {}", p.display());
        }
    }

    let (mut vst3, mut lv2, mut clap, mut vst2) =
    (Vec::new(), Vec::new(), Vec::new(), Vec::new());

    for root in &paths {
        scan(root, &mut vst3, &mut lv2, &mut clap, &mut vst2);
    }

    for v in [&mut vst3, &mut lv2, &mut clap, &mut vst2] {
        v.sort();
        v.dedup();
    }

    print_group("VST3", &vst3);
    print_group("LV2",  &lv2);
    print_group("CLAP", &clap);
    print_group("VST2", &vst2);

    let total = vst3.len() + lv2.len() + clap.len() + vst2.len();
    println!("\n{total} plugin(s) found");
    Ok(())
}

fn print_group(label: &str, items: &[PathBuf]) {
    if items.is_empty() { return; }
    println!("{label} ({}):", items.len());
    for p in items {
        println!("  {}", p.display());
    }
}

fn search_paths() -> Vec<PathBuf> {
    let mut v = Vec::new();

    // Repo-local, from cwd (cargo run) and relative to the binary.
    v.push(PathBuf::from("plugins"));
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            // target/release/trackscript-player → ../../plugins
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

fn scan(root: &Path,
        vst3: &mut Vec<PathBuf>, lv2: &mut Vec<PathBuf>,
        clap: &mut Vec<PathBuf>, vst2: &mut Vec<PathBuf>) {
    scan_depth(root, 0, 3, vst3, lv2, clap, vst2);
        }

        fn scan_depth(dir: &Path, depth: usize, max: usize,
                      vst3: &mut Vec<PathBuf>, lv2: &mut Vec<PathBuf>,
                      clap: &mut Vec<PathBuf>, vst2: &mut Vec<PathBuf>) {
            if depth > max { return; }
            let Ok(entries) = std::fs::read_dir(dir) else { return };

            for e in entries.flatten() {
                let p = e.path();
                let is_dir = p.is_dir();
                let ext = p.extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();

                match (ext.as_str(), is_dir) {
                    // VST3 bundles are directories on Linux; standalone files
                    // exist too.  Don't recurse into the bundle.
                    ("vst3", true)  => { vst3.push(p); continue; }
                    ("vst3", false) => { vst3.push(p); continue; }

                    ("lv2", true)   => { lv2.push(p); continue; }

                    ("clap", false) => { clap.push(p); continue; }

                    // VST2 plugins are .so files, but only inside a vst/ dir.
                    ("so", false) => {
                        if dir.components().any(|c| c.as_os_str() == "vst") {
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
