use std::fmt::Display;

use super::FxOutput;

#[repr(C)]
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub enum ChorusMode {
	#[default]
	Disabled = 0,
	Dry = 1,
	Enabled = 2,
}

impl ChorusMode {
	pub fn from_raw(val: i32) -> Option<ChorusMode> {
		match val {
			0 => Some(Self::Disabled),
			1 => Some(Self::Dry),
			2 => Some(Self::Enabled),
			_ => None,
		}
	}
}

impl Display for ChorusMode {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			ChorusMode::Disabled => write!(f, "disabled"),
			ChorusMode::Dry => write!(f, "dry"),
			ChorusMode::Enabled => write!(f, "enabled"),
		}
	}
}

/// FluidSynth's chorus parameters (`synth.chorus.*`), same names/ranges.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChorusParams {
	/// 1 ..= 99   (`synth.chorus.nr`, default 3)
	pub voices: usize,
	/// 0.0 ..= 10.0 (`synth.chorus.level`, default 2.0)
	pub level: f32,
	/// 0.1 ..= 5.0 Hz (`synth.chorus.speed`, default 0.3)
	pub speed_hz: f32,
	/// 0.0 ..= 21.0 ms (`synth.chorus.depth`, default 8.0)
	pub depth_ms: f32,
}

impl Default for ChorusParams {
	fn default() -> Self {
		ChorusParams { voices: 3, level: 2.0, speed_hz: 0.3, depth_ms: 8.0 }
	}
}

// ── Chorus ─────────────────────────────────────────────────────────────
//
// FluidSynth topology: a MONO input feeds one delay line; `voices`
// sine-modulated taps (LFO phases spread evenly over 360°) are read
// with interpolation, and voice i goes to output channel i & 1, so the
// chorus also widens the image.  Interpolation here is linear (the
// reference uses a first-order allpass).
//
// Output scaling is `level * 0.35 / voices` per voice, which equals the
// old fixed 0.7 wet at the defaults (level 2.0, 3 voices).  That
// normalisation is a calibration choice, not taken from FluidSynth's
// source: tune the 0.35 against a render if the level is off.

const WET_SCALE: f32 = 0.35;

#[derive(Debug, Clone)]
struct Lfo {
	a1: f32,
	buf1: f32,
	buf2: f32,
	reset: f32,
}

impl Lfo {
	fn new(freq: f32, sample_rate: f32, phase_deg: f32) -> Self {
		// Resonant-form sine oscillator: y[n] = a1*y[n-1] - y[n-2].
		// Double precision internally for stability.
		let w = (std::f64::consts::TAU * freq as f64) / sample_rate as f64;
		let a = std::f64::consts::TAU * (phase_deg as f64 / 360.0);
		Lfo {
			a1: (2.0 * w.cos()) as f32,
			buf1: a.sin() as f32,
			buf2: (a - w).sin() as f32,
			reset: ((std::f64::consts::PI / 2.0) - w).sin() as f32,
		}
	}

	fn tick(&mut self) -> f32 {
		let out = self.a1 * self.buf1 - self.buf2;
		self.buf2 = self.buf1;
		if out >= 1.0 {
			self.buf1 = self.reset;
			return 1.0;
		}
		if out <= -1.0 {
			self.buf1 = -self.reset;
			return -1.0;
		}
		self.buf1 = out;
		out
	}
}

#[derive(Debug, Clone)]
pub struct ChorusUnit {
	pub input: [Vec<f32>; 2],
	mode: ChorusMode,
	sample_rate: u32,
	params: ChorusParams,

	buf: Vec<f32>,
	write_idx: usize,

	mod_depth: f32,
	voice_gain: f32,
	lfos: Vec<Lfo>,
}

impl Default for ChorusUnit {
	fn default() -> Self { Self::new() }
}

impl ChorusUnit {
	pub fn new() -> Self {
		ChorusUnit {
			input: [vec![], vec![]],
			mode: ChorusMode::Disabled,
			sample_rate: 44100,
			params: ChorusParams::default(),
			buf: vec![],
			write_idx: 0,
			mod_depth: 0.0,
			voice_gain: 0.0,
			lfos: vec![],
		}
	}

	pub fn set_mode(&mut self, mode: ChorusMode, sample_rate: u32) {
		if sample_rate != self.sample_rate || self.buf.is_empty() {
			self.sample_rate = sample_rate;
			self.rebuild();
		}
		self.mode = mode;
	}

	/// Apply FluidSynth-style chorus parameters (resets the delay line).
	pub fn set_params(&mut self, p: ChorusParams) {
		self.params = ChorusParams {
			voices: p.voices.clamp(1, 99),
			level: p.level.clamp(0.0, 10.0),
			speed_hz: p.speed_hz.clamp(0.1, 5.0),
			depth_ms: p.depth_ms.clamp(0.0, 21.0),
		};
		self.rebuild();
	}

	fn rebuild(&mut self) {
		let sr = self.sample_rate as f64;
		let depth = (self.params.depth_ms as f64 * sr * 0.001) as f32;
		// The delay sweeps from 1 sample up to depth + 1 samples.
		self.mod_depth = depth * 0.5;

		let size = ((depth + 8.0).ceil() as usize).max(16);
		self.buf.clear();
		self.buf.resize(size, 0.0);
		self.write_idx = 0;

		let nr = self.params.voices;
		self.lfos = (0..nr)
			.map(|i| Lfo::new(
				self.params.speed_hz,
				self.sample_rate as f32,
				360.0 * i as f32 / nr as f32,
			))
			.collect();
		self.voice_gain = self.params.level * WET_SCALE / nr as f32;
	}

	pub fn process(&mut self, mut output: impl FxOutput, gain: [f32; 2]) {
		let n = output.channel_len();
		self.input[0].resize(n, 0.0);
		self.input[1].resize(n, 0.0);

		match self.mode {
			ChorusMode::Disabled => {}
			ChorusMode::Dry => {
				let [bufl, bufr] = &mut self.input;
				output.write(&[bufl, bufr], gain);
			}
			ChorusMode::Enabled => {
				let mut out_l = vec![0.0f32; n];
				let mut out_r = vec![0.0f32; n];

				let len = self.buf.len();
				let len_f = len as f32;

				for i in 0..n {
					// Mono chorus input, as in FluidSynth.
					let mono = 0.5 * (self.input[0][i] + self.input[1][i]);
					self.buf[self.write_idx] = mono;

					let mut acc = [0.0f32; 2];
					for (k, lfo) in self.lfos.iter_mut().enumerate() {
						let l = lfo.tick();
						let delay = 1.0 + self.mod_depth * (1.0 + l);

						let mut read = self.write_idx as f32 - delay;
						if read < 0.0 { read += len_f; }

						let i0 = read.floor() as usize % len;
						let i1 = (i0 + 1) % len;
						let frac = read - read.floor();

						acc[k & 1] += self.buf[i0] * (1.0 - frac)
							+ self.buf[i1] * frac;
					}

					out_l[i] = acc[0] * self.voice_gain;
					out_r[i] = acc[1] * self.voice_gain;

					self.write_idx = (self.write_idx + 1) % len;
				}

				output.write(&[&out_l, &out_r], gain);
			}
		}
	}
}