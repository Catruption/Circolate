#pragma once

#include "rust/cxx.h"

#include <cstdint>
#include <memory>

namespace ymfm_sys
{

// forward-declared (opaque enum) rather than pulling in the generated
// cxxbridge header here, which would try to re-include this very file while
// it is still being processed; the full enum definitions come from
// ffi.rs.h, included wherever the enumerators are actually needed (shim.cpp).
enum class ChipType : std::uint32_t;
enum class AccessClass : std::uint32_t;
enum class Fidelity : std::uint32_t;
struct InterfaceCallbacks;

// type-erased handle to a single emulated chip instance; faithfully exposes
// the per-chip API exposed by ymfm (immediate register writes/reads,
// native-rate sample generation, save/restore).
//
// Not exposed: ymfm::ssg_override, which lets a caller substitute a custom
// SSG core implementation. Doing so would require Rust to implement a C++
// callback interface invoked on every SSG register access, which conflicts
// with this binding's zero-cost goals; deliberately out of scope for now.
class Chip
{
public:
	virtual ~Chip() = default;

	// which chip this instance represents.
	virtual ChipType chip_type() const = 0;

	// number of output channels this chip produces per generated sample.
	virtual std::uint32_t channels() const = 0;

	// native output sample rate for the clock this chip was created with.
	virtual std::uint32_t sample_rate() const = 0;

	// reset the chip to its post-power-on state.
	virtual void reset() = 0;

	// select the sample-rate/accuracy tradeoff (YM2203/YM2608/YM2610/YM2610B
	// only, via ymfm::opn_fidelity; a no-op on chips that don't support it).
	virtual void set_fidelity(Fidelity fidelity) = 0;

	// replace the 0x90-byte instrument ROM on OPLL-family chips; returns false
	// for unsupported chips or an incorrectly sized data buffer.
	virtual bool set_instrument_data(rust::Slice<const std::uint8_t> data) = 0;

	// write/read a register via the ymfm write(offset,data)/read(offset) API
	// (0/1 = address/data port, 2/3 = extended address/data port on chips that
	// support it).
	virtual void write(std::uint32_t offset, std::uint8_t data) = 0;
	virtual std::uint8_t read(std::uint32_t offset) = 0;

	// generate buffer.size()/channels() samples at the chip's native sample
	// rate, overwriting buffer (channel-interleaved); this also advances the
	// internal clock counter used to track busy/timer/IRQ state below.
	virtual void generate(rust::Slice<std::int32_t> buffer) = 0;

	// Optional isolated mono/stereo channel output. Returns false for chips
	// whose backend does not provide channel taps.
	virtual bool generate_voice_outputs(rust::Slice<std::int32_t> buffer) { return false; }

	// serialize the full internal chip state.
	virtual rust::Vec<std::uint8_t> save_state() = 0;

	// restore state previously produced by save_state(); the chip must be of
	// the same concrete type and clock as when the state was saved, since
	// ymfm itself does not version or validate the saved data.
	virtual void restore_state(rust::Slice<const std::uint8_t> data) = 0;
};

// create a chip instance of the given type, clocked at the given frequency.
std::unique_ptr<Chip> create_chip(ChipType type, std::uint32_t clock);

// create a chip instance with callbacks owned by the Rust handler.
std::unique_ptr<Chip> create_chip_with_callbacks(
	ChipType type, std::uint32_t clock, rust::Box<InterfaceCallbacks> callbacks);

} // namespace ymfm_sys
