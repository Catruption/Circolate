//! Braille cell rasterisation.  A terminal cell is 2 wide × 4 tall in
//! braille dots; these helpers turn a set of lit dots into the glyph.

use std::sync::OnceLock;

static BRAILLE_STRINGS: OnceLock<[Box<str>; 256]> = OnceLock::new();

/// Bit value for the dot at (lx, ly), where lx ∈ {0,1}, ly ∈ {0,1,2,3}.
#[inline]
pub fn dot_bit(lx: usize, ly: usize) -> u8 {
    match (lx, ly) {
        (0, 0) => 0x01,
        (0, 1) => 0x02,
        (0, 2) => 0x04,
        (1, 0) => 0x08,
        (1, 1) => 0x10,
        (1, 2) => 0x20,
        (0, 3) => 0x40,
        (1, 3) => 0x80,
        _ => 0,
    }
}

pub fn braille_char(bits: u8) -> char {
    if bits == 0 {
        ' '
    } else {
        char::from_u32(0x2800 + bits as u32).unwrap_or(' ')
    }
}

/// Cached UTF-8 glyphs for ratatui spans. Returning static strings avoids a
/// one-character heap allocation for every waveform cell on every redraw.
pub fn braille_str(bits: u8) -> &'static str {
    if bits == 0 { return " "; }
    BRAILLE_STRINGS.get_or_init(|| std::array::from_fn(|i| {
        char::from_u32(0x2800 + i as u32).unwrap_or(' ').to_string().into_boxed_str()
    }))[bits as usize].as_ref()
}
