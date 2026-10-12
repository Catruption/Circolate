// src/convert/flp/sf2.rs
//! SoundFont support: read the SF2 path (bank, patch) out of FL's
//! SoundFont player state, then find that file on disk.

use std::path::{Path, PathBuf};

pub(super) struct SfState {
    pub path: String,
    /// MIDI bank.  FL stores drum kits on bank 128, melodic on bank 0.
    pub bank: u32,
    /// Patch within the bank.
    pub preset: u32,
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

/// State layout: header, then a little-endian u32 length prefix, then the
/// path bytes (starting right after the prefix) up to and including
/// `.sf2`, then a little-endian u32 packing bank and patch:
///
///   bank  = word & 0xFF
///   patch = (word >> 8) & 0x7F
///
/// FL stores drum kits as bank 128, melodic patches on bank 0.
///
/// The u32 prefix check is what identifies the start of the path: its
/// value equals the number of bytes between the end of the prefix and
/// the end of the `.sf2` marker.  No printable-run walk-back, so no
/// overrun into the wrapper header, and no false positives inside the
/// path itself.
pub(super) fn parse_sf2_state(state: &[u8]) -> Option<SfState> {
    let debug = std::env::var_os("FLP_DEBUG").is_some();
    let low: Vec<u8> = state.iter().map(|b| b.to_ascii_lowercase()).collect();
    let marker_pos = low.windows(4).position(|w| w == b".sf2")?;
    let at = marker_pos + 4;

    let mut path_start: Option<usize> = None;
    for s in 4..at {
        let prefix = u32::from_le_bytes([
            state[s - 4], state[s - 3], state[s - 2], state[s - 1],
        ]);
        if prefix as usize == at - s
            && state[s..at].iter().all(|&b| (0x20..0x7f).contains(&b))
        {
            path_start = Some(s);
            break;
        }
    }
    let s = path_start?;
    let path = String::from_utf8_lossy(&state[s..at]).into_owned();

    let tail = &state[at..];
    let word = u32::from_le_bytes(tail.get(0..4)?.try_into().ok()?);
    let bank = word & 0xFF;
    let preset = (word >> 8) & 0x7F;

    if debug {
        eprintln!(
            "sf2: OK path_start={} path_len={} bank={} preset={}",
            s, at - s, bank, preset
        );
    }

    Some(SfState { path, bank, preset })
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
    let debug = std::env::var_os("FLP_DEBUG").is_some();

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

    if debug {
        eprintln!(
            "flp: find_sf2 chan {:?} keys {:?} dirs {} candidates {}",
            chan_name,
            keys,
            dirs.len(),
            files.len(),
        );
    }

    for k in &keys {
        for f in &files {
            let stem = f.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if normalize_sf2_key(stem) == *k {
                if debug {
                    eprintln!("flp:   matched key {:?} -> {}", k, f.display());
                }
                return Some(f.clone());
            }
        }
    }

    if debug {
        for f in &files {
            eprintln!("flp:   candidate {}", f.display());
        }
    }
    None
}