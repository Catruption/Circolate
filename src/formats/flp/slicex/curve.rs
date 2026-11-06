//! Envelope segment shapes (port of slicex_curve.py).  Used by the
//! player's `notes::value_at` when a waypoint carries a curve kind, and
//! by the Slicex converter's preview code.
//!
//! `kind` is a byte, `t` is the segment tension. Returns the normalized
//! segment position; caller does `y0 + s*(y1-y0)`.
//!
//! ## Reverse-engineering provenance
//!
//! Ported against `slicex-decomp-session3/slicex_curve.py`: evaluator
//! `TCustomMEnvelope.vslot25` at VA `0x793950`, precompute at `0x791610`,
//! coefficient setter at `0x6168e0`, and cycle-count helper at `0x792f80`.
//! The notes classify kinds 0, 1, 2, 3, 5, 7, 8, 9, 11, and 12 as high
//! confidence; kinds 4 and 6 as medium; kind 10's tangent construction is
//! still low confidence. Our kind-10 path supplies zero tangents, so it is
//! a smoothstep approximation, not a verified Hermite reconstruction.
//! See `slicex-decomp-session3/SLICEX_IMPLEMENTATION_MAP.md` for scope and
//! known gaps.

fn sgn(t: f64) -> f64 {
    if t == 0.0 { 0.0 } else if t > 0.0 { 1.0 } else { -1.0 }
}

fn round_bankers(x: f64) -> f64 {
    let fl = x.floor();
    let fr = x - fl;
    if (fr - 0.5).abs() < 1e-12 {
        if (fl as i64) % 2 == 0 { fl } else { fl + 1.0 }
    } else {
        x.round()
    }
}

fn cycle_count(kind: u8, t: f64) -> f64 {
    let n = ((1_000_001f64.powf(1.0 - 0.6 * t.abs()) - 1.0) * 0.001).round();
    if kind == 3 && t >= 0.0 { n + 1.0 } else { n }
}

fn exp_coef(t: f64) -> (f64, f64, f64) {
    let k = sgn(t) * (1_000_001f64.powf(t.abs()) - 1.0);
    if k == 0.0 { return (0.0, 0.0, 0.0); }
    (k, 1.0 / k.abs(), (k.abs() + 1.0).ln())
}

fn exp_shape(u: f64, a: f64, b: f64, c: f64) -> f64 {
    if a == 0.0 { u }
    else if a > 0.0 { 1.0 - ((1.0 - u) * c).exp_m1() * b }
    else { (u * c).exp_m1() * b }
}

fn rat_coef(kind: u8, t: f64) -> (f64, f64, f64) {
    let m = (t.abs() * 0.9).max(2f64.powi(-16));
    let a = 1.0 - (1.0 - m).powi(3);
    let mut b = (1.0 - a) / a;
    let mut c = b;
    if sgn(t) > 0.0 { b = -b; c = b - 1.0; }
    if kind == 12 { b *= 0.5; c *= 0.5; }
    (a, b, c)
}

fn rat(u: f64, a: f64, b: f64, c: f64, pos: bool) -> f64 {
    if pos { b / (1.0 - a * (1.0 - u)) - c } else { b / (1.0 - a * u) - c }
}

fn hermite(u: f64, a: f64, b: f64, c: f64) -> f64 {
    let h = u * u;
    let p = u - 1.0;
    u + ((3.0 * h - 2.0 * h * u) + h * p * a + u * p * p * b - u) * c
}

/// Segment shape.  `kind` = waypoint curve byte; `t` = tension; `u` in 0..1.
pub fn shape_by_kind(kind: u8, t: f64, u: f64) -> f64 {
    match kind {
        0 => { let (a,b,c) = exp_coef(t); exp_shape(u, a, b, c) }
        1 => {
            let (a,b,c) = exp_coef(t);
            if u < 0.5 { 0.5 * exp_shape(2.0 * u, a, b, c) }
            else { let v = 2.0 * (1.0 - u); 1.0 - 0.5 * exp_shape(v, a, b, c) }
        }
        2 => 0.0,
        3 => {
            let n = cycle_count(3, t); let a = n + 1.0;
            let b = 1.0 / (a - if t >= 0.0 { 1.0 } else { -1.0 });
            let c = if t >= 0.0 { 0.5 } else { -0.5 };
            round_bankers(u * a - c) * b
        }
        4 => {
            let n = cycle_count(4, t); let a = n + 1.0;
            let v = u * a - 0.5; let k = round_bankers(v - 0.5);
            let w = 2.0 * (v - k) - 1.0;
            let sh = if sgn(t) < 0.0 {
                (1.0 - (1.0 - w.abs()).powi(3)) * if w >= 0.0 { 1.0 } else { -1.0 }
            } else { w.powi(3) };
            (k + 1.0 + 0.5 * sh) / a
        }
        5 => {
            let n = cycle_count(5, t); let a = 2.0 * (n + 1.0);
            let c = if t >= 0.0 { 0.5 } else { -0.5 };
            if (round_bankers(u * a - c) as i64) & 1 != 0 { 1.0 } else { 0.0 }
        }
        6 => {
            let n = cycle_count(6, t); let a = n + 0.5;
            if sgn(t) < 0.0 {
                ((u * a + 0.75) * 2.0 * std::f64::consts::PI).sin() * 0.5 + 0.5
            } else {
                let v = u * a - 0.5; let k = round_bankers(v - 0.5);
                (2.0 * (v - k) - 1.0).abs()
            }
        }
        7 => {
            let v = if sgn(t) > 0.0 { 1.0 - u } else { u };
            u - (v.powi(6) - v) * t
        }
        8 => {
            let f = |x: f64| {
                let v = if sgn(t) > 0.0 { 1.0 - x } else { x };
                x - (v.powi(6) - v) * t
            };
            if u < 0.5 { 0.5 * f(2.0 * u) } else { 1.0 - 0.5 * f(2.0 * (1.0 - u)) }
        }
        9 => {
            let g = if sgn(t) > 0.0 {
                (u * std::f64::consts::PI / 2.0).sin()
            } else {
                1.0 - (u * std::f64::consts::PI / 2.0).cos()
            };
            u - (u - g) * t.abs()
        }
        10 => hermite(u, 0.0, 0.0, 1.0 - t.abs()),
        11 => { let (a,b,c) = rat_coef(11, t); rat(u, a, b, c, sgn(t) > 0.0) }
        12 => {
            let (a,b,c) = rat_coef(12, t);
            if u < 0.5 { rat(2.0 * u, a, b, c, sgn(t) > 0.0) }
            else { let v = 2.0 * (1.0 - u); 1.0 - rat(v, a, b, c, sgn(t) > 0.0) }
        }
        _ => u,
    }
}