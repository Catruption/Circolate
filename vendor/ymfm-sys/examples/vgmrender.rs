//! Rust port of ymfm's `vgmrender` example
//! (components/ymfm/examples/vgmrender/vgmrender.cpp).
//!
//! Renders a VGM chip-command log to a 16-bit stereo WAV file using the
//! `ymfm-sys` cxx bindings. Compressed (.vgz) files are not supported.
//! Instead, use a pipe, for example:
//! `gunzip -c example.vgz | vgmrender - -o example.wav`

use std::cell::RefCell;
use std::env;
use std::fs;
use std::io::{self, BufWriter, Read, Write};
use std::path::Path;
use std::process::ExitCode;
use std::rc::Rc;

use ymfm_sys::ffi::{self, AccessClass, ChipType};
use ymfm_sys::{ChipPtr, InterfaceCallbacks, InterfaceHandler};

/// 32.32 fixed-point emulated time, matching ymfm's `emulated_time`.
type EmulatedTime = i64;

/// A single active chip instance, matching vgmrender.cpp's `vgm_chip`:
/// register writes are queued and applied one at a time on each call to
/// `generate` (matching the pacing assumed by VGM files), and emulation is
/// resampled from the chip's native rate to the output rate.
struct ActiveChip {
    chip: ChipPtr,
    channels: usize,
    queue: std::collections::VecDeque<(u32, u8)>,
    pos: EmulatedTime,
    step: EmulatedTime,
    native: Vec<i32>,
    handler_state: Rc<RefCell<VgmHandlerState>>,
    pcm_offset: Rc<RefCell<u32>>,
}

impl ActiveChip {
    fn new(chip_type: ChipType, clock: u32) -> Self {
        let state = Rc::new(RefCell::new(VgmHandlerState {
            data: std::array::from_fn(|_| Vec::new()),
        }));
        let pcm_offset = Rc::new(RefCell::new(0u32));
        let chip = ffi::create_chip_with_callbacks(
            chip_type,
            clock,
            Box::new(InterfaceCallbacks::new(vgm_handler_with_state(Rc::clone(
                &state,
            )))),
        );
        let channels = chip.channels() as usize;
        let step: EmulatedTime = 0x1_0000_0000i64 / i64::from(chip.sample_rate());
        Self {
            chip,
            channels,
            queue: std::collections::VecDeque::new(),
            pos: 0,
            step,
            native: vec![0i32; channels],
            handler_state: state,
            pcm_offset,
        }
    }

    fn chip_type(&self) -> ChipType {
        self.chip.chip_type()
    }

    /// Queue a register write. `reg` encodes the register number in the low
    /// byte and the port index in bits 8-9. Applied on the next call to
    /// `generate`.
    fn write(&mut self, reg: u32, data: u8) {
        self.queue.push_back((reg, data));
    }

    fn write_data(&mut self, access: AccessClass, base: u32, data: &[u8]) {
        let mut state = self.handler_state.borrow_mut();
        for (index, value) in data.iter().copied().enumerate() {
            write_byte(&mut state, access, base + index as u32, value);
        }
    }

    fn seek_pcm(&mut self, pos: u32) {
        *self.pcm_offset.borrow_mut() = pos;
    }

    fn read_pcm(&mut self) -> u8 {
        let mut offset = self.pcm_offset.borrow_mut();
        let state = self.handler_state.borrow();
        let value = read_byte(&state, AccessClass::Pcm, *offset);
        *offset = offset.saturating_add(1);
        value
    }

    /// Advance emulation up to `output_start` and accumulate one stereo
    /// sample into `buffer[0]` (left) / `buffer[1]` (right), matching
    /// vgmrender.cpp's `vgm_chip::generate`.
    fn generate(
        &mut self,
        output_start: EmulatedTime,
        output_step: EmulatedTime,
        buffer: &mut [i32],
    ) {
        let _ = output_step;

        // dequeue at most one pending register write per output sample
        if let Some((reg, data)) = self.queue.pop_front() {
            let addr1 = 2 * ((reg >> 8) & 3);
            let data1 = (reg & 0xff) as u8;
            let addr2 = addr1
                + if self.chip_type() == ChipType::Ym2149 {
                    2
                } else {
                    1
                };
            self.chip.pin_mut().write(addr1, data1);
            self.chip.pin_mut().write(addr2, data);
        }

        // generate at the chip's native rate, catching up to output_start
        while self.pos <= output_start {
            self.chip.pin_mut().generate(&mut self.native);
            self.pos += self.step;
        }

        let channels = self.channels;
        let out = &self.native;
        match self.chip.chip_type() {
            ChipType::Ym2203 => {
                let sum = out[0] + out[1 % channels] + out[2 % channels] + out[3 % channels];
                buffer[0] += sum;
                buffer[1] += sum;
            }
            ChipType::Ym2608 | ChipType::Ym2610 => {
                buffer[0] += out[0] + out[2 % channels];
                buffer[1] += out[1 % channels] + out[2 % channels];
            }
            ChipType::Ymf278B => {
                buffer[0] += out[4 % channels];
                buffer[1] += out[5 % channels];
            }
            _ if channels == 1 => {
                buffer[0] += out[0];
                buffer[1] += out[0];
            }
            _ => {
                buffer[0] += out[0];
                buffer[1] += out[1 % channels];
            }
        }
    }
}

/// Read a little-endian u32 from `buffer` at `offset`, advancing `offset`.
fn read_u32(buffer: &[u8], offset: &mut usize) -> u32 {
    let value = u32::from_le_bytes(buffer[*offset..*offset + 4].try_into().unwrap());
    *offset += 4;
    value
}

/// Find the `index`-th active chip of the given category (0-based), matching
/// vgmrender.cpp's `find_chip`.
fn find_chip(
    chips: &mut [ActiveChip],
    category: ChipType,
    mut index: u8,
) -> Option<&mut ActiveChip> {
    for chip in chips.iter_mut() {
        if chip.chip_type() == category {
            if index == 0 {
                return Some(chip);
            }
            index -= 1;
        }
    }
    None
}

/// Write a register to the `index`-th active chip of the given category, matching
/// vgmrender.cpp's `write_chip`.
fn write_chip(chips: &mut [ActiveChip], category: ChipType, index: u8, reg: u32, data: u8) {
    if let Some(chip) = find_chip(chips, category, index) {
        chip.write(reg, data);
    }
}

/// Create 1 or 2 instances of the given chip type, matching vgmrender.cpp's
/// `add_chips` (bit 30 of the clock value requests a second chip instance).
fn add_chips(chips: &mut Vec<ActiveChip>, chip_type: ChipType, clock: u32, name: &str) {
    let clock_value = clock & 0x3fff_ffff;
    let num_chips = if clock & 0x4000_0000 != 0 { 2 } else { 1 };
    println!(
        "Adding {}{} @ {}Hz",
        if num_chips == 2 { "2 x " } else { "" },
        name,
        clock_value
    );
    for _ in 0..num_chips {
        chips.push(ActiveChip::new(chip_type, clock_value));
    }

    if chip_type == ChipType::Ym2608 {
        match fs::read("ym2608_adpcm_rom.bin") {
            Ok(rom) => {
                for chip in chips
                    .iter_mut()
                    .filter(|c| c.chip_type() == ChipType::Ym2608)
                {
                    chip.write_data(AccessClass::AdpcmA, 0, &rom);
                }
            }
            Err(_) => eprintln!("Warning: YM2608 enabled but ym2608_adpcm_rom.bin not found"),
        }
    }
}

/// Load ROM data for a data-block command, matching vgmrender.cpp's
/// `add_rom_data`: reads a (length, start) pair, then writes the remaining
/// `size` bytes to every active chip of the given category.
fn add_rom_data(
    chips: &mut [ActiveChip],
    category: ChipType,
    access: AccessClass,
    buffer: &[u8],
    mut local_offset: usize,
    size: u32,
) {
    let _length = read_u32(buffer, &mut local_offset);
    let start = read_u32(buffer, &mut local_offset);
    for index in 0..2u8 {
        if let Some(chip) = find_chip(chips, category, index) {
            chip.write_data(
                access,
                start,
                &buffer[local_offset..local_offset + size as usize],
            );
        }
    }
}

/// Parse the VGM header, creating any chips we recognize, and return the
/// offset at which the command stream begins.
fn parse_header(buffer: &[u8]) -> (u32, Vec<ActiveChip>) {
    let mut chips = Vec::new();
    let mut offset = 4usize;

    // +04: total size (informational only; buffer already holds the whole file)
    let _size = read_u32(buffer, &mut offset);

    // +08: version
    let version = read_u32(buffer, &mut offset);
    if version > 0x171 {
        eprintln!("Warning: version > 1.71 detected, some things may not work");
    }

    // +0C: SN76489 clock
    let clock = read_u32(buffer, &mut offset);
    if clock != 0 {
        eprintln!("Warning: clock for SN76489 specified ({clock}), but not supported");
    }

    // +10: YM2413 clock
    let clock = read_u32(buffer, &mut offset);
    if clock != 0 {
        add_chips(&mut chips, ChipType::Ym2413, clock, "YM2413");
    }

    // +14: GD3 offset / +18: total # samples / +1C: loop offset / +20: loop # samples
    // +24: rate / +28: SN76489 feedback/shift/flags
    let _gd3_offset = read_u32(buffer, &mut offset);
    let _total_samples = read_u32(buffer, &mut offset);
    let _loop_offset = read_u32(buffer, &mut offset);
    let _loop_samples = read_u32(buffer, &mut offset);
    let _rate = read_u32(buffer, &mut offset);
    let _sn76489_extra = read_u32(buffer, &mut offset);

    // +2C: YM2612 clock
    let clock = read_u32(buffer, &mut offset);
    if version >= 0x110 && clock != 0 {
        add_chips(&mut chips, ChipType::Ym2612, clock, "YM2612");
    }

    // +30: YM2151 clock
    let clock = read_u32(buffer, &mut offset);
    if version >= 0x110 && clock != 0 {
        add_chips(&mut chips, ChipType::Ym2151, clock, "YM2151");
    }

    // +34: VGM data offset
    let data_offset = read_u32(buffer, &mut offset);
    let data_start = if version < 0x150 {
        0x40
    } else {
        data_offset.wrapping_add(offset as u32 - 4)
    };

    // beyond this point, bail out early (returning what we have so far) if the
    // header is too short to contain the next field
    macro_rules! next_field {
        () => {{
            if offset + 4 > data_start as usize {
                return (data_start, chips);
            }
            read_u32(buffer, &mut offset)
        }};
    }

    // +38: Sega PCM clock
    let clock = read_u32(buffer, &mut offset);
    if version >= 0x151 && clock != 0 {
        eprintln!("Warning: clock for Sega PCM specified, but not supported");
    }

    // +3C: Sega PCM interface register
    let _sega_pcm_if = read_u32(buffer, &mut offset);

    // +40: RF5C68 clock
    let clock = next_field!();
    if version >= 0x151 && clock != 0 {
        eprintln!("Warning: clock for RF5C68 specified, but not supported");
    }

    // +44: YM2203 clock
    let clock = next_field!();
    if version >= 0x151 && clock != 0 {
        add_chips(&mut chips, ChipType::Ym2203, clock, "YM2203");
    }

    // +48: YM2608 clock
    let clock = next_field!();
    if version >= 0x151 && clock != 0 {
        add_chips(&mut chips, ChipType::Ym2608, clock, "YM2608");
    }

    // +4C: YM2610/2610B clock
    let clock = next_field!();
    if version >= 0x151 && clock != 0 {
        if clock & 0x8000_0000 != 0 {
            add_chips(&mut chips, ChipType::Ym2610B, clock, "YM2610B");
        } else {
            add_chips(&mut chips, ChipType::Ym2610, clock, "YM2610");
        }
    }

    // +50: YM3812 clock
    let clock = next_field!();
    if version >= 0x151 && clock != 0 {
        add_chips(&mut chips, ChipType::Ym3812, clock, "YM3812");
    }

    // +54: YM3526 clock
    let clock = next_field!();
    if version >= 0x151 && clock != 0 {
        add_chips(&mut chips, ChipType::Ym3526, clock, "YM3526");
    }

    // +58: Y8950 clock
    let clock = next_field!();
    if version >= 0x151 && clock != 0 {
        add_chips(&mut chips, ChipType::Y8950, clock, "Y8950");
    }

    // +5C: YMF262 clock
    let clock = next_field!();
    if version >= 0x151 && clock != 0 {
        add_chips(&mut chips, ChipType::Ymf262, clock, "YMF262");
    }

    // +60: YMF278B clock
    let clock = next_field!();
    if version >= 0x151 && clock != 0 {
        add_chips(&mut chips, ChipType::Ymf278B, clock, "YMF278B");
    }

    // +64: YMF271 clock
    let clock = next_field!();
    if version >= 0x151 && clock != 0 {
        eprintln!("Warning: clock for YMF271 specified, but not supported");
    }

    // +68: YMF280B clock
    let clock = next_field!();
    if version >= 0x151 && clock != 0 {
        eprintln!("Warning: clock for YMF280B specified, but not supported");
    }

    // +6C: RF5C164 clock
    let clock = next_field!();
    if version >= 0x151 && clock != 0 {
        eprintln!("Warning: clock for RF5C164 specified, but not supported");
    }

    // +70: PWM clock
    let clock = next_field!();
    if version >= 0x151 && clock != 0 {
        eprintln!("Warning: clock for PWM specified, but not supported");
    }

    // +74: AY8910 clock
    let clock = next_field!();
    if version >= 0x151 && clock != 0 {
        eprintln!("Warning: clock for AY8910 specified, substituting YM2149");
        add_chips(&mut chips, ChipType::Ym2149, clock, "YM2149");
    }

    // +78: AY8910 flags
    let _ay8910_flags = next_field!();

    // +7C: volume / loop info
    let volume_info = next_field!();
    if volume_info & 0xff != 0 {
        let modifier = 2f64.powf(f64::from(volume_info & 0xff) / 0x20 as f64);
        println!(
            "Volume modifier: {:02X} (={})",
            volume_info & 0xff,
            modifier as i32
        );
    }

    // +80: GameBoy DMG clock
    let clock = next_field!();
    if version >= 0x161 && clock != 0 {
        eprintln!("Warning: clock for GameBoy DMG specified, but not supported");
    }

    // +84: NES APU clock
    let clock = next_field!();
    if version >= 0x161 && clock != 0 {
        eprintln!("Warning: clock for NES APU specified, but not supported");
    }

    // +88: MultiPCM clock
    let clock = next_field!();
    if version >= 0x161 && clock != 0 {
        eprintln!("Warning: clock for MultiPCM specified, but not supported");
    }

    // +8C: uPD7759 clock
    let clock = next_field!();
    if version >= 0x161 && clock != 0 {
        eprintln!("Warning: clock for uPD7759 specified, but not supported");
    }

    // +90: OKIM6258 clock
    let clock = next_field!();
    if version >= 0x161 && clock != 0 {
        eprintln!("Warning: clock for OKIM6258 specified, but not supported");
    }

    // +94: OKIM6258 Flags / K054539 Flags / C140 Chip Type / reserved
    let _flags = next_field!();

    // +98: OKIM6295 clock
    let clock = next_field!();
    if version >= 0x161 && clock != 0 {
        eprintln!("Warning: clock for OKIM6295 specified, but not supported");
    }

    // +9C: K051649 clock
    let clock = next_field!();
    if version >= 0x161 && clock != 0 {
        eprintln!("Warning: clock for K051649 specified, but not supported");
    }

    // +A0: K054539 clock
    let clock = next_field!();
    if version >= 0x161 && clock != 0 {
        eprintln!("Warning: clock for K054539 specified, but not supported");
    }

    // +A4: HuC6280 clock
    let clock = next_field!();
    if version >= 0x161 && clock != 0 {
        eprintln!("Warning: clock for HuC6280 specified, but not supported");
    }

    // +A8: C140 clock
    let clock = next_field!();
    if version >= 0x161 && clock != 0 {
        eprintln!("Warning: clock for C140 specified, but not supported");
    }

    // +AC: K053260 clock
    let clock = next_field!();
    if version >= 0x161 && clock != 0 {
        eprintln!("Warning: clock for K053260 specified, but not supported");
    }

    // +B0: Pokey clock
    let clock = next_field!();
    if version >= 0x161 && clock != 0 {
        eprintln!("Warning: clock for Pokey specified, but not supported");
    }

    // +B4: QSound clock
    let clock = next_field!();
    if version >= 0x161 && clock != 0 {
        eprintln!("Warning: clock for QSound specified, but not supported");
    }

    // +B8: SCSP clock
    let clock = next_field!();
    if version >= 0x171 && clock != 0 {
        eprintln!("Warning: clock for SCSP specified, but not supported");
    }

    // +BC: extra header offset
    let _extra_header = next_field!();

    // +C0: WonderSwan clock
    let clock = next_field!();
    if version >= 0x171 && clock != 0 {
        eprintln!("Warning: clock for WonderSwan specified, but not supported");
    }

    // +C4: VSU clock
    let clock = next_field!();
    if version >= 0x171 && clock != 0 {
        eprintln!("Warning: clock for VSU specified, but not supported");
    }

    // +C8: SAA1099 clock
    let clock = next_field!();
    if version >= 0x171 && clock != 0 {
        eprintln!("Warning: clock for SAA1099 specified, but not supported");
    }

    // +CC: ES5503 clock
    let clock = next_field!();
    if version >= 0x171 && clock != 0 {
        eprintln!("Warning: clock for ES5503 specified, but not supported");
    }

    // +D0: ES5505/ES5506 clock
    let clock = next_field!();
    if version >= 0x171 && clock != 0 {
        eprintln!("Warning: clock for ES5505/ES5506 specified, but not supported");
    }

    // +D4: ES5503 output channels / ES5505/ES5506 amount of output channels / C352 clock divider
    let _es_channels = next_field!();

    // +D8: X1-010 clock
    let clock = next_field!();
    if version >= 0x171 && clock != 0 {
        eprintln!("Warning: clock for X1-010 specified, but not supported");
    }

    // +DC: C352 clock
    let clock = next_field!();
    if version >= 0x171 && clock != 0 {
        eprintln!("Warning: clock for C352 specified, but not supported");
    }

    // +E0: GA20 clock
    let clock = next_field!();
    if version >= 0x171 && clock != 0 {
        eprintln!("Warning: clock for GA20 specified, but not supported");
    }

    (data_start, chips)
}

/// Interpret the VGM command stream, driving all active chips and
/// accumulating interleaved stereo samples, matching vgmrender.cpp's
/// `generate_all`.
fn generate_all(
    buffer: &[u8],
    data_start: u32,
    output_rate: u32,
    chips: &mut [ActiveChip],
) -> Vec<i32> {
    let mut wav_buffer = Vec::new();
    let mut offset = data_start as usize;
    let mut done = false;
    let output_step: EmulatedTime = 0x1_0000_0000i64 / i64::from(output_rate);
    let mut output_pos: EmulatedTime = 0;

    while !done && offset < buffer.len() {
        let mut delay: i32 = 0;
        let cmd = buffer[offset];
        offset += 1;

        match cmd {
            // register writes: dd to register aa on the selected chip/port
            0x51 | 0xa1 => {
                write_chip(
                    chips,
                    ChipType::Ym2413,
                    cmd >> 7,
                    u32::from(buffer[offset]),
                    buffer[offset + 1],
                );
                offset += 2;
            }
            0x52 | 0xa2 => {
                write_chip(
                    chips,
                    ChipType::Ym2612,
                    cmd >> 7,
                    u32::from(buffer[offset]),
                    buffer[offset + 1],
                );
                offset += 2;
            }
            0x53 | 0xa3 => {
                write_chip(
                    chips,
                    ChipType::Ym2612,
                    cmd >> 7,
                    u32::from(buffer[offset]) | 0x100,
                    buffer[offset + 1],
                );
                offset += 2;
            }
            0x54 | 0xa4 => {
                write_chip(
                    chips,
                    ChipType::Ym2151,
                    cmd >> 7,
                    u32::from(buffer[offset]),
                    buffer[offset + 1],
                );
                offset += 2;
            }
            0x55 | 0xa5 => {
                write_chip(
                    chips,
                    ChipType::Ym2203,
                    cmd >> 7,
                    u32::from(buffer[offset]),
                    buffer[offset + 1],
                );
                offset += 2;
            }
            0x56 | 0xa6 => {
                write_chip(
                    chips,
                    ChipType::Ym2608,
                    cmd >> 7,
                    u32::from(buffer[offset]),
                    buffer[offset + 1],
                );
                offset += 2;
            }
            0x57 | 0xa7 => {
                write_chip(
                    chips,
                    ChipType::Ym2608,
                    cmd >> 7,
                    u32::from(buffer[offset]) | 0x100,
                    buffer[offset + 1],
                );
                offset += 2;
            }
            0x58 | 0xa8 => {
                write_chip(
                    chips,
                    ChipType::Ym2610,
                    cmd >> 7,
                    u32::from(buffer[offset]),
                    buffer[offset + 1],
                );
                offset += 2;
            }
            0x59 | 0xa9 => {
                write_chip(
                    chips,
                    ChipType::Ym2610,
                    cmd >> 7,
                    u32::from(buffer[offset]) | 0x100,
                    buffer[offset + 1],
                );
                offset += 2;
            }
            0x5a | 0xaa => {
                write_chip(
                    chips,
                    ChipType::Ym3812,
                    cmd >> 7,
                    u32::from(buffer[offset]),
                    buffer[offset + 1],
                );
                offset += 2;
            }
            0x5b | 0xab => {
                write_chip(
                    chips,
                    ChipType::Ym3526,
                    cmd >> 7,
                    u32::from(buffer[offset]),
                    buffer[offset + 1],
                );
                offset += 2;
            }
            0x5c | 0xac => {
                write_chip(
                    chips,
                    ChipType::Y8950,
                    cmd >> 7,
                    u32::from(buffer[offset]),
                    buffer[offset + 1],
                );
                offset += 2;
            }
            0x5e | 0xae => {
                write_chip(
                    chips,
                    ChipType::Ymf262,
                    cmd >> 7,
                    u32::from(buffer[offset]),
                    buffer[offset + 1],
                );
                offset += 2;
            }
            0x5f | 0xaf => {
                write_chip(
                    chips,
                    ChipType::Ymf262,
                    cmd >> 7,
                    u32::from(buffer[offset]) | 0x100,
                    buffer[offset + 1],
                );
                offset += 2;
            }

            // wait n samples, n = 0..65535
            0x61 => {
                delay = i32::from(buffer[offset]) | (i32::from(buffer[offset + 1]) << 8);
                offset += 2;
            }
            // wait 735 samples (60th of a second)
            0x62 => delay = 735,
            // wait 882 samples (50th of a second)
            0x63 => delay = 882,
            // end of sound data
            0x66 => done = true,

            // data block
            0x67 => {
                let marker = buffer[offset];
                offset += 1;
                if marker == 0x66 {
                    let dtype = buffer[offset];
                    offset += 1;
                    let size = read_u32(buffer, &mut offset);
                    let local_offset = offset;

                    match dtype {
                        // uncompressed data for use with associated commands: not supported
                        0x01..=0x07 => {}

                        // YM2612 PCM data for use with associated commands
                        0x00 => {
                            if let Some(chip) = find_chip(chips, ChipType::Ym2612, 0) {
                                let len = (size as usize).saturating_sub(8);
                                chip.write_data(
                                    AccessClass::Pcm,
                                    0,
                                    &buffer[local_offset..local_offset + len],
                                );
                            }
                        }

                        // YM2610 ADPCM ROM data
                        0x82 => add_rom_data(
                            chips,
                            ChipType::Ym2610,
                            AccessClass::AdpcmA,
                            buffer,
                            local_offset,
                            size - 8,
                        ),
                        // YM2608 DELTA-T ROM data
                        0x81 => add_rom_data(
                            chips,
                            ChipType::Ym2608,
                            AccessClass::AdpcmB,
                            buffer,
                            local_offset,
                            size - 8,
                        ),
                        // YM2610 DELTA-T ROM data
                        0x83 => add_rom_data(
                            chips,
                            ChipType::Ym2610,
                            AccessClass::AdpcmB,
                            buffer,
                            local_offset,
                            size - 8,
                        ),
                        // YMF278B ROM/RAM data
                        0x84 | 0x87 => add_rom_data(
                            chips,
                            ChipType::Ymf278B,
                            AccessClass::Pcm,
                            buffer,
                            local_offset,
                            size - 8,
                        ),
                        // Y8950 DELTA-T ROM data
                        0x88 => add_rom_data(
                            chips,
                            ChipType::Y8950,
                            AccessClass::AdpcmB,
                            buffer,
                            local_offset,
                            size - 8,
                        ),

                        // ROM data for chips we don't support
                        0x80 | 0x85 | 0x86 | 0x89..=0x93 => {}
                        // RAM writes: not supported
                        0xc0..=0xc2 | 0xe0 | 0xe1 => {}

                        other => {
                            if (0x40..0x7f).contains(&other) {
                                println!("Compressed data block not supported");
                            } else {
                                println!("Unknown data block type {other:#04X}");
                            }
                        }
                    }
                    offset += size as usize;
                }
            }

            // PCM RAM write
            0x68 => println!("68: PCM RAM write"),

            // AY8910, write value dd to register aa
            0xa0 => {
                write_chip(
                    chips,
                    ChipType::Ym2149,
                    buffer[offset] >> 7,
                    u32::from(buffer[offset] & 0x7f),
                    buffer[offset + 1],
                );
                offset += 2;
            }

            // pp aa dd: YMF278B, port pp, write value dd to register aa
            0xd0 => {
                let reg = (u32::from(buffer[offset] & 0x7f) << 8) | u32::from(buffer[offset + 1]);
                write_chip(
                    chips,
                    ChipType::Ymf278B,
                    buffer[offset] >> 7,
                    reg,
                    buffer[offset + 2],
                );
                offset += 3;
            }

            0x70..=0x7f => delay = i32::from(cmd & 15) + 1,

            0x80..=0x8f => {
                if let Some(chip) = find_chip(chips, ChipType::Ym2612, 0) {
                    let sample = chip.read_pcm();
                    chip.write(0x2a, sample);
                }
                delay = i32::from(cmd & 15);
            }

            // ignored, consume one byte
            0x30..=0x3f | 0x4f | 0x50 => offset += 1,

            // ignored, consume two bytes
            0x40..=0x4e | 0x5d | 0xb0..=0xbf => offset += 2,

            // ignored, consume three bytes
            0xc0..=0xc8 | 0xc9..=0xcf | 0xd1..=0xd6 | 0xd7..=0xdf => offset += 3,

            // dddddddd: seek to offset dddddddd in the YM2612 PCM data bank
            0xe0 => {
                let pos = read_u32(buffer, &mut offset);
                if let Some(chip) = find_chip(chips, ChipType::Ym2612, 0) {
                    chip.seek_pcm(pos);
                }
            }
            // ignored, consume four bytes
            0xe1..=0xff => offset += 4,

            // unrecognized command: no parameter bytes to skip
            _ => {}
        }

        for _ in 0..delay {
            let mut outputs = [0i32; 2];
            for chip in chips.iter_mut() {
                chip.generate(output_pos, output_step, &mut outputs);
            }
            output_pos += output_step;
            wav_buffer.push(outputs[0]);
            wav_buffer.push(outputs[1]);
        }
    }

    wav_buffer
}

/// State for the VGM interface handler, which stores separate data banks for
/// each access class (Io, AdpcmA, AdpcmB, Pcm).
struct VgmHandlerState {
    // Separate data banks for Io, AdpcmA, AdpcmB, and Pcm access classes.
    data: [Vec<u8>; 4],
}

/// Return the index into `VgmHandlerState.data` for the given access class.
fn data_index(access: AccessClass) -> usize {
    match access {
        AccessClass::Io => 0,
        AccessClass::AdpcmA => 1,
        AccessClass::AdpcmB => 2,
        AccessClass::Pcm => 3,
        _ => 0,
    }
}

/// Write a byte to the appropriate data bank in `VgmHandlerState`, resizing
/// the bank if necessary.
fn write_byte(state: &mut VgmHandlerState, access: AccessClass, offset: u32, value: u8) {
    let buffer = &mut state.data[data_index(access)];
    let index = offset as usize;
    if buffer.len() <= index {
        buffer.resize(index + 1, 0);
    }
    buffer[index] = value;
}

fn read_byte(state: &VgmHandlerState, access: AccessClass, offset: u32) -> u8 {
    state.data[data_index(access)]
        .get(offset as usize)
        .copied()
        .unwrap_or(0)
}

/// Create an `InterfaceHandler` that writes to a `VgmHandlerState`, matching
/// vgmrender.cpp's `vgm_handler`.
fn vgm_handler_with_state(state: Rc<RefCell<VgmHandlerState>>) -> InterfaceHandler {
    InterfaceHandler {
        write_data: Some(Box::new({
            let state = Rc::clone(&state);
            move |access, base, data| {
                let mut state = state.borrow_mut();
                for (index, value) in data.iter().copied().enumerate() {
                    write_byte(&mut state, access, base + index as u32, value);
                }
            }
        })),
        read_data: Some(Box::new({
            let state = Rc::clone(&state);
            move |access, base, length| {
                let state = state.borrow();
                (0..length)
                    .map(|index| read_byte(&state, access, base + index))
                    .collect()
            }
        })),
        ymfm_external_read: Some(Box::new({
            let state = Rc::clone(&state);
            move |access, offset| read_byte(&state.borrow(), access, offset)
        })),
        ..Default::default()
    }
}

/// Write a 16-bit stereo WAV file from interleaved (L, R) i32 samples,
/// matching vgmrender.cpp's `write_wav` (samples are normalized so the
/// loudest one hits roughly 80% of full scale).
fn write_wav(path: &Path, output_rate: u32, wav_buffer: &[i32]) -> io::Result<()> {
    let max_scale = wav_buffer
        .iter()
        .map(|v| v.unsigned_abs())
        .max()
        .unwrap_or(0);
    let max_scale = if max_scale == 0 {
        eprintln!("The WAV file data will only contain silence.");
        1
    } else {
        max_scale
    };

    let samples: Vec<i16> = wav_buffer
        .iter()
        .map(|&v| (i64::from(v) * 26000 / i64::from(max_scale)) as i16)
        .collect();

    let mut out = BufWriter::new(fs::File::create(path)?);
    let data_len = (samples.len() * 2) as u32;
    let total_size = 40u32 + data_len;
    let byte_rate = output_rate * 2 * 2;

    out.write_all(b"RIFF")?;
    out.write_all(&total_size.to_le_bytes())?;
    out.write_all(b"WAVE")?;
    out.write_all(b"fmt ")?;
    out.write_all(&16u32.to_le_bytes())?; // fmt chunk length
    out.write_all(&1u16.to_le_bytes())?; // PCM
    out.write_all(&2u16.to_le_bytes())?; // channels
    out.write_all(&output_rate.to_le_bytes())?;
    out.write_all(&byte_rate.to_le_bytes())?;
    out.write_all(&4u16.to_le_bytes())?; // block align
    out.write_all(&16u16.to_le_bytes())?; // bits/sample
    out.write_all(b"data")?;
    out.write_all(&data_len.to_le_bytes())?;
    for sample in &samples {
        out.write_all(&sample.to_le_bytes())?;
    }
    out.flush()
}

/// Print usage information to stderr.
fn print_usage() {
    eprintln!("Usage: vgmrender <inputfile|-> -o <outputfile> [-r <rate>]");
    eprintln!("       Use '-' as <inputfile> to read VGM data from stdin.");
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();

    let mut input_file = None;
    let mut output_file = None;
    let mut output_rate: u32 = 44100;
    let mut arg_error = false;

    let mut i = 1;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "-o" | "--output" => {
                i += 1;
                output_file = args.get(i).cloned();
            }
            "-r" | "--samplerate" => {
                i += 1;
                output_rate = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(44100);
            }
            "-" => input_file = Some(arg.to_string()),
            _ if arg.starts_with('-') => {
                eprintln!("Unknown argument: {arg}");
                arg_error = true;
            }
            _ => input_file = Some(arg.to_string()),
        }
        i += 1;
    }

    let (Some(input_file), Some(output_file)) = (input_file, output_file) else {
        print_usage();
        return ExitCode::from(1);
    };
    if arg_error {
        print_usage();
        return ExitCode::from(1);
    }

    let buffer = if input_file == "-" {
        let mut stdin = io::stdin();
        let mut buffer = Vec::new();
        match stdin.read_to_end(&mut buffer) {
            Ok(_) => buffer,
            Err(err) => {
                eprintln!("Error reading VGM data from stdin: {err}");
                return ExitCode::from(2);
            }
        }
    } else {
        match fs::read(&input_file) {
            Ok(buffer) => buffer,
            Err(err) => {
                eprintln!("Error opening file '{input_file}': {err}");
                return ExitCode::from(2);
            }
        }
    };

    if buffer.len() < 64 || &buffer[0..4] != b"Vgm " {
        eprintln!("File '{input_file}' does not appear to be a valid VGM file");
        return ExitCode::from(4);
    }

    let (data_start, mut chips) = parse_header(&buffer);

    if chips.is_empty() {
        eprintln!("No compatible chips found, exiting.");
        return ExitCode::from(5);
    }

    let wav_buffer = generate_all(&buffer, data_start, output_rate, &mut chips);

    if let Err(err) = write_wav(Path::new(&output_file), output_rate, &wav_buffer) {
        eprintln!("Error writing output file '{output_file}': {err}");
        return ExitCode::from(6);
    }

    ExitCode::SUCCESS
}
