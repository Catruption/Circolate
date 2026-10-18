// src/convert/flp/wav.rs
//! WAV inspection, decoding and sample-file lookup.

use anyhow::{Context, Result};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

pub(super) fn wav_secs(wav: &[u8]) -> Option<f64> {
    if wav.len() < 12 || &wav[0..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        return None;
    }
    let (mut rate, mut block, mut data_len) = (0u32, 0u32, None::<u32>);
    let mut p = 12usize;
    while p + 8 <= wav.len() {
        let id = &wav[p..p + 4];
        let size = u32::from_le_bytes([wav[p + 4], wav[p + 5], wav[p + 6], wav[p + 7]]);
        let body = p + 8;
        if id == b"fmt " && body + 16 <= wav.len() {
            rate = u32::from_le_bytes([wav[body + 4], wav[body + 5], wav[body + 6], wav[body + 7]]);
            block = u16::from_le_bytes([wav[body + 12], wav[body + 13]]) as u32;
        } else if id == b"data" {
            data_len = Some(size.min((wav.len() - body) as u32));
            break;
        }
        p = body + size as usize + (size as usize & 1);
    }
    let d = data_len?;
    if rate == 0 || block == 0 {
        return None;
    }
    Some(d as f64 / block as f64 / rate as f64)
}

/// Sample rate of a WAV, in Hz.  Returns `None` if the file has no fmt
/// chunk or is not a WAV at all.
pub(super) fn wav_sample_rate(wav: &[u8]) -> Option<u32> {
    if wav.len() < 12 || &wav[0..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        return None;
    }
    let mut p = 12usize;
    while p + 8 <= wav.len() {
        let id = &wav[p..p + 4];
        let size = u32::from_le_bytes([wav[p + 4], wav[p + 5], wav[p + 6], wav[p + 7]]) as usize;
        let body = p + 8;
        if id == b"fmt " && body + 8 <= wav.len() {
            return Some(u32::from_le_bytes([
                wav[body + 4],
                wav[body + 5],
                wav[body + 6],
                wav[body + 7],
            ]));
        }
        p = body + size + (size & 1);
    }
    None
}

/// True if hound can read this WAV without conversion.  Accepts PCM
/// (tag 1), IEEE float (tag 3), and WAVE_FORMAT_EXTENSIBLE (tag 0xFFFE)
/// at 8/16/24/32 bits.  Files that pass this check are returned to the
/// caller untouched; everything else goes through symphonia.
fn wav_is_hound_readable(wav: &[u8]) -> bool {
    if wav.len() < 12 || &wav[0..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        return false;
    }
    let mut p = 12usize;
    while p + 8 <= wav.len() {
        let id = &wav[p..p + 4];
        let size = u32::from_le_bytes([wav[p + 4], wav[p + 5], wav[p + 6], wav[p + 7]]) as usize;
        let body = p + 8;
        if id == b"fmt " && body + 16 <= wav.len() {
            let fmt_tag = u16::from_le_bytes([wav[body], wav[body + 1]]);
            let bits = u16::from_le_bytes([wav[body + 14], wav[body + 15]]);
            let is_pcm_like = fmt_tag == 1 || fmt_tag == 3 || fmt_tag == 0xFFFE;
            let is_bits = matches!(bits, 8 | 16 | 24 | 32);
            return is_pcm_like && is_bits;
        }
        p = body + size + (size & 1);
    }
    false
}

pub(super) fn decode_audio_to_wav(path: &Path) -> Result<Vec<u8>> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let raw = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;

    if wav_is_hound_readable(&raw) {
        return Ok(raw);
    }

    let cursor = std::io::Cursor::new(raw);
    let mss = MediaSourceStream::new(Box::new(cursor), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
    .format(
        &hint,
        mss,
        &FormatOptions::default(),
            &MetadataOptions::default(),
    )
    .with_context(|| format!("probing {}", path.display()))?;
    let mut format = probed.format;

    let track = format
    .tracks()
    .iter()
    .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
    .ok_or_else(|| anyhow::anyhow!("no audio track in {}", path.display()))?
    .clone();
    let track_id = track.id;

    let mut decoder = symphonia::default::get_codecs()
    .make(&track.codec_params, &DecoderOptions::default())
    .with_context(|| format!("creating decoder for {}", path.display()))?;

    let mut samples: Vec<i16> = Vec::new();
    let mut sample_buffer: Option<SampleBuffer<i16>> = None;
    let mut sample_rate: u32 = track.codec_params.sample_rate.unwrap_or(44100);
    let mut channels: u16 = track
    .codec_params
    .channels
    .map(|c| c.count())
    .unwrap_or(2) as u16;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(symphonia::core::errors::Error::IoError(ref e))
            if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) => {
                let spec = *decoded.spec();
                sample_rate = spec.rate;
                channels = spec.channels.count() as u16;
                if sample_buffer.is_none() {
                    sample_buffer = Some(SampleBuffer::<i16>::new(decoded.capacity() as u64, spec));
                }
                let buf = sample_buffer.as_mut().unwrap();
                buf.copy_interleaved_ref(decoded);
                samples.extend_from_slice(buf.samples());
            }
            Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
            Err(e) => return Err(e).with_context(|| format!("decoding {}", path.display())),
        }
    }

    let data_len = samples.len() * 2;
    let mut body: Vec<u8> = Vec::with_capacity(44 + data_len);
    body.extend_from_slice(b"WAVE");
    body.extend_from_slice(b"fmt ");
    body.extend_from_slice(&16u32.to_le_bytes());
    body.extend_from_slice(&1u16.to_le_bytes());
    body.extend_from_slice(&channels.to_le_bytes());
    body.extend_from_slice(&sample_rate.to_le_bytes());
    body.extend_from_slice(&(sample_rate * 2 * channels as u32).to_le_bytes());
    body.extend_from_slice(&(2 * channels).to_le_bytes());
    body.extend_from_slice(&16u16.to_le_bytes());
    body.extend_from_slice(b"data");
    body.extend_from_slice(&(data_len as u32).to_le_bytes());
    for s in &samples {
        body.extend_from_slice(&s.to_le_bytes());
    }

    let mut file: Vec<u8> = Vec::with_capacity(8 + body.len());
    file.extend_from_slice(b"RIFF");
    file.extend_from_slice(&(body.len() as u32).to_le_bytes());
    file.extend_from_slice(&body);
    Ok(file)
}

/// Locate a sample on disk.  The FLP stores absolute Windows paths that
/// won't exist here, so we try, in order:
///
///   1. explicit `--samples-dir`
///   2. the FLP's own directory
///   3. common subfolders next to the FLP (Audio, Samples, ...)
///   4. bounded recursive search under the FLP directory
///   5. the absolute path, if it happens to exist
pub(super) fn find_sample(
    sample_path: &str,
    samples_dir: Option<&Path>,
    flp_dir: Option<&Path>,
) -> Option<PathBuf> {
    if sample_path.is_empty() {
        return None;
    }
    let normalized = sample_path.replace('\\', "/");
    let p = Path::new(&normalized);
    let basename = p.file_name()?;

    if let Some(dir) = samples_dir {
        let c = dir.join(basename);
        if c.is_file() {
            return Some(c);
        }
    }

    if let Some(dir) = flp_dir {
        let c = dir.join(basename);
        if c.is_file() {
            return Some(c);
        }

        for sub in ["Audio", "Samples", "audio", "samples", "WAV", "wav"] {
            let c = dir.join(sub).join(basename);
            if c.is_file() {
                return Some(c);
            }
        }

        if let Some(found) = search_basename(dir, basename, 3) {
            return Some(found);
        }
    }

    if p.is_file() {
        return Some(p.to_path_buf());
    }
    None
}

/// Breadth-limited DFS: find a file by basename.  `depth` counts levels
/// below `dir`; a value of 3 covers `FLP_dir/Audio/Drums/foo.wav` but
/// stops well short of pathological trees.
fn search_basename(dir: &Path, basename: &OsStr, depth: usize) -> Option<PathBuf> {
    if depth == 0 {
        return None;
    }
    let entries = std::fs::read_dir(dir).ok()?;
    let mut subdirs: Vec<PathBuf> = Vec::new();
    for e in entries.flatten() {
        let path = e.path();
        match std::fs::symlink_metadata(&path) {
            Ok(md) if md.file_type().is_dir() => subdirs.push(path),
            Ok(md) if md.file_type().is_file() => {
                if path.file_name() == Some(basename) {
                    return Some(path);
                }
            }
            _ => {}
        }
    }
    for d in subdirs {
        if let Some(found) = search_basename(&d, basename, depth - 1) {
            return Some(found);
        }
    }
    None
}
// src/convert/flp/wav.rs
// Replace the `crop_wav` function you added with this one.
// (Do not add an extra `use anyhow::Context;` at the bottom, the file already has it.)

/// Physically crop a WAV file to the given start and end sample indices.
/// `start_sample` and `end_sample` are absolute frame indices (not bytes).
/// This slices the raw `data` chunk and patches the RIFF and `data` size fields.
pub fn crop_wav(bytes: &[u8], start_sample: u32, end_sample: u32) -> anyhow::Result<Vec<u8>> {
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Ok(bytes.to_vec()); // Not a valid WAV, return as-is
    }

    let reader = hound::WavReader::new(std::io::Cursor::new(bytes))?;
    let spec = reader.spec();
    let channels = spec.channels as usize;
    let bytes_per_sample = (spec.bits_per_sample / 8) as usize;
    let block_align = channels * bytes_per_sample;

    let start_byte = (start_sample as usize) * block_align;
    let end_byte = (end_sample as usize) * block_align;

    let mut data_offset = 0;
    let mut data_size = 0;
    let mut data_size_offset = 0;
    let mut p = 12;

    while p + 8 <= bytes.len() {
        let chunk_id = &bytes[p..p + 4];
        let chunk_size = u32::from_le_bytes(bytes[p + 4..p + 8].try_into().unwrap()) as usize;
        if chunk_id == b"data" {
            data_offset = p + 8;
            data_size = chunk_size;
            data_size_offset = p + 4;
            break;
        }
        p += 8 + chunk_size;
        if chunk_size % 2 != 0 {
            p += 1;
        }
    }

    if data_offset == 0 {
        return Ok(bytes.to_vec()); // No data chunk found
    }

    let actual_start = data_offset + start_byte;
    let actual_end = (data_offset + end_byte)
    .min(data_offset + data_size)
    .min(bytes.len());
    let new_data_size = actual_end.saturating_sub(actual_start);

    if new_data_size == 0 {
        return Ok(bytes.to_vec());
    }

    let mut out = Vec::with_capacity(actual_start + new_data_size);
    out.extend_from_slice(&bytes[..actual_start]);
    out.extend_from_slice(&bytes[actual_start..actual_end]);

    // Patch RIFF size (total file size minus 8 bytes) and data chunk size
    let out_len = (out.len() - 8) as u32;
    out[4..8].copy_from_slice(&out_len.to_le_bytes());
    out[data_size_offset..data_size_offset + 4]
    .copy_from_slice(&(new_data_size as u32).to_le_bytes());

    Ok(out)
}
