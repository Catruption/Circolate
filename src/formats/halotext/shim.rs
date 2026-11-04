//! Shim event parsing.

use crate::spec::{
    parse_greek_slot, FIELD_DURATION, FIELD_MIX, FIELD_PAN, FIELD_PITCH,
    FIELD_POSITION, FIELD_SOURCE, FIELD_VOLUME,
};

use super::parse_f;
use super::split_fields;
use super::types::{ShimEvent, ShimField, ShimParam, ShimWaypoint};

/// Given `§a/b`, return `(a-1)*subs + (b-1)`.  Non-numeric returns None.
fn parse_position(s: &str, subs: u32) -> Option<f64> {
    let (a, b) = s.split_once('/')?;
    let ai = parse_f(a)?;
    let bi = parse_f(b)?;
    Some((ai - 1.0) * subs as f64 + (bi - 1.0))
}

/// Find the index of the first un-parenthesized, un-quoted `>` starting
/// at `start`.  Used to find the end of a shim body, where the target
/// may itself contain `(params)`.
pub(crate) fn find_shim_close(s: &str, start: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut pos = start;
    let mut in_quote = false;
    let mut paren = 0i32;
    while pos < bytes.len() {
        match bytes[pos] {
            b'\'' => in_quote = !in_quote,
            b'(' if !in_quote => paren += 1,
            b')' if !in_quote => paren = (paren - 1).max(0),
            b'>' if !in_quote && paren == 0 => return Some(pos),
            _ => {}
        }
        pos += 1;
    }
    None
}

/// Split `Fruity_Reeverb_2(α25, β5)` into name and args.
fn split_target(s: &str) -> (String, &str) {
    let s = s.trim();
    if let Some(open) = s.find('(') {
        let name = s[..open].trim().to_string();
        let close = s[open..].rfind(')').map(|i| open + i).unwrap_or(s.len());
        (name, &s[open + 1..close])
    } else {
        (s.to_string(), "")
    }
}

fn parse_effect_params(args: &str) -> Vec<ShimParam> {
    let mut out = Vec::new();
    for part in args.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (slot, consumed) = parse_greek_slot(part);
        if slot == 0 {
            continue;
        }
        if let Some(v) = parse_f(&part[consumed..]) {
            out.push(ShimParam { slot, value: v });
        }
    }
    out
}

/// Parse one `@<...>` body into a `ShimEvent`.  `attached_call` is the
/// index of the preceding call on the same BlockList line, or None if
/// the shim stands alone.
///
/// Wet/dry default: call-attached shims default to fully wet (1.0),
/// standalone shims to dry (0.0) until ramped.
pub(crate) fn parse_shim_event(
    inner: &str,
    line: usize,
    subs: u32,
    attached_call: Option<usize>,
) -> Option<ShimEvent> {
    let default_mix = if attached_call.is_some() { 1.0 } else { 0.0 };
    parse_shim_event_with_default(inner, line, subs, attached_call, default_mix)
}

/// Same as `parse_shim_event`, but the caller picks the default `◊`.
/// Declaration-header shims have no call to attach to but are always
/// wet, so they pass 1.0.
pub(crate) fn parse_shim_event_with_default(
    inner: &str,
    line: usize,
    subs: u32,
    attached_call: Option<usize>,
    default_mix: f64,
) -> Option<ShimEvent> {
    let fields = split_fields(inner);
    let mut row: Option<f64> = None;
    let mut length_rows: Option<f64> = None;
    let mut mix: Option<f64> = None;
    let mut target = String::new();
    let mut params: Vec<ShimParam> = Vec::new();
    let mut percent_value: Option<f64> = None;
    let mut pan_value: Option<f64> = None;
    let mut pitch_value: Option<f64> = None;

    for f in fields {
        if let Some(rest) = f.strip_prefix(FIELD_SOURCE) {
            let (name, args) = split_target(&rest);
            target = name;
            params = parse_effect_params(&args);
        } else if let Some(rest) = f.strip_prefix(FIELD_POSITION) {
            row = parse_position(&rest, subs);
        } else if let Some(rest) = f.strip_prefix(FIELD_DURATION) {
            length_rows = parse_f(rest.trim_start_matches('+'))
            .map(|b| b * subs as f64);
        } else if let Some(rest) = f.strip_prefix(FIELD_MIX) {
            mix = parse_f(rest.trim_start_matches('+')).map(|v| v.clamp(0.0, 1.0));
        } else if let Some(rest) = f.strip_prefix(FIELD_VOLUME) {
            percent_value = parse_f(rest.trim_start_matches('+'));
        } else if let Some(rest) = f.strip_prefix(FIELD_PAN) {
            pan_value = parse_f(rest);
        } else if let Some(rest) = f.strip_prefix(FIELD_PITCH) {
            pitch_value = parse_f(rest.trim_start_matches('+'));
        }
    }

    if target.is_empty() {
        return None;
    }

    let is_global = crate::spec::is_reserved(&target);

    // For a reserved global, `%`/`¶`/`#` are the shim's *property*, not
    // universal gain/pan.  For an effect target, they stay universal.
    let (gain, pan, property, start_value) = if is_global {
        let (prop, val) = if let Some(v) = percent_value {
            (FIELD_VOLUME.to_string(), v)
        } else if let Some(v) = pitch_value {
            (FIELD_PITCH.to_string(), v)
        } else if let Some(v) = pan_value {
            (FIELD_PAN.to_string(), v)
        } else {
            (String::new(), 0.0)
        };
        (100.0, 0.0, prop, val)
    } else {
        let gain = percent_value.unwrap_or(100.0);
        let pan = pan_value.unwrap_or(0.0);
        (gain, pan, String::new(), 0.0)
    };

    Some(ShimEvent {
        line,
         row: row.unwrap_or(0.0),
         target,
         params,
         length_rows,
         mix: mix.unwrap_or(default_mix),
         gain,
         pan,
         attached_call,
         waypoints: Vec::new(),
         property,
         start_value,
         op: String::new(),
         amount: 0.0,
         interval_rows: 0.0,
         count: None,
    })
}

/// Parse a `&<...>` waypoint that follows a shim and attach it.
pub(crate) fn parse_shim_waypoint_into(inner: &str, shim: &mut ShimEvent, subs: u32) {
    let fields = split_fields(inner);
    let mut row: Option<f64> = None;
    let mut field: Option<ShimField> = None;
    let mut value: Option<f64> = None;
    let mut ramp_rows: Option<f64> = None;

    for f in fields {
        if let Some(rest) = f.strip_prefix(FIELD_POSITION) {
            row = parse_position(&rest, subs);
        } else if let Some(rest) = f.strip_prefix(FIELD_MIX) {
            field = Some(ShimField::Mix);
            value = parse_f(rest.trim_start_matches('+'));
        } else if let Some(rest) = f.strip_prefix(FIELD_VOLUME) {
            field = Some(ShimField::Gain);
            value = parse_f(rest.trim_start_matches('+'));
        } else if let Some(rest) = f.strip_prefix(FIELD_PAN) {
            field = Some(ShimField::Pan);
            value = parse_f(rest);
        } else if let Some(rest) = f.strip_prefix(FIELD_DURATION) {
            ramp_rows = parse_f(rest.trim_start_matches('+')).map(|v| v * subs as f64);
        }
    }

    if let (Some(r), Some(fld), Some(v)) = (row, field, value) {
        shim.waypoints.push(ShimWaypoint {
            row: r,
            field: fld,
            value: v,
            ramp_rows,
        });
    }
}
