// vst_shim.cpp — flat C ABI over the Steinberg VST3 SDK.
//
// Hosts instruments (no audio input) and effects (stereo input).
// Detection is automatic: if the plugin declares an input bus, we
// allocate silent scratch for it and pass it through ProcessData.
//
// DPF-based plugins (Dragonfly, x42, most LSP) assert on null host
// pointers in ProcessData and on out-of-order init calls, so we always
// provide non-null inputParameterChanges, outputParameterChanges, and
// processContext, and we initialize() before querying bus counts.

#include <algorithm>
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

// ── per-instance state ─────────────────────────────────────────────────

struct Shim {
    VST3::Hosting::Module::Ptr module;
    IPtr<IHostApplication>     host_app;
    IPtr<IComponent>           component;
    IPtr<IEditController>      controller;
    IPtr<IAudioProcessor>      processor;

    int sampleRate = 44100;
    int blockSize  = 512;

    bool hasInput = false;

    std::vector<float> scratchL, scratchR;
    std::vector<float> inScratchL, inScratchR;
    std::vector<float*> outChannelPtrs;
    std::vector<float*> inChannelPtrs;
    std::vector<AudioBusBuffers> outBuses;
    std::vector<AudioBusBuffers> inBuses;

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

                // initialize() must come before any VST3 call that touches the
                // plugin's internal state.  DPF plugins set up their vst3 pointer
                // here and assert on anything that runs first.
                s->component->initialize(s->host_app);
                if (s->controller.get()) {
                    s->controller->initialize(s->host_app);
                }

                // Now bus counts are safe to query.
                int32 inCount  = s->component->getBusCount(kAudio, kInput);
                int32 outCount = s->component->getBusCount(kAudio, kOutput);
                s->hasInput    = inCount > 0;

                // setBusArrangements before activateBus — the arrangement
                // determines channel counts on DPF plugins, and activating a bus
                // with an undefined layout trips their assert.
                SpeakerArrangement out_arr = SpeakerArr::kStereo;
                if (s->hasInput) {
                    SpeakerArrangement in_arr = SpeakerArr::kStereo;
                    s->processor->setBusArrangements(&in_arr, 1, &out_arr, 1);
                } else {
                    s->processor->setBusArrangements(nullptr, 0, &out_arr, 1);
                }

                if (outCount > 0) {
                    s->component->activateBus(kAudio, kOutput, 0, true);
                }
                if (s->hasInput) {
                    s->component->activateBus(kAudio, kInput, 0, true);
                }

                ProcessSetup setup{};
                setup.processMode        = kRealtime;
                setup.symbolicSampleSize = kSample32;
                setup.maxSamplesPerBlock = block;
                setup.sampleRate         = sr;
                if (s->processor->setupProcessing(setup) != kResultOk) {
                    delete s;
                    return nullptr;
                }

                s->component->setActive(true);
                s->processor->setProcessing(true);

                s->scratchL.assign(block, 0.0f);
                s->scratchR.assign(block, 0.0f);
                s->outChannelPtrs.resize(2);
                AudioBusBuffers outBus{};
                outBus.numChannels      = 2;
                outBus.channelBuffers32 = s->outChannelPtrs.data();
                outBus.silenceFlags     = 0;
                s->outBuses.push_back(outBus);

                if (s->hasInput) {
                    s->inScratchL.assign(block, 0.0f);
                    s->inScratchR.assign(block, 0.0f);
                    s->inChannelPtrs.resize(2);
                    AudioBusBuffers inBus{};
                    inBus.numChannels      = 2;
                    inBus.channelBuffers32 = s->inChannelPtrs.data();
                    inBus.silenceFlags     = 0;
                    s->inBuses.push_back(inBus);
                }

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

    void vst_shim_destroy(void* h) {
        auto s = static_cast<Shim*>(h);
        if (!s) return;
        if (s->processor.get()) s->processor->setProcessing(false);
        if (s->component.get()) s->component->setActive(false);
        if (s->component.get()) s->component->terminate();
        if (s->controller.get()) s->controller->terminate();
        delete s;
    }

    int vst_shim_set_state(void* h, const uint8_t* data, size_t len) {
        auto s = static_cast<Shim*>(h);
        if (!s || !s->component.get()) return 0;
        if (len == 0 || data == nullptr) return 1;

        auto buf = const_cast<uint8_t*>(data);
        MemoryStream stream(buf, static_cast<TSize>(len));
        if (s->component->setState(&stream) == kResultOk) return 1;

        if (s->controller.get()) {
            MemoryStream s2(buf, static_cast<TSize>(len));
            if (s->controller->setComponentState(&s2) == kResultOk) return 1;
        }
        return 0;
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
        if (!s || !s->processor.get()) return 0;
        if (frames > s->blockSize) frames = s->blockSize;
        if (frames <= 0) return 1;

        std::fill(s->scratchL.begin(), s->scratchL.begin() + frames, 0.0f);
        std::fill(s->scratchR.begin(), s->scratchR.begin() + frames, 0.0f);

        s->outChannelPtrs[0] = s->scratchL.data();
        s->outChannelPtrs[1] = s->scratchR.data();

        SimpleEventList evList;
        evList.events.swap(s->pending);

        s->inParams.clearQueue();
        s->outParams.clearQueue();
        s->context.projectTimeSamples = s->samplePos;

        ProcessData pd{};
        pd.processMode             = kRealtime;
        pd.symbolicSampleSize      = kSample32;
        pd.numSamples              = frames;
        pd.numInputs               = 0;
        pd.inputs                  = nullptr;
        pd.numOutputs              = static_cast<int32>(s->outBuses.size());
        pd.outputs                 = s->outBuses.data();
        pd.inputEvents             = &evList;
        pd.outputEvents            = nullptr;
        pd.inputParameterChanges   = &s->inParams;
        pd.outputParameterChanges  = &s->outParams;
        pd.processContext          = &s->context;

        if (s->processor->process(pd) != kResultOk) return 0;

        for (int i = 0; i < frames; ++i) {
            outL[i] = s->scratchL[i];
            outR[i] = s->scratchR[i];
        }
        s->samplePos += frames;
        return 1;
    }

    int vst_shim_process_inplace(void* h, float* bufL, float* bufR, int frames) {
        auto s = static_cast<Shim*>(h);
        if (!s || !s->processor.get()) return 0;
        if (frames > s->blockSize) frames = s->blockSize;
        if (frames <= 0) return 1;
        if (!s->hasInput) return 1;

        std::memcpy(s->inScratchL.data(), bufL, frames * sizeof(float));
        std::memcpy(s->inScratchR.data(), bufR, frames * sizeof(float));

        std::fill(s->scratchL.begin(), s->scratchL.begin() + frames, 0.0f);
        std::fill(s->scratchR.begin(), s->scratchR.begin() + frames, 0.0f);

        s->inChannelPtrs[0]  = s->inScratchL.data();
        s->inChannelPtrs[1]  = s->inScratchR.data();
        s->outChannelPtrs[0] = s->scratchL.data();
        s->outChannelPtrs[1] = s->scratchR.data();

        SimpleEventList evList;
        evList.events.swap(s->pending);

        s->inParams.clearQueue();
        s->outParams.clearQueue();
        s->context.projectTimeSamples = s->samplePos;

        ProcessData pd{};
        pd.processMode             = kRealtime;
        pd.symbolicSampleSize      = kSample32;
        pd.numSamples              = frames;
        pd.numInputs               = static_cast<int32>(s->inBuses.size());
        pd.inputs                  = s->inBuses.data();
        pd.numOutputs              = static_cast<int32>(s->outBuses.size());
        pd.outputs                 = s->outBuses.data();
        pd.inputEvents             = &evList;
        pd.outputEvents            = nullptr;
        pd.inputParameterChanges   = &s->inParams;
        pd.outputParameterChanges  = &s->outParams;
        pd.processContext          = &s->context;

        if (s->processor->process(pd) != kResultOk) return 0;

        std::memcpy(bufL, s->scratchL.data(), frames * sizeof(float));
        std::memcpy(bufR, s->scratchR.data(), frames * sizeof(float));
        s->samplePos += frames;
        return 1;
    }

}  // extern "C"
