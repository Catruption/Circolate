//! Slicex → JSON sidecar.

use serde::Serialize;

use super::trest::EnvelopePoint;

pub(super) struct SliceSpec {
    pub frame_start: u32,
    pub frame_end:   u32,
    pub key:         u32,
    pub pan:         u32,
    pub atten:       u32,
    pub cutoff:      u32,
}

pub(super) fn build_json(
    wav_rel: &str,
    slices: &[SliceSpec],
    amp_env:   Option<&[EnvelopePoint]>,
    speed_env: Option<&[EnvelopePoint]>,
    cut_env:   Option<&[EnvelopePoint]>,
    lfo_env:   Option<&[EnvelopePoint]>,
    env_scale_secs: f64,
    loop_flag: bool,
    speed_octaves: f64,
    amp_amt:   f64,
    speed_amt: f64,
    cut_amt:   f64,
    lfo_amt:   f64,
) -> Vec<u8> {
    let doc = SidecarJson {
        version: 1,
        wav: wav_rel,
        loop_: loop_flag,
        speed_octaves,
        slices: slices.iter().map(|s| SliceJson {
            s: s.frame_start, e: s.frame_end, k: s.key,
            pan: s.pan, atten: s.atten, cut: s.cutoff,
        }).collect(),
        envelopes: EnvelopesJson {
            amp:   amp_env  .map(|p| env_json(p, env_scale_secs, amp_amt)),
            speed: speed_env.map(|p| env_json(p, env_scale_secs, speed_amt)),
            cut:   cut_env  .map(|p| env_json(p, env_scale_secs, cut_amt)),
            lfo:   lfo_env  .map(|p| env_json(p, env_scale_secs, lfo_amt)),
        },
    };
    serde_json::to_vec_pretty(&doc).unwrap_or_default()
}

fn env_json(pts: &[EnvelopePoint], scale: f64, amount: f64) -> EnvJson {
    let mut t = 0.0f64;
    let mut points = Vec::with_capacity(pts.len());
    for (i, p) in pts.iter().enumerate() {
        t += p.dx;
        points.push(PointJson {
            t: t * scale,
            v: p.y,
            tension: if i == 0 { 0.0 } else { p.curve as f64 },
            kind:    if i == 0 { 0   } else { p.kind() },
        });
    }
    EnvJson { points, amount }
}

#[derive(Serialize)]
struct SidecarJson<'a> {
    version: u32,
    wav: &'a str,
    #[serde(rename = "loop")] loop_: bool,
    speed_octaves: f64,
    slices: Vec<SliceJson>,
    envelopes: EnvelopesJson,
}

#[derive(Serialize)]
struct SliceJson {
    s: u32, e: u32, k: u32, pan: u32, atten: u32, cut: u32,
}

#[derive(Serialize)]
struct EnvelopesJson {
    #[serde(skip_serializing_if = "Option::is_none")] amp:   Option<EnvJson>,
    #[serde(skip_serializing_if = "Option::is_none")] speed: Option<EnvJson>,
    #[serde(skip_serializing_if = "Option::is_none")] cut:   Option<EnvJson>,
    #[serde(skip_serializing_if = "Option::is_none")] lfo:   Option<EnvJson>,
}

#[derive(Serialize)]
struct EnvJson {
    points: Vec<PointJson>,
    amount: f64,
}

#[derive(Serialize)]
struct PointJson {
    t: f64, v: f64, tension: f64, kind: u8,
}
