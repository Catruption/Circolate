#![doc = include_str!("../README.md")]

use cxx::UniquePtr;

mod callback;

pub use callback::{InterfaceCallbacks, InterfaceHandler};
pub(crate) use callback::{
    advance_clock, default_callbacks, read_data, write_data, ymfm_external_read,
    ymfm_external_write, ymfm_is_busy, ymfm_set_busy_end, ymfm_set_timer, ymfm_update_irq,
};

/// Convenience alias for the common case of holding a chip instance.
pub type ChipPtr = UniquePtr<ffi::Chip>;

#[cxx::bridge(namespace = "ymfm_sys")]
pub mod ffi {
    extern "Rust" {
        type InterfaceCallbacks;

        fn default_callbacks() -> Box<InterfaceCallbacks>;

        fn advance_clock(callbacks: &InterfaceCallbacks, clocks: i64) -> u8;
        fn read_data(
            callbacks: &InterfaceCallbacks,
            access: AccessClass,
            base: u32,
            length: u32,
        ) -> Vec<u8>;
        fn write_data(callbacks: &InterfaceCallbacks, access: AccessClass, base: u32, data: &[u8]);
        fn ymfm_external_read(
            callbacks: &InterfaceCallbacks,
            access: AccessClass,
            offset: u32,
        ) -> u8;
        fn ymfm_external_write(
            callbacks: &InterfaceCallbacks,
            access: AccessClass,
            offset: u32,
            data: u8,
        );
        fn ymfm_is_busy(callbacks: &InterfaceCallbacks) -> bool;
        fn ymfm_set_busy_end(callbacks: &InterfaceCallbacks, clocks: u32);
        fn ymfm_set_timer(callbacks: &InterfaceCallbacks, tnum: u32, duration_in_clocks: i32);
        fn ymfm_update_irq(callbacks: &InterfaceCallbacks, asserted: bool);
    }

    /// Supported Yamaha FM/SSG chip families.
    ///
    /// `Ym2610B` exists only to select the YM2610B variant at creation time;
    /// `Chip::chip_type` normalizes it back to `Ym2610`, matching how ymfm
    /// itself treats the two revisions identically for register routing.
    #[repr(u32)]
    enum ChipType {
        Ym2149,
        Ym2151,
        Ym2164,
        Ym2203,
        Ym2413,
        Ym2423,
        Ym2608,
        Ym2610,
        Ym2610B,
        Ym2612,
        Ym3438,
        Ymf276,
        Ym3526,
        Ym3533,
        Y8950,
        Ym3812,
        Ymf262,
        Ymf281,
        Ymf289B,
        Ymf278B,
        Ymf288,
        Ym3806,
        Ds1001,
        Ym2414,
    }

    /// External data classes a chip may read ROM/RAM data from.
    #[repr(u32)]
    enum AccessClass {
        Io,
        AdpcmA,
        AdpcmB,
        Pcm,
    }

    /// Sample-rate/accuracy tradeoff, via the ymfm `opn_fidelity` setting.
    /// Only meaningful for YM2203/YM2608/YM2610/YM2610B; a no-op elsewhere.
    #[repr(u32)]
    enum Fidelity {
        Max,
        Min,
        Med,
    }

    unsafe extern "C++" {
        include!("ymfm-sys/src/shim.h");

        /// Opaque handle to a single emulated chip instance.
        type Chip;

        /// Create a chip with all optional interface callbacks disabled.
        fn create_chip(chip_type: ChipType, clock: u32) -> UniquePtr<Chip>;

        /// Create a chip and forward ymfm interface callbacks to `callbacks`.
        fn create_chip_with_callbacks(
            chip_type: ChipType,
            clock: u32,
            callbacks: Box<InterfaceCallbacks>,
        ) -> UniquePtr<Chip>;

        /// Which chip this instance represents.
        fn chip_type(self: &Chip) -> ChipType;

        /// Number of output channels this chip produces per generated sample
        /// (via the concrete ymfm chip class's `OUTPUTS` constant).
        fn channels(self: &Chip) -> u32;

        /// Native output sample rate for the clock this chip was created
        /// with (via the ymfm `sample_rate(uint32_t input_clock)` API).
        fn sample_rate(self: &Chip) -> u32;

        /// Reset the chip to its post-power-on state (via the ymfm `reset()` API).
        fn reset(self: Pin<&mut Chip>);

        /// Select the sample-rate/accuracy tradeoff (via the ymfm
        /// `set_fidelity(opn_fidelity)`). Only meaningful for
        /// YM2203/YM2608/YM2610/YM2610B; a no-op on other chips.
        fn set_fidelity(self: Pin<&mut Chip>, fidelity: Fidelity);

        /// Replace the 0x90-byte instrument data on OPLL-family chips.
        /// Returns false for unsupported chip types or an incorrectly sized
        /// data buffer.
        fn set_instrument_data(self: Pin<&mut Chip>, data: &[u8]) -> bool;

        /// Write to a register at `offset`, via the ymfm
        /// `write(offset, data)` (0/1 = address/data port, 2/3 = extended
        /// address/data port on chips that support it).
        fn write(self: Pin<&mut Chip>, offset: u32, data: u8);

        /// Read from `offset`, via the ymfm `read(offset)` API.
        fn read(self: Pin<&mut Chip>, offset: u32) -> u8;

        /// Generate `buffer.len() / channels()` samples at the chip's native
        /// sample rate, overwriting `buffer` (channel-interleaved); wraps
        /// the ymfm `generate(output_data*, numsamples)` API. This generates
        /// one native sample at a time. For each sample, it also advances the
        /// internal clock counter used by
        /// timers, including those required by modes such as CSM, and by
        /// BUSY state tracking.
        fn generate(self: Pin<&mut Chip>, buffer: &mut [i32]);

        /// Generate one native sample for each independently addressable
        /// voice, interleaved by voice then output channel. Returns false if
        /// this chip backend does not expose isolated voices.
        fn generate_voice_outputs(self: Pin<&mut Chip>, buffer: &mut [i32]) -> bool;

        /// Serialize the full internal chip state via the ymfm
        /// `save_restore(ymfm_saved_state&)` with `saving = true`).
        fn save_state(self: Pin<&mut Chip>) -> Vec<u8>;

        /// Restore state previously produced by `save_state` (via the ymfm
        /// `save_restore(ymfm_saved_state&)` with `saving = false`). The
        /// chip must be of the same type and clock as when the state was
        /// saved; ymfm does not version or validate the saved data itself.
        fn restore_state(self: Pin<&mut Chip>, data: &[u8]);
    }
}
