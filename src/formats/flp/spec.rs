//! FL Studio project format — event IDs and slot constants.
//!
//! Pure data tables. No logic, no I/O.
//!
//! All IDs are the FLP event stream identifiers (bytes 0..=255).
//! Ranges by size: 0–63 = 1 byte, 64–127 = 2, 128–191 = 4,
//! 192–255 = varint-length-prefixed blob.

// ── channel events ────────────────────────────────────────────────────

pub const EV_CHAN_ENABLED: u8 = 0;
pub const EV_CHAN_TYPE: u8 = 21;
pub const EV_CHAN_SAMPLE_START: u8 = 67;
pub const EV_CHAN_SAMPLE_END: u8 = 68;
pub const EV_CHAN_ROOT_NOTE: u8 = 135;
pub const EV_CHAN_FINE_TUNE: u8 = 142;
pub const EV_CHAN_SAMPLER_FLAGS: u8 = 143;
pub const EV_CHAN_ENV_LFO: u8 = 218;
pub const EV_CHAN_LEVELS: u8 = 219;
pub const EV_CHAN_TRACKING: u8 = 228;
pub const EV_CHAN_LEVEL_ADJUSTS: u8 = 229;
pub const EV_CHAN_MIXER_TRACK: u8 = 178;
pub const EV_CHAN_DELAY: u8 = 209;        // 20 bytes, 5×i32
pub const EV_CHAN_PARAMETERS: u8 = 215;   // 157 (FL ≤20.8) / 168 (FL ≥20.9)
pub const EV_CHAN_POLYPHONY: u8 = 221;    // 9 bytes
pub const EV_REMOTE_CONTROLLER: u8 = 227; // 18 bytes, project-scoped
pub const EV_TRACK_DATA: u8 = 238;        // 66 / 70 bytes, versioned

// ── structural events ────────────────────────────────────────────────

pub const EV_NEW_CHAN: u8 = 64;
pub const EV_NEW_PAT: u8 = 65;
pub const EV_TEMPO_OLD: u8 = 66;
pub const EV_FINE_TEMPO: u8 = 156;
pub const EV_COLOR: u8 = 128;
pub const EV_NEW_PLUGIN: u8 = 212;
pub const EV_PLUGIN_PARAMS: u8 = 213;
pub const EV_PAT_NOTES: u8 = 224;
pub const EV_PLAYLIST: u8 = 233;

// ── text events ──────────────────────────────────────────────────────

pub const EV_TEXT_CHAN_NAME: u8 = 192;
pub const EV_TEXT_PAT_NAME: u8 = 193;
pub const EV_TEXT_TITLE: u8 = 194;
pub const EV_TEXT_COMMENT: u8 = 195;
pub const EV_TEXT_SAMPLE: u8 = 196;
pub const EV_TEXT_VERSION: u8 = 199;
pub const EV_TEXT_DEF_PLUGIN: u8 = 201;
pub const EV_TEXT_DATA_PATH: u8 = 202;
pub const EV_TEXT_PLUGIN_NAME: u8 = 203;
pub const EV_TEXT_FX_NAME: u8 = 204;
pub const EV_TEXT_GENRE: u8 = 206;
pub const EV_TEXT_AUTHOR: u8 = 207;

// ── layout ───────────────────────────────────────────────────────────

/// Size of one pattern-note record in bytes.
pub const NOTE_SIZE: usize = 24;

/// Event IDs this parser actively matches. Anything else is preserved
/// raw in `Project.unknown_events` and counted in `Project.coverage`.
pub const HANDLED_EVENT_IDS: &[u8] = &[
    EV_CHAN_ENABLED, EV_CHAN_TYPE, EV_CHAN_SAMPLE_START, EV_CHAN_SAMPLE_END,
    EV_CHAN_ROOT_NOTE, EV_CHAN_FINE_TUNE, EV_CHAN_SAMPLER_FLAGS,
    EV_CHAN_ENV_LFO, EV_CHAN_LEVELS, EV_CHAN_TRACKING, EV_CHAN_LEVEL_ADJUSTS,
    EV_CHAN_MIXER_TRACK, EV_CHAN_DELAY, EV_CHAN_PARAMETERS, EV_CHAN_POLYPHONY,
    EV_REMOTE_CONTROLLER, EV_TRACK_DATA,
    EV_NEW_CHAN, EV_NEW_PAT, EV_TEMPO_OLD, EV_FINE_TEMPO, EV_COLOR,
    EV_TEXT_CHAN_NAME, EV_TEXT_PAT_NAME, EV_TEXT_TITLE, EV_TEXT_COMMENT,
    EV_TEXT_SAMPLE, EV_TEXT_VERSION, EV_TEXT_DEF_PLUGIN, EV_TEXT_DATA_PATH,
    EV_TEXT_PLUGIN_NAME, EV_TEXT_FX_NAME, EV_TEXT_GENRE, EV_TEXT_AUTHOR,
    EV_NEW_PLUGIN, EV_PLUGIN_PARAMS, EV_PAT_NOTES, EV_PLAYLIST,
];

// ── channel-envelope slots ──────────────────────────────────────────

pub const ENV_PANNING: usize = 0;
pub const ENV_VOLUME: usize = 1;
pub const ENV_MOD_X: usize = 2;
pub const ENV_MOD_Y: usize = 3;
pub const ENV_PITCH: usize = 4;

// ── channel type ids ─────────────────────────────────────────────────

/// FL's ChannelType enum in this parser's historical reading:
///   0 = Sampler, 2 = Native, 3 = Layer, 4 = Instrument, 5 = Automation.
/// LYNX-SHARE documents 1 = Native, 2 = Instrument. If channels from
/// newer FL Studio versions misclassify, verify against a known FLP
/// where the type is unambiguous before changing this table.
pub const CHANNEL_TYPE_NAMES: &[(u8, &str)] = &[
    (0, "sampler"),
    (2, "native"),
    (3, "layer"),
    (4, "instrument"),
    (5, "automation"),
];

/// Observed Slicex state layout values used by the FLP adapter.
///
/// Keep uncertain and version-sensitive byte facts here so a revised
/// profile has one authoritative place. The current profile is a working
/// corpus profile; it is not claimed to describe every Slicex version.
pub mod slicex {
    #[derive(Debug, Clone, Copy)]
    pub struct Profile {
        pub state_prefix_len: usize,
        pub path_length_offset: usize,
        pub path_start: usize,
        pub settings_header_len: usize,
        pub slice_record_len: usize,
    }

    pub const OBSERVED: Profile = Profile {
        state_prefix_len: 0x61,
        path_length_offset: 0x60,
        path_start: 0x61,
        settings_header_len: 402,
        slice_record_len: 68,
    };
}
