//! YM2612 DAC (PCM) trigger extraction from the raw VGM command stream.
//!
//! The converter's note analysis never sees the DAC: it is a stream of
//! `0x8n` "write byte from the PCM bank, then wait n samples" commands, with
//! `0xE0` seeking the bank pointer to the start of each sample. This module
//! turns that into discrete hits (start time, bank offset, length, rate) so
//! they can be reported now and turned into sample parts later.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use super::util::rd32;
use super::{DAC_GAP, VGM_RATE};

pub(crate) struct DacHit {
    /// VGM sample of the first byte.
    pub(crate) start: u32,
    /// VGM sample after the last byte's wait.
    pub(crate) end: u32,
    /// PCM bank offset of the first byte.
    pub(crate) offset: usize,
    /// Number of bytes played.
    pub(crate) writes: u32,
    /// Samples between bytes (median wait); playback rate = 44100 / step.
    pub(crate) step: u32,
}

pub(crate) struct DacScan {
    /// Concatenated YM2612 PCM data blocks (type 0x00), unsigned 8-bit.
    pub(crate) bank: Vec<u8>,
    pub(crate) hits: Vec<DacHit>,
}

struct Cur {
    start: u32,
    last_end: u32,
    offset: usize,
    writes: u32,
    waits: Vec<u32>,
}

fn finish(c: Cur, hits: &mut Vec<DacHit>) {
    let mut w = c.waits;
    w.sort_unstable();
    let step = w.get(w.len() / 2).copied().unwrap_or(0);
    hits.push(DacHit {
        start: c.start,
        end: c.last_end,
        offset: c.offset,
        writes: c.writes,
        step,
    });
}

pub(crate) fn extract(bytes: &[u8]) -> Option<DacScan> {
    if bytes.len() < 0x40 || &bytes[0..4] != b"Vgm " {
        return None;
    }
    let len = bytes.len();
    let version = rd32(bytes, 0x08);
    let data_start = if version >= 0x150 {
        let rel = rd32(bytes, 0x34);
        if rel == 0 { 0x40 } else { 0x34usize.saturating_add(rel as usize) }
    } else {
        0x40
    };
    if data_start >= len {
        return None;
    }

    let mut bank: Vec<u8> = Vec::new();
    let mut ptr: usize = 0;
    let mut t: u32 = 0;
    let mut p = data_start;
    let mut hits: Vec<DacHit> = Vec::new();
    let mut cur: Option<Cur> = None;
    let mut seeked = false;

    while p < len {
        let c = bytes[p];
        match c {
            0x61 => {
                let n = (bytes.get(p + 1).copied().unwrap_or(0) as u32)
                    | ((bytes.get(p + 2).copied().unwrap_or(0) as u32) << 8);
                t = t.saturating_add(n);
                p += 3;
            }
            0x62 => { t = t.saturating_add(735); p += 1; }
            0x63 => { t = t.saturating_add(882); p += 1; }
            0x66 => break,
            0x67 => {
                if p + 7 > len { break; }
                let tt = bytes[p + 2];
                let size = rd32(bytes, p + 3) as usize;
                let start = p + 7;
                let end = start.saturating_add(size).min(len);
                if tt == 0x00 {
                    bank.extend_from_slice(&bytes[start..end]);
                }
                p = start.saturating_add(size);
            }
            0x70..=0x7F => { t = t.saturating_add((c & 0x0F) as u32 + 1); p += 1; }
            0x80..=0x8F => {
                let wait = (c & 0x0F) as u32;
                let new_hit = match &cur {
                    None => true,
                    Some(h) => seeked || t.saturating_sub(h.last_end) > DAC_GAP,
                };
                if new_hit {
                    if let Some(h) = cur.take() {
                        finish(h, &mut hits);
                    }
                    cur = Some(Cur { start: t, last_end: t, offset: ptr, writes: 0, waits: Vec::new() });
                }
                seeked = false;
                if let Some(h) = cur.as_mut() {
                    h.writes += 1;
                    h.waits.push(wait);
                    h.last_end = t.saturating_add(wait);
                }
                ptr += 1;
                t = t.saturating_add(wait);
                p += 1;
            }
            0xE0 => {
                ptr = rd32(bytes, p + 1) as usize;
                seeked = true;
                p += 5;
            }
            0x30..=0x3F => p += 2,
            0x40..=0x4E => p += 3,
            0x4F => p += 2,
            0x50 => p += 2,
            0x51..=0x5F => p += 3,
            0x64 => p += 4,
            0x68 => p += 12,
            0x90 | 0x91 | 0x95 => p += 5,
            0x92 => p += 6,
            0x93 => p += 11,
            0x94 => p += 2,
            0xA0..=0xBF => p += 3,
            0xC0..=0xDF => p += 4,
            0xE1..=0xFF => p += 1,   // reserved opcodes: 1 byte each per VGM spec
            _ => p += 1,
        }
    }
    if let Some(h) = cur.take() {
        finish(h, &mut hits);
    }
    Some(DacScan { bank, hits })
}

pub(crate) fn report(bytes: &[u8], total_samples: u32) -> String {
    let mut r = String::new();
    let _ = writeln!(r, "\nDAC (PCM) triggers");
    let Some(scan) = extract(bytes) else {
        let _ = writeln!(r, "  could not scan");
        return r;
    };
    if scan.hits.is_empty() {
        let _ = writeln!(r, "  none");
        return r;
    }

    let writes: u64 = scan.hits.iter().map(|h| h.writes as u64).sum();
    let active: u64 = scan.hits.iter().map(|h| h.end.saturating_sub(h.start) as u64).sum();
    let _ = writeln!(r, "  {} triggers, {} bytes played, PCM bank {} bytes",
        scan.hits.len(), writes, scan.bank.len());
    let _ = writeln!(r, "  DAC sounding for {:.1}s of {:.1}s ({:.0}%)",
        active as f64 / VGM_RATE, total_samples as f64 / VGM_RATE,
        100.0 * active as f64 / total_samples.max(1) as f64);

    let mut steps: BTreeMap<u32, u32> = BTreeMap::new();
    for h in &scan.hits {
        *steps.entry(h.step).or_insert(0) += 1;
    }
    let _ = write!(r, "  playback rates:");
    for (step, n) in &steps {
        if *step == 0 {
            let _ = write!(r, " [no-wait x{}]", n);
        } else {
            let _ = write!(r, " {:.0} Hz x{}", VGM_RATE / *step as f64, n);
        }
    }
    let _ = writeln!(r);

    // Distinct samples = distinct (bank offset, length).
    let mut distinct: BTreeMap<(usize, u32), (u32, u32, u32)> = BTreeMap::new();
    for h in &scan.hits {
        let e = distinct.entry((h.offset, h.writes)).or_insert((0, h.step, h.start));
        e.0 += 1;
    }
    let _ = writeln!(r, "  {} distinct samples (by bank offset + length)", distinct.len());
    let mut rows: Vec<_> = distinct.iter().collect();
    rows.sort_by(|a, b| (b.1 .0 as u64 * b.0 .1 as u64).cmp(&(a.1 .0 as u64 * a.0 .1 as u64)));
    let _ = writeln!(r, "    {:>9} {:>7} {:>9} {:>9} {:>9}", "offset", "bytes", "ms", "triggers", "first at");
    for ((off, n), (count, step, first)) in rows.into_iter().take(16) {
        let ms = *n as f64 * (*step).max(1) as f64 * 1000.0 / VGM_RATE;
        let _ = writeln!(r, "    {:>9} {:>7} {:>9.0} {:>9} {:>8.2}s",
            off, n, ms, count, *first as f64 / VGM_RATE);
    }
    if let Some(longest) = scan.hits.iter().max_by_key(|h| h.end.saturating_sub(h.start)) {
        let _ = writeln!(r, "  longest single trigger: {:.2}s at {:.2}s",
            longest.end.saturating_sub(longest.start) as f64 / VGM_RATE,
            longest.start as f64 / VGM_RATE);
    }
    r
}
