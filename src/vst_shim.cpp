// vst_shim.cpp — flat C ABI over the Steinberg VST3 SDK.
//
// Hosts instruments (no audio input) and effects (stereo input).
// Detection is automatic: if the plugin declares an input bus, we
// allocate silent scratch for it and pass it through ProcessData.
//
// Bus topology.  Some instruments (SFLT) declare many output buses —
// one for the main mix and one per internal send or zone.  The shim
// must allocate per-bus channel buffers and pass the full bus list to
// ProcessData.  Hardcoding a single stereo output crashes any plugin
// whose wrapper iterates over its own declared bus count.
//
// Bus summing.  SFLT routes dry to bus 0, reverb to bus 1, chorus to
// bus 2, and per-zone outputs to 3..17.  For a stereo host, all of
// those land on the same physical pair, so we sum every bus the plugin
// declares rather than reading only bus 0.
//
// DPF-based plugins (Dragonfly, x42, most LSP) assert on null host
// pointers in ProcessData and on out-of-order init calls, so we always
// provide non-null inputParameterChanges, outputParameterChanges, and
// processContext, and we initialize() before querying bus counts.
//
// State and activation ordering.  The VST3 spec allows setState() at
// any point, but plugins are permitted to size their internal buffers
// at setActive(true) based on the state they see at that moment.  A
// sampler like SFLT allocates playback buffers for whatever zones its
// current state says exist.  If we activate before loading state, SFLT
// sizes for "nothing" and then, when we hand it a real preset, tries
// to clear a buffer that was never allocated — memset(NULL, ...),
// SIGSEGV, dead process, dead song.
//
// So the create path does NOT setActive.  It configures buses, calls
// setupProcessing, and stops.  setState() runs next (from Rust), and
// then vst_shim_activate() is called to bring the plugin online with
// its state already in place.  Subsequent setState() calls run the
// same deactivate → load → reactivate cycle so buffer sizing is redone
// against the new state.
//
// Instrument input buses.  Some NIH-plug instruments declare a
// sidechain input bus even though they don't process audio from it.
// If we activate that bus but then tell the plugin numInputs=0 at
// process time, NIH-plug's VST3 wrapper is left holding a bus pointer
// array whose entry count doesn't match what it was told, and its
// buffer manager crashes on the first output fill.  The instrument
// path therefore mirrors the effect path: if the plugin has an input
// bus, feed it silent scratch.

#include <algorithm>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <memory>
#include <string>
#include <vector>

#include "public.sdk/source/vst/hosting/module.h"
#include "public.sdk/source/vst/hosting/hostclasses.h"
#include "public.sdk/source/vst/hosting/parameterchanges.h"
#include "public.sdk/source/common/memorystream.h"
#include "pluginterfaces/base/ipluginbase.h"
#include "pluginterfaces/vst/ivstcomponent.h"
#include "pluginterfaces/vst/ivsteditcontroller.h"
#include "pluginterfaces/vst/ivstaudioprocessor.h"
#include "pluginterfaces/vst/ivstevents.h"
#include "pluginterfaces/vst/ivstprocesscontext.h"
#include "pluginterfaces/vst/vstspeaker.h"

using namespace Steinberg;
using namespace Steinberg::Vst;

// ── IEventList backed by a std::vector ─────────────────────────────────

class SimpleEventList : public IEventList {
public:
    std::vector<Event> events;

    int32 PLUGIN_API getEventCount() override {
        return static_cast<int32>(events.size());
    }
    tresult PLUGIN_API getEvent(int32 i, Event& e) override {
        if (i < 0 || i >= static_cast<int32>(events.size())) return kInvalidArgument;
        e = events[i];
        return kResultOk;
    }
    tresult PLUGIN_API addEvent(Event& e) override {
        events.push_back(e);
        return kResultOk;
    }
    tresult PLUGIN_API queryInterface(const TUID, void** obj) override {
        *obj = nullptr;
        return kNoInterface;
    }
    uint32 PLUGIN_API addRef() override { return 1; }
    uint32 PLUGIN_API release() override { return 1; }
};

// ── per-bus scratch ────────────────────────────────────────────────────

struct BusScratch {
    int32 numChannels = 0;
    int32 numSamples = 0;
    std::vector<float> scratch;
    std::vector<float*> ptrs;
    AudioBusBuffers bus{};

    void resize(int32 channels, int32 blockSize) {
        numChannels = channels;
        numSamples = blockSize;
        scratch.assign(static_cast<size_t>(channels) * blockSize, 0.0f);
        ptrs.resize(channels);
        for (int32 c = 0; c < channels; ++c) {
            ptrs[c] = scratch.data() + static_cast<size_t>(c) * blockSize;
        }
        bus.numChannels = channels;
        bus.channelBuffers32 = ptrs.data();
        bus.silenceFlags = 0;
    }

    void clear(int32 frames) {
        if (frames > numSamples) frames = numSamples;
        std::fill(scratch.begin(),
                  scratch.begin() + static_cast<size_t>(numChannels) * frames,
                  0.0f);
    }
};

// ── per-instance state ─────────────────────────────────────────────────

struct Shim {
    VST3::Hosting::Module::Ptr module;
    IPtr<IHostApplication>     host_app;
    IPtr<IComponent>           component;
    IPtr<IEditController>      controller;
    IPtr<IAudioProcessor>      processor;

    int sampleRate = 44100;
    int blockSize  = 512;

    bool hasInput  = false;
    bool active    = false;

    std::vector<BusScratch> outBuses;
    std::vector<BusScratch> inBuses;

    std::vector<Event> pending;

    ParameterChanges inParams;
    ParameterChanges outParams;
    ProcessContext   context;

    int64 samplePos = 0;
};

static bool pick_class(const VST3::Hosting::PluginFactory& factory,
                       VST3::Hosting::ClassInfo& out)
{
    const auto& infos = factory.classInfos();
    if (infos.empty()) return false;

    for (const auto& ci : infos) {
        if (ci.category() != kVstAudioEffectClass) continue;
        for (const auto& s : ci.subCategories()) {
            if (s == "Instrument" || s == "Synth") {
                out = ci;
                return true;
            }
        }
    }
    for (const auto& ci : infos) {
        if (ci.category() == kVstAudioEffectClass) {
            out = ci;
            return true;
        }
    }
    return false;
}

static bool configure(Shim* s) {
    s->component->initialize(s->host_app);
    if (s->controller.get()) {
        s->controller->initialize(s->host_app);
    }

    int32 inCount  = s->component->getBusCount(kAudio, kInput);
    int32 outCount = s->component->getBusCount(kAudio, kOutput);
    s->hasInput    = inCount > 0;

    if (std::getenv("VST_SHIM_DEBUG")) {
        BusInfo bi{};
        std::fprintf(stderr, "shim: %d input bus(es), %d output bus(es)\n",
                     inCount, outCount);
        for (int32 b = 0; b < inCount; ++b) {
            s->component->getBusInfo(kAudio, kInput, b, bi);
            std::fprintf(stderr, "shim: in bus %d channels=%d\n",
                         b, bi.channelCount);
        }
        for (int32 b = 0; b < outCount; ++b) {
            s->component->getBusInfo(kAudio, kOutput, b, bi);
            std::fprintf(stderr, "shim: out bus %d channels=%d\n",
                         b, bi.channelCount);
        }
    }

    SpeakerArrangement out_arr = SpeakerArr::kStereo;
    if (s->hasInput) {
        SpeakerArrangement in_arr = SpeakerArr::kStereo;
        s->processor->setBusArrangements(&in_arr, 1, &out_arr, 1);
    } else {
        s->processor->setBusArrangements(nullptr, 0, &out_arr, 1);
    }

    for (int32 b = 0; b < outCount; ++b) {
        s->component->activateBus(kAudio, kOutput, b, true);
    }
    for (int32 b = 0; b < inCount; ++b) {
        s->component->activateBus(kAudio, kInput, b, true);
    }

    ProcessSetup setup{};
    setup.processMode        = kRealtime;
    setup.symbolicSampleSize = kSample32;
    setup.maxSamplesPerBlock = s->blockSize;
    setup.sampleRate         = s->sampleRate;
    if (s->processor->setupProcessing(setup) != kResultOk) {
        return false;
    }

    return true;
}

static void alloc_scratch(Shim* s) {
    BusInfo bi{};
    s->outBuses.clear();
    int32 outCount = s->component->getBusCount(kAudio, kOutput);
    for (int32 b = 0; b < outCount; ++b) {
        BusScratch bs;
        s->component->getBusInfo(kAudio, kOutput, b, bi);
        int32 ch = bi.channelCount > 0 ? bi.channelCount : 2;
        bs.resize(ch, s->blockSize);
        s->outBuses.push_back(std::move(bs));
    }
    if (s->outBuses.empty()) {
        BusScratch bs;
        bs.resize(2, s->blockSize);
        s->outBuses.push_back(std::move(bs));
    }

    s->inBuses.clear();
    int32 inCount = s->component->getBusCount(kAudio, kInput);
    for (int32 b = 0; b < inCount; ++b) {
        BusScratch bs;
        s->component->getBusInfo(kAudio, kInput, b, bi);
        int32 ch = bi.channelCount > 0 ? bi.channelCount : 2;
        bs.resize(ch, s->blockSize);
        s->inBuses.push_back(std::move(bs));
    }
}

// Sum every output bus into the host's stereo pair.  SFLT routes dry
// to bus 0 and its reverb / chorus returns to buses 1 and 2; a real
// multi-bus host would route those separately, but for stereo playback
// they all land on the same pair.  Mono buses are duplicated to both
// sides; stereo buses contribute L→L and R→R.  Anything beyond the
// first two channels of a bus is dropped.
static void sum_buses_to_stereo(const std::vector<BusScratch>& buses,
                                float* outL, float* outR, int frames)
{
    for (int i = 0; i < frames; ++i) { outL[i] = 0.0f; outR[i] = 0.0f; }

    for (const auto& bus : buses) {
        if (bus.numChannels <= 0 || bus.numSamples == 0) continue;
        int n = std::min(frames, bus.numSamples);
        if (bus.numChannels == 1) {
            const float* src = bus.scratch.data();
            for (int i = 0; i < n; ++i) {
                outL[i] += src[i];
                outR[i] += src[i];
            }
        } else {
            const float* L = bus.scratch.data();
            const float* R = bus.scratch.data() + bus.numSamples;
            for (int i = 0; i < n; ++i) {
                outL[i] += L[i];
                outR[i] += R[i];
            }
        }
    }
}

extern "C" {

    void* vst_shim_create(const char* path, int sr, int block) {
        try {
            std::string err;
            auto mod = VST3::Hosting::Module::create(path, err);
            if (!mod) return nullptr;

            auto factory = mod->getFactory();
            VST3::Hosting::ClassInfo ci;
            if (!pick_class(factory, ci)) return nullptr;

            auto s = new Shim();
            s->module     = mod;
            s->sampleRate = sr;
            s->blockSize  = block;

            s->host_app = new HostApplication();

            s->component = factory.createInstance<IComponent>(ci.ID());
            if (!s->component.get()) {
                delete s;
                return nullptr;
            }
            s->controller = factory.createInstance<IEditController>(ci.ID());

            if (s->component->queryInterface(IAudioProcessor::iid,
                reinterpret_cast<void**>(&s->processor)) != kResultOk
                || !s->processor.get()) {
                delete s;
                return nullptr;
            }

            if (!configure(s)) {
                delete s;
                return nullptr;
            }

            alloc_scratch(s);

            s->context = ProcessContext{};
            s->context.state              = ProcessContext::kPlaying;
            s->context.sampleRate         = static_cast<double>(sr);
            s->context.projectTimeSamples = 0;
            s->context.tempo              = 120.0;
            s->context.timeSigNumerator   = 4;
            s->context.timeSigDenominator = 4;

            return s;
        } catch (...) {
            return nullptr;
        }
    }

    void vst_shim_activate(void* h) {
        auto s = static_cast<Shim*>(h);
        if (!s || s->active) return;
        s->component->setActive(true);
        s->processor->setProcessing(true);
        s->active = true;
    }

    void vst_shim_destroy(void* h) {
        auto s = static_cast<Shim*>(h);
        if (!s) return;
        if (s->active) {
            s->processor->setProcessing(false);
            s->component->setActive(false);
            s->active = false;
        }
        if (s->component.get()) s->component->terminate();
        if (s->controller.get()) s->controller->terminate();
        delete s;
    }

    int vst_shim_set_state(void* h, const uint8_t* data, size_t len) {
        auto s = static_cast<Shim*>(h);
        if (!s || !s->component.get()) return 0;
        if (len == 0 || data == nullptr) return 1;

        bool was_active = s->active;
        if (was_active) {
            s->processor->setProcessing(false);
            s->component->setActive(false);
            s->active = false;
        }

        auto buf = const_cast<uint8_t*>(data);
        MemoryStream stream(buf, static_cast<TSize>(len));
        bool ok = (s->component->setState(&stream) == kResultOk);

        if (!ok && s->controller.get()) {
            MemoryStream s2(buf, static_cast<TSize>(len));
            ok = (s->controller->setComponentState(&s2) == kResultOk);
        }

        if (was_active) {
            s->component->setActive(true);
            s->processor->setProcessing(true);
            s->active = true;
        }

        return ok ? 1 : 0;
    }

    void vst_shim_note_on(void* h, int note, int vel, int sample_offset) {
        auto s = static_cast<Shim*>(h);
        if (!s) return;
        Event e{};
        e.type            = Event::kNoteOnEvent;
        e.sampleOffset    = sample_offset;
        e.noteOn.channel  = 0;
        e.noteOn.pitch    = static_cast<int16>(note);
        e.noteOn.velocity = vel / 127.0f;
        e.noteOn.noteId   = note;
        e.noteOn.length   = 0;
        e.noteOn.tuning   = 0;
        s->pending.push_back(e);
    }

    void vst_shim_note_off(void* h, int note, int sample_offset) {
        auto s = static_cast<Shim*>(h);
        if (!s) return;
        Event e{};
        e.type             = Event::kNoteOffEvent;
        e.sampleOffset     = sample_offset;
        e.noteOff.channel  = 0;
        e.noteOff.pitch    = static_cast<int16>(note);
        e.noteOff.velocity = 0.0f;
        e.noteOff.noteId   = note;
        e.noteOff.tuning   = 0;
        s->pending.push_back(e);
    }

    int vst_shim_process(void* h, float* outL, float* outR, int frames) {
        auto s = static_cast<Shim*>(h);
        if (!s || !s->processor.get() || !s->active) return 0;
        if (frames > s->blockSize) frames = s->blockSize;
        if (frames <= 0) return 1;

        for (auto& b : s->outBuses) b.clear(frames);
        for (auto& b : s->inBuses)  b.clear(frames);

        std::vector<AudioBusBuffers> in_refs, out_refs;
        in_refs.reserve(s->inBuses.size());
        out_refs.reserve(s->outBuses.size());
        for (auto& b : s->inBuses)  in_refs.push_back(b.bus);
        for (auto& b : s->outBuses) out_refs.push_back(b.bus);

        SimpleEventList evList;
        evList.events.swap(s->pending);

        s->inParams.clearQueue();
        s->outParams.clearQueue();
        s->context.projectTimeSamples = s->samplePos;

        ProcessData pd{};
        pd.processMode             = kRealtime;
        pd.symbolicSampleSize      = kSample32;
        pd.numSamples              = frames;
        pd.numInputs               = static_cast<int32>(in_refs.size());
        pd.inputs                  = in_refs.empty() ? nullptr : in_refs.data();
        pd.numOutputs              = static_cast<int32>(out_refs.size());
        pd.outputs                 = out_refs.data();
        pd.inputEvents             = &evList;
        pd.outputEvents            = nullptr;
        pd.inputParameterChanges   = &s->inParams;
        pd.outputParameterChanges  = &s->outParams;
        pd.processContext          = &s->context;

        if (s->processor->process(pd) != kResultOk) return 0;

        sum_buses_to_stereo(s->outBuses, outL, outR, frames);
        s->samplePos += frames;
        return 1;
    }

    int vst_shim_process_inplace(void* h, float* bufL, float* bufR, int frames) {
        auto s = static_cast<Shim*>(h);
        if (!s || !s->processor.get() || !s->active) return 0;
        if (frames > s->blockSize) frames = s->blockSize;
        if (frames <= 0) return 1;
        if (s->inBuses.empty()) return 1;

        auto& in0 = s->inBuses[0];
        int n = std::min(frames, in0.numSamples);
        if (in0.numChannels >= 2) {
            std::memcpy(in0.ptrs[0], bufL, n * sizeof(float));
            std::memcpy(in0.ptrs[1], bufR, n * sizeof(float));
        } else if (in0.numChannels == 1) {
            for (int i = 0; i < n; ++i)
                in0.ptrs[0][i] = 0.5f * (bufL[i] + bufR[i]);
        }
        for (size_t b = 1; b < s->inBuses.size(); ++b) {
            s->inBuses[b].clear(frames);
        }

        for (auto& b : s->outBuses) b.clear(frames);

        std::vector<AudioBusBuffers> in_refs, out_refs;
        in_refs.reserve(s->inBuses.size());
        out_refs.reserve(s->outBuses.size());
        for (auto& b : s->inBuses)  in_refs.push_back(b.bus);
        for (auto& b : s->outBuses) out_refs.push_back(b.bus);

        SimpleEventList evList;
        evList.events.swap(s->pending);

        s->inParams.clearQueue();
        s->outParams.clearQueue();
        s->context.projectTimeSamples = s->samplePos;

        ProcessData pd{};
        pd.processMode             = kRealtime;
        pd.symbolicSampleSize      = kSample32;
        pd.numSamples              = frames;
        pd.numInputs               = static_cast<int32>(in_refs.size());
        pd.inputs                  = in_refs.empty() ? nullptr : in_refs.data();
        pd.numOutputs              = static_cast<int32>(out_refs.size());
        pd.outputs                 = out_refs.data();
        pd.inputEvents             = &evList;
        pd.outputEvents            = nullptr;
        pd.inputParameterChanges   = &s->inParams;
        pd.outputParameterChanges  = &s->outParams;
        pd.processContext          = &s->context;

        if (s->processor->process(pd) != kResultOk) return 0;

        sum_buses_to_stereo(s->outBuses, bufL, bufR, frames);
        s->samplePos += frames;
        return 1;
    }

}  // extern "C"