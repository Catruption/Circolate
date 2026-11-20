//! Stateful Slicex amp-envelope playback, based on FUN_00794590/FUN_007937c0.

use super::curve::shape_by_kind;
use super::Points;

#[derive(Clone)]
pub struct EnvState {
    /// 0 = running, 5 = held at sustain, 6 = release stage, 9 = stopped
    pub state: u32,
    pub current_point: i32,
    /// 0..1 fraction within current segment
    pub position: f64,
    /// 1 / segment_duration (seconds^-1)
    pub rate: f64,
    pub y_prev: f64,
    pub y_delta: f64,
    pub output: f64,
    // Markers from the RegionRecord; set once at construction
    pub sustain_index: i32,
    pub release_index: i32,
}

impl EnvState {
    pub fn new(env: &Points, sustain_index: i32, release_index: i32) -> Self {
        let pts = &env.0;
        let initial_y = pts.get(0).map(|p| p.value).unwrap_or(0.0);
        let next_y = pts.get(1).map(|p| p.value).unwrap_or(initial_y);
        let first_dur = pts.get(1).map(|p| (p.time - pts[0].time).max(0.0)).unwrap_or(0.0);
        let stopped = pts.len() < 2;
        Self {
            state: if stopped { 9 } else { 0 },
            // `current_point` is the target point of the active segment.
            current_point: if stopped { 0 } else { 1 },
            position: if stopped || first_dur > 0.0 { 0.0 } else { 1.0 },
            rate: if first_dur > 0.0 { 1.0 / first_dur } else { 1.0 },
            y_prev: initial_y,
            y_delta: next_y - initial_y,
            output: initial_y,
            sustain_index,
            release_index,
        }
    }

    /// Advance the active segment. Point times in sidecars are cumulative, so
    /// each segment duration is the difference between adjacent point times.
    pub fn advance(&mut self, env: &Points, dt: f64) {
        if self.state == 9 || self.state == 5 { return; }
        let pts = &env.0;
        if pts.is_empty() { return; }

        self.position += dt * self.rate;

        // Fast float compare: DLL checks high32(pos) > 0x3fefffff (i.e. pos >= 1.0)
        while self.position >= 1.0 {
            let reached = self.current_point as usize;
            self.y_prev = pts.get(reached).map(|p| p.value).unwrap_or(self.y_prev);

            // The endpoint was reached. The next segment targets the next point.
            if reached + 1 >= pts.len() {
                self.output = self.y_prev;
                self.state = 9;
                break;
            }

            // Reaching the sustain point holds its value until note-off.
            if self.current_point == self.sustain_index && self.state == 0 {
                self.position = 0.0;
                self.y_prev = pts[reached].value;
                self.y_delta = 0.0;
                self.output = self.y_prev;
                self.state = 5;
                return;
            }
            self.current_point += 1;

            // y_delta = value_at(next target) - y_prev.
            let next_y = pts.get(self.current_point as usize)
                .map(|p| p.value).unwrap_or(self.y_prev);
            self.y_delta = next_y - self.y_prev;

            let idx = self.current_point as usize;
            let dur = pts.get(idx).zip(pts.get(idx - 1))
                .map(|(target, previous)| (target.time - previous.time).max(0.0)).unwrap_or(0.0);
            if dur <= 0.0 {
                self.rate = 1.0;
                self.position = 1.0;
            } else {
                self.rate = 1.0 / dur;
                // Carry only elapsed time beyond the segment boundary.
                self.position = (self.position - 1.0).max(0.0) * self.rate;
            }
        }

        if self.state == 9 { return; }

        // Apply curve shape at current position
        let u = self.position.clamp(0.0, 1.0);
        let shape = pts.get(self.current_point as usize)
            .map(|p| shape_by_kind(p.kind, p.tension, u))
            .unwrap_or(u);
        self.output = self.y_prev + shape * self.y_delta;
    }

    /// Start the release stage at the serialized end/release marker.
    pub fn note_off(&mut self, env: &Points) {
        if self.state >= 6 { return; }
        let pts = &env.0;
        let marker = if self.release_index >= 0 { self.release_index } else { self.sustain_index };
        if marker < 0 || marker as usize + 1 >= pts.len() {
            self.state = 9;
            return;
        }
        let previous = marker as usize;
        let target = previous + 1;
        self.current_point = target as i32;
        self.y_prev = pts[previous].value;
        self.y_delta = pts[target].value - pts[previous].value;
        let dur = (pts[target].time - pts[previous].time).max(0.0);
        self.position = 0.0;
        self.rate = if dur > 0.0 { 1.0 / dur } else { 1.0 };
        self.output = self.y_prev;
        self.state = 6;
    }

    pub fn release_duration(env: &Points, release_index: i32) -> f64 {
        let pts = &env.0;
        let Some(start) = usize::try_from(release_index).ok().filter(|&i| i < pts.len()) else {
            return 0.020;
        };
        let tail = pts.last().map_or(0.0, |last| (last.time - pts[start].time).max(0.0));
        tail.max(0.020)
    }

    pub fn value(&self) -> f64 { self.output }
    pub fn is_ended(&self) -> bool { self.state == 9 || (self.state == 6 && self.position >= 1.0) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::flp::slicex::Point;

    fn env(rows: &[(f64, f64)]) -> Points {
        Points(rows.iter().map(|&(time, value)| Point {
            time, value, tension: 0.0, kind: 0,
        }).collect())
    }

    #[test]
    fn follows_segment_timing_and_note_release() {
        let points = env(&[(0.0, 0.0), (0.05, 1.0), (0.15, 0.5), (0.25, 0.0)]);
        let mut state = EnvState::new(&points, 2, 2);

        state.advance(&points, 0.025);
        assert!((state.value() - 0.5).abs() < 1e-6);
        state.advance(&points, 0.025);
        assert!((state.value() - 1.0).abs() < 1e-6);
        state.advance(&points, 0.05);
        assert!((state.value() - 0.75).abs() < 1e-6);
        state.advance(&points, 0.05);
        assert!((state.value() - 0.5).abs() < 1e-6);
        assert_eq!(state.state, 5);

        state.note_off(&points);
        state.advance(&points, 0.05);
        assert!((state.value() - 0.25).abs() < 1e-6);
        state.advance(&points, 0.05);
        assert!(state.is_ended());
        assert!(state.value().abs() < 1e-6);
    }

    #[test]
    fn skips_zero_length_segments() {
        let points = env(&[(0.0, 0.0), (0.0, 1.0), (0.1, 0.0)]);
        let mut state = EnvState::new(&points, -1, -1);
        state.advance(&points, 0.001);
        assert_eq!(state.current_point, 2);
        assert!((state.value() - 0.99).abs() < 1e-6);
    }
}
