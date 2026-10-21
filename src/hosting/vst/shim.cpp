// vst_shim.cpp — flat C ABI over the Steinberg VST3 SDK.
//
// [unchanged doc header]
//
// Controller discovery.  VST3 has three architectures in the wild:
//   * Multi-component (most commercial plugins): IComponent and
//     IEditController share the same class ID; createInstance with
//     each interface from that ID works.
//   * Single-component (JUCE, some open-source): only IComponent is
//     registered; the same object also implements IEditController,
//     reachable via queryInterface.
//   * Split-registration (DPF — Dragonfly, x42, LSP, ProM): the
//     factory advertises *two* separate kVstAudioEffectClass entries,
//     one for the processor and one for the controller, under
//     different class IDs.
// The discovery path tries each in order: direct createInstance →
// component queryInterface → factory scan over every class ID.  Only
// the first successful path is used; if it was queryInterface the
// `singleComponent` flag is set so terminate() isn't called twice on
// a shared object.

#include <algorithm>
#include <cctype>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <dirent.h>
#include <dlfcn.h>
#include <exception>
#include <memory>
#include <string>
#include <sys/stat.h>
#include <vector>

#include "public.sdk/source/vst/hosting/module.h"
#include "public.sdk/source/vst/hosting/hostclasses.h"
#include "public.sdk/source/vst/hosting/parameterchanges.h"
#include "public.sdk/source/common/memorystream.h"
#include "pluginterfaces/base/ipluginbase.h"
#include "pluginterfaces/base/ustring.h"
#include "pluginterfaces/vst/ivstcomponent.h"
#include "pluginterfaces/vst/ivsteditcontroller.h"
#include "pluginterfaces/vst/ivstaudioprocessor.h"
#include "pluginterfaces/vst/ivstevents.h"
#include "pluginterfaces/vst/ivstprocesscontext.h"
#include "pluginterfaces/vst/vstspeaker.h"

using namespace Steinberg;
using namespace Steinberg::Vst;

// ── debug logging ──────────────────────────────────────────────────────

static bool shim_debug_enabled() {
    static const bool enabled = std::getenv("VST_SHIM_DEBUG") != nullptr;
    return enabled;
}

#define SHIM_LOG(...) \
do { if (shim_debug_enabled()) std::fprintf(stderr, __VA_ARGS__); } while (0)

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

    // ── manual module loader ───────────────────────────────────────────────

    struct LoadedModule {
        void* lib = nullptr;
        IPtr<IPluginFactory> factory;

        ~LoadedModule() {
            factory = nullptr;
            if (lib) {
                using ModuleExitFn = bool (PLUGIN_API*)();
                auto exit_fn = reinterpret_cast<ModuleExitFn>(dlsym(lib, "ModuleExit"));
                if (exit_fn) exit_fn();
                dlclose(lib);
            }
        }
    };

    static std::string find_so_in_bundle(const std::string& bundle) {
        std::string contents = bundle + "/Contents";
        DIR* cd = opendir(contents.c_str());
        if (!cd) return {};

        std::string found;
        struct dirent* arch_entry;
        while (found.empty() && (arch_entry = readdir(cd))) {
            std::string an = arch_entry->d_name;
            if (an == "." || an == "..") continue;

            std::string arch_dir = contents + "/" + an;
            DIR* ad = opendir(arch_dir.c_str());
            if (!ad) continue;

            struct dirent* so_entry;
            while ((so_entry = readdir(ad))) {
                std::string n = so_entry->d_name;
                std::string lower = n;
                for (auto& c : lower)
                    c = static_cast<char>(std::tolower(static_cast<unsigned char>(c)));
                if (lower.size() > 3 && lower.compare(lower.size() - 3, 3, ".so") == 0) {
                    found = arch_dir + "/" + n;
                    break;
                }
            }
            closedir(ad);
        }
        closedir(cd);
        return found;
    }

    static LoadedModule* load_module(const char* path, std::string& err) {
        struct stat st{};
        if (stat(path, &st) != 0) {
            err = "path does not exist";
            return nullptr;
        }

        std::string so_path;
        if (S_ISDIR(st.st_mode)) {
            so_path = find_so_in_bundle(path);
            if (so_path.empty()) {
                err = "no .so in any Contents/*/ subdirectory";
                return nullptr;
            }
        } else {
            so_path = path;
        }

        void* lib = dlopen(so_path.c_str(), RTLD_NOW | RTLD_LOCAL);
        if (!lib) {
            err = std::string("dlopen failed: ") + dlerror();
            return nullptr;
        }

        using ModuleEntryFn = bool (PLUGIN_API*)(void*);
        using GetFactoryFn  = IPluginFactory* (PLUGIN_API*)();

        auto entry = reinterpret_cast<ModuleEntryFn>(dlsym(lib, "ModuleEntry"));
        if (entry) entry(lib);

        auto get_factory = reinterpret_cast<GetFactoryFn>(dlsym(lib, "GetPluginFactory"));
        if (!get_factory) {
            err = "no GetPluginFactory export in .so";
            dlclose(lib);
            return nullptr;
        }

        IPluginFactory* raw = get_factory();
        if (!raw) {
            err = "GetPluginFactory returned nullptr";
            dlclose(lib);
            return nullptr;
        }
        raw->addRef();

        auto* m = new LoadedModule();
        m->lib = lib;
        m->factory = IPtr<IPluginFactory>(raw);
        return m;
    }

    // ── per-instance state ─────────────────────────────────────────────────

    struct PendingParam {
        ParamID id;
        ParamValue normalized;
    };

    struct Shim {
        LoadedModule* module = nullptr;
        IPtr<IHostApplication> host_app;
        IPtr<IComponent>       component;
        IPtr<IEditController>  controller;
        IPtr<IAudioProcessor>  processor;

        int sampleRate = 44100;
        int blockSize  = 512;

        bool hasInput        = false;
        bool active          = false;
        bool singleComponent = false;

        std::vector<BusScratch> outBuses;
        std::vector<BusScratch> inBuses;

        std::vector<Event> pending;
        std::vector<PendingParam> pendingParams;

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

    /// Try every plausible way of getting an IEditController for a
    /// component that lives at `ci`.  Returns the controller (or null) and
    /// sets `single_component` if the object is shared with the component.
    static IPtr<IEditController> discover_controller(
        const VST3::Hosting::PluginFactory& factory,
        const VST3::Hosting::ClassInfo& ci,
        IComponent* component,
        bool& single_component)
    {
        single_component = false;

        // 1. Same class ID as the component (multi-component normal case).
        IPtr<IEditController> ctrl = factory.createInstance<IEditController>(ci.ID());
        if (ctrl.get()) {
            SHIM_LOG("vst_shim: controller via factory (multi-component)\n");
            return ctrl;
        }

        // 2. queryInterface on the component (single-component JUCE-style).
        {
            IEditController* raw = nullptr;
            if (component->queryInterface(
                IEditController::iid,
                reinterpret_cast<void**>(&raw)) == kResultOk && raw) {
                raw->addRef();
            ctrl = IPtr<IEditController>(raw);
            single_component = true;
            SHIM_LOG("vst_shim: controller via queryInterface (single-component)\n");
            return ctrl;
                }
        }

        // 3. Factory scan — DPF registers a *separate* class ID for the
        //    controller.  Try every class the factory advertises, skipping
        //    the one we used for the component (memcmp on TUIDs).
        for (const auto& other : factory.classInfos()) {
            if (other.ID() == ci.ID()) continue;

            ctrl = factory.createInstance<IEditController>(other.ID());
            if (ctrl.get()) {
                SHIM_LOG("vst_shim: controller via factory scan (%s)\n",
                         other.name().c_str());
                return ctrl;
            }
        }

        SHIM_LOG("vst_shim: no IEditController (all discovery paths failed)\n");
        return nullptr;
    }

    static bool configure(Shim* s) {
        tresult r = s->component->initialize(s->host_app);
        if (r != kResultOk) {
            SHIM_LOG("vst_shim: component->initialize returned %d\n", (int)r);
            return false;
        }
        if (s->controller.get() && !s->singleComponent) {
            // Multi-component plugin: separate controller instance needs
            // its own initialize.  Single-component plugins alias component
            // and controller — the component->initialize above already
            // covered it.
            r = s->controller->initialize(s->host_app);
            if (r != kResultOk) {
                SHIM_LOG("vst_shim: controller->initialize returned %d (ignoring)\n",
                         (int)r);
            }
        }

        int32 inCount  = s->component->getBusCount(kAudio, kInput);
        int32 outCount = s->component->getBusCount(kAudio, kOutput);
        s->hasInput    = inCount > 0;

        SHIM_LOG("vst_shim: bus counts in=%d out=%d\n", inCount, outCount);

        if (std::getenv("VST_SHIM_DEBUG")) {
            BusInfo bi{};
            for (int32 b = 0; b < inCount; ++b) {
                s->component->getBusInfo(kAudio, kInput, b, bi);
                SHIM_LOG("vst_shim:   in bus %d channels=%d\n", b, bi.channelCount);
            }
            for (int32 b = 0; b < outCount; ++b) {
                s->component->getBusInfo(kAudio, kOutput, b, bi);
                SHIM_LOG("vst_shim:   out bus %d channels=%d\n", b, bi.channelCount);
            }
        }

        SpeakerArrangement out_arr = SpeakerArr::kStereo;
        tresult arr_res;
        if (s->hasInput) {
            SpeakerArrangement in_arr = SpeakerArr::kStereo;
            arr_res = s->processor->setBusArrangements(&in_arr, 1, &out_arr, 1);
        } else {
            arr_res = s->processor->setBusArrangements(nullptr, 0, &out_arr, 1);
        }
        if (arr_res != kResultOk) {
            SHIM_LOG("vst_shim: setBusArrangements returned %d (continuing)\n",
                     (int)arr_res);
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
        tresult sr_res = s->processor->setupProcessing(setup);
        if (sr_res != kResultOk) {
            SHIM_LOG("vst_shim: setupProcessing returned %d (sr=%d block=%d)\n",
                     (int)sr_res, s->sampleRate, s->blockSize);
            return false;
        }
        SHIM_LOG("vst_shim: setupProcessing ok\n");

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

    static void flush_pending_params(Shim* s) {
        if (s->pendingParams.empty()) return;
        for (const auto& p : s->pendingParams) {
            int32 queueIndex = -1;
            s->inParams.addParameterData(p.id, queueIndex);
            if (queueIndex >= 0) {
                auto* queue = s->inParams.getParameterData(queueIndex);
                if (queue) {
                    int32 pointIdx = 0;
                    queue->addPoint(0, p.normalized, pointIdx);
                }
            }
        }
        s->pendingParams.clear();
    }

    static void copy_string128_to_ascii(String128 src, char* dst, int dstSize) {
        if (dstSize <= 0) return;
        UString(src, 128).toAscii(dst, dstSize);
        dst[dstSize - 1] = '\0';
    }

    static std::string slot_to_greek(int32 slot) {
        static const char* glyphs[] = {
            "α","β","γ","δ","ε","ζ","η","θ","ι","κ"
        };
        if (slot <= 0) return {};
        std::string out;
        while (slot > 0) {
            int32 digit = ((slot - 1) % 10) + 1;
            out = std::string(glyphs[digit - 1]) + out;
            slot = (slot - 1) / 10;
        }
        return out;
    }

    extern "C" {

        void* vst_shim_create(const char* path, int sr, int block) {
            try {
                std::string err;
                auto* mod = load_module(path, err);
                if (!mod) {
                    SHIM_LOG("vst_shim: load_module(%s) failed: %s\n",
                             path, err.c_str());
                    return nullptr;
                }
                SHIM_LOG("vst_shim: module loaded from %s\n", path);

                VST3::Hosting::PluginFactory factory(mod->factory);

                SHIM_LOG("vst_shim: factory has %zu class(es)\n",
                         factory.classInfos().size());
                for (const auto& info : factory.classInfos()) {
                    SHIM_LOG("vst_shim:   class=%s cat=%s\n",
                             info.name().c_str(), info.category().c_str());
                }

                VST3::Hosting::ClassInfo ci;
                if (!pick_class(factory, ci)) {
                    SHIM_LOG("vst_shim: no audio-effect class\n");
                    delete mod;
                    return nullptr;
                }
                SHIM_LOG("vst_shim: picked class %s\n", ci.name().c_str());

                auto s = new Shim();
                s->module     = mod;
                s->sampleRate = sr;
                s->blockSize  = block;

                s->host_app = new HostApplication();

                s->component = factory.createInstance<IComponent>(ci.ID());
                if (!s->component.get()) {
                    SHIM_LOG("vst_shim: createInstance<IComponent> failed\n");
                    delete mod;
                    delete s;
                    return nullptr;
                }

                s->controller = discover_controller(
                    factory, ci, s->component.get(), s->singleComponent);

                if (s->component->queryInterface(IAudioProcessor::iid,
                    reinterpret_cast<void**>(&s->processor)) != kResultOk
                    || !s->processor.get()) {
                    SHIM_LOG("vst_shim: queryInterface(IAudioProcessor) failed\n");
                delete mod;
                delete s;
                return nullptr;
                    }

                    if (!configure(s)) {
                        SHIM_LOG("vst_shim: configure() failed\n");
                        delete mod;
                        delete s;
                        return nullptr;
                    }
                    SHIM_LOG("vst_shim: configured\n");

                    alloc_scratch(s);

                    s->context = ProcessContext{};
                    s->context.state              = ProcessContext::kPlaying;
                    s->context.sampleRate         = static_cast<double>(sr);
                    s->context.projectTimeSamples = 0;
                    s->context.tempo              = 120.0;
                    s->context.timeSigNumerator   = 4;
                    s->context.timeSigDenominator = 4;

                    return s;
            } catch (const std::exception& e) {
                SHIM_LOG("vst_shim: exception: %s\n", e.what());
                return nullptr;
            } catch (...) {
                SHIM_LOG("vst_shim: unknown exception\n");
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
            // Single-component plugins alias controller and component —
            // terminate once, on the component, so the shared object isn't
            // torn down twice.
            if (s->controller.get() && !s->singleComponent) s->controller->terminate();
            if (s->component.get()) s->component->terminate();

            s->processor = nullptr;
            s->controller = nullptr;
            s->component = nullptr;
            s->host_app = nullptr;

            delete s->module;
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

        int vst_shim_set_param_by_index(void* h, int index, double normalized) {
            auto s = static_cast<Shim*>(h);
            if (!s || !s->controller.get()) return 0;

            int32 count = s->controller->getParameterCount();
            if (index < 0 || index >= count) return 0;

            ParameterInfo info{};
            if (s->controller->getParameterInfo(index, info) != kResultOk) return 0;

            double v = std::clamp(normalized, 0.0, 1.0);

            s->controller->setParamNormalized(info.id, v);
            s->pendingParams.push_back({info.id, v});

            SHIM_LOG("vst_shim: set_param_by_index(%d → id=%u, v=%.4f)\n",
                     index, (unsigned)info.id, v);
            return 1;
        }

        int vst_shim_dump_params(void* h) {
            auto s = static_cast<Shim*>(h);
            if (!s || !s->controller.get()) return -1;

            int32 count = s->controller->getParameterCount();
            for (int32 i = 0; i < count; ++i) {
                ParameterInfo info{};
                if (s->controller->getParameterInfo(i, info) != kResultOk) continue;

                char titleStr[128] = {0};
                char unitsStr[64]  = {0};
                copy_string128_to_ascii(info.title, titleStr, sizeof(titleStr));
                copy_string128_to_ascii(info.units, unitsStr, sizeof(unitsStr));

                std::string slot = slot_to_greek(i + 1);
                std::fprintf(stderr,
                             "param[%d] slot=%-4s id=%u title=\"%s\" default=%.4f units=\"%s\" flags=0x%x\n",
                             i, slot.c_str(), (unsigned)info.id, titleStr,
                             (double)info.defaultNormalizedValue, unitsStr,
                             (unsigned)info.flags);
            }
            return count;
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
            flush_pending_params(s);
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
            flush_pending_params(s);
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
