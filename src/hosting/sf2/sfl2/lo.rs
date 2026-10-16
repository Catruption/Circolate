	use std::{fmt::Debug, hash::Hash, io::{self, Cursor, ErrorKind}, ops::{Index, IndexMut}, path::PathBuf, sync::{Arc, atomic::{AtomicU32, Ordering}}};

	use lewton::inside_ogg::OggStreamReader;

	use super::{riff::{self, OGG_FLAG}, util::TryPush};

	pub struct Sf2Data {
		pub path: PathBuf,
		pub name: Option<String>,
		pub version_major: u16,
		pub version_minor: u16,

		pub smpl: Vec<i16>,
		pub sm24: Vec<u8>,
		pub phdr: Vec<PresetHeader>,
		pub pbag: Vec<Bag>,
		pub pmod: Vec<ModList>,
		pub pgen: Vec<GenList>,
		pub inst: Vec<Inst>,
		pub ibag: Vec<Bag>,
		pub imod: Vec<ModList>,
		pub igen: Vec<GenList>,
		pub shdr: Vec<Sample>,
	}

	// SAFETY:
	// This function returns a raw, mutable byte view into any object passed into it.
	// This can be used to construct objects with invalid bit patterns, which is UB.
	// The responsibility is on the caller to modify this
	unsafe fn as_u8_slice_mut<T: ?Sized>(from: &mut T) -> &mut [u8] {
		let ptr: *mut u8 = (from as *mut T).cast();
		unsafe { std::slice::from_raw_parts_mut(ptr, size_of_val(from)) }
	}

	// SAFETY:
	// This function reads data directly from a u8 buffer into a value of type T.
	// If the data read does not represent a valid value of T, the behaviour is undefined.
	unsafe fn read_from_chunk<T: ?Sized>(i: &mut usize, chunk_data: &[u8], t: &mut T) {
		let size = size_of_val(t);

		assert!(size <= chunk_data[*i..].len());

		unsafe { as_u8_slice_mut(t) }.copy_from_slice(&chunk_data[*i..(*i+size)]);

		*i += size;
	}

	fn chunk_read_gen(i: &mut usize, chunk_data: &[u8], t: &mut Gen) -> std::io::Result<()> {
		let mut data = 0u16;

		unsafe {
			read_from_chunk(i, chunk_data, &mut data);

			if data >= Gen::GeneratorMax as u16 {
				*t = Gen::Unused1;
				return Ok(())
			}

			as_u8_slice_mut(t).copy_from_slice(as_u8_slice_mut(&mut data));
		}

		Ok(())
	}

	fn chunk_read_transform(i: &mut usize, chunk_data: &[u8], t: &mut Transform) -> std::io::Result<()> {
		let mut data = 0u16;
		unsafe {
			read_from_chunk(i, chunk_data, &mut data);
		}

		*t = match data {
			2 => Transform::Absolute,
			_ => Transform::Linear,
		};

		Ok(())
	}

	fn chunk_read_u16(i: &mut usize, chunk_data: &[u8], t: &mut u16) {
		unsafe { read_from_chunk(i, chunk_data, t) }
	}

	fn chunk_read_i16(i: &mut usize, chunk_data: &[u8], t: &mut i16) {
		unsafe { read_from_chunk(i, chunk_data, t) }
	}

	fn chunk_read_u32(i: &mut usize, chunk_data: &[u8], t: &mut u32) {
		unsafe { read_from_chunk(i, chunk_data, t) }
	}

	fn chunk_read_bytes(i: &mut usize, chunk_data: &[u8], t: &mut [u8]) {
		unsafe { read_from_chunk(i, chunk_data, t) }
	}


	impl Sf2Data {
		pub fn load_from_file(path: PathBuf, load_sm24: bool, load_progress: Arc<AtomicU32>) -> std::io::Result<Self> {
			load_progress.store(0, Ordering::SeqCst);

			let mut smpl = vec![];
			let mut smpl_compressed = vec![];
			let mut smpl_opt_enabled = true;
			let mut riff = riff::RiffChunk::load_from_file(&path, &mut smpl, &mut smpl_opt_enabled, load_sm24, &load_progress)?;

			let mut result = Sf2Data {
				path,
				name: None,
				version_major: 2,
				version_minor: 1,
				smpl,
				sm24: vec![],
				phdr: vec![],
				pbag: vec![],
				pmod: vec![],
				pgen: vec![],
				inst: vec![],
				ibag: vec![],
				imod: vec![],
				igen: vec![],
				shdr: vec![],
			};

			for chunk in riff.sub_chunks.iter_mut() {

				match chunk.sub_id_str() {
					"INFO" => {
						let Some(ifil) = chunk.get_subchunk("ifil") else {
							return Err(io::Error::new(ErrorKind::InvalidData, "missing `ifil` chunk!"))
						};

						let mut i = 0;

						chunk_read_u16(&mut i, &ifil.chunk_data, &mut result.version_major);
						chunk_read_u16(&mut i, &ifil.chunk_data, &mut result.version_minor);

						if let Some(inam) = chunk.get_subchunk("INAM")
							&& let Some(term) = inam.chunk_data.iter().position(|c| *c == 0)
						{
							let Ok(conv) = String::from_utf8(inam.chunk_data[0..term].into()) else {
								return Err(io::Error::new(ErrorKind::InvalidData, "invalid `INAM` data!"))
							};

							result.name = Some(conv);
						}
					}

					"sdta" => {
						for sub in chunk.sub_chunks.iter_mut() {
							match sub.chunk_id_str() {
								// 16-bit sample data (RAM). no-op in most cases, we already got it earlier
								"smpl" => if !smpl_opt_enabled {
									smpl_compressed = std::mem::take(&mut sub.chunk_data);
								}

								// Least Significant Bytes for 24-bit sample data, combined with `smpl` when resampling
								"sm24" => if load_sm24 {
									result.sm24 = std::mem::take(&mut sub.chunk_data);
								}

								_ => {}
							}
						}
					}

					"pdta" => {
						for sub in chunk.sub_chunks.iter() {
							match sub.chunk_id_str() {
								"phdr" => {
									let mut i = 0;

									while i < sub.chunk_data.len() {
										let mut header = PresetHeader::default();

										chunk_read_bytes(&mut i, &sub.chunk_data, &mut header.name);
										chunk_read_u16(&mut i, &sub.chunk_data, &mut header.preset);
										chunk_read_u16(&mut i, &sub.chunk_data, &mut header.bank);
										chunk_read_u16(&mut i, &sub.chunk_data, &mut header.preset_bag_idx);
										chunk_read_u32(&mut i, &sub.chunk_data, &mut header.library);
										chunk_read_u32(&mut i, &sub.chunk_data, &mut header.genre);
										chunk_read_u32(&mut i, &sub.chunk_data, &mut header.morphology);

										result.phdr.try_push(header)?;
									}
								}

								"pbag" | "ibag" => {
									let mut i = 0;

									while i < sub.chunk_data.len() {
										let mut bag = Bag {
											gen_idx: 0,
											mod_idx: 0
										};

										chunk_read_u16(&mut i, &sub.chunk_data, &mut bag.gen_idx);
										chunk_read_u16(&mut i, &sub.chunk_data, &mut bag.mod_idx);

										if sub.chunk_id_str() == "pbag" {
											result.pbag.try_push(bag)?;
										} else {
											result.ibag.try_push(bag)?;
										}
									}
								}

								"pmod" | "imod" => {
									let mut i = 0;

									while i < sub.chunk_data.len() {
										let mut modulator = ModList::default();

										chunk_read_i16(&mut i, &sub.chunk_data, &mut modulator.src_oper.data);
										chunk_read_gen(&mut i, &sub.chunk_data, &mut modulator.dest_oper)?;
										chunk_read_i16(&mut i, &sub.chunk_data, &mut modulator.mod_amt);
										chunk_read_i16(&mut i, &sub.chunk_data, &mut modulator.mod_amt_src_oper.data);
										chunk_read_transform(&mut i, &sub.chunk_data, &mut modulator.mod_amt_trans_oper)?;

										if sub.chunk_id_str() == "pmod" {
											result.pmod.try_push(modulator)?;
										} else {
											result.imod.try_push(modulator)?;
										}
									}
								}

								"pgen" | "igen" => {
									let mut i = 0;

									while i < sub.chunk_data.len() {
										let mut genlist = GenList {
											oper: Gen::default(),
											amount: GenAmountType::default(),
										};

										chunk_read_gen(&mut i, &sub.chunk_data, &mut genlist.oper)?;
										chunk_read_i16(&mut i, &sub.chunk_data, genlist.amount.sh_amount_mut());

										if sub.chunk_id_str() == "pgen" {
											result.pgen.try_push(genlist)?;
										} else {
											result.igen.try_push(genlist)?;
										}
									}
								}

								"inst" => {
									let mut i = 0;

									while i < sub.chunk_data.len() {
										let mut inst = Inst {
											name: [0; 20],
											inst_bag_idx: 0,
										};

										chunk_read_bytes(&mut i, &sub.chunk_data, &mut inst.name);
										chunk_read_u16(&mut i, &sub.chunk_data, &mut inst.inst_bag_idx);

										result.inst.try_push(inst)?;
									}
								}

								"shdr" => {
									let mut i = 0;

									while i < sub.chunk_data.len() {
										let mut sample = Sample {
											name: [0; 20],
											start: 0,
											end: 0,
											start_loop: 0,
											end_loop: 0,
											sample_rate: 0,
											original_pitch: 0,
											pitch_correction: 0,
											sample_link: 0,
											sample_type: SampleType::MonoSample as u16,
										};

										chunk_read_bytes(&mut i, &sub.chunk_data, &mut sample.name);
										chunk_read_u32(&mut i, &sub.chunk_data, &mut sample.start);
										chunk_read_u32(&mut i, &sub.chunk_data, &mut sample.end);
										chunk_read_u32(&mut i, &sub.chunk_data, &mut sample.start_loop);
										chunk_read_u32(&mut i, &sub.chunk_data, &mut sample.end_loop);
										chunk_read_u32(&mut i, &sub.chunk_data, &mut sample.sample_rate);

										// can't be fucked to make separate functions for these.
										// SAFETY: i8 and u8 don't have any invalid bit patterns
										unsafe {
											read_from_chunk(&mut i, &sub.chunk_data, &mut sample.original_pitch);
											read_from_chunk(&mut i, &sub.chunk_data, &mut sample.pitch_correction);
										}

										chunk_read_u16(&mut i, &sub.chunk_data, &mut sample.sample_link);
										chunk_read_u16(&mut i, &sub.chunk_data, &mut sample.sample_type);

										result.shdr.try_push(sample)?;
									}
								}

								_ => {}
							}
						}
					}

					_ => {}
				}
			}

			if result.version_major == 3 {
				result.decompressed(smpl_compressed, load_progress)
			} else {
				Ok(result)
			}
		}

		fn decompressed(mut self, smpl: Vec<u8>, load_progress: Arc<AtomicU32>) -> std::io::Result<Self> {
			let mut pcm_range_end = 0;

			// find non-compressed samples. these must be at the beginning of the SMPL subchunk
			for header in self.shdr.iter() {
				if (header.sample_type & SAMPLE_TYPE_COMPRESSED) == 0 {
					pcm_range_end = pcm_range_end.max(header.end as usize);
				}
		 	}

			// just in case
			self.smpl.clear();

			if pcm_range_end != 0 {	
				self.smpl.try_reserve_exact(pcm_range_end)?;

				pcm_range_end *= 2; // convert from short index to byte index
				
				let slice = unsafe { &mut as_u8_slice_mut(self.smpl.spare_capacity_mut())[0..pcm_range_end] };

				slice.copy_from_slice(&smpl[0..pcm_range_end]);

				unsafe {
					self.smpl.set_len(pcm_range_end);
				}
			}

			let shdr_len = self.shdr.len();

			// now go through samples and decompress any ogg ones
			for (n, header) in self.shdr.iter_mut().enumerate() {
				if (header.sample_type & SAMPLE_TYPE_COMPRESSED) == 0 {
					continue
				}

				load_progress.store((n as u64 * 1000 / shdr_len as u64) as u32 | OGG_FLAG, Ordering::Relaxed);

				let slice = Cursor::new(&smpl[header.start as usize..header.end as usize]);

				let Ok(mut reader) = OggStreamReader::new(slice) else {
					return Err(std::io::Error::new(ErrorKind::InvalidData, "failed to create OGG stream reader!"));
				};

				if reader.ident_hdr.audio_channels != 1 {
					return Err(std::io::Error::new(ErrorKind::InvalidData, "multiple channels not supported!"));
				}

				let new_start = self.smpl.len();
				
				while let Ok(Some(mut packet)) = reader.read_dec_packet() {
					self.smpl.append(&mut packet[0]);
				}

				header.start = new_start as u32;
				header.end = self.smpl.len() as u32;
				header.start_loop += header.start;
				header.end_loop += header.start;
			}

			self.smpl.shrink_to_fit();

			Ok(self)
		}
	}

	#[derive(Default)]
	pub struct PresetHeader {
		pub name: [u8; 20],
		pub preset: u16,
		pub bank: u16,
		pub preset_bag_idx: u16,
		pub library: u32,
		pub genre: u32,
		pub morphology: u32,
	}

	pub struct Bag {
		pub gen_idx: u16,
		pub mod_idx: u16,
	}

	#[repr(C)]
	#[derive(Clone, Copy, PartialEq, Eq)]
	pub struct RangesType {
		pub lo: u8,
		pub hi: u8,
	}

	#[repr(C)]
	#[derive(Clone, Copy)]
	pub union GenAmountType {
		pub ranges: RangesType,
		pub sh_amount: i16,
		pub w_amount: u16,
	}

	impl Debug for GenAmountType {
		fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
			write!(f, "{}", self.w_amount())
		}
	}

	impl GenAmountType {
		// SAFETY:
		//
		// this entire union is repr(C), and all bit
		// patterns are valid for every union field

		pub fn ranges(&self) -> RangesType {
			unsafe { self.ranges }
		}

		pub fn sh_amount(&self) -> i16 {
			unsafe { self.sh_amount }
		}

		pub fn w_amount(&self) -> u16 {
			unsafe { self.w_amount }
		}

		pub fn ranges_mut(&mut self) -> &mut RangesType {
			unsafe { &mut self.ranges }
		}

		pub fn sh_amount_mut(&mut self) -> &mut i16 {
			unsafe { &mut self.sh_amount }
		}

		pub fn w_amount_mut(&mut self) -> &mut u16 {
			unsafe { &mut self.w_amount }
		}
	}

	#[derive(PartialEq, Eq, Clone, Copy, Debug)]
	pub enum GenType {
		Range,
		Short,
		Word
	}

	impl Default for GenAmountType {
		fn default() -> Self {
			GenAmountType {
				w_amount: 0,
			}
		}
	}

	#[repr(u16)]
	#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Debug)]
	pub enum Gen {
		StartAddrsOffset = 0,
		EndAddrsOffset = 1,
		StartLoopAddrsOffset = 2,
		EndLoopAddrsOffset = 3,
		StartAddrsCoarseOffset = 4,
		ModLfoToPitch = 5,
		VibLfoToPitch = 6,
		ModEnvToPitch = 7,
		InitialFilterFc = 8,
		InitialFilterQ = 9,
		ModLfoToFilterFc = 10,
		ModEnvToFilterFc = 11,
		EndAddrsCoarseOffset = 12,
		ModLfoToVolume = 13,
		Unused1 = 14,
		ChorusEffectsSend = 15,
		ReverbEffectsSend = 16,
		Pan = 17,
		Unused2 = 18,
		Unused3 = 19,
		Unused4 = 20,
		DelayModLFO = 21,
		FreqModLFO = 22,
		DelayVibLFO = 23,
		FreqVibLFO = 24,
		DelayModEnv = 25,
		AttackModEnv = 26,
		HoldModEnv = 27,
		DecayModEnv = 28,
		SustainModEnv = 29,
		ReleaseModEnv = 30,
		KeynumToModEnvHold = 31,
		KeynumToModEnvDecay = 32,
		DelayVolEnv = 33,
		AttackVolEnv = 34,
		HoldVolEnv = 35,
		DecayVolEnv = 36,
		SustainVolEnv = 37,
		ReleaseVolEnv = 38,
		KeynumToVolEnvHold = 39,
		KeynumToVolEnvDecay = 40,
		Instrument = 41,
		Unused5 = 42,
		KeyRange = 43,
		VelRange = 44,
		StartLoopAddrsCoarseOffset = 45,
		KeyNum = 46,
		Velocity = 47,
		InitialAttenuation = 48,
		EndLoopAddrsCoarseOffset = 50,
		CoarseTune = 51,
		FineTune = 52,
		SampleID = 53,
		SampleModes = 54,
		Unused6 = 55,
		ScaleTuning = 56,
		ExclusiveClass = 57,
		OverridingRootKey = 58,
		#[default]
		GeneratorMax = 59,
	}

	#[repr(u8)]
	pub enum GeneralControlPalette {
		NoController = 0,
		NoteOnVelocity = 2,
		NoteOnKeyNumber = 3,
		PolyPressure = 10,
		ChannelPressure = 13,
		PitchWheel = 14,
		PitchWheelSensitivity = 16,
	}


	impl TryFrom<u8> for GeneralControlPalette {
		type Error = ();

		fn try_from(i: u8) -> Result<GeneralControlPalette, ()> {
			match i {
				0 => Ok(GeneralControlPalette::NoController),
				2 => Ok(GeneralControlPalette::NoteOnVelocity),
				3 => Ok(GeneralControlPalette::NoteOnKeyNumber),
				10 => Ok(GeneralControlPalette::PolyPressure),
				13 => Ok(GeneralControlPalette::ChannelPressure),
				14 => Ok(GeneralControlPalette::PitchWheel),
				16 => Ok(GeneralControlPalette::PitchWheelSensitivity),
				_ => Err(())
			}
		}
	}


	#[repr(u8)]
	pub enum ControllerType {
		Linear = 0,
		Concave = 1,
		Convex = 2,
		Switch = 3,
	}


	impl TryFrom<u8> for ControllerType {
		type Error = ();

		fn try_from(i: u8) -> Result<ControllerType, ()> {
			match i {
				0 => Ok(ControllerType::Linear),
				1 => Ok(ControllerType::Concave),
				2 => Ok(ControllerType::Convex),
				3 => Ok(ControllerType::Switch),
				_ => Err(())
			}
		}
	}


	#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Debug)]
	pub struct Mod {
		data: i16,
	}

	impl Mod {
		pub fn is_midi_palette_enabled(&self) -> bool {
			(self.data & 0b10000000) != 0
		}

		pub fn is_general_palette_enabled(&self) -> bool {
			!self.is_midi_palette_enabled()
		}

		pub fn direction_bit(&self) -> bool {
			(self.data & 0b100000000) != 0
		}

		pub fn polarity_bit(&self) -> bool {
			(self.data & 0b1000000000) != 0
		}

		pub fn controller_source(&self) -> u8 {
			(self.data & 0b1111111) as u8
		}

		pub fn general_palette_value(&self) -> GeneralControlPalette {
			GeneralControlPalette::try_from(self.controller_source()).unwrap_or(GeneralControlPalette::NoController)
		}

		pub fn controller_type(&self) -> ControllerType {
			ControllerType::try_from((self.data >> 10) as u8).unwrap_or(ControllerType::Linear)
		}
	}

	#[repr(u16)]
	#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Debug)]
	pub enum Transform {
		#[default]
		Linear = 0,
		Absolute = 2,
	}

	#[repr(u16)]
	#[derive(Debug)]
	pub enum SampleType {
		MonoSample = 0x1,
		RightSample = 0x2,
		LeftSample = 0x4,
		LinkedSample = 0x8,
		CompressedSample = 0x10, // sf3
		RomSample = 0x8000, // unsupported
	}

	pub const SAMPLE_TYPE_MONO: u16         = 0x1;
	pub const SAMPLE_TYPE_RIGHT: u16        = 0x2;
	pub const SAMPLE_TYPE_LEFT: u16         = 0x4;
	pub const SAMPLE_TYPE_LINKED: u16       = 0x8;
	pub const SAMPLE_TYPE_COMPRESSED: u16   = 0x10;
	pub const SAMPLE_TYPE_ROM: u16          = 0x8000;

	/*
	#[repr(u16)]
	#[derive(Debug)]
	pub enum SampleLink {
		MonoSample = 1,
		RightSample = 2,
		LeftSample = 4,
		LinkedSample = 8,
		RomMonoSample = 0x8001,
		RomRightSample = 0x8002,
		RomLeftSample = 0x8004,
		RomLinkedSample = 0x8008,
	}
	*/

	pub struct Inst {
		pub name: [u8; 20],
		pub inst_bag_idx: u16,
	}

	#[derive(Clone, Copy, Debug)]
	pub struct GenList {
		pub oper: Gen,
		pub amount: GenAmountType,
	}

	impl PartialEq for GenList {
		fn eq(&self, other: &Self) -> bool {
			self.oper == other.oper
		}
	}

	#[derive(Clone, Copy, Default, Debug)]
	pub struct ModList {
		pub src_oper: Mod,
		pub dest_oper: Gen,
		pub mod_amt: i16,
		pub mod_amt_src_oper: Mod,
		pub mod_amt_trans_oper: Transform,
	}

	impl ModList {
		pub const fn new(src_oper: i16, dest_oper: Gen, mod_amt: i16, mod_amt_src_oper: i16, mod_amt_trans_oper: Transform) -> Self {
			Self {
				src_oper: Mod { data: src_oper },
				dest_oper,
				mod_amt,
				mod_amt_src_oper: Mod { data: mod_amt_src_oper },
				mod_amt_trans_oper
			}
		}
	}

	impl PartialEq for ModList {
		fn eq(&self, other: &Self) -> bool {
			self.src_oper == other.src_oper &&
			self.dest_oper == other.dest_oper &&
			self.mod_amt_src_oper == other.mod_amt_src_oper
		}
	}

	impl Eq for ModList {}

	impl Hash for ModList {
		fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
			self.src_oper.hash(state);
			self.dest_oper.hash(state);
			self.mod_amt_src_oper.hash(state);
		}
	}

	#[derive(Debug)]
	pub struct GenValues {
		// using the raw value here for better rust-analyzer lints: will error in GenValues::new() if incorrect
		values: [GenAmountType; 59],
	}

	impl Index<Gen> for GenValues {
		type Output = GenAmountType;

		fn index(&self, index: Gen) -> &Self::Output {
			&self.values[index as usize]
		}
	}

	impl IndexMut<Gen> for GenValues {
		fn index_mut(&mut self, index: Gen) -> &mut Self::Output {
			&mut self.values[index as usize]
		}
	}

	impl GenValues {
		pub fn new() -> Self {
			let mut result = GenValues { values: [Default::default(); Gen::GeneratorMax as usize] };

			// set default values for generators

			result[Gen::InitialFilterFc].sh_amount = 13500;

			result[Gen::DelayModLFO].sh_amount = -12000;
			result[Gen::DelayVibLFO].sh_amount = -12000;

			result[Gen::DelayModEnv].sh_amount = -12000;
			result[Gen::AttackModEnv].sh_amount = -12000;
			result[Gen::HoldModEnv].sh_amount = -12000;
			result[Gen::DecayModEnv].sh_amount = -12000;
			result[Gen::SustainModEnv].sh_amount = 0;
			result[Gen::ReleaseModEnv].sh_amount = -12000;

			result[Gen::DelayVolEnv].sh_amount = -12000;
			result[Gen::AttackVolEnv].sh_amount = -12000;
			result[Gen::HoldVolEnv].sh_amount = -12000;
			result[Gen::DecayVolEnv].sh_amount = -12000;
			result[Gen::SustainVolEnv].sh_amount = 0;
			result[Gen::ReleaseVolEnv].sh_amount = -12000;

			result[Gen::KeyRange].ranges.lo = 0;
			result[Gen::KeyRange].ranges.hi = 127;
			result[Gen::VelRange].ranges.lo = 0;
			result[Gen::VelRange].ranges.hi = 127;

			result[Gen::KeyNum].sh_amount = -1;
			result[Gen::Velocity].sh_amount = -1;

			result[Gen::ScaleTuning].sh_amount = 100;

			result[Gen::OverridingRootKey].sh_amount = -1;

			result
		}

		// reset generator values, preserving keynum and velocity
		pub fn reset(&mut self) {
			let keynum = self[Gen::KeyNum];
			let vel = self[Gen::Velocity];
			
			*self = GenValues::new();

			self[Gen::KeyNum] = keynum;
			self[Gen::Velocity] = vel;
		}

		pub fn is_realtime_gen(generator: Gen) -> bool {
			match generator {
				Gen::KeyRange |
				Gen::VelRange |
				Gen::KeyNum |
				Gen::Velocity |
				Gen::SampleID |
				Gen::SampleModes |
				Gen::ScaleTuning |
				Gen::ExclusiveClass |
				Gen::OverridingRootKey |

					// below are technically defined as realtime by the spec, but
					// have no clear meaning when changed midway through a voice
					// or don't support it internally yet
				Gen::DelayModLFO |
				Gen::DelayVibLFO |
				Gen::DelayVolEnv |
				Gen::DelayModEnv |
				Gen::AttackVolEnv |
				Gen::AttackModEnv |
				Gen::HoldVolEnv |
				Gen::HoldModEnv |
				Gen::SustainVolEnv |
				Gen::SustainModEnv |
				Gen::ReleaseVolEnv |
				Gen::ReleaseModEnv
					=> false,

				_
					=> true,
			}
		}

		pub fn get_gen_type(generator: Gen) -> GenType {
			match generator {
				Gen::KeyRange | Gen::VelRange => GenType::Range,

				Gen::StartAddrsOffset |
				Gen::StartAddrsCoarseOffset |
				Gen::InitialFilterQ |
				Gen::ChorusEffectsSend |
				Gen::ReverbEffectsSend |
				Gen::SustainModEnv |
				Gen::SustainVolEnv |
				Gen::InitialAttenuation |
				Gen::ScaleTuning |
				Gen::ExclusiveClass => GenType::Word,

				_ => GenType::Short,
			}

		}

		pub(crate) fn apply_mod_list(&mut self, modlist: &ModList, amt_val: f64, mod_amt_val: f64) {
			let dest = modlist.dest_oper;
			let amt = modlist.mod_amt;
			let gen_type = GenValues::get_gen_type(dest);

			if gen_type == GenType::Range {
				return
			}

			let mut value = amt as f64 * amt_val * mod_amt_val;

			if modlist.mod_amt_trans_oper == Transform::Absolute {
				value = value.abs();
			}

			let value = value.clamp(i16::MIN as f64, i16::MAX as f64) as i16;

			match gen_type {
				GenType::Word => {
					*self[dest].w_amount_mut() = self[dest].w_amount().wrapping_add_signed(value);
				}

				GenType::Short => {
					*self[dest].sh_amount_mut() = self[dest].sh_amount().saturating_add(value);
				}

				_ => unreachable!(),
			}
		}

		pub fn apply_gen_list(&mut self, list: &GenList, abs: bool) {
			if abs {
				self[list.oper] = list.amount;
			} else {
				match Self::get_gen_type(list.oper) {
					GenType::Range => {
						let current = self[list.oper].ranges_mut();
						let update = list.amount.ranges();

						current.lo = current.lo.max(update.lo);
						current.hi = current.hi.min(update.hi);
					}

					GenType::Word => {
						self[list.oper].w_amount = self[list.oper].w_amount().wrapping_add(list.amount.w_amount());
					}

					GenType::Short => {
						let mut result = self[list.oper].sh_amount().saturating_add(list.amount.sh_amount());

						match list.oper {
							Gen::ModLfoToPitch |
							Gen::VibLfoToPitch |
							Gen::ModEnvToPitch |
							Gen::ModLfoToFilterFc |
							Gen::ModEnvToFilterFc => {
								result = result.clamp(-12000, 12000);
							}

							Gen::InitialFilterFc => {
								result = result.clamp(1500, 13500);
							}

							_ => {}
						}

						self[list.oper].sh_amount = result;
					}
				}
			}
		}
	}

	// NOTE: Sinc interpolation is unimplemented
	#[repr(C)]
	#[derive(Clone, Copy, Debug, PartialEq)]
	pub enum ResampleMethod {
		Linear = 0,
		Hermite = 1,
		Sinc16 = 2,
		Sinc32 = 3,
		Sinc64 = 4,
		Nearest = 5,
		Gaussian = 6,
	}

	impl TryFrom<i32> for ResampleMethod {
		type Error = ();

		fn try_from(value: i32) -> Result<ResampleMethod, ()> {
			match value {
				0 => Ok(Self::Linear),
				1 => Ok(Self::Hermite),
				2 => Ok(Self::Sinc16),
				3 => Ok(Self::Sinc32),
				4 => Ok(Self::Sinc64),
				5 => Ok(Self::Nearest),
				6 => Ok(Self::Gaussian),
				_ => Err(()),
			}
		}
	}

	impl std::fmt::Display for ResampleMethod {
		fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
			match self {
				ResampleMethod::Linear => write!(f, "linear"),
				ResampleMethod::Hermite => write!(f, "hermite"),
				ResampleMethod::Sinc16 => write!(f, "sinc16"),
				ResampleMethod::Sinc32 => write!(f, "sinc32"),
				ResampleMethod::Sinc64 => write!(f, "sinc64"),
				ResampleMethod::Nearest => write!(f, "nearest"),
				ResampleMethod::Gaussian => write!(f, "gaussian"),
			}
		}
	}

	// courtesy of https://problemkaputt.de/fullsnes.htm#snesapudspbrrpitch
	const GAUSSIAN_TABLE: &[i32] = &[
		0x000,0x000,0x000,0x000,0x000,0x000,0x000,0x000,0x000,0x000,0x000,0x000,0x000,0x000,0x000,0x000,
		0x001,0x001,0x001,0x001,0x001,0x001,0x001,0x001,0x001,0x001,0x001,0x002,0x002,0x002,0x002,0x002,
		0x002,0x002,0x003,0x003,0x003,0x003,0x003,0x004,0x004,0x004,0x004,0x004,0x005,0x005,0x005,0x005,
		0x006,0x006,0x006,0x006,0x007,0x007,0x007,0x008,0x008,0x008,0x009,0x009,0x009,0x00A,0x00A,0x00A,
		0x00B,0x00B,0x00B,0x00C,0x00C,0x00D,0x00D,0x00E,0x00E,0x00F,0x00F,0x00F,0x010,0x010,0x011,0x011,
		0x012,0x013,0x013,0x014,0x014,0x015,0x015,0x016,0x017,0x017,0x018,0x018,0x019,0x01A,0x01B,0x01B,
		0x01C,0x01D,0x01D,0x01E,0x01F,0x020,0x020,0x021,0x022,0x023,0x024,0x024,0x025,0x026,0x027,0x028,
		0x029,0x02A,0x02B,0x02C,0x02D,0x02E,0x02F,0x030,0x031,0x032,0x033,0x034,0x035,0x036,0x037,0x038,
		0x03A,0x03B,0x03C,0x03D,0x03E,0x040,0x041,0x042,0x043,0x045,0x046,0x047,0x049,0x04A,0x04C,0x04D,
		0x04E,0x050,0x051,0x053,0x054,0x056,0x057,0x059,0x05A,0x05C,0x05E,0x05F,0x061,0x063,0x064,0x066,
		0x068,0x06A,0x06B,0x06D,0x06F,0x071,0x073,0x075,0x076,0x078,0x07A,0x07C,0x07E,0x080,0x082,0x084,
		0x086,0x089,0x08B,0x08D,0x08F,0x091,0x093,0x096,0x098,0x09A,0x09C,0x09F,0x0A1,0x0A3,0x0A6,0x0A8,
		0x0AB,0x0AD,0x0AF,0x0B2,0x0B4,0x0B7,0x0BA,0x0BC,0x0BF,0x0C1,0x0C4,0x0C7,0x0C9,0x0CC,0x0CF,0x0D2,
		0x0D4,0x0D7,0x0DA,0x0DD,0x0E0,0x0E3,0x0E6,0x0E9,0x0EC,0x0EF,0x0F2,0x0F5,0x0F8,0x0FB,0x0FE,0x101,
		0x104,0x107,0x10B,0x10E,0x111,0x114,0x118,0x11B,0x11E,0x122,0x125,0x129,0x12C,0x130,0x133,0x137,
		0x13A,0x13E,0x141,0x145,0x148,0x14C,0x150,0x153,0x157,0x15B,0x15F,0x162,0x166,0x16A,0x16E,0x172,
		0x176,0x17A,0x17D,0x181,0x185,0x189,0x18D,0x191,0x195,0x19A,0x19E,0x1A2,0x1A6,0x1AA,0x1AE,0x1B2,
		0x1B7,0x1BB,0x1BF,0x1C3,0x1C8,0x1CC,0x1D0,0x1D5,0x1D9,0x1DD,0x1E2,0x1E6,0x1EB,0x1EF,0x1F3,0x1F8,
		0x1FC,0x201,0x205,0x20A,0x20F,0x213,0x218,0x21C,0x221,0x226,0x22A,0x22F,0x233,0x238,0x23D,0x241,
		0x246,0x24B,0x250,0x254,0x259,0x25E,0x263,0x267,0x26C,0x271,0x276,0x27B,0x280,0x284,0x289,0x28E,
		0x293,0x298,0x29D,0x2A2,0x2A6,0x2AB,0x2B0,0x2B5,0x2BA,0x2BF,0x2C4,0x2C9,0x2CE,0x2D3,0x2D8,0x2DC,
		0x2E1,0x2E6,0x2EB,0x2F0,0x2F5,0x2FA,0x2FF,0x304,0x309,0x30E,0x313,0x318,0x31D,0x322,0x326,0x32B,
		0x330,0x335,0x33A,0x33F,0x344,0x349,0x34E,0x353,0x357,0x35C,0x361,0x366,0x36B,0x370,0x374,0x379,
		0x37E,0x383,0x388,0x38C,0x391,0x396,0x39B,0x39F,0x3A4,0x3A9,0x3AD,0x3B2,0x3B7,0x3BB,0x3C0,0x3C5,
		0x3C9,0x3CE,0x3D2,0x3D7,0x3DC,0x3E0,0x3E5,0x3E9,0x3ED,0x3F2,0x3F6,0x3FB,0x3FF,0x403,0x408,0x40C,
		0x410,0x415,0x419,0x41D,0x421,0x425,0x42A,0x42E,0x432,0x436,0x43A,0x43E,0x442,0x446,0x44A,0x44E,
		0x452,0x455,0x459,0x45D,0x461,0x465,0x468,0x46C,0x470,0x473,0x477,0x47A,0x47E,0x481,0x485,0x488,
		0x48C,0x48F,0x492,0x496,0x499,0x49C,0x49F,0x4A2,0x4A6,0x4A9,0x4AC,0x4AF,0x4B2,0x4B5,0x4B7,0x4BA,
		0x4BD,0x4C0,0x4C3,0x4C5,0x4C8,0x4CB,0x4CD,0x4D0,0x4D2,0x4D5,0x4D7,0x4D9,0x4DC,0x4DE,0x4E0,0x4E3,
		0x4E5,0x4E7,0x4E9,0x4EB,0x4ED,0x4EF,0x4F1,0x4F3,0x4F5,0x4F6,0x4F8,0x4FA,0x4FB,0x4FD,0x4FF,0x500,
		0x502,0x503,0x504,0x506,0x507,0x508,0x50A,0x50B,0x50C,0x50D,0x50E,0x50F,0x510,0x511,0x511,0x512,
		0x513,0x514,0x514,0x515,0x516,0x516,0x517,0x517,0x517,0x518,0x518,0x518,0x518,0x518,0x519,0x519,
	];

	#[derive(Debug)]
	pub struct Sample {
		pub name: [u8; 20],
		pub start: u32,
		pub end: u32,
		pub start_loop: u32,
		pub end_loop: u32,
		pub sample_rate: u32,
		pub original_pitch: u8,
		pub pitch_correction: i8,
		pub sample_link: u16,
		pub sample_type: u16,
	}

	impl Sample {
		pub fn is_rom(&self) -> bool {
			(self.sample_type & SAMPLE_TYPE_ROM) != 0
		}

		pub fn should_loop(sample_mode: i16, released: bool) -> bool {
			// NOTE: the spec technically says `sample_mode == 2` should not loop,
			// but in practice implementations seem to treat it like `sample_mode == 1`
			sample_mode == 1 || sample_mode == 2 || (sample_mode == 3 && !released)
		}

		fn offset_calc(origin: u32, gen_values: &GenValues, coarse: Gen, fine: Gen) -> u32 {
			let coarse_amt = gen_values[coarse].sh_amount() as i64;
			let fine_amt = gen_values[fine].sh_amount() as i64;

			(origin as i64 + 32768 * coarse_amt + fine_amt) as u32
		}

		pub fn get_start(&self, gen_values: &GenValues) -> u32 {
			Self::offset_calc(self.start, gen_values, Gen::StartAddrsCoarseOffset, Gen::StartAddrsOffset)
		}

		pub fn get_end(&self, gen_values: &GenValues) -> u32 {
			Self::offset_calc(self.end, gen_values, Gen::EndAddrsCoarseOffset, Gen::EndAddrsOffset)
		}

		pub fn get_start_loop(&self, gen_values: &GenValues) -> u32 {
			Self::offset_calc(self.start_loop, gen_values, Gen::StartLoopAddrsCoarseOffset, Gen::StartLoopAddrsOffset)
		}

		pub fn get_end_loop(&self, gen_values: &GenValues) -> u32 {
			Self::offset_calc(self.end_loop, gen_values, Gen::EndLoopAddrsCoarseOffset, Gen::EndLoopAddrsOffset)
		}

		pub fn get_remapped_offset(
			mut current_pos: f64,
			should_loop: bool,
			end: f64,
			start_loop: f64,
			end_loop: f64,
		) -> f64 {
			if should_loop && end_loop != start_loop {
				while current_pos >= end_loop {
					current_pos -= end_loop - start_loop;
				}

				current_pos
			} else {
				current_pos.min(end - 1.0)
			}
		}

		pub fn resample(
			&self,
			offset: &mut f64,
			phase_inc: f64,
			method: ResampleMethod,
			fetch_sample: impl Fn(usize) -> f32,
			out: &mut [f32],
			released: bool,
			gen_values: &GenValues,
		) {
			let mut start_offset = *offset;
			let start = self.get_start(gen_values);
			let end = self.get_end(gen_values).max(start);

			let start_loop = (self.get_start_loop(gen_values).max(start) - start) as f64;
			let mut end_loop = (self.get_end_loop(gen_values).min(end) - start) as f64;
			let end = (end - start) as f64;

			if end_loop < start_loop {
				end_loop = end;
			}

			let should_loop = Sample::should_loop(gen_values[Gen::SampleModes].sh_amount(), released);

			match method {
				ResampleMethod::Nearest => {
					for sample in out.iter_mut() {
						let s = fetch_sample(start_offset as usize);

						*sample = s;

						start_offset = Self::get_remapped_offset(
							start_offset + phase_inc,
							should_loop,
							end,
							start_loop,
							end_loop,
						)
					}
				}

				ResampleMethod::Linear => {
					for sample in out.iter_mut() {
						let next = Self::get_remapped_offset(
							start_offset + 1.0,
							should_loop,
							end,
							start_loop,
							end_loop,
						);

						let a = start_offset as usize;
						let b = next as usize;
						let t = start_offset - a as f64;

						let sa = fetch_sample(a);
						let sb = fetch_sample(b);

						let t = t as f32;
						let s = sa * (1.0 - t) + sb * t;

						*sample = s;

						start_offset = Self::get_remapped_offset(
							start_offset + phase_inc,
							should_loop,
							end,
							start_loop,
							end_loop,
						)
					}
				}

				ResampleMethod::Hermite => {
					for sample in out.iter_mut() {
						let y = |i| fetch_sample(Self::get_remapped_offset(
								start_offset + i,
								should_loop,
								end,
								start_loop,
								end_loop,
							) as usize);

						// compensate for phase difference
						let ofs = -(start_offset as isize).min(2) as f64;

						let x = start_offset.fract() as f32;
						let ym2 = y(ofs+0.0);
						let ym1 = y(ofs+1.0);
						let y0 = y(ofs+2.0);
						let y1 = y(ofs+3.0);
						let y2 = y(ofs+4.0);
						let y3 = y(ofs+5.0);

						let c0 = y0;
						let c1 = 1.0/12.0*(ym2-y2) + 2.0/3.0*(y1-ym1);
						let c2 = 5.0/4.0*ym1 - 7.0/3.0*y0 + 5.0/3.0*y1 - 0.5*y2 + 1.0/12.0*y3 - 1.0/6.0*ym2;
						let c3 = 1.0/12.0*(ym2-y3) + 7.0/12.0*(y2-ym1) + 4.0/3.0*(y0-y1);

						*sample = ((c3*x+c2)*x+c1)*x+c0;

						start_offset = Self::get_remapped_offset(
							start_offset + phase_inc,
							should_loop,
							end,
							start_loop,
							end_loop,
						)
					}
				}

				ResampleMethod::Gaussian => {
					let maprange = i16::MAX as f32;
					let inv_maprange = 1.0 / maprange;

					for sample in out.iter_mut() {
						let y = |i| (fetch_sample(Self::get_remapped_offset(
								start_offset + i as f64,
								should_loop,
								end,
								start_loop,
								end_loop,
							) as usize) * maprange) as i32;


						let x0 = y(0);
						let x1 = y(1);
						let x2 = y(2);
						let x3 = y(3);
						
						let f = ((start_offset.fract() * 4096.0) as usize >> 4) & 0xFF;

						let mut s = 0;

						// SAFETY: `f` is always <= 0xFF, so none of these indices
						// can leave the range 0x0..0x200
						unsafe {
							s += (GAUSSIAN_TABLE.get_unchecked(0x0FF - f) * x0) >> 10;
							s += (GAUSSIAN_TABLE.get_unchecked(0x1FF - f) * x1) >> 10;
							s += (GAUSSIAN_TABLE.get_unchecked(0x100 + f) * x2) >> 10;
							s += (GAUSSIAN_TABLE.get_unchecked(0x000 + f) * x3) >> 10;
						}
						s >>= 1;

						*sample = s as f32 * inv_maprange;

						start_offset = Self::get_remapped_offset(
							start_offset + phase_inc,
							should_loop,
							end,
							start_loop,
							end_loop,
						)
					}
				}

				ResampleMethod::Sinc16 | ResampleMethod::Sinc32 | ResampleMethod::Sinc64 => {}
			}

			*offset = start_offset;
		}
	}