// src/convert/it/wav.rs
//! WAV encoding and samples.json.

use anyhow::Result;
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::formats::it::{Module, Sample};

pub(super) fn write_wav(s: &Sample) -> Vec<u8> {
    let rate: u32 = (if s.c5 == 0 { 8363 } else { s.c5 }).clamp(100, 192_000);

    let (data, bits): (Vec<u8>, u16) = if s.is16 {
        (
            s.pcm
                .iter()
                .flat_map(|&x| (x.clamp(i16::MIN as i32, i16::MAX as i32) as i16).to_le_bytes())
                .collect(),
            16,
        )
    } else {
        (
            s.pcm
                .iter()
                .map(|&x| ((x + 128).clamp(0, 255)) as u8)
                .collect(),
            8,
        )
    };

    let block = bits / 8;

    let mut body: Vec<u8> = Vec::new();
    body.extend_from_slice(b"WAVE");

    body.extend_from_slice(b"fmt ");
    body.extend_from_slice(&16u32.to_le_bytes());
    body.extend_from_slice(&1u16.to_le_bytes());
    body.extend_from_slice(&1u16.to_le_bytes());
    body.extend_from_slice(&rate.to_le_bytes());
    body.extend_from_slice(&(rate * block as u32).to_le_bytes());
    body.extend_from_slice(&block.to_le_bytes());
    body.extend_from_slice(&bits.to_le_bytes());

    body.extend_from_slice(b"data");
    body.extend_from_slice(&(data.len() as u32).to_le_bytes());
    body.extend_from_slice(&data);
    if data.len() & 1 == 1 {
        body.push(0);
    }

    if s.r#loop && s.loop_end > s.loop_beg {
        let mut smpl: Vec<u8> = Vec::new();
        for v in [
            0u32,
            0,
            (1e9 / rate as f64) as u32,
            60,
            0,
            0,
            0,
            1,
            0,
        ] {
            smpl.extend_from_slice(&v.to_le_bytes());
        }
        for v in [
            0u32,
            if s.pingpong { 1 } else { 0 },
            s.loop_beg as u32,
            (s.loop_end - 1) as u32,
            0,
            0,
        ] {
            smpl.extend_from_slice(&v.to_le_bytes());
        }

        body.extend_from_slice(b"smpl");
        body.extend_from_slice(&(smpl.len() as u32).to_le_bytes());
        body.extend_from_slice(&smpl);
    }

    let mut file: Vec<u8> = Vec::with_capacity(body.len() + 8);
    file.extend_from_slice(b"RIFF");
    file.extend_from_slice(&(body.len() as u32).to_le_bytes());
    file.extend_from_slice(&body);

    file
}

pub(super) fn build_samples_json(
    module: &Module,
    sample_files: &HashMap<u32, String>,
) -> Result<String> {
    let mut samples: Vec<Value> = Vec::new();

    for (i, s) in module.samples.iter().enumerate() {
        let idx = i as u32 + 1;
        let mut entry = serde_json::Map::new();
        entry.insert("id".into(), json!(idx));
        entry.insert("name".into(), json!(s.name));
        entry.insert("length".into(), json!(s.length));
        if let Some(f) = sample_files.get(&idx) {
            entry.insert("file".into(), json!(f));
        }
        entry.insert("c5Speed".into(), json!(if s.c5 == 0 { 8363 } else { s.c5 }));

        let mut loops = serde_json::Map::new();
        if s.r#loop && s.loop_end > s.loop_beg {
            loops.insert(
                "main".into(),
                json!({ "start": s.loop_beg, "end": s.loop_end, "pingpong": s.pingpong }),
            );
        }
        if s.sus_loop && s.sus_end > s.sus_beg {
            loops.insert(
                "sustain".into(),
                json!({ "start": s.sus_beg, "end": s.sus_end, "pingpong": false }),
            );
        }
        if !loops.is_empty() {
            entry.insert("loops".into(), Value::Object(loops));
        }

        entry.insert(
            "volume".into(),
            json!({
                "default": s.vol as u32 * 4,
                "global": s.gvl,
                "panning": (s.dfp & 0x7F) as u32 * 4,
            }),
        );
        entry.insert(
            "source_it".into(),
            json!({
                "global_volume": s.gvl,
                "default_volume": s.vol,
                "flags": s.flg,
                "conversion_flags": s.cvt,
                "default_pan": s.dfp,
                "length_frames": s.length,
                "loop_start": s.loop_beg,
                "loop_end": s.loop_end,
                "c5_speed": s.c5,
                "sustain_loop_start": s.sus_beg,
                "sustain_loop_end": s.sus_end,
                "has_data": s.has_data,
                "is_16_bit": s.is16,
                "stereo": s.stereo,
                "compressed": s.compressed,
                "loop": s.r#loop,
                "pingpong_loop": s.pingpong,
                "sustain_loop": s.sus_loop,
                "pingpong_sustain": s.pingpong_sus,
            }),
        );
        samples.push(Value::Object(entry));
    }

    let mut instruments: Vec<Value> = Vec::new();

    for (i, ins) in module.instruments.iter().enumerate() {
        let mut envelopes = serde_json::Map::new();
        for name in ["volume", "pan", "pitch"] {
            if let Some(env) = ins.envelopes.get(name) {
                envelopes.insert(
                    name.into(),
                    json!({
                        "enabled": env.enabled,
                        "loop_enabled": env.loop_enabled,
                        "sustain_enabled": env.sustain_enabled,
                        "loop_start_index": env.loop_start,
                        "loop_end_index": env.loop_end,
                        "sustain_start_index": env.sustain_start,
                        "sustain_end_index": env.sustain_end,
                        "points": env.points.iter()
                            .map(|&(tick, value)| json!({ "tick": tick, "value": value }))
                            .collect::<Vec<_>>(),
                    }),
                );
            }
        }
        instruments.push(json!({
            "id": i + 1,
            "name": ins.name,
            "new_note_action": ins.nna,
            "duplicate_check_type": ins.dct,
            "duplicate_check_action": ins.dca,
            "fadeout": ins.fadeout,
            "global_volume": ins.gbv,
            "default_pan": ins.dfp,
            "keymap": ins.keymap.iter()
                .map(|&(note, sample)| json!({ "note": note, "sample": sample }))
                .collect::<Vec<_>>(),
            "envelopes": envelopes,
        }));
    }

    let out = json!({
        "format": "trackscript-sample-library",
        "version": 2,
        "samples": samples,
        "instruments": instruments,
    });

    Ok(serde_json::to_string_pretty(&out)?)
}