//! Timestamped YM2612 DAC writes extracted from a VGM stream.

use crate::hosting::chip::engine::core::ChipCore;

/// One `0xE0`/`0x8X` sample run. `write_times` stores the elapsed VGM
/// 44.1 kHz samples at which each byte is written to YM2612 register 0x2A.
#[derive(Clone, Debug)]
pub struct PcmHit {
    pub t: u32,
    pub pcm_start: u32,
    pub pcm_len: u32,
    pub wait_total: u32,
    pub pan: u8,
    pub write_times: Vec<u32>,
}

#[derive(Clone, Debug, Default)]
pub struct DacTrack {
    pub pcm: Vec<u8>,
    pub hits: Vec<PcmHit>,
}

impl DacTrack {
    /// Compact versioned encoding used by any callers that persist extracted
    /// DAC data. Version 2 preserves the individual write timestamps.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(12 + self.hits.len() * 24 + self.pcm.len());
        out.extend_from_slice(b"DAC2");
        out.extend_from_slice(&(self.hits.len() as u32).to_le_bytes());
        for h in &self.hits {
            out.extend_from_slice(&h.t.to_le_bytes());
            out.extend_from_slice(&h.pcm_start.to_le_bytes());
            out.extend_from_slice(&h.pcm_len.to_le_bytes());
            out.extend_from_slice(&h.wait_total.to_le_bytes());
            out.push(h.pan);
            out.extend_from_slice(&(h.write_times.len() as u32).to_le_bytes());
            for time in &h.write_times { out.extend_from_slice(&time.to_le_bytes()); }
        }
        out.extend_from_slice(&(self.pcm.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.pcm);
        out
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 12 || &bytes[..4] != b"DAC2" { return None; }
        let mut p = 4usize;
        let n = read_u32(bytes, &mut p)? as usize;
        let mut hits = Vec::with_capacity(n);
        for _ in 0..n {
            let t = read_u32(bytes, &mut p)?;
            let pcm_start = read_u32(bytes, &mut p)?;
            let pcm_len = read_u32(bytes, &mut p)?;
            let wait_total = read_u32(bytes, &mut p)?;
            let pan = *bytes.get(p)?;
            p += 1;
            let count = read_u32(bytes, &mut p)? as usize;
            if count > (bytes.len().saturating_sub(p) / 4) { return None; }
            let mut write_times = Vec::with_capacity(count);
            for _ in 0..count { write_times.push(read_u32(bytes, &mut p)?); }
            hits.push(PcmHit { t, pcm_start, pcm_len, wait_total, pan, write_times });
        }
        let len = read_u32(bytes, &mut p)? as usize;
        if len > bytes.len().saturating_sub(p) { return None; }
        Some(Self { pcm: bytes[p..p + len].to_vec(), hits })
    }
}

fn read_u32(bytes: &[u8], p: &mut usize) -> Option<u32> {
    let end = p.checked_add(4)?;
    let value = u32::from_le_bytes(bytes.get(*p..end)?.try_into().ok()?);
    *p = end;
    Some(value)
}

/// Parse VGM data blocks and DAC stream commands, retaining each sample's
/// original timing instead of reconstructing an average playback rate.
pub fn extract_dac_track(vgm: &[u8]) -> Option<DacTrack> {
    if vgm.len() < 0x40 || &vgm[0..4] != b"Vgm " { return None; }
    let version = u32::from_le_bytes(vgm[0x08..0x0C].try_into().ok()?);
    let start = if version >= 0x150 {
        let off = u32::from_le_bytes(vgm[0x34..0x38].try_into().ok()?) as usize;
        if off != 0 { 0x34usize.saturating_add(off) } else { 0x40 }
    } else { 0x40 };
    let mut pcm = Vec::new();
    let mut hits = Vec::new();
    let mut cur: Option<(u32, u32)> = None;
    let mut write_times = Vec::new();
    let mut wait_total = 0u32;
    let mut pan = 0xC0;
    let mut t = 0u32;
    let mut i = start;
    while i < vgm.len() {
        let c = vgm[i];
        match c {
            0x50 => i = i.saturating_add(2),
            0x52 | 0x53 => {
                if i + 3 > vgm.len() { break; }
                if c == 0x53 && vgm[i + 1] == 0xB6 { pan = vgm[i + 2]; }
                i += 3;
            }
            0x61 => {
                if i + 3 > vgm.len() { break; }
                t = t.saturating_add(u16::from_le_bytes([vgm[i+1], vgm[i+2]]) as u32); i += 3;
            }
            0x62 => { t = t.saturating_add(735); i += 1; }
            0x63 => { t = t.saturating_add(882); i += 1; }
            0x66 => break,
            0x67 => {
                if i + 7 > vgm.len() { break; }
                let size = u32::from_le_bytes(vgm[i+3..i+7].try_into().ok()?) as usize & 0x7FFF_FFFF;
                let end = i.checked_add(7)?.checked_add(size)?;
                if end > vgm.len() { break; }
                if vgm[i+2] == 0 { pcm.extend_from_slice(&vgm[i+7..end]); }
                i = end;
            }
            // Data stream setup/control commands have fixed lengths and may
            // occur in the same VGM before the 0xE0/0x8X DAC sequence.
            0x90 | 0x91 => i = i.saturating_add(5),
            0x92 => i = i.saturating_add(6),
            0x93 => i = i.saturating_add(11),
            0x94 => i = i.saturating_add(2),
            0x95 => i = i.saturating_add(5),
            0x70..=0x7F => { t = t.saturating_add((c & 0xF) as u32 + 1); i += 1; }
            0x80..=0x8F => {
                if let Some((st, sp)) = cur {
                    let offset = t.saturating_sub(st);
                    write_times.push(offset);
                    wait_total = wait_total.saturating_add((c & 0xF) as u32);
                    let _ = sp;
                }
                // The low nibble is the delay after this DAC write.
                t = t.saturating_add((c & 0xF) as u32);
                i += 1;
            }
            0xE0 => {
                if i + 5 > vgm.len() { break; }
                if let Some((st, sp)) = cur.take() {
                    if !write_times.is_empty() {
                        hits.push(PcmHit { t: st, pcm_start: sp, pcm_len: write_times.len() as u32,
                            wait_total, pan, write_times: std::mem::take(&mut write_times) });
                    }
                }
                let ptr = u32::from_le_bytes(vgm[i+1..i+5].try_into().ok()?);
                cur = Some((t, ptr));
                wait_total = 0;
                i += 5;
            }
            _ => i += 1,
        }
    }
    if let Some((st, sp)) = cur {
        if !write_times.is_empty() {
            hits.push(PcmHit { t: st, pcm_start: sp, pcm_len: write_times.len() as u32,
                wait_total, pan, write_times });
        }
    }
    if pcm.is_empty() || hits.is_empty() { return None; }
    Some(DacTrack { pcm, hits })
}

/// Maps note triggers onto extracted VGM DAC runs and schedules their actual
/// register writes on the chip clock. No PCM mixing or resampling occurs here.
pub struct DacState { track: DacTrack, host_sr: f64 }

impl DacState {
    pub fn new(track: DacTrack, host_sr: u32) -> Self {
        Self { track, host_sr: host_sr.max(1) as f64 }
    }

    pub fn max_event_count(&self) -> usize {
        self.track.hits.iter().map(|h| h.write_times.len().saturating_add(2)).max().unwrap_or(0)
    }

    pub fn schedule_note(&self, start_sample: i64, engine: &mut dyn ChipCore) {
        let target = (start_sample.max(0) as f64 / self.host_sr * 44100.0) as u32;
        let Some(hit) = self.track.hits.iter().min_by_key(|h| h.t.abs_diff(target)) else { return };
        if hit.t.abs_diff(target) > 4410 || hit.write_times.is_empty() { return; }
        let start_t = start_sample as f64 / self.host_sr;
        let first = hit.pcm_start as usize;
        let len = hit.write_times.len().min(hit.pcm_len as usize)
            .min(self.track.pcm.len().saturating_sub(first));
        if len == 0 { return; }
        // DAC is monophonic: a retrigger interrupts the previous sample, so
        // discard its not-yet-played writes before queuing this run.
        engine.cancel_register_writes_from(start_t);
        engine.schedule_register_write(start_t, 0, 0x2B, 0x80);
        engine.schedule_register_write(start_t, 1, 0xB6, hit.pan);
        for (idx, &relative) in hit.write_times.iter().take(len).enumerate() {
            let sample = self.track.pcm[first + idx];
            let t = start_t + relative as f64 / 44100.0;
            engine.schedule_register_write(t, 0, 0x2A, sample);
        }
    }
}
