// src/convert/flp/sf2.rs
//! Match a native channel's name to a same-named `.sf2` next to the FLP.
//! The converter emits an `.instrument` sidecar pointing at the SF2; the
//! player loads it and picks the right sample per note.

use std::path::{Path, PathBuf};

fn normalize_sf2_key(s: &str) -> String {
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

pub(super) fn find_matching_sf2(chan_name: &str, dir: &Path) -> Option<PathBuf> {
    if chan_name.is_empty() {
        return None;
    }
    let key = normalize_sf2_key(chan_name);
    if key.len() < 4 {
        return None;
    }
    let entries = std::fs::read_dir(dir).ok()?;
    for e in entries.flatten() {
        let p = e.path();
        let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("");
        if !ext.eq_ignore_ascii_case("sf2") {
            continue;
        }
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if normalize_sf2_key(stem) == key {
            return Some(p);
        }
    }
    None
}
