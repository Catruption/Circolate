//! `halotext-player export` — convert a source file to halotext.
//!
//! Two output shapes:
//!   * directory (default) — a .halo.txt file plus manifest and assets
//!   * archive (--archive) — a single .halo.zip file
//!
//! Both share the same `convert_file` pipeline; only the write-back
//! step differs.

use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};

use crate::archive;
use crate::pipe::{ ConvertOptions, PitchMode};
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
            "usage: halotext-player export <file> [-o DIR] [--archive] \
[--include-plugins[=free|all]] [--pitch names|hz]"
        )
    })?;

    let conv = crate::pipe::convert_file(&src, &opts)?;
    let stem = crate::pipe::output_stem(&src);

    if archive_mode {
        write_archive_output(&src, &conv, &stem, out_dir.as_deref(), include_plugins.as_deref())?;
    } else {
        write_directory_output(&src, &conv, &stem, out_dir.as_deref())?;
    }

    print_footer(&conv.halotext);
    Ok(())
}

// ── directory output ───────────────────────────────────────────────────

fn write_directory_output(
    src: &Path,
    conv: &crate::pipe::Converted,
    stem: &str,
    out_dir: Option<&Path>,
) -> Result<()> {
    let out = out_dir.map(Path::to_path_buf).unwrap_or_else(|| paths::export_dir().join(stem));
    std::fs::create_dir_all(&out).with_context(|| format!("creating {}", out.display()))?;
    let main_filename = format!("{}{}", stem, crate::formats::halotext::spec::SOURCE_EXT);
    let cover = find_cover_for_source(src);
    let cover_arg = cover.as_ref().map(|(n, b)| (n.as_str(), b.as_slice()));
    archive::write_bundle_directory(
        &out, conv, &main_filename,
        src.file_stem().and_then(|s| s.to_str()).unwrap_or(stem),
        44100, cover_arg,
    )?;
    println!("wrote {}", out.join(main_filename).display());
    Ok(())
}

// ── archive output ─────────────────────────────────────────────────────

fn write_archive_output(
    src: &Path,
    conv: &crate::pipe::Converted,
    stem: &str,
    out_dir: Option<&Path>,
    include_plugins: Option<&str>,
) -> Result<()> {
    let out_path = match out_dir {
        Some(p) => {
            if archive::is_archive_path(p) {
                p.to_path_buf()
            } else {
                std::fs::create_dir_all(p)
                .with_context(|| format!("creating {}", p.display()))?;
                p.join(format!("{}.{}", stem, archive::EXTENSION))
            }
        }
        None => {
            // FIX: Point to the standard user halotexts directory.
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

    let opts = archive::WriteOptions { include_plugins: include };

    let main_filename = format!("{}{}", stem, crate::formats::halotext::spec::SOURCE_EXT);
    let manifest = archive::write_archive(
        &out_path,
        conv,
        &main_filename,
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

fn print_footer(halotext: &str) {
    let mut in_footer = false;
    for line in halotext.lines() {
        if line.starts_with("// ------") {
            in_footer = true;
        } else if in_footer && line.starts_with("//") {
            println!("  {}", line);
        }
    }
}
