use std::{collections::HashMap, error::Error, sync::{LazyLock, RwLock}};

static DEFAULT_KB_MAP_LIST: LazyLock<RwLock<HashMap<u8, KeyboardMapping>>> = LazyLock::new(|| RwLock::new(HashMap::new()));

#[derive(Clone, Copy, Debug)]
pub struct ScaleDegree { pub ratio: f64 }

impl ScaleDegree {
	pub fn from_cent_value(cents: f64) -> ScaleDegree {
		Self {
			ratio: 2.0f64.powf(1.0 / 12.0).powf(cents / 100.0)
		}
	}

	pub fn from_ratio(n: i64, d: i64) -> ScaleDegree {
		Self {
			ratio: n as f64 / d as f64
		}
	} 
}

#[derive(Debug)]
pub struct Tuning {
	pub description: String,
	pub degrees: Vec<ScaleDegree>,
}

impl Tuning {
	pub fn get_frequency_for_key(&self, key: u8, mapping: Option<&KeyboardMapping>) -> Option<f64> {
		let kb_map;
		let mut rwlock;

		if let Some(map) = mapping {
			kb_map = map
		} else {
			let octave_size = self.degrees.len() as u8;

			rwlock = DEFAULT_KB_MAP_LIST.read().unwrap();

			if let Some(map) = rwlock.get(&octave_size) {
				kb_map = map;
			} else {
				drop(rwlock);
				
				DEFAULT_KB_MAP_LIST.write().unwrap().insert(octave_size, KeyboardMapping::from_octave_size(octave_size));

				rwlock = DEFAULT_KB_MAP_LIST.read().unwrap();
				kb_map = rwlock.get(&octave_size).unwrap();
			}
		};

		if key < kb_map.retune_start || key > kb_map.retune_end {
			return Some(default_freq_for_key(key))
		}

		if self.degrees.is_empty() {
			return None
		}

		let Some((scale_index, octave)) = self.get_scale_degree_index(key, kb_map) else {
			return None
		};

		let degree = self.degrees[scale_index.rem_euclid(self.degrees.len() as i32) as usize];

		Some(self.get_root_pitch(kb_map) * self.get_octave_ratio().powf(octave as f64) * degree.ratio)
	}

	fn get_root_pitch(&self, kb_map: &KeyboardMapping) -> f64 {
		let Some((scale_index, octave)) = self.get_scale_degree_index(kb_map.ref_note, kb_map) else {
			return 440.0
		};

		(kb_map.ref_freq / self.get_octave_ratio().powf(octave as f64)) / self.degrees[scale_index as usize].ratio
	}

	fn get_scale_degree_index(&self, key: u8, kb_map: &KeyboardMapping) -> Option<(i32, i32)> {
		let relative_key = key as i32 - kb_map.middle_note as i32;
		let wrapped_key = relative_key.rem_euclid(kb_map.formal_octave);

		let Some(scale_index) = kb_map.mapping[((wrapped_key - 1).rem_euclid(kb_map.mapping.len() as i32)) as usize] else {
			return None
		};

		let final_index = scale_index.rem_euclid(self.degrees.len() as i32);

		let octave = (relative_key - 1).div_euclid(self.degrees.len() as i32);

		Some((final_index, octave))
	}

	fn get_octave_ratio(&self) -> f64 {
		self.degrees.last().unwrap().ratio
	}

	pub fn load_from_string(buf: String) -> Result<Self, Box<dyn Error>> {
		let mut desc = None;
		let mut count = None;
		let mut degrees = vec![];

		for line in buf.lines() {
			let line = line.trim();

			if line.is_empty() {
				continue
			}

			// comment check
			if line.starts_with('!') {
				continue
			}

			// read description from first non-comment line
			if desc.is_none() {
				desc = Some(line.to_owned());
				continue
			}

			if count.is_none() {
				count = Some(line.parse::<usize>()?);
				continue
			}

			// ok now for the actual scale degrees
			
			if line.contains('.') {
				// cent value
				degrees.push(ScaleDegree::from_cent_value(line.split_whitespace().next().unwrap().parse()?));
			}
			else if let Some(pos) = line.find('/') {
				// ratio
				let (l, mut r) = line.split_whitespace().next().unwrap().split_at(pos);

				r = &r[1..];

				degrees.push(ScaleDegree::from_ratio(l.parse()?, r.parse()?));
			} else if let Some(line) = line.split_whitespace().next() {
				degrees.push(ScaleDegree::from_ratio(line.parse()?, 1));
			}
		}

		let (Some(count), Some(description)) = (count, desc) else {
			return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "no count or description found!").into())
		};

		if count != degrees.len() {
			return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "mismatch between degree count and entry number!").into())
		}

		Ok(Tuning {
			degrees,
			description,
		})
	}
}

#[derive(Debug)]
pub struct KeyboardMapping {
	pub retune_start: u8,
	pub retune_end: u8,
	pub middle_note: u8,
	pub ref_note: u8,
	pub ref_freq: f64,
	pub formal_octave: i32,
	pub mapping: Vec<Option<i32>>,
}

impl KeyboardMapping {
	pub fn load_from_string(buf: String) -> Result<Self, Box<dyn Error>> {
		let mut lines_read = 0;
		let mut result = KeyboardMapping::default();

		for line in buf.lines() {
			let line = line.trim();

			if line.is_empty() {
				continue
			}

			// comment check (allow leading whitespace)
			if line.starts_with('!') {
				continue
			}

			match lines_read {
				0 => {
					// fill mapping with N empty elements
					result.mapping = vec![None; line.parse()?];
				}

				1 => {
					result.retune_start = line.parse()?;
				}

				2 => {
					result.retune_end = line.parse()?;
				}

				3 => {
					result.middle_note = line.parse()?;
				}

				4 => {
					result.ref_note = line.parse()?;
				}

				5 => {
					result.ref_freq = line.parse()?;
				}

				6 => {
					result.formal_octave = line.parse()?;
				}

				_ => if line != "x" {
					let i = lines_read - 7;
					
					if i >= result.mapping.len() {
						return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "index outside of map range!").into())
					}

					result.mapping[i] = Some(line.parse()?);
				}
			}

			lines_read += 1;
		}

		Ok(result)
	}

	fn from_octave_size(octave_size: u8) -> Self {
		KeyboardMapping {
			ref_note: 60,
			ref_freq: 261.625565301,
			middle_note: 60,
			retune_start: 0,
			retune_end: 127,
			formal_octave: octave_size as i32,
			mapping: (0..octave_size).into_iter().map(|i| Some(i as i32)).collect()
		}
	}
}

impl Default for KeyboardMapping {
	fn default() -> Self {
		KeyboardMapping {
			ref_note: 60,
			ref_freq: 261.625565301,
			middle_note: 60,
			retune_start: 0,
			retune_end: 127,
			formal_octave: 12,
			mapping: vec![
				Some(0),
				Some(1),
				Some(2),
				Some(3),
				Some(4),
				Some(5),
				Some(6),
				Some(7),
				Some(8),
				Some(9),
				Some(10),
				Some(11),
			]
		}
	}
}

pub fn default_freq_for_key(key: u8) -> f64 {
	440.0 * 2.0f64.powf((key as f64 - 69.0) / 12.0)
}