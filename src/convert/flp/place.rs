// src/convert/flp/place.rs
//! Playlist + pattern → (channel, absolute tick) placements.
//!
//! Pattern clips on the playlist drive placement.  A pattern with no
//! playlist item is silent, matching FL's behaviour.  A pattern clip shows
//! the window [clip_start_ticks, clip_start_ticks + length) of its pattern:
//! notes outside the window are skipped and the rest are re-based so the
//! window start lands on the clip start.  Audio clips (playlist items that
//! reference a channel directly) are placed as synthetic C5 notes covering
//! the clip, carrying the clip's source offset so playback starts at the
//! right point in the file.  Disabled (muted) channels are skipped.

use std::collections::{HashMap, HashSet};

use crate::sources::flp::{Note, Project};

use super::{debug_on, hex, FL_C5_KEY, MAX_NOTE_TICKS};

#[derive(Clone)]
pub(super) struct Placed {
    pub(super) abs: u32,
    pub(super) note: Note,
    /// For audio clips: where inside the source file playback should
    /// start, in milliseconds.  `None` for pattern notes.
    pub(super) clip_offset_ms: Option<f32>,
    /// For audio clips: where inside the source file the clip's window
    /// ends, in milliseconds.  `None` for pattern notes; negative means
    /// the clip is untrimmed (the window runs to the end of the file).
    pub(super) clip_end_ms: Option<f32>,
    /// For audio clips: the clip's own gain multiplier (FL stores it as a
    /// float at byte 52 of the playlist record; 1.0 = no change).  1.0 for
    /// pattern notes.
    pub(super) clip_gain: f32,
}

pub(super) fn place_notes(prj: &Project) -> (HashMap<u16, Vec<Placed>>, bool, usize) {
    // FL emits NewPattern twice per pattern.  Both entries are in
    // prj.patterns; one carries the notes, the other carries metadata.
    // Keep whichever has more notes so we don't read the empty duplicate.
    let mut pats: HashMap<u16, &crate::sources::flp::Pattern> = HashMap::new();
    for p in &prj.patterns {
        pats.entry(p.iid)
        .and_modify(|old| {
            if p.notes.len() > old.notes.len() {
                *old = p;
            }
        })
        .or_insert(p);
    }

    // Channels switched off in the channel rack.  `enabled == None` means
    // the flag was absent, which is treated as enabled.
    let disabled: HashSet<u16> = prj
    .channels
    .iter()
    .filter(|c| c.enabled == Some(false))
    .map(|c| c.iid)
    .collect();
    if debug_on() && !disabled.is_empty() {
        let mut v: Vec<u16> = disabled.iter().copied().collect();
        v.sort_unstable();
        eprintln!("flp: skipping disabled channels {:?}", v);
    }

    let mut placed: HashMap<u16, Vec<Placed>> = HashMap::new();
    let mut dropped = 0usize;
    let mut used_playlist = false;

    if debug_on() {
        let on_playlist: HashSet<u16> = prj
        .playlist
        .iter()
        .filter(|i| i.kind == "pattern")
        .map(|i| i.ref_id)
        .collect();
        eprintln!(
            "flp: {} patterns total; {} unique pattern refs on playlist",
            prj.patterns.len(),
                  on_playlist.len()
        );
        for target in [4u16, 6, 10, 5] {
            let pats: Vec<u16> = prj
            .patterns
            .iter()
            .filter(|p| p.notes.iter().any(|n| n.channel == target))
            .map(|p| p.iid)
            .collect();
            let on_pl: Vec<u16> = pats
            .iter()
            .copied()
            .filter(|i| on_playlist.contains(i))
            .collect();
            eprintln!(
                "flp: chan {:>3} notes in patterns {:?} (on playlist: {:?})",
                      target, pats, on_pl
            );
        }
    }

    // 1. Pattern clips on the playlist drive placement.
    for item in prj.playlist.iter().filter(|i| i.kind == "pattern") {
        let Some(pat) = pats.get(&item.ref_id) else {
            continue;
        };
        used_playlist = true;

        // The clip shows pattern ticks [win_start, win_end).  A length of 0
        // is treated as "no end".
        // FLP_NO_WINDOW=1 restores the old behaviour (play from pattern tick
        // 0, cut at the clip length) so the two can be compared.
        let legacy = std::env::var_os("FLP_NO_WINDOW").is_some();
        let win_start = if legacy { 0 } else { item.clip_start_ticks };
        let win_end = if item.length > 0 {
            Some(win_start.saturating_add(item.length))
        } else {
            None
        };
        let mut kept = 0usize;

        for n in &pat.notes {
            if disabled.contains(&n.channel) {
                continue;
            }
            if n.position > MAX_NOTE_TICKS {
                if debug_on() && dropped < 20 {
                    eprintln!(
                        "flp: dropped note pat {} chan {} pos {} len {} key {} flags {:#06x}",
                        pat.iid, n.channel, n.position, n.length, n.key, n.flags
                    );
                }
                dropped += 1;
                continue;
            }
            if n.position < win_start {
                continue;
            }
            if let Some(end) = win_end {
                if n.position >= end {
                    continue;
                }
            }

            let mut note = n.clone();
            // A note can't ring past the end of its clip.
            if let Some(end) = win_end {
                note.length = note.length.min(end - n.position);
            }

            kept += 1;
            placed.entry(n.channel).or_default().push(Placed {
                abs: item.position.saturating_add(n.position - win_start),
                                                      note,
                                                      clip_offset_ms: None,
                clip_end_ms: None,
                clip_gain: 1.0,
            });
        }
        if debug_on() {
            eprintln!(
                "flp: clip pat {:>3} at {:>7} len {:>6} window {}..{:?}: kept {}/{} notes",
                pat.iid,
                item.position,
                item.length,
                win_start,
                win_end,
                kept,
                pat.notes.len()
            );
        }
    }

    // 2. Fallback only when the playlist carries zero pattern clips.
    if !used_playlist {
        for pat in &prj.patterns {
            for n in &pat.notes {
                if disabled.contains(&n.channel) {
                    continue;
                }
                if n.position > MAX_NOTE_TICKS {
                    if debug_on() && dropped < 20 {
                        eprintln!(
                            "flp: dropped note (no playlist) chan {} pos {} len {} key {}",
                                  n.channel, n.position, n.length, n.key
                        );
                    }
                    dropped += 1;
                    continue;
                }
                placed.entry(n.channel).or_default().push(Placed {
                    abs: n.position,
                    note: n.clone(),
                                                          clip_offset_ms: None,
                clip_end_ms: None,
                clip_gain: 1.0,
                });
            }
        }
    }

    // 3. Audio clips (playlist items referencing a channel directly).
    for item in prj.playlist.iter().filter(|i| i.kind == "channel") {
        if disabled.contains(&item.ref_id) {
            continue;
        }
        let is_audio_clip = prj.channels.iter().any(|c| {
            c.iid == item.ref_id && c.channel_type != "automation" && !c.sample_path.is_empty()
        });
        if !is_audio_clip {
            continue;
        }
        if item.position > MAX_NOTE_TICKS || item.length > MAX_NOTE_TICKS {
            if debug_on() {
                eprintln!(
                    "flp: dropped audio clip chan {} pos {} len {} raw {}",
                    item.ref_id,
                    item.position,
                    item.length,
                    hex(&item.raw[..item.raw.len().min(32)])
                );
            }
            dropped += 1;
            continue;
        }
        let clip_gain = item
            .raw
            .get(52..56)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .filter(|g| g.is_finite() && *g > 0.0 && *g < 16.0)
            .unwrap_or(1.0);
        placed.entry(item.ref_id).or_default().push(Placed {
            abs: item.position,
            note: Note {
                position: item.position,
                length: item.length,
                key: FL_C5_KEY,
                channel: item.ref_id,
                velocity: 127,
                pan: 64,
                fine_pitch: 120,
                release: 0,
                mod_x: 0,
                mod_y: 0,
                flags: 0,
                group: 0,
            },
            // The file stores -1.0 for "no trim"; clamp it to 0.
            clip_offset_ms: Some(item.start_offset_ms.max(0.0)),
            clip_end_ms: Some(item.end_offset_ms),
            clip_gain,
        });
    }

    for v in placed.values_mut() {
        v.sort_by_key(|p| p.abs);
    }
    (placed, used_playlist, dropped)
}

pub(super) fn notes_in_measure<'a>(
    placed: &'a HashMap<u16, Vec<Placed>>,
    iid: u16,
    m_start: u32,
    m_end: u32,
) -> Vec<&'a Placed> {
    placed
    .get(&iid)
    .map(|v| {
        v.iter()
        .filter(|p| m_start <= p.abs && p.abs < m_end)
        .collect()
    })
    .unwrap_or_default()
}

pub(super) fn end_tick_of(placed: &HashMap<u16, Vec<Placed>>, iid: u16) -> u32 {
    placed.get(&iid).map_or(0, |v| {
        v.iter()
        .map(|p| p.abs.saturating_add(p.note.length))
        .max()
        .unwrap_or(0)
    })
}