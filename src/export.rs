//! `trackscript-player export` — convert a source file to trackscript.
//!
//! Two output shapes:
//!   * directory (default) — trackscript + samples/ + instruments/ + samples.json
//!   * archive (--archive) — a single .tsz file
//!
//! Both share the same `convert_file` pipeline; only the write-back
//! step differs.

use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};

use crate::archive;
use crate::convert::{self, ConvertOptions, PitchMode};
use crate::paths; // Added import

pub fn run(argv: &[String]) -> Result<()> {
    let mut src: Option<PathBuf> = None;
    let mut out_dir: Option<PathBuf> = None;
    let mut archive_mode = false;
    let mut include_plugins: Option<String> = None;
    let mut opts = ConvertOptions::default();

    let mut it = argv.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-o" | "--out" => {
                out_dir = Some(PathBuf::from(
                    it.next().ok_or_else(|| anyhow!("{a} needs a path"))?,
                ));
            }
            "--pitch" => {
                let v = it
                .next()
                .ok_or_else(|| anyhow!("--pitch needs a value"))?;
                opts.pitch_mode = match v.as_str() {
                    "names" => PitchMode::Names,
                    "hz" => PitchMode::Hz,
                    other => bail!("unknown pitch mode: {other}"),
                };
            }
            "--subdivisions" => {
                opts.subdivisions = it
                .next()
                .ok_or_else(|| anyhow!("--subdivisions needs a value"))?
                .parse()?;
            }
            "--samples-dir" => {
                opts.samples_dir = Some(PathBuf::from(
                    it.next().ok_or_else(|| anyhow!("--samples-dir needs a path"))?,
                ));
            }
            "--sf2" => {
                opts.sf2_path = Some(PathBuf::from(
                    it.next().ok_or_else(|| anyhow!("--sf2 needs a path"))?,
                ));
            }
            "--skip-native" => opts.skip_native = true,
            "--no-message" => opts.with_message = false,
            "--archive" | "-a" => archive_mode = true,
            "--include-plugins" => include_plugins = Some("free".into()),
            other if other.starts_with("--include-plugins=") => {
                include_plugins = Some(
                    other.trim_start_matches("--include-plugins=").to_string(),
                );
            }
            other if other.starts_with('-') && other.len() > 1 => {
                bail!("unknown flag: {other}");
            }
            other if src.is_none() => src = Some(PathBuf::from(other)),
            other => bail!("unexpected argument: {other}"),
        }
    }

    let src = src.ok_or_else(|| {
        anyhow!(
            "usage: trackscript-player export <file> [-o DIR] [--archive] \
[--include-plugins[=free|all]] [--pitch names|hz]"
        )
    })?;

    let conv = convert::convert_file(&src, &opts)?;
    let stem = convert::output_stem(&src);

    if archive_mode {
        write_archive_output(&src, &conv, &stem, out_dir.as_deref(), include_plugins.as_deref())?;
    } else {
        write_directory_output(&src, &conv, &stem, out_dir.as_deref())?;
    }

    print_footer(&conv.trackscript);
    Ok(())
}

// ── directory output ───────────────────────────────────────────────────

fn write_directory_output(
    src: &Path,
    conv: &convert::Converted,
    stem: &str,
    out_dir: Option<&Path>,
) -> Result<()> {
    let out = out_dir
    .map(|p| p.to_path_buf())
    .unwrap_or_else(|| {
        // FIX: Point to the standard user trackscripts directory.
        // We append `stem` so multiple exports of the same project don't
        // overwrite each other's samples/ folders.
        paths::export_dir().join(stem)
    });

    std::fs::create_dir_all(&out)
    .with_context(|| format!("creating {}", out.display()))?;

    let ts_path = out.join(format!("{}.trackscript", stem));
    std::fs::write(&ts_path, &conv.trackscript)
    .with_context(|| format!("writing {}", ts_path.display()))?;

    for s in &conv.samples {
        let p = out.join(&s.rel_path);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&p, &s.wav)
        .with_context(|| format!("writing {}", p.display()))?;
    }

    for a in &conv.aux {
        let p = out.join(&a.rel_path);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&p, &a.bytes)
        .with_context(|| format!("writing {}", p.display()))?;
    }

    if !conv.samples_json.is_empty() {
        let sj = out.join("samples.json");
        std::fs::write(&sj, &conv.samples_json)
        .with_context(|| format!("writing {}", sj.display()))?;
    }

    let cover = find_cover_for_source(src);
    if let Some((name, bytes)) = &cover {
        let dest = out.join(name);
        let _ = std::fs::write(&dest, bytes);
    }

    let title = src.file_stem().and_then(|s| s.to_str()).unwrap_or(stem);
    let cover_arg = cover.as_ref().map(|(n, b)| (n.as_str(), b.as_slice()));
    let entry_path = format!("{}.trackscript", stem);

    archive::write_manifest_and_requires(
        &out,
        conv,
        &entry_path,
        title,
        44100,
        cover_arg,
    )?;

    println!("wrote {}", ts_path.display());
    Ok(())
}

// ── archive output ─────────────────────────────────────────────────────

fn write_archive_output(
    src: &Path,
    conv: &convert::Converted,
    stem: &str,
    out_dir: Option<&Path>,
    include_plugins: Option<&str>,
) -> Result<()> {
    let out_path = match out_dir {
        Some(p) => {
            if p.extension().and_then(|s| s.to_str()) == Some(archive::EXTENSION) {
                p.to_path_buf()
            } else {
                std::fs::create_dir_all(p)
                .with_context(|| format!("creating {}", p.display()))?;
                p.join(format!("{}.{}", stem, archive::EXTENSION))
            }
        }
        None => {
            // FIX: Point to the standard user trackscripts directory.
            let dir = paths::export_dir();
            std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating {}", dir.display()))?;
            dir.join(format!("{}.{}", stem, archive::EXTENSION))
        }
    };

    let title = src.file_stem().and_then(|s| s.to_str()).unwrap_or(stem);

    let cover = find_cover_for_source(src);
    let cover_arg = cover.as_ref().map(|(n, b)| (n.as_str(), b.as_slice()));

    let include = match include_plugins {
        None | Some("none") => archive::IncludePlugins::None,
        Some("free") => archive::IncludePlugins::Free,
        Some("all") => archive::IncludePlugins::All,
        Some(other) => bail!("unknown --include-plugins value: {other}"),
    };

    let opts = archive::WriteOptions {
        flac_max_ratio: 0.95,
        include_plugins: include,
    };

    let manifest = archive::write_archive(
        &out_path,
        conv,
        title,
        44100,
        cover_arg,
        opts,
    )?;

    let plugin_entries = manifest
    .entries
    .iter()
    .filter(|e| matches!(e.kind, archive::EntryKind::Plugin { .. }))
    .count();

    if plugin_entries > 0 {
        println!(
            "wrote {} ({} entries, {} format, {} plugin files embedded)",
                 out_path.display(),
                 manifest.entries.len(),
                 manifest.sample_format,
                 plugin_entries,
        );
    } else {
        println!(
            "wrote {} ({} entries, {} format)",
                 out_path.display(),
                 manifest.entries.len(),
                 manifest.sample_format,
        );
    }
    Ok(())
}

// ── cover lookup ───────────────────────────────────────────────────────

fn find_cover_for_source(src: &Path) -> Option<(String, Vec<u8>)> {
    let parent = src.parent()?;

    for name in [
        "cover.jpg",
        "cover.jpeg",
        "cover.png",
        "cover.webp",
        "cover.gif",
    ] {
        let candidate = parent.join(name);
        if candidate.is_file() {
            if let Ok(bytes) = std::fs::read(&candidate) {
                return Some((name.to_string(), bytes));
            }
        }
    }

    let rd = std::fs::read_dir(parent).ok()?;
    let mut matches: Vec<PathBuf> = rd
    .flatten()
    .map(|e| e.path())
    .filter(|p| p.is_file())
    .filter(|p| {
        p.file_stem()
        .and_then(|s| s.to_str())
        .map_or(false, |s| s.eq_ignore_ascii_case("cover"))
    })
    .collect();
    matches.sort();
    for path in matches {
        let name = path.file_name().and_then(|s| s.to_str())?.to_string();
        if let Ok(bytes) = std::fs::read(&path) {
            return Some((name, bytes));
        }
    }
    None
}

// ── footer printing ────────────────────────────────────────────────────

fn print_footer(trackscript: &str) {
    let mut in_footer = false;
    for line in trackscript.lines() {
        if line.starts_with("// ------") {
            in_footer = true;
        } else if in_footer && line.starts_with("//") {
            println!("  {}", line);
        }
    }
}
