//! MIDI — parsed data structures.
//!
//! Pure data. The parse step flattens midly types into this shape so
//! downstream code never has to touch `midly`.

pub struct MidiFile {
    /// Pulses per quarter note (metrical timing). Usually 480.
    pub ppq: u16,
    /// Initial time signature (numerator, denominator). Denominator is
    /// already converted from the file's log2 encoding: `[4, 2]` in the
    /// file becomes `(4, 4)` here.
    pub time_sig: (u8, u8),
    /// Initial tempo in BPM.
    pub initial_bpm: f64,
    /// One entry per MIDI channel that carried notes.
    pub channels: Vec<MidiChannel>,
}

pub struct MidiChannel {
    /// MIDI channel number, 0-15. Channel 9 is percussion.
    pub channel: u8,
    /// Track name, if every track carrying this channel had the same one.
    pub name: Option<String>,
    /// Last program change on this channel. Defaults to 0.
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