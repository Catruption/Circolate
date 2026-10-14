//! `trackscript-player dump` — print a source file's parsed structure.
//!
//! Prints the *source* IR, not the trackscript translation.  For `.flp` and
//! `.it` this is the raw parse; for `.trackscript` it is the parsed
//! structure as JSON.
//!
//! By default, also writes every plugin state blob to `<file>.states/`
//! next to the source, so binary payloads (Zenology, Arcade, DirectWave,
//! …) survive the round-trip that JSON cannot carry.  Pass `--no-states`
//! to suppress that, or `--raw` to dump the raw event stream instead of
//! the JSON IR.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

use crate::sources::{self, SourceKind};

pub fn run(argv: &[String]) -> Result<()> {
    let mut path_arg: Option<&str> = None;
    let mut write_states = true;
    let mut raw_events = false;

    for a in argv {
        match a.as_str() {
            "--no-states" => write_states = false,
            "--raw" => raw_events = true,
            "-h" | "--help" => {
                eprintln!(
                    "usage: trackscript-player dump [--no-states] [--raw] <file.flp|file.it|file.mid|file.trackscript>"
                );
                return Ok(());
            }
            other if other.starts_with('-') => {
                bail!("unknown flag: {other}");
            }
            other => {
                if path_arg.is_some() {
                    bail!("only one input file is supported");
                }
                path_arg = Some(other);
            }
        }
    }

    let Some(arg) = path_arg else {
        bail!("usage: trackscript-player dump [--no-states] [--raw] <file.flp|file.it|file.trackscript>");
    };
    let path = Path::new(arg);

    match SourceKind::from_path(path)? {
        SourceKind::Flp => {
            let prj = sources::flp::parse_file(path)?;

            if raw_events {
                for (ev, raw) in &prj.events {
                    println!("{:3}  {:>8}  {}", ev, raw.len(), hex(raw));
                }
                return Ok(());
            }

            if write_states {
                let dir = states_dir_for(path);
                match write_flp_states(&prj, &dir) {
                    Ok(n) if n > 0 => {
                        eprintln!("wrote {n} state blob(s) to {}", dir.display());
                    }
                    Ok(_) => {}
                    Err(e) => {
                        eprintln!("warning: could not write state blobs: {e:#}");
                    }
                }
            }

            println!("{}", serde_json::to_string_pretty(&prj)?);
        }
        SourceKind::It => {
            let module = sources::it::parse_file(path)?;
            println!("{}", serde_json::to_string_pretty(&module)?);
        }
        SourceKind::Trackscript => {
            let parsed = crate::sources::trackscript::parse_file(path)?;
            println!("{}", serde_json::to_string_pretty(&parsed)?);
        }
        SourceKind::Midi => {
            let mf = sources::midi::parse_file(path)?;
            let out = serde_json::json!({
                "ppq": mf.ppq,
                "time_sig": [mf.time_sig.0, mf.time_sig.1],
                "initial_bpm": mf.initial_bpm,
                "channels": mf.channels.iter().map(|c| serde_json::json!({
                    "channel": c.channel,
                    "name": c.name,
                    "program": c.program,
                    "bank_msb": c.bank_msb,
                    "bank_lsb": c.bank_lsb,
                    "notes": c.notes.len(),
                })).collect::<Vec<_>>(),
            });
            println!("{}", serde_json::to_string_pretty(&out)?);
        }
    }
    Ok(())
}

// ── state blob extraction ──────────────────────────────────────────────

fn states_dir_for(src: &Path) -> PathBuf {
    let mut stem = src
    .file_stem()
    .and_then(|s| s.to_str())
    .unwrap_or("project")
    .to_string();
    stem.push_str(".states");
    src.with_file_name(stem)
}

fn write_flp_states(prj: &sources::flp::Project, dir: &Path) -> Result<usize> {
    std::fs::create_dir_all(dir)
    .with_context(|| format!("creating {}", dir.display()))?;

    let mut written = 0usize;

    for chan in &prj.channels {
        let Some(p) = chan.plugin.as_ref() else { continue };
        if p.state.is_empty() {
            continue;
        }
        let label = if !p.display_name.is_empty() {
            p.display_name.clone()
        } else if !p.internal_name.is_empty() {
            p.internal_name.clone()
        } else {
            format!("chan{:02}", chan.iid)
        };
        let file = format!("chan{:02}_{}.state", chan.iid, sanitize(&label));
        let full = dir.join(&file);
        std::fs::write(&full, &p.state)
        .with_context(|| format!("writing {}", full.display()))?;
        written += 1;
    }

    for (insert_idx, ins) in &prj.inserts {
        for (slot, p) in ins.plugins.iter().enumerate() {
            if p.state.is_empty() {
                continue;
            }
            let label = if !p.display_name.is_empty() {
                p.display_name.clone()
            } else if !p.internal_name.is_empty() {
                p.internal_name.clone()
            } else if !ins.name.is_empty() {
                format!("{}_{}", ins.name, slot)
            } else {
                format!("slot{slot}")
            };
            let file = format!(
                "insert{:02}_slot{:02}_{}.state",
                insert_idx,
                slot,
                sanitize(&label),
            );
            let full = dir.join(&file);
            std::fs::write(&full, &p.state)
            .with_context(|| format!("writing {}", full.display()))?;
            written += 1;
        }
    }

    Ok(written)
}

fn sanitize(s: &str) -> String {
    let mut out: String = s
    .chars()
    .map(|c| {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
            c
        } else {
            '_'
        }
    })
    .collect();
    if out.len() > 80 {
        out.truncate(80);
    }
    if out.is_empty() {
        out.push_str("unnamed");
    }
    out
}

fn hex(b: &[u8]) -> String {
    b.iter()
    .map(|x| format!("{:02x}", x))
    .collect::<Vec<_>>()
    .join(" ")
}
