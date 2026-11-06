//! Slicex envelope point model and playback mapping.  Shared by the converter (which bakes
//! amp envelopes into SF2) and the player (which evaluates them live for
//! Slicex parts).  Curve law ported from `slicex_curve.py`
//! (evaluator 0x793950 / precompute 0x791610 / coef setter 0x6168e0).

mod curve;
pub use curve::shape_by_kind;
pub mod convert;

#[derive(Debug, Clone, Copy)]
pub struct Point {
    pub time: f64,      // seconds, cumulative
    pub value: f64,     // 0..1
    pub tension: f64,   // ~ -1..1
    pub kind: u8,       // 0..12
}

#[derive(Debug, Clone, Default)]
pub struct Points(pub Vec<Point>);

impl Points {
    pub fn from_rows(rows: impl Iterator<Item = (f64, f64, f64, u8)>, sec_per_beat: f64) -> Self {
        let mut pts: Vec<Point> = rows
            .map(|(r, v, t, k)| Point { time: r * sec_per_beat, value: v, tension: t, kind: k })
            .collect();
        pts.sort_by(|a, b| a.time.partial_cmp(&b.time).unwrap_or(std::cmp::Ordering::Equal));
        Points(pts)
    }

    pub fn eval_at(&self, t: f64) -> f64 {
        let p = &self.0;
        let Some(first) = p.first() else { return 1.0 };
        if t <= first.time { return first.value; }
        let mut t0 = first.time;
        let mut v0 = first.value;
        for w in p.windows(2) {
            let n = &w[1];
            if t < n.time {
                let u = if n.time > t0 { (t - t0) / (n.time - t0) } else { 1.0 };
                return v0 + (n.value - v0) * shape_by_kind(n.kind, n.tension, u);
            }
            t0 = n.time;
            v0 = n.value;
        }
        p.last().map_or(1.0, |x| x.value)
    }

    pub fn is_empty(&self) -> bool { self.0.is_empty() }

    pub fn is_flat_at(&self, y: f64, eps: f64) -> bool {
        self.0.iter().all(|p| (p.value - y).abs() < eps)
    }
}

/// App-side speed-envelope mapping (0..1, 0.5 = unity) to playback rate.
/// This exponential mapping is a product approximation; decomp notes map
/// Slicex articulator slot 4 to a linear scratch-depth formula in
/// `0x87f7f0`, so this is not claimed as a faithful port of that slot.
pub fn speed_mult(y: f64, octaves: f64) -> f64 {
    2f64.powf((y - 0.5) * 2.0 * octaves)
}
