use std::fmt::Display;

use super::FxOutput;

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub enum ReverbMode {
    #[default]
    Disabled = 0,
    Dry = 1,
    Enabled = 2,
}

impl ReverbMode {
    pub fn from_raw(val: i32) -> Option<ReverbMode> {
        match val {
            0 => Some(Self::Disabled),
            1 => Some(Self::Dry),
            2 => Some(Self::Enabled),
            _ => None,
        }
    }
}

/// FluidSynth's reverb parameters (`synth.reverb.*`), same names/ranges.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReverbParams {
    /// 0.0 ..= 1.2  (`synth.reverb.room-size`, default 0.2)
    pub room_size: f32,
    /// 0.0 ..= 1.0  (`synth.reverb.damp`, default 0.0)
    pub damping: f32,
    /// 0.0 ..= 100.0 (`synth.reverb.width`, default 0.5)
    pub width: f32,
    /// 0.0 ..= 1.0  (`synth.reverb.level`, default 0.9)
    pub level: f32,
}

impl Default for ReverbParams {
    fn default() -> Self {
        ReverbParams { room_size: 0.2, damping: 0.0, width: 0.5, level: 0.9 }
    }
}

impl Display for ReverbMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReverbMode::Disabled => write!(f, "disabled"),
            ReverbMode::Dry => write!(f, "dry"),
            ReverbMode::Enabled => write!(f, "enabled"),
        }
    }
}

// ── Dattorro plate reverb ──────────────────────────────────────────────
//
// Jon Dattorro, "Effect Design Part 1: Reverberator and Other Filters",
// J. Audio Eng. Soc., 1997.  Delay lengths and tap positions from tables
// 1 and 2 of the paper; all constants normalized against the paper's
// 29761 Hz reference rate and scaled to the runtime sample rate at
// set_mode().

const DATTORRO_TRIM: f32 = 0.6;
const INPUT_DIFFUSION_1: f32 = 0.75;
const INPUT_DIFFUSION_2: f32 = 0.625;
const DECAY_DIFFUSION_1: f32 = 0.7;
const DECAY_DIFFUSION_2: f32 = 0.5;
const PREDELAY_S: f64 = 4.0 / 1000.0;
const REFERENCE_RATE: f64 = 29761.0;

// Figure 1 delay times, in the paper's sample units.
const DELAY_SAMPLES: [f64; 12] = [
    142.0, 107.0,   // input diffusion 1 (ap0, ap1)
    379.0, 277.0,   // input diffusion 2 (ap2, ap3)
    672.0,          // decay diffusion 1, left tank
    4453.0,         // delay L ap0 → L ap1
    1800.0,         // decay diffusion 2, left tank
    3720.0,         // delay L ap1 → R ap2
    908.0,          // decay diffusion 1, right tank
    4217.0,         // delay R ap2 → R ap3
    2656.0,         // decay diffusion 2, right tank
    3163.0,         // delay R ap3 → L ap0
];

// Table 2 tap positions, seconds.
const TAP_SECS: [f64; 14] = [
    266.0 / REFERENCE_RATE, 2974.0 / REFERENCE_RATE, 1913.0 / REFERENCE_RATE,
    1996.0 / REFERENCE_RATE, 1990.0 / REFERENCE_RATE, 187.0 / REFERENCE_RATE,
    1066.0 / REFERENCE_RATE,
    353.0 / REFERENCE_RATE, 3627.0 / REFERENCE_RATE, 1228.0 / REFERENCE_RATE,
    2673.0 / REFERENCE_RATE, 2111.0 / REFERENCE_RATE, 335.0 / REFERENCE_RATE,
    121.0 / REFERENCE_RATE,
];

// ── delay line ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
struct DelayLine {
    buf: Vec<f32>,
    write: usize,
}

impl DelayLine {
    fn with_len(len: usize) -> Self {
        DelayLine { buf: vec![0.0; len.max(1)], write: 0 }
    }

    fn resize(&mut self, len: usize) {
        let len = len.max(1);
        if len == self.buf.len() { return; }
        self.buf.clear();
        self.buf.resize(len, 0.0);
        self.write = 0;
    }

    fn len(&self) -> usize { self.buf.len() }

    fn clear(&mut self) {
        for x in self.buf.iter_mut() { *x = 0.0; }
        self.write = 0;
    }

    /// Read relative to the write head.  `tap(0)` is the OLDEST sample
    /// (a full line length old); `tap(-1)` is the most recently written
    /// one; `tap(-k)` is k-1 samples old.
    fn tap(&self, tap: i32) -> f32 {
        let n = self.buf.len() as i32;
        let mut i = self.write as i32 + tap;
        i %= n;
        if i < 0 { i += n; }
        self.buf[i as usize]
    }

    /// Write input at the current head, return the sample that was
    /// there (one full line delay old), advance.
    fn process(&mut self, input: f32) -> f32 {
        let out = self.buf[self.write];
        self.buf[self.write] = input;
        self.write += 1;
        if self.write >= self.buf.len() { self.write = 0; }
        out
    }
}

// ── allpass ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
struct Allpass {
    delay: DelayLine,
    feedback: f32,
}

impl Allpass {
    fn with_len(len: usize) -> Self {
        Allpass { delay: DelayLine::with_len(len), feedback: 0.5 }
    }

    fn resize(&mut self, len: usize) { self.delay.resize(len); }
    fn set_feedback(&mut self, g: f32) { self.feedback = g; }

    fn clear(&mut self) { self.delay.clear(); }

    fn tap(&self, tap: i32) -> f32 { self.delay.tap(tap) }

    /// Schroeder allpass:
    ///   read   = delay.read()          (sample from `len` samples ago)
    ///   write  = x + g * read
    ///   y      = read - g * write
    ///   delay.write(write); delay.advance()
    ///
    /// Computed in the canonical form so the two reads and the write
    /// don't fight over the same borrow.
    fn process(&mut self, x: f32) -> f32 {
        let n = self.delay.buf.len();
        debug_assert!(n > 0);

        let read_idx = self.delay.write;
        let read = self.delay.buf[read_idx];
        let write = x + self.feedback * read;
        let y = read - self.feedback * write;

        self.delay.buf[read_idx] = write;
        self.delay.write = (read_idx + 1) % n;

        y
    }
}

// ── reverb unit ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ReverbUnit {
    pub input: [Vec<f32>; 2],
    mode: ReverbMode,
    sample_rate: u32,

    predelay: DelayLine,
    input_ap: [Allpass; 4],
    tank_ap: [Allpass; 4],
    tank_delay: [DelayLine; 4],

    // Input bandwidth one-pole state and coefficients.
    bw_buf: f32,
    bw_b0: f32,
    bw_a1: f32,

    // Damping one-pole state and coefficients (in each tank loop).
    damp_buf: [f32; 2],
    damp_b0: f32,
    damp_a1: f32,

    taps: [i32; 14],

    // Cross-feedback between the two tanks: the delayed output of each
    // tank's last delay line from the previous sample.
    fb_l: f32,
    fb_r: f32,

    params: ReverbParams,
    decay: f32,
    wet1: f32,
    wet2: f32,
}

impl Default for ReverbUnit {
    fn default() -> Self { Self::new() }
}

impl ReverbUnit {
    pub fn new() -> Self {
        ReverbUnit {
            input: [vec![], vec![]],
            mode: ReverbMode::Disabled,
            sample_rate: 0,   // forces a size pass on first set_mode

            predelay: DelayLine::with_len(4),
            input_ap: std::array::from_fn(|_| Allpass::with_len(4)),
            tank_ap: std::array::from_fn(|_| Allpass::with_len(4)),
            tank_delay: std::array::from_fn(|_| DelayLine::with_len(4)),

            bw_buf: 0.0,
            bw_b0: 1.0,
            bw_a1: 0.0,
            damp_buf: [0.0; 2],
            damp_b0: 1.0,
            damp_a1: 0.0,

            taps: [0; 14],

            fb_l: 0.0,
            fb_r: 0.0,

            params: ReverbParams::default(),
            decay: 0.5,
            wet1: 0.5,
            wet2: 0.5,
        }
    }

    pub fn set_mode(&mut self, mode: ReverbMode, sample_rate: u32) {
        if sample_rate != self.sample_rate {
            self.sample_rate = sample_rate;
            self.resize_for_rate(sample_rate);
        }

        self.set_params(self.params);

        self.mode = mode;
    }

    /// Apply FluidSynth-style reverb parameters.
    pub fn set_params(&mut self, p: ReverbParams) {
        let room    = p.room_size.clamp(0.0, 1.2);
        let damping = p.damping.clamp(0.0, 1.0);
        let width   = p.width.clamp(0.0, 100.0);
        let level   = p.level.clamp(0.0, 1.0);
        self.params = ReverbParams { room_size: room, damping, width, level };

        self.decay = (0.2 + room * 0.78).min(0.97);

        // Input bandwidth one-pole: b0 = bandwidth, a1 = 1 - bandwidth.
        let bandwidth = 0.9999f32;
        self.bw_b0 = bandwidth;
        self.bw_a1 = 1.0 - bandwidth;

        // Damping one-pole in the tank loops: a1 = damping, b0 = 1 - damping.
        self.damp_b0 = 1.0 - damping;
        self.damp_a1 = damping;

        // Wet mix.
        let wet = level / (1.0 + width * 0.2);
        self.wet1 = wet * (width / 2.0 + 0.5);
        self.wet2 = wet * ((1.0 - width) / 2.0);
    }

    fn resize_for_rate(&mut self, sample_rate: u32) {
        let sr = sample_rate as f64;
        let scale = sr / REFERENCE_RATE;

        let s = |paper_samples: f64| -> usize {
            ((paper_samples * scale).round() as usize).max(1)
        };

        self.predelay.resize(((PREDELAY_S * sr).round() as usize).max(1));

        self.input_ap[0].resize(s(DELAY_SAMPLES[0]));
        self.input_ap[1].resize(s(DELAY_SAMPLES[1]));
        self.input_ap[2].resize(s(DELAY_SAMPLES[2]));
        self.input_ap[3].resize(s(DELAY_SAMPLES[3]));
        self.input_ap[0].set_feedback(INPUT_DIFFUSION_1);
        self.input_ap[1].set_feedback(INPUT_DIFFUSION_1);
        self.input_ap[2].set_feedback(INPUT_DIFFUSION_2);
        self.input_ap[3].set_feedback(INPUT_DIFFUSION_2);

        self.tank_ap[0].resize(s(DELAY_SAMPLES[4]));
        self.tank_delay[0].resize(s(DELAY_SAMPLES[5]));
        self.tank_ap[1].resize(s(DELAY_SAMPLES[6]));
        self.tank_delay[1].resize(s(DELAY_SAMPLES[7]));
        self.tank_ap[2].resize(s(DELAY_SAMPLES[8]));
        self.tank_delay[2].resize(s(DELAY_SAMPLES[9]));
        self.tank_ap[3].resize(s(DELAY_SAMPLES[10]));
        self.tank_delay[3].resize(s(DELAY_SAMPLES[11]));
        self.tank_ap[0].set_feedback(DECAY_DIFFUSION_1);
        self.tank_ap[1].set_feedback(DECAY_DIFFUSION_2);
        self.tank_ap[2].set_feedback(DECAY_DIFFUSION_1);
        self.tank_ap[3].set_feedback(DECAY_DIFFUSION_2);

        for (i, t) in TAP_SECS.iter().enumerate() {
            self.taps[i] = -((t * sr).round() as i32).max(1);
        }

        // Reset filter state so old-rate values don't leak in.
        self.bw_buf = 0.0;
        self.damp_buf = [0.0; 2];
        self.fb_l = 0.0;
        self.fb_r = 0.0;
        for ap in self.input_ap.iter_mut() { ap.clear(); }
        for ap in self.tank_ap.iter_mut() { ap.clear(); }
        for d in self.tank_delay.iter_mut() { d.clear(); }
        self.predelay.clear();
    }

    pub fn process(&mut self, mut output: impl FxOutput, gain: [f32; 2]) {
        let n = output.channel_len();
        self.input[0].resize(n, 0.0);
        self.input[1].resize(n, 0.0);

        match self.mode {
            ReverbMode::Disabled => {}
            ReverbMode::Dry => {
                let [bufl, bufr] = &mut self.input;
                output.write(&[bufl, bufr], gain);
            }
            ReverbMode::Enabled => {
                let mut out_l = vec![0.0f32; n];
                let mut out_r = vec![0.0f32; n];
                for i in 0..n {
                    let (l, r) = self.tick(self.input[0][i], self.input[1][i]);
                    out_l[i] = l;
                    out_r[i] = r;
                }
                output.write(&[&out_l, &out_r], gain);
            }
        }
    }

    fn tick(&mut self, in_l: f32, in_r: f32) -> (f32, f32) {
        // Mono sum, trim, predelay, bandwidth one-pole.
        let mono = (in_l + in_r) * 0.5 * DATTORRO_TRIM;
        let pre = self.predelay.process(mono);
        self.bw_buf = self.bw_b0 * pre + self.bw_a1 * self.bw_buf;
        let mut x = self.bw_buf;

        // Input diffusers.
        x = self.input_ap[0].process(x);
        x = self.input_ap[1].process(x);
        x = self.input_ap[2].process(x);
        x = self.input_ap[3].process(x);

        // ── left tank ────────────────────────────────────────────────
        // Cross feedback is the right tank's last delay line OUTPUT
        // (full 3163-sample delay), from the previous sample.  The old
        // code read tap(-1), the newest *input* sample, which skipped
        // the delay and shortened the loop to a tiny metallic echo.
        let left_in = x + self.decay * self.fb_r;

        let lp = self.tank_ap[0].process(left_in);
        let d0 = self.tank_delay[0].process(lp);

        // Damping filter in the left feedback path.
        self.damp_buf[0] = self.damp_b0 * d0 + self.damp_a1 * self.damp_buf[0];
        let damp_l = self.damp_buf[0];

        let lp2 = self.tank_ap[1].process(self.decay * damp_l);
        let d1 = self.tank_delay[1].process(lp2);

        // ── right tank ───────────────────────────────────────────────
        let right_in = x + self.decay * self.fb_l;

        let rp = self.tank_ap[2].process(right_in);
        let d2 = self.tank_delay[2].process(rp);

        self.damp_buf[1] = self.damp_b0 * d2 + self.damp_a1 * self.damp_buf[1];
        let damp_r = self.damp_buf[1];

        let rp2 = self.tank_ap[3].process(self.decay * damp_r);
        let d3 = self.tank_delay[3].process(rp2);

        // Both tanks consume the other's previous-sample output.
        self.fb_l = d1;
        self.fb_r = d3;

        // ── tap sums (table 2) ───────────────────────────────────────
        let out_l =
              self.tank_delay[2].tap(self.taps[0])
            + self.tank_delay[2].tap(self.taps[1])
            - self.tank_ap[3].tap(self.taps[2])
            + self.tank_delay[3].tap(self.taps[3])
            - self.tank_delay[0].tap(self.taps[4])
            - self.tank_ap[1].tap(self.taps[5])
            - self.tank_delay[1].tap(self.taps[6]);

        let out_r =
              self.tank_delay[0].tap(self.taps[7])
            + self.tank_delay[0].tap(self.taps[8])
            - self.tank_ap[1].tap(self.taps[9])
            + self.tank_delay[1].tap(self.taps[10])
            - self.tank_delay[2].tap(self.taps[11])
            - self.tank_ap[3].tap(self.taps[12])
            - self.tank_delay[3].tap(self.taps[13]);

        let l = out_l * self.wet1 + out_r * self.wet2;
        let r = out_r * self.wet1 + out_l * self.wet2;
        (l, r)
    }
}