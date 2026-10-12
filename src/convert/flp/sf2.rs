// src/convert/flp/sf2.rs
//! SoundFont support: read the SF2 path (and preset) out of FL's SoundFont
//! player state, then find that file on disk.

use std::path::{Path, PathBuf};

pub(super) struct SfState {
    pub path: String,
    pub preset: Option<i32>,
}

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

/// State layout: header, then a length-prefixed path ending in ".sf2",
/// then a 28-byte tail whose bytes 21..25 are an i32 (probably the preset).
pub(super) fn parse_sf2_state(state: &[u8]) -> Option<SfState> {
    let low: Vec<u8> = state.iter().map(|b| b.to_ascii_lowercase()).collect();
    let at = low.windows(4).position(|w| w == b".sf2")? + 4;

    // Walk back over printable bytes, then find the length byte: the
    // position s where state[s] == number of bytes between s+1 and `at`.
    let mut start = at;
    while start > 0 && (0x20..0x7f).contains(&state[start - 1]) {
        start -= 1;
    }
    let s = (start.saturating_sub(1)..at).find(|&s| state[s] as usize == at - (s + 1))?;
    let path = String::from_utf8_lossy(&state[s + 1..at]).into_owned();

    let tail = &state[at..];
    let preset = (tail.len() >= 25)
    .then(|| i32::from_le_bytes(tail[21..25].try_into().unwrap()));
    Some(SfState { path, preset })
}

fn collect_sf2(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth == 0 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(md) = std::fs::symlink_metadata(&p) else { continue };
        if md.file_type().is_dir() {
            collect_sf2(&p, depth - 1, out);
        } else if md.file_type().is_file()
            && p.extension()
            .and_then(|s| s.to_str())
            .map_or(false, |x| x.eq_ignore_ascii_case("sf2"))
            {
                out.push(p);
            }
    }
}

/// Look up the SF2 by the stem of the path stored in the state first, then
/// by channel name.  Searches three levels deep under each dir.
pub(super) fn find_sf2(st: Option<&SfState>, chan_name: &str, dirs: &[&Path]) -> Option<PathBuf> {
    let mut keys = Vec::new();
    if let Some(s) = st {
        let base = s.path.rsplit(|c| c == '\\' || c == '/').next().unwrap_or("");
        let stem = base.rsplit_once('.').map_or(base, |(a, _)| a);
        keys.push(normalize_sf2_key(stem));
    }
    keys.push(normalize_sf2_key(chan_name));
    keys.retain(|k| k.len() >= 4);

    let mut files = Vec::new();
    for d in dirs {
        collect_sf2(d, 3, &mut files);
    }
    for k in &keys {
        for f in &files {
            let stem = f.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if normalize_sf2_key(stem) == *k {
                return Some(f.clone());
            }
        }
    }
    None
}
