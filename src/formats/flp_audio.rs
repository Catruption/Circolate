//! Extract embedded audio files from FLP plugin state blobs.
//!
//! DirectWave and some other FL-native samplers embed their sample data
//! inside the plugin state.  The outer container is undocumented, but the
//! audio inside is standard: RIFF/WAVE or FLAC.  We scan for the magic
//! bytes and slice out complete files.
//!
//! FLAC has no length field, so a FLAC stream runs to the next recognized
//! marker or to the end of the blob.  Candidates are validated (a FLAC must
//! start with a STREAMINFO block header) so stray `fLaC`/`RIFF` byte runs in
//! unrelated data are not treated as audio.  Trailing bytes after the real
//! stream are tolerated by `flac_to_wav`, which stops at the declared sample
//! count.

use anyhow::{bail, Context, Result};

pub struct EmbeddedAudio {
    pub data: Vec<u8>,
    pub extension: &'static str,
}

/// `fLaC` followed by a STREAMINFO metadata block header (type 0, length 34).
fn is_flac_header(blob: &[u8], i: usize) -> bool {
    i + 8 <= blob.len()
    && &blob[i..i + 4] == b"fLaC"
    && (blob[i + 4] & 0x7F) == 0
    && blob[i + 5] == 0
    && blob[i + 6] == 0
    && blob[i + 7] == 34
}

/// `RIFF` .. `WAVE` whose declared size fits in the blob.  Returns total length.
fn wav_len_at(blob: &[u8], i: usize) -> Option<usize> {
    if i + 12 > blob.len() || &blob[i..i + 4] != b"RIFF" || &blob[i + 8..i + 12] != b"WAVE" {
        return None;
    }
    let size = u32::from_le_bytes([blob[i + 4], blob[i + 5], blob[i + 6], blob[i + 7]]) as usize;
    let total = size.checked_add(8)?;
    if size >= 4 && i.checked_add(total)? <= blob.len() {
        Some(total)
    } else {
        None
    }
}

/// Find every complete RIFF/WAVE or FLAC file in a blob.
pub fn extract_all(blob: &[u8]) -> Vec<EmbeddedAudio> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 12 <= blob.len() {
        if let Some(total) = wav_len_at(blob, i) {
            out.push(EmbeddedAudio {
                data: blob[i..i + total].to_vec(),
                     extension: "wav",
            });
            i += total;
            continue;
        }
        if is_flac_header(blob, i) {
            let mut end = blob.len();
            let mut j = i + 4;
            while j + 4 <= blob.len() {
                if is_flac_header(blob, j) || wav_len_at(blob, j).is_some() {
                    end = j;
                    break;
                }
                j += 1;
            }
            out.push(EmbeddedAudio {
                data: blob[i..end].to_vec(),
                     extension: "flac",
            });
            i = end;
            continue;
        }
        i += 1;
    }
    out
}

/// Root key (MIDI note number) from a WAV's `smpl` chunk, if it has one.
///
/// Only present when the sample was authored with sampler metadata; many
/// plain WAVs have no such chunk, in which case this returns `None`.
pub fn wav_root_key(wav: &[u8]) -> Option<u8> {
    if wav.len() < 12 || &wav[0..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        return None;
    }
    let mut p = 12usize;
    while p + 8 <= wav.len() {
        let id = &wav[p..p + 4];
        let size = u32::from_le_bytes([wav[p + 4], wav[p + 5], wav[p + 6], wav[p + 7]]) as usize;
        let body = p + 8;
        if id == b"smpl" && size >= 16 && body + 16 <= wav.len() {
            // manufacturer, product, sample period, MIDI unity note
            let note = u32::from_le_bytes([
                wav[body + 12],
                wav[body + 13],
                wav[body + 14],
                wav[body + 15],
            ]);
            return if note <= 127 { Some(note as u8) } else { None };
        }
        p = body.checked_add(size)?.checked_add(size & 1)?;
    }
    None
}

/// Sample-frame offsets from a WAV's `cue ` chunk, sorted and deduplicated.
///
/// Slicex stores its slice markers here.  Empty if the WAV has no cue chunk.
pub fn wav_cue_points(wav: &[u8]) -> Vec<u32> {
    let mut out = Vec::new();
    if wav.len() < 12 || &wav[0..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        return out;
    }
    let mut p = 12usize;
    while p + 8 <= wav.len() {
        let id = &wav[p..p + 4];
        let size = u32::from_le_bytes([wav[p + 4], wav[p + 5], wav[p + 6], wav[p + 7]]) as usize;
        let body = p + 8;
        if id == b"cue " && body + 4 <= wav.len() {
            let n = u32::from_le_bytes([wav[body], wav[body + 1], wav[body + 2], wav[body + 3]])
            as usize;
            for k in 0..n {
                // CuePoint: id, position, fccChunk, chunkStart, blockStart, sampleOffset
                let o = body + 4 + k * 24;
                if o + 24 > wav.len() {
                    break;
                }
                out.push(u32::from_le_bytes([
                    wav[o + 20],
                    wav[o + 21],
                    wav[o + 22],
                    wav[o + 23],
                ]));
            }
        }
        p = match body.checked_add(size).and_then(|x| x.checked_add(size & 1)) {
            Some(x) => x,
            None => break,
        };
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Decode a FLAC stream to a WAV byte buffer.
///
/// The player only loads WAVs (`hound`), so FLAC samples must be converted.
/// Output is 16-bit PCM (samples deeper than 16 bits are shifted down,
/// shallower ones shifted up), or 8-bit PCM for 8-bit sources.
///
/// FLAC has no container length, so the slice handed in may carry trailing
/// non-audio bytes.  Decoding stops at the sample count declared in
/// STREAMINFO; if a later frame fails to decode after some audio was read,
/// the audio so far is kept.
pub fn flac_to_wav(flac: &[u8]) -> Result<Vec<u8>> {
    let mut reader = claxon::FlacReader::new(flac).context("decoding embedded FLAC")?;
    let info = reader.streaminfo();
    let channels = (info.channels as u16).max(1);
    let ch = channels as usize;
    let sample_rate = info.sample_rate;
    let bps = info.bits_per_sample;
    let declared: Option<usize> = info.samples.map(|n| n as usize * ch);

    let mut pcm: Vec<i32> = Vec::with_capacity(declared.unwrap_or(0).min(1 << 26));
    for sample in reader.samples() {
        match sample {
            Ok(v) => pcm.push(v),
            Err(e) => {
                if pcm.is_empty() {
                    bail!("decoding embedded FLAC samples: {e}");
                }
                break; // trailing junk after the real stream
            }
        }
        if Some(pcm.len()) == declared {
            break;
        }
    }
    pcm.truncate(pcm.len() / ch * ch);
    if pcm.is_empty() {
        bail!("embedded FLAC contains no samples");
    }

    let frames = pcm.len() / ch;
    let (out_bits, bytes_per_sample): (u16, usize) = if bps <= 8 { (8, 1) } else { (16, 2) };
    let shift = bps as i32 - 16; // >0: drop bits, <0: scale up

    let data_len = frames * ch * bytes_per_sample;
    let mut body: Vec<u8> = Vec::with_capacity(data_len + 64);
    body.extend_from_slice(b"WAVE");
    body.extend_from_slice(b"fmt ");
    body.extend_from_slice(&16u32.to_le_bytes());
    body.extend_from_slice(&1u16.to_le_bytes()); // PCM
    body.extend_from_slice(&channels.to_le_bytes());
    body.extend_from_slice(&sample_rate.to_le_bytes());
    body.extend_from_slice(&(sample_rate * bytes_per_sample as u32 * channels as u32).to_le_bytes());
    body.extend_from_slice(&((bytes_per_sample as u16) * channels).to_le_bytes());
    body.extend_from_slice(&out_bits.to_le_bytes());

    body.extend_from_slice(b"data");
    body.extend_from_slice(&(data_len as u32).to_le_bytes());

    if out_bits == 16 {
        for &v in &pcm {
            let s = if shift >= 0 { v >> shift } else { v << (-shift) };
            body.extend_from_slice(&(s.clamp(-32768, 32767) as i16).to_le_bytes());
        }
    } else {
        for &v in &pcm {
            body.push((v.clamp(-128, 127) + 128) as u8); // 8-bit WAV is unsigned
        }
    }
    if data_len & 1 == 1 {
        body.push(0);
    }

    let mut file: Vec<u8> = Vec::with_capacity(body.len() + 8);
    file.extend_from_slice(b"RIFF");
    file.extend_from_slice(&(body.len() as u32).to_le_bytes());
    file.extend_from_slice(&body);
    Ok(file)
}
