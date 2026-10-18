// src/convert/flp/sf2.rs
//! SoundFont support: read the SF2 path (bank, patch) out of FL's
//! SoundFont player state, then find that file on disk.
//!
//! Two state layouts are supported for *parsing*:
//!
//!   * SFLT: path preceded by a u32 length prefix, tail packs bank and
//!     preset into a single word.
//!   * Fruity Soundfont Player (FL native): a fixed 56-byte prologue,
//!     then a u8-length-prefixed path, then an 11-byte trailer. Bank
//!     and preset are stored separately in the prologue:
//!         [4..8]   u32: preset (0..127)
//!         [8..12]  u32: bank (0..128)
//!     When the prologue is too short (older format), both default to 0.
//!
//! The two are told apart by trying the SFLT shape first, then falling
//! back to the Fruity shape.  Both are strict enough that a false
//! positive is very unlikely: the SFLT check requires an exact u32
//! match, the Fruity check requires an exact u8 match plus printable
//! bytes for the whole candidate path.

use std::path::{Path, PathBuf};

use crate::formats::flp::Plugin;

pub(super) struct SfState {
    pub path: String,
    pub bank: u32,
    pub preset: u32,
}

/// True when the plugin is one of FL's SoundFont players.
pub(super) fn is_sf2_player(p: &Plugin) -> bool {
    let norm = |s: &str| s.to_ascii_lowercase();
    let i = norm(&p.internal_name);
    let d = norm(&p.display_name);
    let hit = |s: &str| {
        s.contains("soundfont")
        || s.contains("sf2 player")
        || s.contains("sf2player")
        || s == "sflt"
        || s.starts_with("sflt")
        || s.contains("sflt ")
    };
    hit(&i) || hit(&d)
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

fn find_sf2_marker(state: &[u8]) -> Option<usize> {
    let low: Vec<u8> = state.iter().map(|b| b.to_ascii_lowercase()).collect();
    let pos = low.windows(4).position(|w| w == b".sf2")?;
    Some(pos + 4)
}

fn sflt_path_start(state: &[u8], at: usize) -> Option<usize> {
    if at > state.len() {
        return None;
    }
    (4..at).find(|&s| {
        let prefix = u32::from_le_bytes([
            state[s - 4],
            state[s - 3],
            state[s - 2],
            state[s - 1],
        ]);
        prefix as usize == at - s && state[s..at].iter().all(|&b| b >= 0x20 && b != 0x7f)
    })
}

fn try_sflt(state: &[u8], at: usize) -> Option<SfState> {
    let s = sflt_path_start(state, at)?;
    let path = String::from_utf8_lossy(&state[s..at]).into_owned();

    let tail = &state[at..];
    let word = u32::from_le_bytes(tail.get(0..4)?.try_into().ok()?);
    let bank = word & 0xFF;
    let preset = (word >> 8) & 0x7F;
    Some(SfState { path, bank, preset })
}

fn try_fruity(state: &[u8], at: usize) -> Option<SfState> {
    let mut path_start: Option<usize> = None;
    for p in 0..at {
        if p + 1 >= at {
            break;
        }
        let expected = at - p - 1;
        if expected > 255 {
            continue;
        }
        if state[p] as usize != expected {
            continue;
        }
        if state[p + 1..at].iter().all(|&b| (0x20..0x7f).contains(&b)) {
            path_start = Some(p + 1);
            break;
        }
    }
    let s = path_start?;
    let path = String::from_utf8_lossy(&state[s..at]).into_owned();

    // Modern Fruity Soundfont Player state layout (version 2):
    //   [0..4]   u32: version = 2
    //   [4..8]   u32: preset (0..127)
    //   [8..12]  u32: bank   (0..128, 128 = drum bank)
    //   [12..16] u32: likely volume
    //   [16..20] u32: likely pan
    //   ...
    //
    // Byte 8-12 is the bank field. This was previously hardcoded to 0
    // which made bank-1 channels (e.g. `bee` and `oh` in Carol Roll,
    // which use preset (1, 2) and (1, 3)) indistinguishable from bank-0
    // channels. They aren't; the FLP author's soundfont put alternative
    // voice variants in a second bank.
    //
    // When the prologue is shorter than expected (older format), both
    // fields default to 0 — the previous behavior for the whole state.
    let preset = state
    .get(4..8)
    .and_then(|b| b.try_into().ok())
    .map(u32::from_le_bytes)
    .filter(|&p| p <= 127)
    .unwrap_or(0);
    let bank = state
    .get(8..12)
    .and_then(|b| b.try_into().ok())
    .map(u32::from_le_bytes)
    .filter(|&b| b <= 128)
    .unwrap_or(0);

    Some(SfState { path, bank, preset })
}

pub(super) fn parse_sf2_state(state: &[u8]) -> Option<SfState> {
    let debug = std::env::var_os("FLP_DEBUG").is_some();

    if debug {
        let hex: Vec<String> = state.iter().map(|b| format!("{:02x}", b)).collect();
        eprintln!("sf2: state {} bytes: {}", state.len(), hex.join(" "));
        let ascii: String = state
        .iter()
        .map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' })
        .collect();
        eprintln!("sf2:            ascii: {}", ascii);
    }

    let at = find_sf2_marker(state)?;

    if let Some(s) = try_sflt(state, at) {
        if debug {
            eprintln!(
                "sf2: SFLT OK path={:?} bank={} preset={}",
                s.path, s.bank, s.preset
            );
        }
        return Some(s);
    }

    if let Some(s) = try_fruity(state, at) {
        if debug {
            eprintln!(
                "sf2: Fruity SF2 Player OK path={:?} bank={} preset={}",
                s.path, s.bank, s.preset
            );
        }
        return Some(s);
    }

    if debug {
        eprintln!("sf2: no layout matched — marker at {}, length unparsed", at);
    }
    None
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

pub(super) fn find_sf2(st: Option<&SfState>, chan_name: &str, dirs: &[&Path]) -> Option<PathBuf> {
    let debug = std::env::var_os("FLP_DEBUG").is_some();

    let mut keys: Vec<String> = Vec::new();
    if let Some(st) = st {
        let base = st.path.rsplit(|c| c == '\\' || c == '/').next().unwrap_or("");
        let stem = base.rsplit_once('.').map_or(base, |(a, _)| a);
        let k = normalize_sf2_key(stem);
        if !k.is_empty() {
            keys.push(k);
        }
    }
    let name_key = normalize_sf2_key(chan_name);
    if name_key.len() >= 4 {
        keys.push(name_key);
    }

    let mut files = Vec::new();
    for d in dirs {
        collect_sf2(d, 6, &mut files);
    }
    files.sort();
    files.dedup();

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
