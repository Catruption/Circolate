//! Small numeric helpers shared by the VGM analysis modules.

pub(crate) const C5_HZ: f64 = 523.251_130_6;

pub(crate) fn rd32(b: &[u8], o: usize) -> u32 {
    if o + 4 <= b.len() {
        u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
    } else { 0 }
}

pub(crate) fn hz_to_midi(hz: f64) -> Option<i32> {
    if !(hz > 0.0) { return None; }
    Some(((60.0 + 12.0 * (hz / C5_HZ).log2()).round() as i32).clamp(0, 127))
}

pub(crate) fn ym_midi(fnum: u16, block: u8, clock: u32) -> Option<i32> {
    if fnum == 0 || clock == 0 { return None; }
    let hz = fnum as f64 * (clock as f64 / 144.0) / 2f64.powi(21 - block as i32);
    hz_to_midi(hz)
}

pub(crate) fn psg_percent(vol: u8) -> f64 {
    if vol >= 15 { 0.0 } else { 100.0 * 10f64.powf(-0.1 * vol as f64) }
}