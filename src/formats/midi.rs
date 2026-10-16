// src/sources/midi.rs
//! Standard MIDI File parsing.
//!
//! Wraps `midly` and flattens the event stream into a per-channel note
//! list.  Nothing here emits trackscript; the shape is chosen so the
//! converter doesn't have to touch midly types.
//!
//! Format 0 and 1 are supported.  Format 2 (independent sequences) is
//! rejected — it isn't one song, so there's no meaningful conversion.
//! Pitch bend, CC1, CC64, aftertouch and SysEx are ignored: the first
//! pass is notes-only, which is a large fraction of the value.

use anyhow::{bail, Context, Result};
use midly::{Format, MetaMessage, MidiMessage, Smf, Timing, TrackEventKind};
use std::collections::{HashMap, HashSet};
use std::path::Path;

pub struct MidiFile {
    /// Pulses per quarter note (metrical timing).  Usually 480.
    pub ppq: u16,
    /// Initial time signature (numerator, denominator).  Denominator is
    /// already converted from the file's log2 encoding: `[4, 2]` in the
    /// file becomes `(4, 4)` here.
    pub time_sig: (u8, u8),
    /// Initial tempo in BPM.
    pub initial_bpm: f64,
    /// One entry per MIDI channel that carried notes.
    pub channels: Vec<MidiChannel>,
}

pub struct MidiChannel {
    /// MIDI channel number, 0-15.  Channel 9 is percussion.
    pub channel: u8,
    /// Track name, if every track carrying this channel had the same one.
    pub name: Option<String>,
    /// Last program change on this channel.  Defaults to 0.
    pub program: u8,
    /// Bank select MSB (CC 0).
    pub bank_msb: u8,
    /// Bank select LSB (CC 32).
    pub bank_lsb: u8,
    pub notes: Vec<MidiNote>,
}

#[derive(Clone, Debug)]
pub struct MidiNote {
    /// Start, in quarter-note beats from the file's start.
    pub start_beat: f64,
    pub dur_beat: f64,
    /// MIDI key number, 0-127.
    pub key: u8,
    /// Note-on velocity, 1-127.
    pub velocity: u8,
}

pub fn parse_file(path: &Path) -> Result<MidiFile> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("reading {}", path.display()))?;
    parse_bytes(&bytes)
}

pub fn parse_bytes(bytes: &[u8]) -> Result<MidiFile> {
    let smf = Smf::parse(bytes).context("parsing MIDI")?;

    if matches!(smf.header.format, Format::Sequential) {
        bail!("format 2 (Sequential) MIDI files aren't supported");
    }

    let ppq = match smf.header.timing {
        Timing::Metrical(p) => p.as_int(),
        Timing::Timecode(_, _) => {
            bail!("SMPTE-timed MIDI files aren't supported (metrical timing required)")
        }
    };
    if ppq == 0 {
        bail!("MIDI file has PPQ = 0");
    }
    let ppq_f = ppq as f64;

    struct Acc {
        program: u8,
        bank_msb: u8,
        bank_lsb: u8,
        held: HashMap<u8, (u64, u8)>, // key -> (start_tick, velocity)
        notes: Vec<MidiNote>,
    }
    impl Acc {
        fn new() -> Self {
            Acc {
                program: 0,
                bank_msb: 0,
                bank_lsb: 0,
                held: HashMap::new(),
                notes: Vec::new(),
            }
        }
        fn close(&mut self, key: u8, tick: u64, ppq_f: f64) {
            if let Some((start, vel)) = self.held.remove(&key) {
                self.notes.push(MidiNote {
                    start_beat: start as f64 / ppq_f,
                    dur_beat: (tick - start) as f64 / ppq_f,
                    key,
                    velocity: vel,
                });
            }
        }
    }

    let mut accs: HashMap<u8, Acc> = HashMap::new();
    let mut initial_bpm = 120.0;
    let mut time_sig = (4u8, 4u8);
    let mut seen_time_sig = false;

    // Per track, record the track name (if any) and which channels it
    // touched — used below to name each channel's part.
    let mut track_names: Vec<Option<String>> = Vec::with_capacity(smf.tracks.len());
    let mut track_channels: Vec<HashSet<u8>> = Vec::with_capacity(smf.tracks.len());

    for track in &smf.tracks {
        let mut tick: u64 = 0;
        let mut this_name: Option<String> = None;
        let mut this_channels: HashSet<u8> = HashSet::new();

        for ev in track {
            tick += ev.delta.as_int() as u64;

            match ev.kind {
                TrackEventKind::Midi { channel, message } => {
                    let ch = channel.as_int();
                    this_channels.insert(ch);
                    let acc = accs.entry(ch).or_insert_with(Acc::new);

                    match message {
                        MidiMessage::NoteOn { key, vel } => {
                            let k = key.as_int();
                            let v = vel.as_int();
                            if v == 0 {
                                // Note-on with velocity 0 == note-off.
                                acc.close(k, tick, ppq_f);
                            } else {
                                // Retrigger: close any pending same-key
                                // note first so the previous one has a
                                // duration.
                                acc.close(k, tick, ppq_f);
                                acc.held.insert(k, (tick, v));
                            }
                        }
                        MidiMessage::NoteOff { key, .. } => {
                            acc.close(key.as_int(), tick, ppq_f);
                        }
                        MidiMessage::ProgramChange { program } => {
                            acc.program = program.as_int();
                        }
                        MidiMessage::Controller { controller, value } => {
                            match controller.as_int() {
                                0  => acc.bank_msb = value.as_int(),
                                32 => acc.bank_lsb = value.as_int(),
                                _  => {}
                            }
                        }
                        _ => {}
                    }
                }
                TrackEventKind::Meta(MetaMessage::Tempo(us)) => {
                    // Honour only the file's initial tempo.  Later
                    // tempo changes would need trackscript tempo shims,
                    // which is a later pass.
                    if tick == 0 {
                        initial_bpm = 60_000_000.0 / us.as_int() as f64;
                    }
                }
                TrackEventKind::Meta(MetaMessage::TimeSignature(n, d, _, _)) => {
                    if !seen_time_sig {
                        // The second byte is log2(denominator): 2 → 4,
                        // 3 → 8, 1 → 2, 0 → 1.  A value above 7 is
                        // malformed; clamp rather than shifting past
                        // u8.
                        let denom: u8 = match d {
                            0..=7 => 1u8 << d,
                            _     => 128,
                        };
                        time_sig = (n, denom);
                        seen_time_sig = true;
                    }
                }
                TrackEventKind::Meta(MetaMessage::TrackName(name)) => {
                    if this_name.is_none() {
                        this_name = Some(String::from_utf8_lossy(name).into_owned());
                    }
                }
                _ => {}
            }
        }

        track_names.push(this_name);
        track_channels.push(this_channels);
    }

    // Close anything still held (damaged file, or a track that ends
    // with notes on).  Half a beat is a safe default that won't click.
    for (_, acc) in accs.iter_mut() {
        let held: Vec<u8> = acc.held.keys().copied().collect();
        for k in held {
            if let Some((start, vel)) = acc.held.remove(&k) {
                acc.notes.push(MidiNote {
                    start_beat: start as f64 / ppq_f,
                    dur_beat: 0.5,
                    key: k,
                    velocity: vel,
                });
            }
        }
        acc.notes.sort_by(|a, b| {
            a.start_beat
                .partial_cmp(&b.start_beat)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    // Per-channel names: use a track name only if the track owned
    // exactly one channel.  Otherwise fall back to None (part becomes
    // ChNN).
    let mut chan_name: HashMap<u8, String> = HashMap::new();
    for (i, chans) in track_channels.iter().enumerate() {
        if chans.len() == 1 {
            if let (Some(name), Some(ch)) = (&track_names[i], chans.iter().next()) {
                chan_name.entry(*ch).or_insert_with(|| name.clone());
            }
        }
    }

    let mut channels: Vec<MidiChannel> = accs
        .into_iter()
        .filter(|(_, acc)| !acc.notes.is_empty())
        .map(|(ch, acc)| MidiChannel {
            channel: ch,
            name: chan_name.get(&ch).cloned(),
            program: acc.program,
            bank_msb: acc.bank_msb,
            bank_lsb: acc.bank_lsb,
            notes: acc.notes,
        })
        .collect();
    channels.sort_by_key(|c| c.channel);

    if channels.is_empty() {
        bail!("MIDI file has no notes");
    }

    Ok(MidiFile {
        ppq,
        time_sig,
        initial_bpm,
        channels,
    })
}