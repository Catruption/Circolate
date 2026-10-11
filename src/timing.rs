//! Trackscript timing: tempo integration and value automation lanes.
//!
//! Rust port of the relevant parts of trackscript_timing.py. Same
//! semantics: rows are the time axis, tempo is piecewise-linear over
//! rows, and value lanes are piecewise-linear over seconds.
//!
//! Performance
//! -----------
//! `time_at_row` is called once per waypoint when building automation
//! lanes.  For a song with long waypoint chains (HARP parts can carry
//! 200+ points per voice), the segment walk dominates load time.  A
//! precomputed row->seconds table, built once by `prepare`, turns the
//! hot path into a binary-free integer lookup.

use std::sync::OnceLock;

// ── TempoMap ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
struct Segment {
    start_row: f64,
    end_row: f64, // may be f64::INFINITY
    bpm_start: f64,
    bpm_end: f64,
}

/// A tempo event: (row, target_bpm, ramp_beats).
pub type TempoEvent = (f64, f64, f64);

#[derive(Debug, Clone)]
pub struct TempoMap {
    pub subdivisions: f64,
    segments: Vec<Segment>,
    /// row_seconds[n] = seconds from row 0 to row n.  Filled by `prepare`.
    table: OnceLock<Vec<f64>>,
}

impl TempoMap {
    pub fn new(initial_bpm: f64, events: &[TempoEvent], subdivisions: u32) -> Self {
        let subs = subdivisions.max(1) as f64;

        let mut sorted: Vec<TempoEvent> = events.to_vec();
        sorted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

        let mut segments: Vec<Segment> = Vec::new();
        let mut cursor = 0.0f64;
        let mut bpm = initial_bpm.max(1e-6);
        let mut ramp: Option<(f64, f64, f64, f64)> = None;

        for (event_row, target_bpm, ramp_beats) in sorted {
            let event_row = cursor.max(0.0).max(event_row);
            let (nbpm, _) = advance(&mut segments, cursor, event_row, bpm, ramp);
            bpm = nbpm;
            cursor = event_row;

            let target_bpm = target_bpm.max(1e-6);
            let ramp_rows = ramp_beats.max(0.0) * subs;

            if ramp_rows > 0.0 {
                ramp = Some((cursor, cursor + ramp_rows, bpm, target_bpm));
            } else {
                bpm = target_bpm;
                ramp = None;
            }
        }

        if let Some((_rs, re, _rf, _rt)) = ramp {
            let ramp_end = re;
            let (nbpm, _nramp) = advance(&mut segments, cursor, ramp_end, bpm, ramp);
            bpm = nbpm;
            cursor = ramp_end;
        }

        segments.push(Segment {
            start_row: cursor,
            end_row: f64::INFINITY,
            bpm_start: bpm,
            bpm_end: bpm,
        });

        TempoMap {
            subdivisions: subs,
            segments,
            table: OnceLock::new(),
        }
    }

    /// Precompute row->seconds up to `total_rows`.  Called once after the
    /// project is loaded; makes every subsequent `time_at_row` O(1).
    ///
    /// Safe to call more than once; the first call wins.
    pub fn prepare(&self, total_rows: f64) {
        let _ = self.table.set(self.build_table(total_rows));
    }

    fn build_table(&self, total_rows: f64) -> Vec<f64> {
        let n = total_rows.max(0.0).ceil() as usize + 2;
        let mut out: Vec<f64> = Vec::with_capacity(n);
        let mut seg_i = 0usize;
        let mut seconds = 0.0f64;
        let mut row = 0.0f64;

        while out.len() < n {
            // Skip past any segment we've already finished.
            while seg_i < self.segments.len() && row >= self.segments[seg_i].end_row {
                seg_i += 1;
            }
            if seg_i >= self.segments.len() {
                out.push(seconds);
                row += 1.0;
                continue;
            }
            let seg = self.segments[seg_i];
            if row < seg.start_row {
                // Contiguity guard: segments should tile [0, inf).
                out.push(seconds);
                row += 1.0;
                continue;
            }
            // Row is inside this segment.  Fill rows until we leave it.
            let stop = seg.end_row.min(n as f64);
            while row < stop && out.len() < n {
                out.push(seconds);
                let next_row = row + 1.0;
                seconds += self.time_in_segment(&seg, row, next_row);
                row = next_row;
            }
        }
        out
    }

    /// Time to advance from `from` to `to` within a single segment.
    fn time_in_segment(&self, seg: &Segment, from: f64, to: f64) -> f64 {
        let rows = to - from;
        if rows <= 0.0 {
            return 0.0;
        }
        if (seg.bpm_start - seg.bpm_end).abs() < 1e-12 || seg.end_row.is_infinite() {
            return 60.0 * rows / (self.subdivisions * seg.bpm_start);
        }
        let full_rows = seg.end_row - seg.start_row;
        let bpm_from = seg.bpm_start
        + (seg.bpm_end - seg.bpm_start) * (from - seg.start_row) / full_rows;
        let bpm_to = seg.bpm_start
        + (seg.bpm_end - seg.bpm_start) * (to - seg.start_row) / full_rows;
        let slope = (bpm_to - bpm_from) / rows;
        if slope.abs() < 1e-12 {
            60.0 * rows / (self.subdivisions * bpm_from)
        } else {
            60.0 / (self.subdivisions * slope) * (bpm_to / bpm_from).ln()
        }
    }

    /// Seconds from row 0 to `row`.  Uses the precomputed table when
    /// available (the common case), falls back to the segment walk
    /// before `prepare` runs.
    pub fn time_at_row(&self, row: f64) -> f64 {
        let target = row.max(0.0);
        if let Some(table) = self.table.get() {
            let i = target.floor() as usize;
            if i + 1 >= table.len() {
                return *table.last().unwrap_or(&0.0);
            }
            let frac = target - i as f64;
            let a = table[i];
            let b = table[i + 1];
            return a + (b - a) * frac;
        }
        self.walk_time_at_row(target)
    }

    fn walk_time_at_row(&self, target: f64) -> f64 {
        let mut seconds = 0.0f64;
        for seg in &self.segments {
            if target <= seg.start_row {
                break;
            }
            let stop = target.min(seg.end_row);
            let delta_rows = stop - seg.start_row;
            if delta_rows <= 0.0 {
                continue;
            }
            if (seg.bpm_start - seg.bpm_end).abs() < 1e-12 || seg.end_row.is_infinite() {
                seconds += 60.0 * delta_rows / (self.subdivisions * seg.bpm_start);
            } else {
                let full_rows = seg.end_row - seg.start_row;
                let stop_bpm = seg.bpm_start
                + (seg.bpm_end - seg.bpm_start) * delta_rows / full_rows;
                let slope = (stop_bpm - seg.bpm_start) / delta_rows;
                if slope.abs() < 1e-12 {
                    seconds += 60.0 * delta_rows / (self.subdivisions * seg.bpm_start);
                } else {
                    seconds += 60.0 / (self.subdivisions * slope)
                    * (stop_bpm / seg.bpm_start).ln();
                }
            }
            if target <= seg.end_row {
                break;
            }
        }
        seconds
    }
}

fn advance(
    segments: &mut Vec<Segment>,
    start: f64,
    end: f64,
    bpm: f64,
    ramp: Option<(f64, f64, f64, f64)>,
) -> (f64, Option<(f64, f64, f64, f64)>) {
    if end <= start {
        return (bpm, ramp);
    }

    let Some((ramp_start, ramp_end, ramp_from, ramp_to)) = ramp else {
        segments.push(Segment {
            start_row: start,
            end_row: end,
            bpm_start: bpm,
            bpm_end: bpm,
        });
        return (bpm, None);
    };

    let ramp_stop = end.min(ramp_end);
    let mut cur_bpm = bpm;
    let mut cur_start = start;

    if cur_start < ramp_stop {
        let duration = ramp_end - ramp_start;
        let start_bpm = ramp_from + (ramp_to - ramp_from) * (cur_start - ramp_start) / duration;
        let stop_bpm = ramp_from + (ramp_to - ramp_from) * (ramp_stop - ramp_start) / duration;
        segments.push(Segment {
            start_row: cur_start,
            end_row: ramp_stop,
            bpm_start: start_bpm,
            bpm_end: stop_bpm,
        });
        cur_bpm = stop_bpm;
        cur_start = ramp_stop;
    }

    if end > cur_start {
        cur_bpm = ramp_to;
        segments.push(Segment {
            start_row: cur_start,
            end_row: end,
            bpm_start: cur_bpm,
            bpm_end: cur_bpm,
        });
    }

    if end >= ramp_end {
        return (ramp_to, None);
    }
    (cur_bpm, Some((ramp_start, ramp_end, ramp_from, ramp_to)))
}

// ── AutomationLane ─────────────────────────────────────────────────────

/// A waypoint in the source: (row, duration_rows, target_value).
pub type Waypoint = (f64, f64, f64);

/// One-dimensional value that changes over time.
///
/// Breakpoints are (time_seconds, value). Interpolation is linear.
#[derive(Debug, Clone)]
pub struct AutomationLane {
    times: Vec<f64>,
    values: Vec<f64>,
}

impl AutomationLane {
    pub fn new(
        initial_value: f64,
        start_row: f64,
        events: &[Waypoint],
        tempo_map: &TempoMap,
    ) -> Self {
        let start_time = tempo_map.time_at_row(start_row);

        let mut sorted: Vec<Waypoint> = events.to_vec();
        sorted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

        let mut points: Vec<(f64, f64)> = vec![(start_time, initial_value)];

        for (row, duration_rows, target) in sorted {
            let event_time = tempo_map.time_at_row(row).max(start_time);
            let end_time = tempo_map.time_at_row(row + duration_rows.max(0.0));
            let start_value = lookup(&points, event_time);

            while let Some(last) = points.last() {
                if last.0 >= event_time {
                    points.pop();
                } else {
                    break;
                }
            }
            points.push((event_time, start_value));

            if end_time <= event_time {
                points.push((event_time, target));
            } else {
                points.push((end_time, target));
            }
        }

        let (times, values) = points.into_iter().unzip();
        AutomationLane { times, values }
    }

    pub fn is_empty(&self) -> bool {
        self.times.len() < 2
    }

    /// Linear interpolation at a given time in seconds.
    pub fn value_at(&self, time: f64) -> f64 {
        self.interp(time)
    }

    fn interp(&self, time: f64) -> f64 {
        let times = &self.times;
        if times.is_empty() {
            return 0.0;
        }
        if time <= times[0] {
            return self.values[0];
        }
        if time >= *times.last().unwrap() {
            return *self.values.last().unwrap();
        }

        let mut lo = 0usize;
        let mut hi = times.len() - 1;
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if times[mid] <= time {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let (t0, v0) = (times[lo], self.values[lo]);
        let (t1, v1) = (times[hi], self.values[hi]);
        if t1 <= t0 {
            return v1;
        }
        let frac = (time - t0) / (t1 - t0);
        v0 + (v1 - v0) * frac
    }
}

fn lookup(points: &[(f64, f64)], time: f64) -> f64 {
    if points.is_empty() {
        return 0.0;
    }
    if time <= points[0].0 {
        return points[0].1;
    }
    if time >= points.last().unwrap().0 {
        return points.last().unwrap().1;
    }
    let mut lo = 0usize;
    let mut hi = points.len() - 1;
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if points[mid].0 <= time {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let (t0, v0) = points[lo];
    let (t1, v1) = points[hi];
    if t1 <= t0 {
        return v1;
    }
    v0 + (v1 - v0) * ((time - t0) / (t1 - t0))
}
