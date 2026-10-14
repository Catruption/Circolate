// src/convert/flp/sf2.rs
//! SoundFont support: read the SF2 path (bank, patch) out of FL's
//! SoundFont player state, then find that file on disk.
//!
//! Two state layouts are supported for *parsing*:
//!
//!   * SFLT: path preceded by a u32 length prefix, tail packs bank and
//!     preset into a single word.
//!   * Fruity SoundFont Player (FL native): path preceded by a single
//!     u8 length byte, preset stored as an LE u32 in the trailer.  No
//!     bank concept — always melodic bank 0.
//!
//! The two are told apart by trying the SFLT shape first, then falling
//! back to the Fruity shape.  Both are strict enough that a false
//! positive is very unlikely: the SFLT check requires an exact u32
//! match, the Fruity check requires an exact u8 match plus printable
//! bytes for the whole candidate path.
//!
//! Emitting SF2 sidecars (what `classify.rs` does) is handled by
//! `write_sf2_sidecar` — the state we produce is `Kind: sf2` pointing
//! at a copied .sf2 file, which the native `Sf2Engine` consumes at load
//! time.  No JSON state blob, no VST3 plugin.

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

/// Find the position one past a case-insensitive `.sf2` marker.
fn find_sf2_marker(state: &[u8]) -> Option<usize> {
    let low: Vec<u8> = state.iter().map(|b| b.to_ascii_lowercase()).collect();
    let pos = low.windows(4).position(|w| w == b".sf2")?;
    Some(pos + 4)
}

/// Start of the SFLT path: the first `s` where the u32 just before it
/// equals the byte count from `s` up to and including ".sf2", and every
/// byte in between could belong to a path (anything printable; bytes
/// >= 0x80 are allowed so non-ASCII user names do not break parsing).
fn sflt_path_start(state: &[u8], at: usize) -> Option<usize> {
    if at > state.len() {
        return None;
    }
    (4..at).find(|&s| {
        let prefix = u32::from_le_bytes([
            state[s - 4], state[s - 3], state[s - 2], state[s - 1],
        ]);
        prefix as usize == at - s
            && state[s..at].iter().all(|&b| b >= 0x20 && b != 0x7f)
    })
}

/// SFLT layout.  u32 length prefix; the value equals the number of
/// bytes from the byte after the prefix up to and including `.sf2`.
/// Bank and preset pack into one little-endian u32 immediately after
/// the path.
fn try_sflt(state: &[u8], at: usize) -> Option<SfState> {
    let s = sflt_path_start(state, at)?;
    let path = String::from_utf8_lossy(&state[s..at]).into_owned();

    let tail = &state[at..];
    let word = u32::from_le_bytes(tail.get(0..4)?.try_into().ok()?);
    let bank = word & 0xFF;
    let preset = (word >> 8) & 0x7F;
    Some(SfState { path, bank, preset })
}

/// Fruity SoundFont Player layout.  u8 length prefix; the path is the
/// next `len` bytes (ASCII, Windows backslashes).  Preset is an LE u32
/// at a fixed offset (21) inside the 27-byte trailer that follows the
/// path.  Bank is implicitly 0 — FSP has no bank parameter, only a
/// patch selector.
fn try_fruity(state: &[u8], at: usize) -> Option<SfState> {
    let mut path_start: Option<usize> = None;
    for p in 0..at {
        // Need at least one byte of path before the marker.
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

    // Preset sits at a fixed offset inside the trailer.  Verify the
    // bytes are available before reading.
    let preset_at = at + 21;
    let preset = u32::from_le_bytes(
        state.get(preset_at..preset_at + 4)?.try_into().ok()?,
    );

    // FSP presets are 0..=127 in the melodic range.  A value outside
    // that suggests we matched a false positive; bail rather than
    // route a bogus preset into the SF2 reader.
    if preset > 127 {
        return None;
    }

    Some(SfState { path, bank: 0, preset })
}

/// Parse an FL SoundFont player state blob.  Tries both known layouts
/// and returns whichever one validates.  Emits a hex + ASCII dump when
/// `FLP_DEBUG` is set, so unrecognised layouts can be inspected
/// without another rebuild.
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
                "sf2: Fruity SF2 Player OK path={:?} preset={}",
                s.path, s.preset
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

/// Look up the SF2 by the stem of the path stored in the state first,
/// then by channel name.  Searches six levels deep under each dir.
pub(super) fn find_sf2(st: Option<&SfState>, chan_name: &str, dirs: &[&Path]) -> Option<PathBuf> {
    let debug = std::env::var_os("FLP_DEBUG").is_some();

    let mut keys: Vec<String> = Vec::new();
    if let Some(st) = st {
        let base = st.path.rsplit(|c| c == '\\' || c == '/').next().unwrap_or("");
        let stem = base.rsplit_once('.').map_or(base, |(a, _)| a);
        // The stored file name is the real signal, however short ("gm").
        let k = normalize_sf2_key(stem);
        if !k.is_empty() {
            keys.push(k);
        }
    }
    // Channel names are noisy ("SFLT #3"), so only trust longer ones.
    let name_key = normalize_sf2_key(chan_name);
    if name_key.len() >= 4 {
        keys.push(name_key);
    }

    let mut files = Vec::new();
    for d in dirs {
        // Depth from each search root.  soundfonts/github-collection/SF2/GM/
        // is four levels down from the pool root, so 3 misses the GM banks.
        collect_sf2(d, 6, &mut files);
    }
    // read_dir order is arbitrary; sort so duplicates (e.g. "X.sf2" and
    // "X_.sf2") always resolve the same way.
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