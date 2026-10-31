#include "ymfm-sys/src/shim.h"
#include "ymfm-sys/src/lib.rs.h"

#include "ymfm_misc.h"
#include "ymfm_opl.h"
#include "ymfm_opm.h"
#include "ymfm_opn.h"
#include "ymfm_opq.h"
#include "ymfm_opz.h"

#include <cstdio>
#include <cstdlib>
#include <vector>
#include <type_traits>

namespace ymfm_sys
{

// calls chip.set_fidelity(fidelity) if T supports it (YM2203/YM2608/YM2610),
// otherwise silently does nothing; expression-SFINAE picks the first overload
// only when the member function actually exists.
template<typename T>
auto set_fidelity_if_supported(T &chip, ymfm::opn_fidelity fidelity, int) -> decltype(chip.set_fidelity(fidelity), void())
{
	chip.set_fidelity(fidelity);
}

template<typename T>
void set_fidelity_if_supported(T &, ymfm::opn_fidelity, long)
{
}

template<typename T>
auto set_instrument_data_if_supported(T &chip, rust::Slice<const std::uint8_t> data, int)
	-> decltype(chip.set_instrument_data(data.data()), bool())
{
	if (data.size() != 0x90)
		return false;
	chip.set_instrument_data(data.data());
	return true;
}

template<typename T>
bool set_instrument_data_if_supported(T &, rust::Slice<const std::uint8_t>, long)
{
	return false;
}

// Common callback and ymfm_interface plumbing shared by every chip
// instantiation. Host-side data and timing policy live in the Rust handler.
class ChipBase : public Chip, public ymfm::ymfm_interface
{
public:
	explicit ChipBase(InterfaceCallbacks *callbacks) : m_callbacks(callbacks) { }

protected:
	// ymfm::ymfm_interface override: serve external ROM/RAM reads.
	std::uint8_t ymfm_external_read(ymfm::access_class type, std::uint32_t offset) override
	{
		return ymfm_sys::ymfm_external_read(*m_callbacks, static_cast<AccessClass>(type), offset);
	}

	// ymfm::ymfm_interface override: accept external writes (ADPCM-B
	// recording, YMF278B RAM writes, OPM/OPZ/SSG/Y8950 I/O port output),
	// growing the backing buffer as needed just like write_data() does.
	void ymfm_external_write(ymfm::access_class type, std::uint32_t offset, std::uint8_t data) override
	{
		ymfm_sys::ymfm_external_write(*m_callbacks, static_cast<AccessClass>(type), offset, data);
	}

	void ymfm_update_irq(bool asserted) override
	{
		ymfm_sys::ymfm_update_irq(*m_callbacks, asserted);
	}

	void ymfm_set_busy_end(std::uint32_t clocks) override
	{
		ymfm_sys::ymfm_set_busy_end(*m_callbacks, clocks);
	}

	// ymfm::ymfm_interface override: report whether the chip is currently busy.
	bool ymfm_is_busy() override
	{
		return ymfm_sys::ymfm_is_busy(*m_callbacks);
	}

	// ymfm::ymfm_interface override: set a timer to fire after the given number of clocks.
	void ymfm_set_timer(std::uint32_t tnum, std::int32_t duration_in_clocks) override
	{
		ymfm_sys::ymfm_set_timer(*m_callbacks, tnum, duration_in_clocks);
	}

	// advance the clock counter by the given number of raw chip clocks and
	// fire any timers whose deadline has passed. This is done per generated
	// sample so timer-dependent modes such as CSM continue to work.
	void advance_clock(std::int64_t clocks)
	{
		std::uint8_t expired = ymfm_sys::advance_clock(*m_callbacks, clocks);
		for (std::uint32_t tnum = 0; tnum < TIMER_COUNT; tnum++)
			if ((expired & (1u << tnum)) != 0)
				m_engine->engine_timer_expired(tnum);
	}

	static constexpr std::uint32_t TIMER_COUNT = 2;

	InterfaceCallbacks *m_callbacks;
};

// concrete implementation for a specific ymfm chip class: a thin, faithful
// wrapper around the corresponding ymfm chip.
template<typename T>
class ChipImpl final : public ChipBase
{
public:
	ChipImpl(std::uint32_t clock, ChipType reported_type, rust::Box<InterfaceCallbacks> callbacks) :
		ChipBase(&*callbacks),
		m_reported_type(reported_type),
		m_chip(*this),
		m_clock(clock),
		m_clocks_per_sample(static_cast<std::int64_t>(clock) / static_cast<std::int64_t>(m_chip.sample_rate(clock))),
		m_callbacks(std::move(callbacks))
	{
		m_chip.reset();
	}

	ChipType chip_type() const override { return m_reported_type; }

	std::uint32_t channels() const override { return T::OUTPUTS; }

	std::uint32_t sample_rate() const override { return m_chip.sample_rate(m_clock); }

	void reset() override { m_chip.reset(); }

	void set_fidelity(Fidelity fidelity) override
	{
		set_fidelity_if_supported(m_chip, static_cast<ymfm::opn_fidelity>(fidelity), 0);
		m_clocks_per_sample = static_cast<std::int64_t>(m_clock) /
			static_cast<std::int64_t>(m_chip.sample_rate(m_clock));
	}

	bool set_instrument_data(rust::Slice<const std::uint8_t> data) override
	{
		return set_instrument_data_if_supported(m_chip, data, 0);
	}

	void write(std::uint32_t offset, std::uint8_t data) override { m_chip.write(offset, data); }

	std::uint8_t read(std::uint32_t offset) override { return m_chip.read(offset); }

	// generate `numsamples` of interleaved output into the given buffer,
	// advancing the internal clock counter by the appropriate number of raw chip clocks.
	void generate(rust::Slice<std::int32_t> buffer) override
	{
		std::uint32_t channels = T::OUTPUTS;
		std::uint32_t numsamples = static_cast<std::uint32_t>(buffer.size()) / channels;

		for (std::uint32_t sample = 0; sample < numsamples; sample++)
		{
			typename T::output_data output;
			m_chip.generate(&output, 1);
			advance_clock(m_clocks_per_sample);

			for (std::uint32_t channel = 0; channel < channels; channel++)
				buffer[sample * channels + channel] = output.data[channel];
		}
	}

	bool generate_voice_outputs(rust::Slice<std::int32_t> buffer) override
	{
		if constexpr (std::is_same_v<T, ymfm::ym2612>)
		{
			constexpr std::uint32_t voice_count = 6;
			constexpr std::uint32_t output_count = T::OUTPUTS;
			if (buffer.size() != voice_count * output_count)
				return false;
			m_chip.generate_voice_outputs(buffer.data());
			advance_clock(m_clocks_per_sample);
			return true;
		}
		return false;
	}

	rust::Vec<std::uint8_t> save_state() override
	{
		std::vector<std::uint8_t> buffer;
		ymfm::ymfm_saved_state state(buffer, true);
		m_chip.save_restore(state);

		rust::Vec<std::uint8_t> result;
		result.reserve(buffer.size());
		for (std::uint8_t byte : buffer)
			result.push_back(byte);
		return result;
	}

	void restore_state(rust::Slice<const std::uint8_t> data) override
	{
		std::vector<std::uint8_t> buffer(data.begin(), data.end());
		ymfm::ymfm_saved_state state(buffer, false);
		m_chip.save_restore(state);
	}

private:
	ChipType m_reported_type;
	T m_chip;
	std::uint32_t m_clock;
	std::int64_t m_clocks_per_sample;
		rust::Box<InterfaceCallbacks> m_callbacks;
};

std::unique_ptr<Chip> create_chip(ChipType type, std::uint32_t clock)
{
	return create_chip_with_callbacks(type, clock, default_callbacks());
}

std::unique_ptr<Chip> create_chip_with_callbacks(ChipType type, std::uint32_t clock, rust::Box<InterfaceCallbacks> callbacks)
{
	switch (type)
	{
		case ChipType::Ym2149:  return std::make_unique<ChipImpl<ymfm::ym2149>>(clock, ChipType::Ym2149, std::move(callbacks));
		case ChipType::Ym2151:  return std::make_unique<ChipImpl<ymfm::ym2151>>(clock, ChipType::Ym2151, std::move(callbacks));
		case ChipType::Ym2164:  return std::make_unique<ChipImpl<ymfm::ym2164>>(clock, ChipType::Ym2164, std::move(callbacks));
		case ChipType::Ym2203:  return std::make_unique<ChipImpl<ymfm::ym2203>>(clock, ChipType::Ym2203, std::move(callbacks));
		case ChipType::Ym2413:  return std::make_unique<ChipImpl<ymfm::ym2413>>(clock, ChipType::Ym2413, std::move(callbacks));
		case ChipType::Ym2423:  return std::make_unique<ChipImpl<ymfm::ym2423>>(clock, ChipType::Ym2423, std::move(callbacks));
		case ChipType::Ym2608:  return std::make_unique<ChipImpl<ymfm::ym2608>>(clock, ChipType::Ym2608, std::move(callbacks));
		case ChipType::Ym2610:  return std::make_unique<ChipImpl<ymfm::ym2610>>(clock, ChipType::Ym2610, std::move(callbacks));
		case ChipType::Ym2610B: return std::make_unique<ChipImpl<ymfm::ym2610b>>(clock, ChipType::Ym2610, std::move(callbacks));
		case ChipType::Ym2612:  return std::make_unique<ChipImpl<ymfm::ym2612>>(clock, ChipType::Ym2612, std::move(callbacks));
		case ChipType::Ym3438:  return std::make_unique<ChipImpl<ymfm::ym3438>>(clock, ChipType::Ym3438, std::move(callbacks));
		case ChipType::Ymf276:  return std::make_unique<ChipImpl<ymfm::ymf276>>(clock, ChipType::Ymf276, std::move(callbacks));
		case ChipType::Ym3526:  return std::make_unique<ChipImpl<ymfm::ym3526>>(clock, ChipType::Ym3526, std::move(callbacks));
		case ChipType::Ym3533:  return std::make_unique<ChipImpl<ymfm::ym3533>>(clock, ChipType::Ym3533, std::move(callbacks));
		case ChipType::Y8950:   return std::make_unique<ChipImpl<ymfm::y8950>>(clock, ChipType::Y8950, std::move(callbacks));
		case ChipType::Ym3812:  return std::make_unique<ChipImpl<ymfm::ym3812>>(clock, ChipType::Ym3812, std::move(callbacks));
		case ChipType::Ymf262:  return std::make_unique<ChipImpl<ymfm::ymf262>>(clock, ChipType::Ymf262, std::move(callbacks));
		case ChipType::Ymf281:  return std::make_unique<ChipImpl<ymfm::ymf281>>(clock, ChipType::Ymf281, std::move(callbacks));
		case ChipType::Ymf289B: return std::make_unique<ChipImpl<ymfm::ymf289b>>(clock, ChipType::Ymf289B, std::move(callbacks));
		case ChipType::Ymf278B: return std::make_unique<ChipImpl<ymfm::ymf278b>>(clock, ChipType::Ymf278B, std::move(callbacks));
		case ChipType::Ymf288:  return std::make_unique<ChipImpl<ymfm::ymf288>>(clock, ChipType::Ymf288, std::move(callbacks));
		case ChipType::Ym3806:  return std::make_unique<ChipImpl<ymfm::ym3806>>(clock, ChipType::Ym3806, std::move(callbacks));
		case ChipType::Ds1001:  return std::make_unique<ChipImpl<ymfm::ds1001>>(clock, ChipType::Ds1001, std::move(callbacks));
		case ChipType::Ym2414:  return std::make_unique<ChipImpl<ymfm::ym2414>>(clock, ChipType::Ym2414, std::move(callbacks));
		default:
			std::fprintf(stderr, "ymfm-sys: unsupported chip type (%d)\n", static_cast<int>(type));
			std::abort();
	}
}

} // namespace ymfm_sys
