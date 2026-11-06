//! SF2 RIFF chunk IDs, generator opcodes, record sizes.
//!
//! Reference: SF2.01 spec §6 (chunk layout), §7 (pdta records),
//! §8.1 (generator opcodes).

// ── RIFF container ────────────────────────────────────────────────────

pub const RIFF: [u8; 4] = *b"RIFF";
pub const SFBK: [u8; 4] = *b"sfbk";
pub const LIST: [u8; 4] = *b"LIST";
pub const SDTA: [u8; 4] = *b"sdta";
pub const PDTA: [u8; 4] = *b"pdta";

// ── sdta chunks ───────────────────────────────────────────────────────

pub const SMPL: [u8; 4] = *b"smpl";

// ── pdta chunks ───────────────────────────────────────────────────────

pub const SHDR: [u8; 4] = *b"shdr";
pub const INST: [u8; 4] = *b"inst";
pub const IBAG: [u8; 4] = *b"ibag";
pub const IGEN: [u8; 4] = *b"igen";
pub const PHDR: [u8; 4] = *b"phdr";
pub const PBAG: [u8; 4] = *b"pbag";
pub const PGEN: [u8; 4] = *b"pgen";

// ── record sizes ──────────────────────────────────────────────────────

pub const SHDR_RECORD_SIZE: usize = 46;
pub const INST_RECORD_SIZE: usize = 22;
pub const IBAG_RECORD_SIZE: usize = 4;
pub const IGEN_RECORD_SIZE: usize = 4;
pub const PHDR_RECORD_SIZE: usize = 38;
pub const PBAG_RECORD_SIZE: usize = 4;
pub const PGEN_RECORD_SIZE: usize = 4;

pub const TERMINAL_NAME: &str = "EOS";
pub const PRESET_TERMINAL_NAME: &str = "EOP";

// ── generator opcodes (§8.1.2) ────────────────────────────────────────
//
// Only the ones the parser actually reads are listed.

pub const GEN_START_ADDRS1: u16 = 0;      // startAddrsOffset
pub const GEN_END_ADDRS1: u16 = 1;        // endAddrsOffset
pub const GEN_STARTLOOP_ADDRS1: u16 = 2;  // startloopAddrsOffset
pub const GEN_ENDLOOP_ADDRS1: u16 = 3;    // endloopAddrsOffset
pub const GEN_START_ADDRS2: u16 = 4;      // startAddrsCoarseOffset
pub const GEN_INITIAL_FILTER_FC: u16 = 8;
pub const GEN_INITIAL_FILTER_Q: u16 = 9;
pub const GEN_END_ADDRS2: u16 = 12;       // endAddrsCoarseOffset
pub const GEN_DELAY_MOD_ENV: u16 = 33;
pub const GEN_ATTACK_MOD_ENV: u16 = 34;
pub const GEN_HOLD_MOD_ENV: u16 = 35;
pub const GEN_DECAY_MOD_ENV: u16 = 36;
pub const GEN_SUSTAIN_MOD_ENV: u16 = 37;
pub const GEN_RELEASE_MOD_ENV: u16 = 38;
pub const GEN_INSTRUMENT: u16 = 41;
pub const GEN_KEY_RANGE: u16 = 43;
pub const GEN_VEL_RANGE: u16 = 44;
pub const GEN_STARTLOOP_ADDRS2: u16 = 45; // startloopAddrsCoarseOffset
pub const GEN_INITIAL_ATTENUATION: u16 = 48;
pub const GEN_ENDLOOP_ADDRS2: u16 = 50;   // endloopAddrsCoarseOffset
pub const GEN_COARSE_TUNE: u16 = 51;
pub const GEN_FINE_TUNE: u16 = 52;
pub const GEN_SAMPLE_ID: u16 = 53;
pub const GEN_SAMPLE_MODES: u16 = 54;
pub const GEN_OVERRIDING_ROOT_KEY: u16 = 58;