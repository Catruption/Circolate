//! Braille cell rasterisation.  A terminal cell is 2 wide × 4 tall in
//! braille dots; these helpers turn a set of lit dots into the glyph.

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
