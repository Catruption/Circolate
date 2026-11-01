//! Canonical uncompressed payload shared by directory and ZIP exports.

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use crate::archive::{Entry, EntryKind, Manifest, FORMAT_TAG, FORMAT_VERSION};
use crate::audio_decode::decode_audio;
use crate::convert::{AuxFile, Converted};

#[derive(Clone)]
pub struct BundleFile {
    pub relative_path: String,
    pub data: Vec<u8>,
    pub kind: EntryKind,
}

pub struct BundlePayload {
    pub files: Vec<BundleFile>,
    pub manifest: Manifest,
}

/// Build the exact same project payload for a directory or a ZIP archive.
/// The ZIP writer only changes the outer container; each payload file and its
/// manifest hash are otherwise identical.
pub fn build_payload(
    converted: &Converted,
    main_filename: &str,
    title: &str,
    sample_rate: u32,
    cover: Option<(&str, &[u8])>,
) -> Result<BundlePayload> {
    let mut files = Vec::new();
    files.push(BundleFile {
        relative_path: main_filename.to_string(),
        data: converted.halotext.as_bytes().to_vec(),
        kind: EntryKind::Halotext,
    });

    let library: serde_json::Value = if converted.samples_json.trim().is_empty() {
        serde_json::json!({
            "format": "halotext-sample-library",
            "version": 2,
            "samples": [],
            "instruments": []
        })
    } else {
        serde_json::from_str(&converted.samples_json).context("parsing samples.json")?
    };
    let sample_ids: std::collections::HashMap<String, u16> = library
        .get("samples")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|s| {
            let path = s.get("file")?.as_str()?.to_string();
            let id = s.get("id")?.as_u64()? as u16;
            Some((path, id))
        })
        .collect();

    for sample in &converted.samples {
        let decoded = decode_audio(&sample.wav, &sample.rel_path)?;
        files.push(BundleFile {
            relative_path: sample.rel_path.clone(),
            data: sample.wav.clone(),
            kind: EntryKind::Sample {
                sample_id: sample_ids.get(&sample.rel_path).copied().unwrap_or(0),
                encoding: "wav".into(),
                channels: decoded.channels_count() as u8,
                rate: decoded.rate as u32,
            },
        });
    }

    let library_bytes = if converted.samples_json.trim().is_empty() {
        serde_json::to_vec_pretty(&library)?
    } else {
        converted.samples_json.as_bytes().to_vec()
    };
    files.push(BundleFile {
        relative_path: "samples.json".into(),
        data: library_bytes,
        kind: EntryKind::SampleLibrary,
    });

    for aux in &converted.aux {
        if aux.rel_path.starts_with("source/") { continue; }
        let (kind, _) = classify_aux(aux);
        files.push(BundleFile {
            relative_path: aux.rel_path.clone(),
            data: aux.bytes.clone(),
            kind,
        });
    }
    if let Some((name, bytes)) = cover {
        files.push(BundleFile {
            relative_path: name.to_string(),
            data: bytes.to_vec(),
            kind: EntryKind::Cover,
        });
    }

    let requires = crate::deps::requires::build_from_aux(&converted.aux);
    if !requires.plugins.is_empty() || !requires.fallbacks.is_empty() {
        files.push(BundleFile {
            relative_path: crate::deps::requires::FILENAME.into(),
            data: serde_json::to_vec_pretty(&requires)?,
            kind: EntryKind::Requires,
        });
    }

    let entries = files.iter().map(|f| Entry {
        path: f.relative_path.clone(),
        size: f.data.len() as u64,
        sha256: sha256_hex(&f.data),
        kind: f.kind.clone(),
    }).collect();
    let manifest = Manifest {
        format: FORMAT_TAG.into(),
        version: FORMAT_VERSION,
        tool: format!("circolate {}", env!("CARGO_PKG_VERSION")),
        created: now_iso8601(),
        title: title.into(),
        entry: main_filename.into(),
        sample_format: "wav".into(),
        sample_rate,
        entries,
    };
    Ok(BundlePayload { files, manifest })
}

fn classify_aux(aux: &AuxFile) -> (EntryKind, bool) {
    let p = aux.rel_path.as_str();
    if p == crate::fidelity::FIDELITY_FILENAME {
        (EntryKind::Fidelity, false)
    } else if p.starts_with("instruments/") && p.ends_with(".instrument") {
        (EntryKind::Instrument, false)
    } else if p.starts_with("instruments/") && p.ends_with(".state") {
        (EntryKind::InstrumentState, false)
    } else {
        (EntryKind::Other, false)
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

fn now_iso8601() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, mo, d, rem / 3600, (rem / 60) % 60, rem % 60)
}
