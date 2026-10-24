//! GUI file pickers via kdialog or zenity.
//!
//! Used by main.rs when a verb that expects a file is invoked without one.
//! Shelling out avoids a GTK build dependency (rfd) and matches what the
//! project's own run.sh already does.

use std::path::{Path, PathBuf};
use std::process::Command;

pub const FILTER_TRACKSCRIPT: &str =
"*.trackscript *.tsz *.flp *.it *.mid *.midi|Trackscript sources";
pub const FILTER_SOURCE: &str =
"*.flp *.it *.mid *.midi|FLP / IT / MIDI sources";
pub const FILTER_SF2: &str =
"*.sf2 *.sf3 *.SF2 *.SF3|SoundFont files";

/// Show a file-open dialog. Returns `None` if the user cancelled, no
/// graphical environment is available, or neither kdialog nor zenity
/// is installed.
pub fn pick_file(start: &Path, kdialog_filter: &str, title: &str) -> Option<PathBuf> {
    if !gui_available() {
        return None;
    }
    let start_s = start.to_string_lossy();

    // kdialog (KDE) — same as run.sh. A non-zero exit means the user
    // cancelled; do not fall through to zenity or they get a second dialog.
    if let Ok(out) = Command::new("kdialog")
        .args(["--title", title, "--getopenfilename", &start_s, kdialog_filter])
        .output()
        {
            return extract_path(&out);
        }

        // zenity (GNOME) fallback. kdialog's "patterns|Label" maps to
        // zenity's "--file-filter=Label | patterns".
        let (patterns, label) = kdialog_filter
        .split_once('|')
        .unwrap_or((kdialog_filter, "Files"));
        let zenity_filter = format!("{} | {}", label.trim(), patterns.trim());
        let start_arg = format!("--filename={}/", start_s.trim_end_matches('/'));

        if let Ok(out) = Command::new("zenity")
            .args([
                "--file-selection",
                &format!("--title={title}"),
                  &start_arg,
                  &format!("--file-filter={zenity_filter}"),
            ])
            .output()
            {
                return extract_path(&out);
            }

            None
}

fn extract_path(out: &std::process::Output) -> Option<PathBuf> {
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(PathBuf::from(s)) }
}

fn gui_available() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
    || std::env::var_os("DISPLAY").is_some()
}
