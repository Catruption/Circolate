//! Per-application volume via PipeWire/PulseAudio.
//!
//! Uses `pactl --format=json list sink-inputs` to locate the sink-input
//! belonging to this process, then steps or sets its volume.  This is
//! the same object the desktop mixer (KDE's per-application tab, GNOME's
//! sound panel, pavucontrol) manipulates.
//!
//! Volume adjustments are sent as *relative* deltas (`+5%` / `-5%`)
//! rather than absolute values.  PipeWire performs the arithmetic on
//! its own authoritative current value, so there's no read-modify-write
//! race and no accumulation error from the round trip.

use std::process::Command;

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).output().ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        None
    }
}

fn is_ours(entry: &serde_json::Value, pid: &str) -> bool {
    let Some(props) = entry.get("properties") else { return false };
    let Some(id) = props.get("application.process.id") else { return false };
    if let Some(s) = id.as_str() {
        return s == pid;
    }
    if let Some(n) = id.as_u64() {
        return n.to_string() == pid;
    }
    false
}

fn is_playback(entry: &serde_json::Value) -> bool {
    entry
        .get("properties")
        .and_then(|p| p.get("media.class"))
        .and_then(|c| c.as_str())
        .map(|s| s == "Stream/Output/Audio")
        .unwrap_or(true)
}

fn find_entry() -> Option<serde_json::Value> {
    let pid = std::process::id().to_string();
    let dump = run("pactl", &["--format=json", "list", "sink-inputs"])?;
    let v: serde_json::Value = serde_json::from_str(&dump).ok()?;
    let entries = v.as_array()?;

    entries
        .iter()
        .find(|e| is_ours(e, &pid) && is_playback(e))
        .or_else(|| entries.iter().find(|e| is_ours(e, &pid)))
        .cloned()
}

fn find_index() -> Option<u32> {
    find_entry()?
        .get("index")
        .and_then(|i| i.as_u64())
        .map(|n| n as u32)
}

/// Adjust the volume by `delta`, a signed fraction (e.g. 0.05 = +5%).
/// Sent to pactl as a relative delta so the server does the arithmetic.
pub fn step(delta: f64) {
    let pct = (delta * 100.0).round() as i64;
    if pct == 0 {
        return;
    }
    let arg = if pct > 0 {
        format!("+{}%", pct)
    } else {
        format!("{}%", pct) // pct already includes the minus sign
    };
    if let Some(id) = find_index() {
        let _ = Command::new("pactl")
            .args(["set-sink-input-volume", &id.to_string(), &arg])
            .status();
        return;
    }
    // Stream not open yet — fall back to the default sink.
    let _ = Command::new("pactl")
        .args(["set-sink-volume", "@DEFAULT_SINK@", &arg])
        .status();
}

/// Set an absolute volume (0.0..=1.5).  Used only by the initial
/// startup path, if any; the key handler uses `step` instead.
pub fn set(volume: f64) {
    let vol = format!("{:.2}", volume.clamp(0.0, 1.5));
    if let Some(id) = find_index() {
        let _ = Command::new("pactl")
            .args(["set-sink-input-volume", &id.to_string(), &vol])
            .status();
        return;
    }
    let _ = Command::new("pactl")
        .args(["set-sink-volume", "@DEFAULT_SINK@", &vol])
        .status();
}

pub fn toggle_mute() {
    if let Some(id) = find_index() {
        let _ = Command::new("pactl")
            .args(["set-sink-input-mute", &id.to_string(), "toggle"])
            .status();
        return;
    }
    let _ = Command::new("pactl")
        .args(["set-sink-mute", "@DEFAULT_SINK@", "toggle"])
        .status();
}

pub fn get() -> Option<(f64, bool)> {
    let entry = find_entry()?;
    let vol_pct = entry
        .get("volume")
        .and_then(|v| v.as_object())
        .and_then(|obj| obj.values().next())
        .and_then(|ch| ch.get("value_percent"))
        .and_then(|s| s.as_str())
        .and_then(|s| s.trim_end_matches('%').parse::<f64>().ok())?;
    let muted = entry
        .get("mute")
        .and_then(|m| m.as_bool())
        .unwrap_or(false);
    Some((vol_pct / 100.0, muted))
}