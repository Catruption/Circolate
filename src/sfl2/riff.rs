// `sfl2`'s implementation of the RIFF format, which SoundFonts are stored in
//
// this was ported from the C++ implementation in favour of using an existing RIFF crate,
// due to their APIs not being easily compatible, and a need for deeper control to
// implement certain SF2-related optimizations (specifically, reading the `smpl` chunk as
// a Vec<i16> directly, rather than having to clone potentially hundreds of megabytes later)
//
// definitely not a world-class implementation or anything,
// but it's succinct and works for our purposes.

use std::{fs::File, io::{BufReader, Read, Seek}, path::Path, sync::atomic::{AtomicU32, Ordering}};

const READ_CHUNK_SIZE: usize = 2 << 14;
pub const SM24_FLAG: u32 = 1 << 30;
pub const OGG_FLAG: u32 = 1 << 31;

#[repr(C)]
#[derive(Copy, Clone)]
pub union Fcc {
	dw: u32,
	c: [u8; 4],
}

impl Fcc {
	fn new() -> Self {
		return Self {
			dw: 0
		}
	}
}

pub struct RiffChunk {
	pub chunk_id: Fcc,
	pub sub_id: Fcc,
	pub sub_chunks: Vec<RiffChunk>,
	pub chunk_data: Vec<u8>,
}

// for smpl_opt: access [u16] as [u8]
unsafe fn as_u8_slice_mut<T>(from: &mut [T]) -> &mut [u8] {
    let len = from.len().checked_mul(size_of::<T>()).unwrap();
    let ptr: *mut u8 = from.as_mut_ptr().cast();
    unsafe { std::slice::from_raw_parts_mut(ptr, len) }
}

impl RiffChunk {
	pub fn load_from_file(path: &Path, smpl_opt: &mut Vec<i16>, smpl_opt_enabled: &mut bool, use_sm24: bool, load_progress: &AtomicU32) -> std::io::Result<Self> {
		let file = File::open(path)?;
		let mut file = BufReader::with_capacity(READ_CHUNK_SIZE, file);

		Self::read_chunk(&mut file, smpl_opt, smpl_opt_enabled, use_sm24, load_progress)
	}

	fn read_chunk(
		file: &mut (impl Read + Seek),
		smpl_opt: &mut Vec<i16>,
		smpl_opt_enabled: &mut bool,
		use_sm24: bool,
		load_progress: &AtomicU32
	) -> std::io::Result<Self> {
		let mut result = Self {
			chunk_id: Fcc::new(),
			sub_chunks: vec![],
			sub_id: Fcc::new(),
			chunk_data: vec![]
		};

		// read chunk id
		unsafe {
			file.read_exact(&mut result.chunk_id.c)?;
		}

		// read length
		let mut len_bytes = [0u8; 4];
		file.read_exact(&mut len_bytes)?;
		let len = u32::from_le_bytes(len_bytes) as u64;

		if result.has_subchunks() {
			let start = file.stream_position()?;
			let mut current = start;

			unsafe {
				file.read_exact(&mut result.sub_id.c)?;
			}

			while current - start < len {
				let subchunk = RiffChunk::read_chunk(file, smpl_opt, smpl_opt_enabled, use_sm24, load_progress)?;

				result.sub_chunks.push(subchunk);

				if current == file.stream_position()? {
					break
				}

				current = file.stream_position()?;
			}
		} else {
			// no subchunks, just read into chunk_data

			if result.chunk_id_str() == "smpl" {
				// smpl optimization - read directly into i16 buffer instead of typical u8 buffer
				let slice;
				let smpl_len = len as usize / 2;

				if *smpl_opt_enabled {
					if len % 2 != 0 {
						return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid smpl buffer!"))
					}

					if len >= usize::MAX as u64 {
						return Err(std::io::Error::new(std::io::ErrorKind::OutOfMemory, "len >= usize::MAX"))
					}

					// SAFETY: this is apparently unstable-but-currently-fine,
					// we're passing an uninitialized buffer into read_exact,
					// so we avoid the overhead of initializing potentially huge
					// amounts of sample data to 0 before overwriting it
					//
					// https://users.rust-lang.org/t/how-to-read-from-tcpstream-and-append-to-vec-u8-efficiently/89059/14
					*smpl_opt = vec![];
					smpl_opt.try_reserve_exact(smpl_len)?;

					slice = unsafe { &mut as_u8_slice_mut(smpl_opt.spare_capacity_mut())[0..len as usize] };
				} else {
					// reading sf3 which might contain compressed samples - no smpl_opt here :(
					
					result.chunk_data.try_reserve_exact(len as usize)?;
					slice = unsafe { &mut as_u8_slice_mut(result.chunk_data.spare_capacity_mut())[0..len as usize] };
				}

				let mut n = 0;

				while n < slice.len() {
					// we read in chunks (equal to BufReader buffer size)
					// so we can update load_progress regularly
					let end = (n + READ_CHUNK_SIZE).min(slice.len());
					let bytes_read = file.read(&mut slice[n..end])?;

					n += bytes_read;
					load_progress.store((n as u64 * 1000 / slice.len() as u64) as u32, Ordering::Relaxed);

					if bytes_read == 0 {
						return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "unexpected end of smpl buffer"))
					}
				}

				if *smpl_opt_enabled {
					unsafe {
						smpl_opt.set_len(smpl_len);
					}
				} else {
					unsafe {
						result.chunk_data.set_len(len as usize);
					}
				}
			} else if result.chunk_id_str() == "sm24" {
				if use_sm24 {
					result.chunk_data.try_reserve_exact(len as usize)?;

					// SAFETY: see smpl chunk above
					let slice = unsafe { &mut as_u8_slice_mut(result.chunk_data.spare_capacity_mut())[0..len as usize] };

					let mut n = 0;

					while n < slice.len() {
						let end = (n + READ_CHUNK_SIZE).min(slice.len());
						let bytes_read = file.read(&mut slice[n..end])?;

						n += bytes_read;
						load_progress.store((n as u64 * 1000 / slice.len() as u64) as u32 | SM24_FLAG, Ordering::Relaxed);

						if bytes_read == 0 {
							return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "unexpected end of sm24 buffer"))
						}
					}

					unsafe {
						result.chunk_data.set_len(len as usize);
					}
				} else {
					file.seek_relative(len as i64)?;
				}
			} else {
				result.chunk_data.try_reserve_exact(len as usize)?;
				result.chunk_data.resize(len as usize, 0);

				file.read_exact(&mut result.chunk_data)?;
				
				if result.chunk_id_str() == "ifil" {
					if result.chunk_data[0] == 3 {
						// SF3 file spotted. disable smpl_opt
						*smpl_opt_enabled = false;
					}
				}
			}

			if len % 2 != 0 {
				file.seek_relative(1)?;
			}
		}

		Ok(result)
	}

	pub fn chunk_id_str(&self) -> &str {
		str::from_utf8(unsafe { &self.chunk_id.c }).unwrap_or("")
	}

	pub fn sub_id_str(&self) -> &str {
		str::from_utf8(unsafe { &self.sub_id.c }).unwrap_or("")
	}

	pub fn has_subchunks(&self) -> bool {
		matches!(self.chunk_id_str(), "RIFF" | "LIST")
	}

	pub fn get_subchunk(&self, id: &str) -> Option<&RiffChunk> {
		self.sub_chunks.iter().find(|chunk| unsafe { &chunk.chunk_id.c } == id.as_bytes())
	}
}