# ymfm-sys

![](https://github.com/h1romas4/ymfm-sys/workflows/Build/badge.svg)

Rust bindings to [ymfm](https://github.com/aaronsgiles/ymfm), by Aaron Giles-san
(BSD 3-Clause License), a portable C++ library for emulating Yamaha FM and related
sound chips.

This is not a complete raw binding to ymfm's C++ API; it is an adapter that
makes the relevant chip interfaces usable from Rust.

This crate exposes ymfm's per-chip interface through a small C++ shim and
[`cxx`](https://cxx.rs/). It is intended for emulator authors and other Rust
applications that need direct access to chip registers and native-rate audio
generation.

## Public API

The public API is under the [`ffi`] module:

| API | Purpose | Notes |
| --- | --- | --- |
| [`create_chip(chip_type: ChipType, clock: u32)`](ffi::create_chip) | Construct an opaque chip instance. | Optional interface callbacks are disabled. |
| [`create_chip_with_callbacks(chip_type: ChipType, clock: u32, callbacks: Box<InterfaceCallbacks>)`](ffi::create_chip_with_callbacks) | Construct an opaque chip instance with Rust callbacks. | Forwards the selected ymfm interface callbacks to the supplied receiver; see [Callbacks](#callbacks) below for details. |
| [`chip_type(&self)`](ffi::Chip::chip_type) | Identify the chip type. | `Ym2610B` is reported as `Ym2610`, matching ymfm's register routing. |
| [`channels(&self)`](ffi::Chip::channels) | Return the number of output channels per native sample. | Use this to size interleaved output buffers. |
| [`sample_rate(&self)`](ffi::Chip::sample_rate) | Return the chip's native output sample rate. | Depends on the input clock and, where supported, fidelity setting. |
| [`write(&mut self, offset: u32, data: u8)`](ffi::Chip::write) and [`read(&self, offset: u32)`](ffi::Chip::read) | Access the chip's register and status ports directly. | Port layout is chip-specific and follows ymfm. |
| [`generate(&mut self, buffer: &mut [i32])`](ffi::Chip::generate) | Generate interleaved signed 32-bit samples at the chip's native rate. | Generates one sample at a time and advances timers and the internal clock for each sample. |
| [`reset(&mut self)`](ffi::Chip::reset) | Reset the emulated chip state. | Restores the chip to its post-power-on state. |
| [`save_state(&mut self)`](ffi::Chip::save_state) and [`restore_state(&mut self, data: &[u8])`](ffi::Chip::restore_state) | Serialize and restore the emulated state. | Use with the same chip type and input clock. |
| [`set_fidelity(&mut self, fidelity: Fidelity)`](ffi::Chip::set_fidelity) | Select the accuracy/speed tradeoff. | Applies to OPN-family chips that support ymfm's `opn_fidelity`; it is a no-op for other chip types. |
| [`set_instrument_data(&mut self, data: &[u8])`](ffi::Chip::set_instrument_data) | Replace the instrument data used by OPLL-family chips. | Requires exactly `0x90` bytes; returns `false` for unsupported chips or an invalid length. |

The supported chip types are [`Ym2149`](ffi::ChipType::Ym2149),
[`Ym2151`](ffi::ChipType::Ym2151), [`Ym2164`](ffi::ChipType::Ym2164),
[`Ym2203`](ffi::ChipType::Ym2203), [`Ym2413`](ffi::ChipType::Ym2413),
[`Ym2414`](ffi::ChipType::Ym2414), [`Ym2423`](ffi::ChipType::Ym2423),
[`Ym2608`](ffi::ChipType::Ym2608), [`Ym2610`](ffi::ChipType::Ym2610),
[`Ym2610B`](ffi::ChipType::Ym2610B), [`Ym2612`](ffi::ChipType::Ym2612),
[`Ym3438`](ffi::ChipType::Ym3438), [`Ymf276`](ffi::ChipType::Ymf276),
[`Ym3526`](ffi::ChipType::Ym3526), [`Ym3533`](ffi::ChipType::Ym3533),
[`Y8950`](ffi::ChipType::Y8950), [`Ym3812`](ffi::ChipType::Ym3812),
[`Ymf262`](ffi::ChipType::Ymf262), [`Ymf281`](ffi::ChipType::Ymf281),
[`Ymf278B`](ffi::ChipType::Ymf278B), [`Ymf289B`](ffi::ChipType::Ymf289B),
[`Ymf288`](ffi::ChipType::Ymf288), [`Ym3806`](ffi::ChipType::Ym3806),
and [`Ds1001`](ffi::ChipType::Ds1001).

The ymfm `ssg_override` interface is not exposed. Replacing the built-in SSG
engine with a custom implementation would require a C++ callback interface and
is outside the scope of this binding.

## Example

```rust
use ymfm_sys::ffi::{self, ChipType};

let mut chip = ffi::create_chip(ChipType::Ym2612, 7_670_000);
let channels = chip.channels() as usize;
let mut samples = vec![0_i32; channels * 256];

// ymfm uses address/data ports. The exact register protocol depends on the
// selected chip.
chip.pin_mut().write(0, 0x22);
chip.pin_mut().write(1, 0x00);
chip.pin_mut().generate(&mut samples);
```

`generate` overwrites the supplied buffer. Its length must be a multiple of
`channels()`; the buffer is channel-interleaved. The returned sample rate is
available from `sample_rate()`.

## Callbacks

Clients that need host-side behavior can use
`create_chip_with_callbacks`. Every callback in `InterfaceHandler` is optional.
`create_chip` uses the same handler with every callback set to `None`, which
provides the defaults shown below.

`generate()` produces one native output sample at a time. For each sample it
advances ymfm's audio and clock state together, then forwards any expired timer
bits returned by `advance_clock` to ymfm. This clock progression is required for
timer expiry, CSM key-on behavior, and BUSY state tracking; it is not a
separate user-controlled clocking step.

| Callback | Purpose | Default when omitted |
| --- | --- | --- |
| `advance_clock(clocks)` | Advance the host-side clock and return a bit mask of timers that expired during the interval. | Returns `0`. |
| `read_data(access, base, length)` | Read a block of external ROM/RAM data for the selected [`AccessClass`](ffi::AccessClass). | Returns a zero-filled buffer of `length` bytes. |
| `write_data(access, base, data)` | Write a block of external ROM/RAM data for the selected [`AccessClass`](ffi::AccessClass). | Does nothing. |
| `ymfm_external_read(access, offset)` | Return one byte when ymfm reads external chip memory or I/O during emulation, such as ADPCM or PCM data. | Returns `0`. |
| `ymfm_external_write(access, offset, data)` | Receive one byte when ymfm writes external chip memory or I/O during emulation, such as RAM or device-port output. | Does nothing. |
| `ymfm_is_busy()` | Report whether the emulated device is currently BUSY. | Returns `false`. |
| `ymfm_set_busy_end(clocks)` | Set or extend the BUSY period in chip clocks. | Does nothing. |
| `ymfm_set_timer(tnum, duration_in_clocks)` | Receive a timer number and its duration, allowing the host to schedule expiry. | Does nothing. |
| `ymfm_update_irq(asserted)` | Receive ymfm's IRQ assertion state. | Does nothing. |

The `ymfm_*` callbacks correspond directly to ymfm interface operations. The
other callbacks provide host-side data access and time progression used by the
shim when implementing those operations.

## Building

The crate builds the bundled C++ implementation through `build.rs`, so a Rust
toolchain and a C++17-compatible compiler are required.

```sh
git clone --recurse-submodules https://github.com/h1romas4/ymfm-sys.git
cd ymfm-sys
```

```sh
cargo build
cargo test
cargo doc --open --no-deps
```

The `components/ymfm` directory contains the upstream ymfm source used by the
build. The C++ implementation is compiled as part of this crate and does not
need to be installed separately.

The following targets are not supported yet (WIP):

- `wasm32-unknown-unknown` ([`wasm32-unknown-unknown-libcxx`](https://github.com/maximmaxim345/wasm32-unknown-unknown-libcxx))
- `wasm32-wasip2` (Component Model)

## Upstream

The bundled ymfm source is pinned to commit
[`81aec25ccbb98f4873a255f7551ac4dadac59b4a`](https://github.com/aaronsgiles/ymfm/commit/81aec25ccbb98f4873a255f7551ac4dadac59b4a).

## License

BSD 3-Clause License.
