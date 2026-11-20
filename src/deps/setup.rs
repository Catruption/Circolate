//! CLI commands that change Circolate's on-disk setup.
//!
//! `install` creates the directory tree.  `directories` edits the layered
//! `directories.json` that every resolver reads.  Both are safe to re-run.

use anyhow::{bail, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize)]
pub struct InstallResult {
    pub created: Vec<PathBuf>,
    pub existing: Vec<PathBuf>,
    pub files_written: Vec<PathBuf>,
    pub failed: Vec<(PathBuf, String)>,
}

pub fn run_install(argv: &[String]) -> Result<()> {
    let mut json = false;
    let mut dry = false;
    let mut fish_command = false;
    let mut log_only = false;
    for a in argv {
        match a.as_str() {
            "--json" | "-j" => json = true,
            "--dry-run" | "-n" => dry = true,
            "--fish-command" | "--fish" | "--command" => fish_command = true,
            // The TUI passes this so details land in the log widget instead
            // of writing to the terminal behind the alternate screen.
            "--log-only" => log_only = true,
            "-h" | "--help" => { print_install_help(); return Ok(()); }
            other => bail!("install: unknown flag {other}"),
        }
    }

    crate::log_event!("deps.install.started", dry_run: dry);
    let mut report = if dry {
        super::layout::InstallReport::default()
    } else {
        super::layout::install()
    };

    if fish_command {
        let path = crate::paths::fish_functions_dir().join("circolate.fish");
        match install_fish_command(&path, dry) {
            Ok(()) => {
                crate::log_event!(
                    "deps.install.fish_command",
                    path: path.display(),
                    dry_run: if dry { " (dry run)" } else { "" },
                );
                if dry {
                    if !log_only {
                        eprintln!("would write fish command {}", path.display());
                    }
                } else {
                    report.files_written.push(path);
                }
            }
            Err(e) if dry => return Err(e),
            Err(e) => report.failed.push((path, e.to_string())),
        }
    }

    for p in &report.created {
        crate::log_event!("deps.install.created", path: p.display());
    }
    for p in &report.files_written {
        crate::log_event!("deps.install.wrote", path: p.display());
    }

    let result = InstallResult {
        created: report.created.clone(),
        existing: report.existing.clone(),
        files_written: report.files_written.clone(),
        failed: report.failed.clone(),
    };

    crate::log_event!(
        "deps.install.summary",
        created: report.created.len(),
        existing: report.existing.len(),
        files_written: report.files_written.len(),
        failed: report.failures(),
    );
    for (path, err) in &report.failed {
        crate::log_event!("deps.install.failed", path: path.display(), err: err.clone());
    }

    if !log_only {
        if json {
            println!("{}", serde_json::to_string_pretty(&result)?);
        } else {
            if dry {
                println!("dry run: no changes made");
                for entry in super::layout::all() {
                    if entry.installable() {
                        let mark = if entry.exists { "exists" } else { "create" };
                        println!("  {mark:<6} {:<22} {}", entry.label, entry.path.display());
                    }
                }
            } else {
                println!("{}", super::layout::summarize(&report));
                for p in &report.created { println!("  created   {}", p.display()); }
                for p in &report.files_written { println!("  wrote     {}", p.display()); }
                for (p, e) in &report.failed { eprintln!("  FAILED    {}: {e}", p.display()); }
            }
        }
    }

    if report.failures() > 0 && !dry {
        bail!("install: {} path(s) could not be created", report.failures())
    }
    Ok(())
}

/// Quote a value for fish's single-quoted string syntax.  Paths can contain
/// spaces, backslashes and apostrophes, all of which must survive generation.
fn fish_quote(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn fish_function_body(exe: &Path) -> String {
    let bin = fish_quote(&exe.to_string_lossy());
    format!(
        "function circolate --description \"Run this Circolate install\"\n\
         \x20   set -l bin {bin}\n\
         \x20   if not test -x $bin\n\
         \x20       echo \"circolate: binary missing: $bin\" >&2\n\
         \x20       return 127\n\
         \x20   end\n\
         \x20   command $bin $argv\n\
         end\n"
    )
}

/// Write the fish autoload function that calls the binary we are running.
/// Re-running after moving/updating Circolate refreshes the path.
fn install_fish_command(path: &Path, dry: bool) -> Result<()> {
    let exe = std::env::current_exe()
        .map_err(|e| anyhow::anyhow!("locate current executable: {e}"))?;
    let body = fish_function_body(&exe);

    if dry {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        crate::paths::ensure_dir(parent)?;
    }
    std::fs::write(path, body)
        .map_err(|e| anyhow::anyhow!("write {}: {e}", path.display()))
}

fn print_install_help() {
    println!("usage: circolate install [--dry-run] [--json] [--fish-command]");
    println!();
    println!("Create every user-writable Circolate directory and write first-run");
    println!("helper files.  System-owned and package directories are reported");
    println!("by `doctor`, never created here.  Re-running is safe.");
    println!();
    println!("  --fish-command       also write ~/.config/fish/functions/circolate.fish");
    println!("                       (or $XDG_CONFIG_HOME/fish/functions/circolate.fish)");
    println!("                       pointing at the binary running this command.");
    println!("                       Aliases: --fish, --command.");
    println!("  --log-only           emit log events only (used by the TUI)");
}

pub fn run_directories(argv: &[String]) -> Result<()> {
    let mut cfg = crate::paths::load_directories();
    let mut json = false;
    let mut changed = false;
    let mut it = argv.iter();
    while let Some(a) = it.next() {
        let mut next_value = |flag: &str| -> Result<String> {
            it.next().cloned().ok_or_else(|| anyhow::anyhow!("{flag} needs a directory"))
        };
        match a.as_str() {
            "--json" | "-j" => json = true,
            "--set-root" | "--user-root" => {
                let v = next_value(a)?;
                cfg.user_root = Some(v.clone());
                crate::log_event!("deps.directories.added", kind: "user_root", path: v);
                changed = true;
            }
            "--clear-root" => {
                cfg.user_root = None;
                crate::log_event!("deps.directories.removed", kind: "user_root", path: "user_root".to_string());
                changed = true;
            }
            "--add-plugin" | "--plugin" | "--add-soundfont" | "--soundfont"
            | "--add-halotext" | "--halotext" | "--add-sample" | "--sample"
            | "--add-patch" | "--patch" | "--remove" => {
                bail!("per-type roots are deprecated; use --set-root DIR and put files in DIR/plugins, DIR/soundfonts, ...");
            }
            "-h" | "--help" => { print_directories_help(); return Ok(()); }
            other => bail!("directories: unknown flag {other}"),
        }
    }

    if changed {
        crate::paths::save_user_directories(&cfg)?;
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&cfg)?);
    } else {
        match crate::paths::user_root() {
            Some(root) => println!("user root: {}", root.display()),
            None => println!("user root: (none) — Circolate uses localshare and system roots"),
        }
        let legacy = crate::paths::legacy_dirs();
        if !legacy.is_empty() {
            println!("\nLegacy roots (still scanned, move under user root):");
            for path in legacy { println!("  {}", path.display()); }
        }
        println!("\nAlways checked (valid locations; not edited here):");
        let roots = crate::deps::layout::always_checked_roots();
        if roots.is_empty() {
            println!("  (none)");
        } else {
            for root in roots {
                let marker = if root.exists { "ok" } else { "--" };
                println!("  {:<9}{} {}", root.kind, marker, root.path.display());
            }
        }
        println!("\nsettings: {}", crate::paths::user_config().join(crate::paths::DIRECTORIES_FILENAME).display());
    }
    Ok(())
}

fn print_directories_help() {
    println!("usage: circolate directories [OPTIONS]");
    println!();
    println!("Show or set the one user root stored in directories.json.");
    println!("Its subdirectories mirror the managed localshare tree:");
    println!("  plugins/  soundfonts/  halotexts/  instruments/  samples/  patches/  covers/");
    println!("Everything else Circolate checks is always scanned and cannot be edited here.");
    println!();
    println!("  --set-root DIR         use DIR as the user root (~ is expanded)");
    println!("  --clear-root           stop using the saved user root");
    println!("  --json                 emit the merged config as JSON");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fish_quote_escapes_backslashes_and_apostrophes() {
        assert_eq!(fish_quote(r"/tmp/a'b\c"), "'/tmp/a\\'b\\\\c'");
        assert_eq!(fish_quote("/tmp/my circolate"), "'/tmp/my circolate'");
    }

    #[test]
    fn fish_function_uses_absolute_binary_and_forwards_args() {
        let body = fish_function_body(Path::new("/tmp/circolate"));
        assert!(body.starts_with("function circolate "));
        assert!(body.contains("set -l bin '/tmp/circolate'"));
        assert!(body.contains("command $bin $argv"));
        assert!(body.contains("return 127"));
        assert!(body.ends_with("end\n"));
    }

    #[test]
    fn install_accepts_log_only_without_terminal_output() {
        // The TUI calls install with --log-only so details reach the log
        // widget instead of the terminal. Dry run keeps this test read-only.
        let result = run_install(&[
            "--dry-run".to_string(),
            "--log-only".to_string(),
            "--fish-command".to_string(),
        ]);
        assert!(result.is_ok(), "log-only install failed: {result:?}");
    }
}
