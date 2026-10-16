//! `trackscript-player fetch` — download missing fallback plugins.
//!
//! Walks `fallbacks.json`, checks each entry's `provides` path, and either
//! reports "already present" or runs the acquire step.  Idempotent:
//! running it twice does nothing the second time.
//!
//! Two acquire shapes:
//!
//!   Declarative — `{ url, archive, extract_to }`:
//!     Downloads with curl, extracts with tar/unzip/7z, verifies
//!     `provides` exists afterward.  Used for 90% of plugins.
//!
//!   Script — `{ script, provides }`:
//!     Runs `sh <script>`.  Used only when the download can't be
//!     expressed declaratively (session-signed URLs, builds from source,
//!     plugins with account gates).

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::deps::registry::{Acquire, RegistryTable};

pub fn run(argv: &[String]) -> Result<()> {
    let mut dry = false;
    let mut only: Option<String> = None;

    let mut it = argv.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--dry" | "-n" => dry = true,
            "--only" => {
                only = Some(it.next().context("--only needs a name")?.clone());
            }
            "-h" | "--help" => {
                println!("usage: trackscript-player fetch [--dry] [--only NAME]");
                println!();
                println!("  --dry        list scripts that would run, don't execute");
                println!("  --only NAME  run only the entry whose fallback name matches");
                return Ok(());
            }
            other => bail!("unknown flag: {other}"),
        }
    }

    let table = RegistryTable::load()
    .context("fallbacks.json not found (run from the repo root)")?;

    // Make sure we're at the repo root for relative paths in the JSON.
    // If fallbacks.json was found via the exe path, cd there first.
    let repo_root = std::env::current_dir()?;

    let mut fetched = 0usize;
    let mut present = 0usize;
    let mut failed  = 0usize;
    let mut no_acq  = 0usize;

    for mapping in table.entries() {
        let Some(acq) = &mapping.fallback.acquire else {
            no_acq += 1;
            continue;
        };

        if let Some(want) = &only {
            if !mapping.fallback.name.eq_ignore_ascii_case(want) {
                continue;
            }
        }

        let provides = repo_root.join(acq.provides());
        if provides.exists() {
            println!("  ok      {}  ({})", mapping.fallback.name, acq.provides());
            present += 1;
            continue;
        }

        if dry {
            match acq {
                Acquire::Download { url, archive, .. } => {
                    println!("  would download {} [{}]  {}", mapping.fallback.name, archive, url);
                }
                Acquire::Script { script, .. } => {
                    println!("  would run {}  ({})", script, mapping.fallback.name);
                }
            }
            continue;
        }

        println!("  fetching {} → {}", mapping.fallback.name, acq.provides());
        match run_one(acq, &repo_root) {
            Ok(()) => {
                if provides.exists() {
                    println!("  done");
                    fetched += 1;
                } else {
                    eprintln!("  WARN: step succeeded but {} still missing", acq.provides());
                    failed += 1;
                }
            }
            Err(e) => {
                eprintln!("  FAILED: {e:#}");
                failed += 1;
            }
        }
    }

    println!();
    println!(
        "{fetched} fetched, {present} already present, {failed} failed, {no_acq} without acquire block"
    );
    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn run_one(acq: &Acquire, repo_root: &Path) -> Result<()> {
    match acq {
        Acquire::Download { url, archive, extract_to, sha256, .. } => {
            download_and_extract(url, archive, extract_to, sha256.as_deref(), repo_root)
        }
        Acquire::Script { script, .. } => {
            let s = repo_root.join(script);
            if !s.exists() {
                bail!("script not found: {}", s.display());
            }
            let status = Command::new("sh")
            .arg(&s)
            .current_dir(repo_root)
            .status()
            .with_context(|| format!("running {}", s.display()))?;
            if !status.success() {
                bail!("script exited with {:?}", status.code());
            }
            Ok(())
        }
    }
}

fn download_and_extract(
    url: &str,
    archive: &str,
    extract_to: &str,
    want_sha: Option<&str>,
    repo_root: &Path,
) -> Result<()> {
    let dest_dir = repo_root.join(extract_to);
    std::fs::create_dir_all(&dest_dir)
    .with_context(|| format!("creating {}", dest_dir.display()))?;

    let ext = archive_ext(archive);
    let tmp = temp_path(ext);

    println!("    GET  {url}");
    let status = Command::new("curl")
    .args(["-fL", "--retry", "3", "--progress-bar", "-o"])
    .arg(&tmp)
    .arg(url)
    .status()
    .context("running curl")?;
    if !status.success() {
        let _ = std::fs::remove_file(&tmp);
        bail!("curl exit {:?}", status.code());
    }

    if let Some(want) = want_sha {
        let got = sha256_of(&tmp)?;
        if !got.eq_ignore_ascii_case(want) {
            let _ = std::fs::remove_file(&tmp);
            bail!("sha256 mismatch: want {want}, got {got}");
        }
        println!("    sha256 ok");
    }

    println!("    extract to {}", dest_dir.display());
    let result = extract(&tmp, archive, &dest_dir);
    let _ = std::fs::remove_file(&tmp);
    result
}

fn archive_ext(archive: &str) -> &'static str {
    match archive {
        "tar.gz" | "tgz" => "tar.gz",
        "tar.xz"         => "tar.xz",
        "tar.bz2"        => "tar.bz2",
        "zip"            => "zip",
        "7z"             => "7z",
        _                => "bin",
    }
}

fn extract(archive: &Path, kind: &str, dest: &Path) -> Result<()> {
    let status = match kind {
        "tar.gz" | "tgz" | "tar.xz" | "tar.bz2" => {
            Command::new("tar")
            .arg("-xf").arg(archive)
            .arg("-C").arg(dest)
            .status()
            .context("running tar")?
        }
        "zip" => {
            Command::new("unzip")
            .args(["-q", "-o"])
            .arg(archive)
            .arg("-d").arg(dest)
            .status()
            .context("running unzip")?
        }
        "7z" => {
            if which("7z").is_none() {
                bail!("7z not found — install p7zip");
            }
            Command::new("7z")
            .arg("x").arg("-y")
            .arg(format!("-o{}", dest.display()))
            .arg(archive)
            .status()
            .context("running 7z")?
        }
        other => bail!("unknown archive type: {other}"),
    };
    if !status.success() {
        bail!("extract exit {:?}", status.code());
    }
    Ok(())
}

fn sha256_of(path: &Path) -> Result<String> {
    let out = Command::new("sha256sum")
    .arg(path)
    .output()
    .context("running sha256sum")?;
    if !out.status.success() {
        bail!("sha256sum exit {:?}", out.status.code());
    }
    let s = String::from_utf8_lossy(&out.stdout);
    Ok(s.split_whitespace().next().unwrap_or("").to_ascii_lowercase())
}

fn which(cmd: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let p = dir.join(cmd);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn temp_path(ext: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|d| d.as_nanos())
    .unwrap_or(0);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "circolate-fetch-{}-{}.{}",
        std::process::id(),
                   nanos,
                   ext
    ));
    p
}
