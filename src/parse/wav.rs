//! Unified WAV/FLAC decoder.  Sniffs the format from magic bytes and
//! returns de-interleaved channel vectors at f32.

use anyhow::{bail, Context, Result};
use std::io::Cursor;

pub struct DecodedAudio {
    pub channels: Vec<Vec<f32>>,
    pub rate: f64,
    pub source_bits: u16,
}

impl DecodedAudio {
    pub fn frames(&self) -> usize {
        self.channels.first().map_or(0, |c| c.len())
    }
    pub fn channels_count(&self) -> usize {
        self.channels.len()
    }
}

pub fn decode_audio(bytes: &[u8], hint: &str) -> Result<DecodedAudio> {
    if bytes.len() < 12 {
        bail!("audio too short ({} bytes) for {}", bytes.len(), hint);
    }
    if &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE" {
        decode_wav(bytes).with_context(|| format!("decoding WAV {}", hint))
    } else if &bytes[0..4] == b"fLaC" {
        decode_flac(bytes).with_context(|| format!("decoding FLAC {}", hint))
    } else {
        bail!(
            "unrecognized audio magic for {} (first 4 bytes: {:02x?})",
            hint,
            &bytes[..4]
        );
    }
}

fn decode_wav(bytes: &[u8]) -> Result<DecodedAudio> {
    let mut reader = hound::WavReader::new(Cursor::new(bytes))?;
    let spec = reader.spec();
    let ch = spec.channels.max(1) as usize;
    let bits = spec.bits_per_sample;
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let max = (1i64 << (bits - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 / max))
                .collect::<Result<_, _>>()?
        }
        hound::SampleFormat::Float => {
            reader.samples::<f32>().collect::<Result<_, _>>()?
        }
    };
    let keep = ch.min(2);
    let mut chans = vec![Vec::with_capacity(interleaved.len() / ch); keep];
    for frame in interleaved.chunks_exact(ch) {
        for (c, out) in chans.iter_mut().enumerate() {
            out.push(frame[c]);
        }
    }
    Ok(DecodedAudio {
        channels: chans,
        rate: spec.sample_rate as f64,
        source_bits: bits,
    })
}

fn decode_flac(bytes: &[u8]) -> Result<DecodedAudio> {
    let mut reader = claxon::FlacReader::new(Cursor::new(bytes))?;
    let info = reader.streaminfo();
    let bits = info.bits_per_sample as u16;
    let ch = info.channels as usize;
    let rate = info.sample_rate as f64;
    let max = (1i64 << (bits - 1)) as f32;

    let mut chans: Vec<Vec<f32>> = (0..ch).map(|_| Vec::new()).collect();
    for (i, sample) in reader.samples().enumerate() {
        let s = sample?;
        chans[i % ch].push(s as f32 / max);
    }
    Ok(DecodedAudio {
        channels: chans,
        rate,
        source_bits: bits,
    })
}