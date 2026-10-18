//! RIFF chunk parsing for the embedded WAV.
//!
//! We read every chunk Slicex is known to emit.  Anything we don't
//! recognize is still recorded so the debug dump can show it.
//!
//! ## Provenance
//!
//! SRC[loc=slot0.wav / slot1.wav (RIFF bytes inside the state) decomp=TODO conf=verified for cue, corpus for the rest]
//!
//! The embedded WAVs are written by the plugin (or FL) as ordinary RIFF.  Cue
//! positions (slice starts) are verified; what the other chunks mean is observed
//! only.  To find the code that writes or reads each chunk, search the DLL for the
//! four-character code as a 32-bit immediate (little-endian):
//!
//!   'RIFF' 0x46464952   'WAVE' 0x45564157   'fmt ' 0x20746D66   'data' 0x61746164
//!   'cue ' 0x20657563   'tlst' 0x74736C74   'strc' 0x63727473   'acid' 0x64696361
//!   'smpl' 0x6C706D73   'inst' 0x74736E69
//!
//! Cross-references to 'tlst' and 'strc' are the most useful: they are not
//! standard chunks, and the explicit per-slice key assignment (the thing
//! default_key in slicex.rs does not know) is probably in tlst.

#[derive(Debug, Clone, Copy)]
pub struct CuePoint {
    pub id: u32,
    pub position: u32,
    pub chunk: [u8; 4],
    pub chunk_start: u32,
    pub block_start: u32,
    pub sample_offset: u32,
}

#[derive(Debug, Clone, Copy)]
// SRC[loc=WAV chunk 'tlst' decomp=TODO conf=corpus]  entry layout and which byte is the key are unverified;
// the stride is also unverified (24 here, 16 is the minimum read).  Search the DLL for 0x74736C74.
pub struct TlstEntry {
    pub cue_id: u32,
    pub count: u32,
    pub key_byte: u8,
    pub raw_key_bytes: [u8; 4],
}

#[derive(Debug, Clone, Copy)]
pub struct SmplChunk {
    pub mfr: u32,
    pub product: u32,
    pub period: u32,
    pub unity_note: u32,
    pub smp_data: u32,
    pub smp_loops: u32,
    pub sampler_data: u32,
    pub num_loops: u32,
}

#[derive(Debug, Clone, Copy)]
// SRC[loc=WAV chunk 'acid' decomp=TODO conf=derived]  standard ACID chunk layout; Slicex's use of it is unverified.
pub struct AcidChunk {
    pub kind: u32,
    pub root_note: u16,
    pub unknown: u16,
    pub float1: f32,
    pub beats: u32,
    pub meter_den: u16,
    pub meter_num: u16,
    pub tempo: f32,
}

#[derive(Debug)]
pub struct UnknownChunk {
    pub offset: usize,
    pub id: [u8; 4],
    pub size: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct StrcRecord {
    pub a: u32,
    pub b: u32,
    pub c: u32,
    pub d: u32,
    pub e: u32,
    pub f: u32,
}

#[derive(Debug)]
// SRC[loc=WAV chunk 'strc' decomp=TODO conf=corpus]  header of 7 u32 then 24-byte records; meaning unknown.  Search the DLL for 0x63727473.
pub struct StrcTable {
    pub header: [u32; 7],
    pub count: u32,
    pub records: Vec<StrcRecord>,
}

impl StrcTable {
    pub fn parse(body: &[u8]) -> Option<Self> {
        if body.len() < 28 {
            return None;
        }
        let header = [
            u32::from_le_bytes([body[0], body[1], body[2], body[3]]),
            u32::from_le_bytes([body[4], body[5], body[6], body[7]]),
            u32::from_le_bytes([body[8], body[9], body[10], body[11]]),
            u32::from_le_bytes([body[12], body[13], body[14], body[15]]),
            u32::from_le_bytes([body[16], body[17], body[18], body[19]]),
            u32::from_le_bytes([body[20], body[21], body[22], body[23]]),
            u32::from_le_bytes([body[24], body[25], body[26], body[27]]),
        ];
        let count = header[1];
        let mut records = Vec::with_capacity(count.min(1024) as usize);
        for k in 0..count as usize {
            let o = 28 + k * 24;
            if o + 24 > body.len() { break; }
            records.push(StrcRecord {
                a: u32::from_le_bytes([body[o], body[o+1], body[o+2], body[o+3]]),
                         b: u32::from_le_bytes([body[o+4], body[o+5], body[o+6], body[o+7]]),
                         c: u32::from_le_bytes([body[o+8], body[o+9], body[o+10], body[o+11]]),
                         d: u32::from_le_bytes([body[o+12], body[o+13], body[o+14], body[o+15]]),
                         e: u32::from_le_bytes([body[o+16], body[o+17], body[o+18], body[o+19]]),
                         f: u32::from_le_bytes([body[o+20], body[o+21], body[o+22], body[o+23]]),
            });
        }
        Some(StrcTable { header, count, records })
    }
}

#[derive(Debug)]
pub struct WavChunks<'a> {
    pub raw: &'a [u8],
    pub fmt: Option<(u16, u16, u32, u32, u16, u16)>,
    pub data_size: u32,
    pub cues: Vec<CuePoint>,
    pub tlst: Vec<TlstEntry>,
    pub inst: Option<[u8; 7]>,
    pub smpl: Option<SmplChunk>,
    pub acid: Option<AcidChunk>,
    pub strc: Option<&'a [u8]>,
    pub info_list: Vec<(String, Vec<u8>)>,
    pub unknown: Vec<UnknownChunk>,
}

impl<'a> WavChunks<'a> {
    pub fn parse(wav: &'a [u8]) -> Option<Self> {
        if wav.len() < 12 || &wav[0..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
            return None;
        }
        let mut s = WavChunks {
            raw: wav,
            fmt: None,
            data_size: 0,
            cues: Vec::new(),
            tlst: Vec::new(),
            inst: None,
            smpl: None,
            acid: None,
            strc: None,
            info_list: Vec::new(),
            unknown: Vec::new(),
        };
        let mut p = 12usize;
        while p + 8 <= wav.len() {
            let id: [u8; 4] = wav[p..p + 4].try_into().ok()?;
            let size = u32::from_le_bytes(wav[p + 4..p + 8].try_into().ok()?) as usize;
            let body_off = p + 8;
            let body_end = (body_off + size).min(wav.len());
            let body = &wav[body_off..body_end];

            match &id {
                b"fmt " if body.len() >= 16 => {
                    s.fmt = Some((
                        u16::from_le_bytes([body[0], body[1]]),
                                  u16::from_le_bytes([body[2], body[3]]),
                                  u32::from_le_bytes([body[4], body[5], body[6], body[7]]),
                                  u32::from_le_bytes([body[8], body[9], body[10], body[11]]),
                                  u16::from_le_bytes([body[12], body[13]]),
                                  u16::from_le_bytes([body[14], body[15]]),
                    ));
                }
                b"data" => {
                    s.data_size = size as u32;
                }
                b"cue " if body.len() >= 4 => {
                    let n = u32::from_le_bytes([body[0], body[1], body[2], body[3]]) as usize;
                    for k in 0..n {
                        let o = 4 + k * 24;
                        if o + 24 > body.len() { break; }
                        s.cues.push(CuePoint {
                            id: u32::from_le_bytes([body[o], body[o+1], body[o+2], body[o+3]]),
                                    position: u32::from_le_bytes([body[o+4], body[o+5], body[o+6], body[o+7]]),
                                    chunk: [body[o+8], body[o+9], body[o+10], body[o+11]],
                                    chunk_start: u32::from_le_bytes([body[o+12], body[o+13], body[o+14], body[o+15]]),
                                    block_start: u32::from_le_bytes([body[o+16], body[o+17], body[o+18], body[o+19]]),
                                    sample_offset: u32::from_le_bytes([body[o+20], body[o+21], body[o+22], body[o+23]]),
                        });
                    }
                }
                b"tlst" if body.len() >= 4 => {
                    let n = u32::from_le_bytes([body[0], body[1], body[2], body[3]]) as usize;
                    for k in 0..n {
                        let o = 4 + k * 24;
                        if o + 16 > body.len() { break; }
                        if &body[o..o+4] != b"cue " { continue; }
                        s.tlst.push(TlstEntry {
                            cue_id: u32::from_le_bytes([body[o+4], body[o+5], body[o+6], body[o+7]]),
                                    count: u32::from_le_bytes([body[o+8], body[o+9], body[o+10], body[o+11]]),
                                    key_byte: body[o + 13],
                                    raw_key_bytes: [body[o+12], body[o+13], body[o+14], body[o+15]],
                        });
                    }
                }
                b"inst" if body.len() >= 7 => {
                    s.inst = Some([body[0], body[1], body[2], body[3], body[4], body[5], body[6]]);
                }
                b"smpl" if body.len() >= 32 => {
                    s.smpl = Some(SmplChunk {
                        mfr: u32::from_le_bytes([body[0], body[1], body[2], body[3]]),
                                  product: u32::from_le_bytes([body[4], body[5], body[6], body[7]]),
                                  period: u32::from_le_bytes([body[8], body[9], body[10], body[11]]),
                                  unity_note: u32::from_le_bytes([body[12], body[13], body[14], body[15]]),
                                  smp_data: u32::from_le_bytes([body[16], body[17], body[18], body[19]]),
                                  smp_loops: u32::from_le_bytes([body[20], body[21], body[22], body[23]]),
                                  sampler_data: u32::from_le_bytes([body[24], body[25], body[26], body[27]]),
                                  num_loops: u32::from_le_bytes([body[28], body[29], body[30], body[31]]),
                    });
                }
                b"acid" if body.len() >= 24 => {
                    s.acid = Some(AcidChunk {
                        kind: u32::from_le_bytes([body[0], body[1], body[2], body[3]]),
                                  root_note: u16::from_le_bytes([body[4], body[5]]),
                                  unknown: u16::from_le_bytes([body[6], body[7]]),
                                  float1: f32::from_le_bytes([body[8], body[9], body[10], body[11]]),
                                  beats: u32::from_le_bytes([body[12], body[13], body[14], body[15]]),
                                  meter_den: u16::from_le_bytes([body[16], body[17]]),
                                  meter_num: u16::from_le_bytes([body[18], body[19]]),
                                  tempo: f32::from_le_bytes([body[20], body[21], body[22], body[23]]),
                    });
                }
                b"strc" => {
                    s.strc = Some(body);
                }
                b"LIST" if body.len() >= 4 && &body[0..4] == b"INFO" => {
                    let mut q = 4usize;
                    while q + 8 <= body.len() {
                        let sid: [u8; 4] = [body[q], body[q+1], body[q+2], body[q+3]];
                        let sz = u32::from_le_bytes([body[q+4], body[q+5], body[q+6], body[q+7]]) as usize;
                        let stop = (q + 8 + sz).min(body.len());
                        s.info_list.push((String::from_utf8_lossy(&sid).to_string(), body[q+8..stop].to_vec()));
                        q = q + 8 + sz + (sz & 1);
                    }
                }
                _ => {
                    s.unknown.push(UnknownChunk { offset: p, id, size });
                }
            }

            p = body_off + size + (size & 1);
        }
        Some(s)
    }

    /// Parse the `strc` chunk into a full table, if present.
    pub fn strc_table(&self) -> Option<StrcTable> {
        self.strc.and_then(StrcTable::parse)
    }
}
