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

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::fidelity::InsertFidelity;
use crate::formats::flp::{Note, Project};

use super::{debug_on, hex, FL_C5_KEY, MAX_NOTE_TICKS};
use super::slicex;

/// A pitch glide attached to a note, from an FL slide note that overlaps it.
/// Tick offsets are relative to the carrier note's start.
#[derive(Clone)]
pub(super) struct Bend {
    pub(super) at_ticks: u32,
    pub(super) len_ticks: u32,
    pub(super) key: u16,
}

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
    /// For audio clips: fade-in / fade-out lengths in milliseconds
    /// (0.0 = none).  0.0 for pattern notes.
    pub(super) clip_fade_in_ms: f32,
    pub(super) clip_fade_out_ms: f32,
    /// Slide-note glides on this note.  Empty for notes with no slides.
    pub(super) bends: Vec<Bend>,
}

/// FL "slide" notes (flag bit 3) are not voices of their own: they tell the
/// overlapping note on the same channel to glide to the slide note's key.
///
/// FIXED: All instruments (Slicex and pitched alike) now use pitch bends.
/// Previously, pitched instruments had their carrier note split into multiple
/// short notes (one per slide), which produced MIDI spam and corrupted
/// subsequent carrier selection.
fn resolve_slides(
    notes: &[Note],
    _slicex_channels: &HashSet<u16>,
) -> Vec<(Note, Vec<Bend>)> {
    const FLAG_SLIDE: u16 = 0x08;
    if std::env::var_os("FLP_NO_SLIDE").is_some() {
        return notes.iter().cloned().map(|n| (n, Vec::new())).collect();
    }
    let mut out: Vec<(Note, Vec<Bend>)> = notes
    .iter()
    .filter(|n| n.flags & FLAG_SLIDE == 0)
    .cloned()
    .map(|n| (n, Vec::new()))
    .collect();
    let mut slides: Vec<&Note> = notes
    .iter()
    .filter(|n| n.flags & FLAG_SLIDE != 0)
    .collect();
    slides.sort_by_key(|n| n.position);

    let mut bent = 0usize;
    for s in slides {
        // Carrier: same channel, latest start at or before the slide, still sounding.
        let mut best: Option<usize> = None;
        for (i, (n, _)) in out.iter().enumerate() {
            if n.channel == s.channel
                && n.position <= s.position
                && s.position < n.position.saturating_add(n.length)
                && best.map_or(true, |b| n.position > out[b].0.position)
                {
                    best = Some(i);
                }
        }
        let Some(i) = best else { continue };

        if out[i].0.position == s.position {
            // Slide starts together with the carrier: retarget immediately.
            out[i].0.key = s.key;
        } else {
            // Emit a pitch bend waypoint instead of splitting the note.
            let start = out[i].0.position;
            out[i].1.push(Bend {
                at_ticks: s.position - start,
                len_ticks: s.length,
                key: s.key,
            });
            bent += 1;
        }
    }
    if debug_on() && bent > 0 {
        eprintln!("flp: {bent} slide note(s) kept as pitch bends");
    }
    out
}

pub(super) fn place_notes(prj: &Project) -> (HashMap<u16, Vec<Placed>>, bool, usize) {
    // FL emits NewPattern twice per pattern.  Both entries are in
    // prj.patterns; one carries the notes, the other carries metadata.
    // Keep whichever has more notes so we don't read the empty duplicate.
    let mut pats: HashMap<u16, &crate::formats::flp::Pattern> = HashMap::new();
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

    // Slicex channels are tracked separately in case we need to special-case
    // them again in the future; the current slide handler treats all channels
    // the same way (pitch bends, no note splits).
    let slicex_channels: HashSet<u16> = prj
    .channels
    .iter()
    .filter(|c| {
        c.plugin
        .as_ref()
        .map_or(false, |p| slicex::is_slicex(p))
    })
    .map(|c| c.iid)
    .collect();
    if debug_on() && !slicex_channels.is_empty() {
        let mut v: Vec<u16> = slicex_channels.iter().copied().collect();
        v.sort_unstable();
        eprintln!("flp: Slicex channels {:?}", v);
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
    }

    // 1. Pattern clips on the playlist drive placement.
    for item in prj.playlist.iter().filter(|i| i.kind == "pattern") {
        let Some(pat) = pats.get(&item.ref_id) else {
            continue;
        };
        used_playlist = true;

        let legacy = std::env::var_os("FLP_NO_WINDOW").is_some();
        let win_start = if legacy { 0 } else { item.clip_start_ticks };
        let win_end = if item.length > 0 {
            Some(win_start.saturating_add(item.length))
        } else {
            None
        };

        let notes = resolve_slides(&pat.notes, &slicex_channels);
        for (n, bends) in &notes {
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
            if let Some(end) = win_end {
                note.length = note.length.min(end - n.position);
            }

            placed.entry(n.channel).or_default().push(Placed {
                abs: item.position.saturating_add(n.position - win_start),
                                                      note,
                                                      clip_offset_ms: None,
                                                      clip_end_ms: None,
                                                      clip_gain: 1.0,
                                                      clip_fade_in_ms: 0.0,
                                                      clip_fade_out_ms: 0.0,
                                                      bends: bends.clone(),
            });
        }
    }

    // 2. Fallback only when the playlist carries zero pattern clips.
    if !used_playlist {
        for pat in &prj.patterns {
            let notes = resolve_slides(&pat.notes, &slicex_channels);
            for (n, bends) in &notes {
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
                                                          clip_fade_in_ms: 0.0,
                                                          clip_fade_out_ms: 0.0,
                                                          bends: bends.clone(),
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
            clip_offset_ms: Some(item.start_offset_ms.max(0.0)),
                                                    clip_end_ms: Some(item.end_offset_ms),
                                                    clip_gain,
                                                    clip_fade_in_ms: item.fade_in_ms,
                                                    clip_fade_out_ms: item.fade_out_ms,
                                                    bends: Vec::new(),
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

// ── shim emission ──────────────────────────────────────────────────────
//
// Shim helpers for the FLP → trackscript conversion.  The top-level
// `convert/flp.rs` calls `channel_shims` once the FLP parser has
// exposed each channel's mixer insert (`Channel.insert`), then appends
// the returned `@<ƒSlug()>` strings to each channel's call line in
// BlockList — via `Song::write()`, which interleaves `MeasureLine.extra`
// with `MeasureLine.calls`.

/// Slug used inside `@<ƒ…>` — letters, digits, `_`, and `.`.  Matches
/// the `.instrument` sidecar naming so `share/fallbacks.json` keys are
/// consistent across the insert-chain and shim paths.
pub(super) fn effect_slug(name: &str) -> String {
    name.chars()
    .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '.' { c } else { '_' })
    .collect()
}

/// Hostable effects from one insert, in slot order.  Skips slots whose
/// resolution produced no `.instrument` sidecar (unresolved / native
/// without fallback) — those have no loadable path.
pub(super) fn effect_slugs_for_insert(insert: &InsertFidelity) -> Vec<String> {
    let mut slugs: Vec<(u32, String)> = insert
    .slots
    .iter()
    .filter(|s| s.instrument.is_some() && !s.plugin.is_empty())
    .map(|s| (s.slot, effect_slug(&s.plugin)))
    .collect();
    slugs.sort_by_key(|(s, _)| *s);
    slugs.into_iter().map(|(_, slug)| slug).collect()
}

/// Channel iid → ordered effect slugs, based on the FLP's routing map.
/// A channel with no insert (or an insert with no hostable effects) is
/// omitted from the result — its calls are emitted plain.
pub(super) fn channel_shims(
    inserts: &BTreeMap<String, InsertFidelity>,
    channel_to_insert: &HashMap<u16, u8>,
) -> HashMap<u16, Vec<String>> {
    let mut out: HashMap<u16, Vec<String>> = HashMap::new();
    for (&channel, &insert_idx) in channel_to_insert {
        let Some(ins) = inserts.get(&insert_idx.to_string()) else { continue };
        let slugs = effect_slugs_for_insert(ins);
        if !slugs.is_empty() {
            out.insert(channel, slugs);
        }
    }
    out
}

/// Format one shim as `@<ƒSlug()>` or `@<ƒSlug(α25, β5)>`.  Emitted
/// immediately after the call, so a call becomes
/// `ƒPart()@<ƒReverb()>;` or `ƒPart()@<ƒReverb()>@<ƒDelay()>;`.
pub(super) fn shim_string(slug: &str, params: &[(u32, f64)]) -> String {
    if params.is_empty() {
        return format!("@<ƒ{}()>", slug);
    }
    let parts: Vec<String> = params
    .iter()
    .map(|(slot, val)| format!("{}{}", crate::spec::slot_to_greek(*slot), val))
    .collect();
    format!("@<ƒ{}({})>", slug, parts.join(", "))
}

/// Concatenate shims for one channel into a suffix the caller appends
/// to a call: `""` for no effects, `"@<ƒReverb()>"`, or a chain.
pub(super) fn shim_suffix(slugs: &[String]) -> String {
    slugs.iter().map(|s| shim_string(s, &[])).collect()
}
