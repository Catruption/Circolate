//! IT flag bits, note terminators, order-list markers.

// ── note terminators ──────────────────────────────────────────────────

pub const ITNOTE_MPT_FADE: u8 = 0xFD;
pub const ITNOTE_FADE: u8 = 0xFD;
pub const ITNOTE_CUT: u8 = 0xFE;
pub const ITNOTE_KEY_OFF: u8 = 0xFF;

// ── order-list markers ────────────────────────────────────────────────

pub const ITORDER_SKIP: u8 = 0xFE;
pub const ITORDER_END: u8 = 0xFF;

// ── sample header flags (Sample.flg) ──────────────────────────────────

pub const ITSF_DATA_PRESENT: u8 = 0x01;
pub const ITSF_BIT_16: u8 = 0x02;
pub const ITSF_STEREO: u8 = 0x04;
pub const ITSF_COMPRESSED: u8 = 0x08;
pub const ITSF_LOOP: u8 = 0x10;
pub const ITSF_SUSTAIN_LOOP: u8 = 0x20;
pub const ITSF_PINGPONG_LOOP: u8 = 0x40;
pub const ITSF_PINGPONG_SUSTAIN: u8 = 0x80;

// ── sample conversion flags (Sample.cvt) ──────────────────────────────

pub const ITCVT_SIGNED: u8 = 0x01;
pub const ITCVT_BIG_ENDIAN: u8 = 0x02;
pub const ITCVT_DELTA: u8 = 0x04;
pub const ITCVT_PTM_8_TO_16: u8 = 0x08;

/// IT ADPCM marker in the cvt byte. Exact value, not a bit flag.
pub const ITCVT_ADPCM: u8 = 0xFF;

// ── module header flags (Module.flags) ────────────────────────────────

pub const ITFLAG_STEREO: u16 = 0x0001;
pub const ITFLAG_VOL0_OPT: u16 = 0x0002;
pub const ITFLAG_USE_INSTRUMENTS: u16 = 0x0004;
pub const ITFLAG_LINEAR_SLIDES: u16 = 0x0008;
pub const ITFLAG_OLD_EFFECTS: u16 = 0x0010;