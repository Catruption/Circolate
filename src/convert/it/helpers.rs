// src/convert/it/helpers.rs
//! Constants and small utilities shared by the IT submodules.

pub(super) const ROWS_PER_BEAT: u32 = 4;
pub(super) const RPB: f64 = ROWS_PER_BEAT as f64;
pub(super) const MEASURE_ROWS: f64 = RPB * 4.0;

pub(super) const C5_HZ: f64 = 523.2511306;
pub(super) const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];
pub(super) const G_TABLE: [i32; 10] = [0, 1, 4, 8, 16, 32, 64, 96, 128, 255];
pub(super) const VIB_PEAK_PER_DEPTH: f64 = 1.0 / 16.0;

pub(super) const ORDER_SKIP: usize = 254;
pub(super) const ORDER_END: usize = 255;

// ── numeric ────────────────────────────────────────────────────────────

/// Round half to even (Python's `round`); `f64::round_ties_even` needs 1.77.
pub(super) fn round_even(x: f64) -> f64 {
    if (x - x.trunc()).abs() == 0.5 {
        2.0 * (x / 2.0).round()
    } else {
        x.round()
    }
}

/// Python-style `round(x, nd)`: round the exact binary value half-to-even.
pub(super) fn pyround(x: f64, nd: i32) -> f64 {
    if !x.is_finite() {
        return x;
    }
    format!("{:.*}", nd.max(0) as usize, x).parse().unwrap_or(x)
}

/// Integer key for `round(x, 3)`, used to group equal positions/durations.
pub(super) fn key3(x: f64) -> i64 {
    (pyround(x, 3) * 1000.0).round() as i64
}

/// Fixed decimals with trailing zeros trimmed ("1.50" -> "1.5", "-0" -> "0").
pub(super) fn fmt(x: f64, nd: usize) -> String {
    let mut s = format!("{:.*}", nd, x);
    if s.contains('.') {
        s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    }
    if s.is_empty() || s == "-0" {
        "0".to_string()
    } else {
        s
    }
}

pub(super) fn hz(semi: f64) -> f64 {
    C5_HZ * 2f64.powf((semi - 60.0) / 12.0)
}

/// Apply signed IT period movement; positive amount raises pitch.
pub(super) fn slide_pitch_period(pitch: f64, amount: f64, c5_speed: f64, linear: bool) -> f64 {
    if linear {
        return (pitch + amount / 64.0).max(0.0);
    }
    let c5 = if c5_speed == 0.0 { 8363.0 } else { c5_speed };
    let c5 = c5.max(1.0);
    let period = (8363.0 * 1712.0) / (c5 * 2f64.powf((pitch - 60.0) / 12.0));
    let next_period = (period - amount).max(1.0);
    (pitch + 12.0 * (period / next_period).log2()).max(0.0)
}

/// Decode D/K/L slide parameter.  Returns (kind, amount).
pub(super) fn slide_decode(p: u8) -> (&'static str, u8) {
    let x = p >> 4;
    let y = p & 15;
    if y == 15 && x != 0 {
        return ("fup", x);
    }
    if x == 15 && y != 0 {
        return ("fdown", y);
    }
    if y == 0 && x != 0 {
        return ("up", x);
    }
    if x == 0 && y != 0 {
        return ("down", y);
    }
    ("none", 0)
}

pub(super) fn sort_dedup(mut v: Vec<f64>) -> Vec<f64> {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v.dedup();
    v
}

/// Evaluate an automation lane at row `t`.  `events` are
/// (time, duration, target) in row units, in time order.
pub(super) fn lane_eval<I>(initial: f64, start: f64, events: I, t: f64) -> f64
where
    I: Iterator<Item = (f64, f64, f64)>,
{
    let mut segment_start = start;
    let mut segment_end = start;
    let mut segment_value = initial;
    let mut segment_target = initial;

    for (event_t, duration, target) in events {
        if event_t > t {
            break;
        }
        let value = if segment_end > segment_start && event_t < segment_end {
            let fraction = ((event_t - segment_start) / (segment_end - segment_start)).max(0.0);
            segment_value + (segment_target - segment_value) * fraction
        } else {
            segment_target
        };
        segment_start = event_t;
        segment_value = value;
        segment_end = event_t + duration.max(0.0);
        segment_target = target;
        if duration <= 0.0 {
            segment_value = target;
            segment_end = event_t;
        }
    }

    if segment_end > segment_start && t < segment_end {
        let fraction = ((t - segment_start) / (segment_end - segment_start)).max(0.0);
        return segment_value + (segment_target - segment_value) * fraction;
    }
    segment_target
}

// ── strings ────────────────────────────────────────────────────────────

pub(super) fn note_name(n: i64) -> String {
    format!(
        "{}{}",
        NOTE_NAMES[n.rem_euclid(12) as usize],
        n.div_euclid(12)
    )
}

pub(super) fn safe_name(s: &str) -> String {
    let mut out = String::new();
    let mut in_bad = false;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-') {
            out.push(ch);
            in_bad = false;
        } else if !in_bad {
            out.push('_');
            in_bad = true;
        }
    }
    let t = out.trim_matches('_');
    if t.is_empty() {
        "sample".to_string()
    } else {
        t.to_string()
    }
}

/// Make a Trackscript identifier (readable names, not generated IDs).
pub(super) fn identifier(s: &str) -> String {
    let s = safe_name(s);
    if s.chars().next().map_or(false, |c| c.is_ascii_digit()) {
        format!("_{}", s)
    } else {
        s
    }
}

/// Minimal Python `repr(str)` for the report.
pub(super) fn py_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(quote);
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}