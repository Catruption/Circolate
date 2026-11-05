//! 68-byte per-slice records.
//!
//! Every channel in the corpus has byte-identical records, so the field
//! layout is unknown.  We parse and expose the whole block so a channel
//! with a modified slice (retune, reverse, per-slice pan) can be
//! debugged by diffing.
//!
//! ## Provenance
//!
//! SRC[loc=slot0.records, count at slot0.params+0x4C+envelopes decomp=TODO conf=verified for size and position (slicex.rs)]
//!
//! Element size 0x44 and "count then elements" are verified (slicex.rs
//! Slot::slice_records).  The content is not decoded here, but see params.rs: slot
//! 1's single record is probably the same structure, and it is read there as 17 x
//! u32 (12800 = 100%, 128 = pan centre).  If that holds, all slot 0 records being
//! identical means "every slice at the defaults", and a retuned or panned slice
//! would show up as a varying column here.
//!
//! To check in the DLL: the slot loader loops count times over a 0x44-byte element;
//! the function that reads one element names the 17 fields.  FUN_008827d0 (see
//! params.rs) is the first candidate.  decomp=TODO.

use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct SliceRecords<'a> {
    pub raw: &'a [u8],
    pub slice_count: u32,
    pub record_size: usize,
}

impl<'a> SliceRecords<'a> {
    pub fn parse(raw: &'a [u8], slice_count: u32, record_size: usize) -> Self {
        SliceRecords { raw, slice_count, record_size }
    }

    pub fn rec(&self, i: usize) -> Option<&'a [u8]> {
        if i >= self.slice_count as usize {
            return None;
        }
        let o = i * self.record_size;
        self.raw.get(o..o + self.record_size)
    }

    /// Group records by content.  Returns (record_bytes, indices).
    pub fn unique(&self) -> Vec<(Vec<u8>, Vec<usize>)> {
        let mut groups: BTreeMap<Vec<u8>, Vec<usize>> = BTreeMap::new();
        for i in 0..self.slice_count as usize {
            if let Some(r) = self.rec(i) {
                groups.entry(r.to_vec()).or_default().push(i);
            }
        }
        let mut out: Vec<(Vec<u8>, Vec<usize>)> = groups.into_iter().collect();
        out.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
        out
    }

    /// Column offsets whose value varies across slices.
    pub fn varying_columns(&self) -> Vec<usize> {
        let n = self.slice_count as usize;
        if n == 0 {
            return Vec::new();
        }
        let mut varying = Vec::new();
        for c in 0..self.record_size {
            let mut first: Option<u8> = None;
            let mut differs = false;
            for i in 0..n {
                let Some(r) = self.rec(i) else { continue };
                match first {
                    None => first = Some(r[c]),
                    Some(f) if f != r[c] => {
                        differs = true;
                        break;
                    }
                    _ => {}
                }
            }
            if differs {
                varying.push(c);
            }
        }
        varying
    }
}
