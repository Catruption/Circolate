// src/log/spec.rs
//! The single registry of every log event in Circolate.
//!
//! Mirror of `src/spec.rs` for logging.  Plain data (std only).
//!
//! Regenerate the docs with:  UPDATE_DOCS=1 cargo test logs_md

#![allow(dead_code)]

use std::fmt::Write as _;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Level { Error, Warn, Info, Debug, Trace }

impl Level {
    pub fn label(self) -> &'static str {
        match self {
            Level::Error => "ERROR",
            Level::Warn  => "WARN",
            Level::Info  => "INFO",
            Level::Debug => "DEBUG",
            Level::Trace => "TRACE",
        }
    }
    pub fn slug(self) -> &'static str {
        match self {
            Level::Error => "error",
            Level::Warn  => "warn",
            Level::Info  => "info",
            Level::Debug => "debug",
            Level::Trace => "trace",
        }
    }
    pub fn parse(s: &str) -> Option<Level> {
        match s.to_ascii_lowercase().as_str() {
            "error" => Some(Level::Error),
            "warn" | "warning" => Some(Level::Warn),
            "info" => Some(Level::Info),
            "debug" => Some(Level::Debug),
            "trace" => Some(Level::Trace),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Category {
    Vst, Sf2, Flp, FlpSlicex, Midi, Player, Deps, Tui, Ffi,
}

impl Category {
    pub fn slug(self) -> &'static str {
        match self {
            Category::Vst       => "vst",
            Category::Sf2       => "sf2",
            Category::Flp       => "flp",
            Category::FlpSlicex => "flp.slicex",
            Category::Midi      => "midi",
            Category::Player    => "player",
            Category::Deps      => "deps",
            Category::Tui       => "tui",
            Category::Ffi       => "ffi",
        }
    }
    pub fn index(self) -> usize {
        match self {
            Category::Vst => 0,       Category::Sf2 => 1,
            Category::Flp => 2,       Category::FlpSlicex => 3,
            Category::Midi => 4,      Category::Player => 5,
            Category::Deps => 6,      Category::Tui => 7,
            Category::Ffi => 8,
        }
    }
    pub fn parse(s: &str) -> Option<Category> {
        match s.to_ascii_lowercase().as_str() {
            "vst" => Some(Category::Vst),
            "sf2" => Some(Category::Sf2),
            "flp" => Some(Category::Flp),
            "flp.slicex" | "slicex" => Some(Category::FlpSlicex),
            "midi" => Some(Category::Midi),
            "player" => Some(Category::Player),
            "deps" => Some(Category::Deps),
            "tui" => Some(Category::Tui),
            "ffi" => Some(Category::Ffi),
            _ => None,
        }
    }
}

pub const CATEGORIES: &[Category] = &[
    Category::Vst, Category::Sf2, Category::Flp, Category::FlpSlicex,
Category::Midi, Category::Player, Category::Deps, Category::Tui,
Category::Ffi,
];

pub const CATEGORY_COUNT: usize = 9;

pub struct LogEvent {
    pub key:      &'static str,
    pub level:    Level,
    pub category: Category,
    pub template: &'static str,
    pub fields:   &'static [&'static str],
    pub doc:      &'static str,
    pub since:    &'static str,
}

const fn ev(
    key: &'static str, level: Level, category: Category,
    template: &'static str, fields: &'static [&'static str],
    doc: &'static str, since: &'static str,
) -> LogEvent {
    LogEvent { key, level, category, template, fields, doc, since }
}

pub const LOG_EVENTS: &[LogEvent] = &[
    // ── VST host ───────────────────────────────────────────────────────
    ev("vst3.sdk.using", Level::Warn, Category::Vst,
       "Using VST3 SDK from {path}", &["path"],
       "build.rs fell back to the vendored SDK because VST3_SDK was unset or invalid.", "0.1.0"),
ev("vst3.sdk.source_missing", Level::Warn, Category::Vst,
   "Expected SDK source missing: {path}", &["path"],
   "The vendored VST3 SDK is missing a file build.rs expects.", "0.1.0"),
ev("vst3.sdk.invalid", Level::Warn, Category::Vst,
   "VST3_SDK set but does not look like a valid SDK: {path}", &["path"],
   "VST3_SDK pointed at a directory that doesn't contain the expected layout.", "0.1.0"),
ev("vst.load_failed", Level::Error, Category::Vst,
   "vst [{name}] failed to load: {err}", &["name", "err"],
   "A VST failed to instantiate.  Fires from both sidecar resolution and shim install.", "0.1.0"),
ev("vst.shim.load_failed", Level::Error, Category::Vst,
   "shim: failed to load {target}: {err}", &["target", "err"],
   "An effect shim's substitute VST failed to instantiate.", "0.1.0"),
ev("vst.smoke.silent", Level::Error, Category::Vst,
   "SILENT — plugin loaded but produced no audio: {path}", &["path"],
   "The plugin instantiated but its default patch rendered nothing.", "0.1.0"),

// ── SF2 engine ─────────────────────────────────────────────────────
ev("sf2.smoke.silent", Level::Error, Category::Sf2,
   "SILENT — SF2 engine loaded but produced no audio", &[],
   "The SF2 engine loaded but its test render was silent.", "0.1.0"),

// ── Player: master FX ──────────────────────────────────────────────
ev("player.master_fx.missing", Level::Warn, Category::Player,
   "master fx: missing {path}", &["path"],
   "A master FX state file was listed but not found on disk.", "0.1.0"),
ev("player.master_fx.state_read", Level::Error, Category::Player,
   "master fx: cannot read state {path}: {err}", &["path", "err"],
   "A master FX state file exists but could not be read.", "0.1.0"),
ev("player.master_fx.state_file_missing", Level::Warn, Category::Player,
   "master fx: state file missing: {path}", &["path"],
   "A master_fx.json entry names a state file that does not exist.", "0.1.0"),
ev("player.master_fx.load_failed", Level::Error, Category::Player,
   "master fx: failed to load {name}: {err}", &["name", "err"],
   "The master FX chain referenced a plugin that failed to instantiate.", "0.1.0"),
ev("player.master_fx.chain_failed", Level::Error, Category::Player,
   "master fx chain: {err}", &["err"],
   "EffectChain::load_default itself errored.", "0.1.0"),
ev("player.master_fx.loaded", Level::Info, Category::Player,
   "master fx: loaded {name}", &["name"],
   "A master FX plugin was loaded into the chain.", "0.1.0"),
ev("player.master_fx.non_utf8", Level::Warn, Category::Player,
   "master fx: non-UTF8 path for {name}", &["name"],
   "A master FX entry resolved to a path the host cannot represent as UTF-8.", "0.1.0"),

// ── Player: session ────────────────────────────────────────────────
ev("player.sample.decode_failed", Level::Warn, Category::Player,
   "  sample {rel} failed to decode: {err}", &["rel", "err"],
   "A sample referenced by samples.json could not be decoded.", "0.1.0"),
ev("player.embed.extract_failed", Level::Warn, Category::Player,
   "  embedded plugin extraction failed: {err}", &["err"],
   "A plugin bundled inside a .tsz/.flp payload could not be extracted.", "0.1.0"),
ev("player.sf2.materialize_failed", Level::Warn, Category::Player,
   "  SF2 materialize failed for {rel}: {err}", &["rel", "err"],
   "A SoundFont referenced by a sidecar could not be copied into the project cache.", "0.1.0"),
ev("player.shims.no_line", Level::Debug, Category::Player,
   "shims: no line {li}", &["li"],
   "A shim event referenced a BlockList line that has no call.", "0.1.0"),
ev("player.shims.debug", Level::Debug, Category::Player,
   "{body}", &["body"],
   "Freeform shim-association trace (formerly SHIMS_DEBUG=1).", "0.1.0"),
   ev("play.session.init_started", Level::Info, Category::Player,
      "Initializing VSTs (this may take a few seconds)...", &[],
      "Session load reached the VST install phase.", "0.1.0"),
ev("play.session.init_done", Level::Info, Category::Player,
   "VSTs initialized.", &[],
   "All VST hosts finished installing.", "0.1.0"),

// ── Player: project load ───────────────────────────────────────────
ev("player.perf", Level::Info, Category::Player,
   "load={load} worst={worst} overruns={overruns} voices={voices} vst={vst} shim={shim} fx={fx}",
   &["load", "worst", "overruns", "voices", "vst", "shim", "fx"],
   "Per-second render load as a fraction of the audio budget.", "0.1.0"),
ev("player.load.fidelity_summary", Level::Info, Category::Player,
   "  fidelity: {channels} channels, {automation} automation lanes, {playlist} playlist items, {inserts} insert buses, {multisample} multi-sample groups",
   &["channels", "automation", "playlist", "inserts", "multisample"],
   "One-line summary of fidelity.json contents when it parses cleanly.", "0.1.0"),
ev("player.load.fidelity_ignored", Level::Warn, Category::Player,
   "  fidelity: {err}, ignoring", &["err"],
   "fidelity.json failed to parse; the project loads without it.", "0.1.0"),
ev("player.load.embed_extracted", Level::Info, Category::Player,
   "  extracted {count} embedded plugin bundle(s)", &["count"],
   "Bundled plugins materialized into the cache.", "0.1.0"),
ev("player.load.no_samples_json", Level::Warn, Category::Player,
   "  (no samples.json in {source})", &["source"],
   "The project has no sample library; only SF2/VST instruments can play.", "0.1.0"),
ev("player.load.samples_count", Level::Info, Category::Player,
   "  loaded {count} samples", &["count"],
   "Count of decoded samples after loading samples.json.", "0.1.0"),
ev("player.load.multi_sample_count", Level::Info, Category::Player,
   "  multi-sample: {count} instrument(s)", &["count"],
   "Count of multi-sample instrument groups built from fidelity.json.", "0.1.0"),
ev("player.load.sidecar_unreadable", Level::Warn, Category::Player,
   "  (sidecar unreadable: {rel})", &["rel"],
   "An `.instrument` sidecar exists but could not be read.", "0.1.0"),
ev("player.load.sf2_not_in_source", Level::Warn, Category::Player,
   "  (SF2 {rel} not in source)", &["rel"],
   "A sidecar references a SoundFont that is absent from the source.", "0.1.0"),
ev("player.load.vst_not_installed", Level::Warn, Category::Player,
   "  (VST {plugin} not installed, skipping; sidecar {sidecar})",
   &["plugin", "sidecar"],
   "A sidecar's VST is not present on disk or in the local plugin scan.", "0.1.0"),
ev("player.load.fallback_trim", Level::Info, Category::Player,
   "  {count} stand-in plugins: trimmed {db:.1} dB each so they sit as backing",
   &["count", "db"],
   "Fallback VSTs receive a per-instance trim so they don't dominate the mix.", "0.1.0"),
ev("player.load.summary", Level::Info, Category::Player,
   "  loaded {sf2s} sf2 instruments, {engines} sf2 engines, {vsts} vst instruments",
   &["sf2s", "engines", "vsts"],
   "End-of-sidecar-loop count.", "0.1.0"),
ev("player.load.requires_ignored", Level::Warn, Category::Player,
   "  requires: {err}, ignoring", &["err"],
   "requires.json failed to parse; the dependency pre-flight is skipped.", "0.1.0"),
ev("player.load.insert_chains", Level::Info, Category::Player,
   "  loaded {chains} insert chain(s), {slots} slot(s) total",
   &["chains", "slots"],
   "Mixer insert chains materialized from sidecars.", "0.1.0"),
ev("player.load.tempo_shims", Level::Info, Category::Player,
   "  tempo shims: {count} event(s)", &["count"],
   "Number of `ƒTempo` shims converted into tempo-map events.", "0.1.0"),
ev("player.load.parsed", Level::Info, Category::Player,
   "  parsed {parts} parts, {blocks} blocks, {lines} BlockList lines, {voices} voices",
   &["parts", "blocks", "lines", "voices"],
   "End-of-parse count: how much was in the trackscript.", "0.1.0"),

// ── Player: install (session setup) ────────────────────────────────
ev("player.install.vst_not_found", Level::Warn, Category::Player,
   "vst [{name}] not found (looked up by name {stem})", &["name", "stem"],
   "A VST referenced by the project wasn't found in the plugin scan; the track is silent.", "0.1.0"),
ev("player.install.sf2_load_failed", Level::Error, Category::Player,
   "sf2 [{name}]: failed to load {path}: {err}", &["name", "path", "err"],
   "A SoundFont engine failed to load during session setup.", "0.1.0"),
ev("player.install.sf2_debug", Level::Debug, Category::Player,
   "{body}", &["body"],
   "Per-SF2-engine gain breakdown at install time (formerly FLP_DEBUG=1).", "0.1.0"),
   ev("player.install.shims_no_share", Level::Warn, Category::Player,
      "  shims: share/ not found; {count} shim(s) skipped", &["count"],
      "share/fallbacks.json is absent, so shim targets cannot be resolved.", "0.1.0"),
ev("player.install.shims_debug", Level::Debug, Category::Player,
   "{body}", &["body"],
   "Freeform shim install trace (formerly SHIM_DEBUG=1).", "0.1.0"),
   ev("player.install.shim_loaded", Level::Info, Category::Player,
      "  shim {id}: loaded '{target}'", &["id", "target"],
      "A shim effect was installed on the mixer.", "0.1.0"),
ev("player.install.shims_summary", Level::Info, Category::Player,
   "  shims: {attached} attachment(s), {skipped} skipped, {unique} unique VST3 host(s) (from {total} shim event(s))",
   &["attached", "skipped", "unique", "total"],
   "End-of-shims-install summary.", "0.1.0"),

// ── Player: loader ─────────────────────────────────────────────────
ev("player.loader.voice_skipped", Level::Debug, Category::Player,
   "flp: voice skipped: part={part} sample_id={sample_id} start_row={start_row} end_row={end_row} start={start} end={end} min_end={min_end} reason={reason}",
   &["part", "sample_id", "start_row", "end_row", "start", "end", "min_end", "reason"],
   "A voice was dropped because its interval is empty or before min_end.  Formerly FLP_DEBUG=1.", "0.1.0"),
ev("player.loader.voice_dead", Level::Debug, Category::Player,
   "flp: voice DEAD: part={part} sample_id={sample_id} sample is empty",
   &["part", "sample_id"],
   "A voice references a sample that decoded to zero frames.", "0.1.0"),
ev("player.loader.voice_trace", Level::Debug, Category::Player,
   "vtrace: scheduled sample {sample_id} start={start} end={end} ({len} samples, {secs:.3}s) offset={offset} pitch_hz={pitch_hz:.2} vol={vol}",
   &["sample_id", "start", "end", "len", "secs", "offset", "pitch_hz", "vol"],
   "Per-voice trace for a specific sample_id, gated by VOICE_TRACE_SAMPLE=<id>.", "0.1.0"),

// ── Player: render ─────────────────────────────────────────────────
ev("player.render.vst_skipped", Level::Warn, Category::Player,
   "  skipping VST [{name}]: plugin {stem} not installed",
   &["name", "stem"],
   "A VST referenced by the project was not found on the local plugin scan.", "0.1.0"),
ev("player.render.vst_loaded", Level::Info, Category::Player,
   "  loaded VST [{iid}] {name} ({tag})", &["iid", "name", "tag"],
   "A VST host was installed for offline rendering.", "0.1.0"),
ev("player.render.vst_load_failed", Level::Error, Category::Player,
   "  failed to load VST {name}: {err}", &["name", "err"],
   "A VST host failed to instantiate during offline render.", "0.1.0"),
ev("player.render.vst_hosts_installed", Level::Info, Category::Player,
   "  installed {count} VST host(s)", &["count"],
   "Summary at end of the offline VST install loop.", "0.1.0"),
ev("player.render.vst_fallback_gain", Level::Info, Category::Player,
   "  VST fallback attenuation: {gain:.3} (VST_FALLBACK_GAIN)", &["gain"],
   "Emitted when VST_FALLBACK_GAIN is not 1.0.", "0.1.0"),
ev("player.render.sf2_load_failed", Level::Error, Category::Player,
   "  sf2 engine failed to load {name}: {err}", &["name", "err"],
   "A SoundFont engine failed to load during offline render.", "0.1.0"),
ev("player.render.sf2_debug", Level::Debug, Category::Player,
   "{body}", &["body"],
   "Per-SF2-engine gain breakdown (formerly FLP_DEBUG=1).", "0.1.0"),
   ev("player.render.sf2_engines_installed", Level::Info, Category::Player,
      "  installed {count} SF2 engine(s)", &["count"],
      "Summary at end of the offline SF2 install loop.", "0.1.0"),
ev("player.render.wav_clipped", Level::Warn, Category::Player,
   "  warning: {clipped} output samples clip at peak {peak:.3}",
   &["clipped", "peak"],
   "Offline render wrote samples outside [-1, 1].", "0.1.0"),

// ── FLP parse / convert ────────────────────────────────────────────
ev("flp.tempo.raw", Level::Debug, Category::Flp,
   "bpm_f64={bpm_f64:.6} as_u32={bpm_u32} round={bpm_round}",
   &["bpm_f64", "bpm_u32", "bpm_round"],
   "Fires where the tempo event is parsed; three candidate truncations.", "0.1.0"),
ev("flp.ev178.raw", Level::Debug, Category::Flp,
   "EV178 raw={raw} chan_idx={chan_idx}", &["raw", "chan_idx"],
   "An EV178 event was decoded.", "0.1.0"),
ev("flp.classify.trial", Level::Debug, Category::Flp,
   "classify chan {iid}: trying {candidate}", &["iid", "candidate"],
   "A candidate plugin/SF2 was tested against a channel.", "0.1.0"),
ev("flp.classify.native", Level::Debug, Category::Flp,
   "flp: chan {iid} native, no plugin", &["iid"],
   "A channel is native (no plugin).", "0.1.0"),
   ev("flp.place.slide", Level::Debug, Category::Flp,
      "slide check chan {chan} key {key} flags {flags}",
      &["chan", "key", "flags"],
      "A Slicex slide note was classified.", "0.1.0"),
ev("flp.place.slides_kept", Level::Debug, Category::Flp,
   "flp: {bent} Slicex slide note(s) kept as pitch bends", &["bent"],
   "Summary of slide notes that became pitch bends.", "0.1.0"),
ev("flp.place.disabled", Level::Debug, Category::Flp,
   "flp: skipping disabled channels {chans}", &["chans"],
   "Channels disabled in the source project were dropped.", "0.1.0"),

// ── Slicex dumps ───────────────────────────────────────────────────
ev("flp.slicex.header", Level::Debug, Category::FlpSlicex,
   "{body}", &["body"],
   "Slicex outer header + WAV1 chunks + WAV2 identity.", "0.1.0"),
ev("flp.slicex.settings", Level::Debug, Category::FlpSlicex,
   "{body}", &["body"],
   "Slicex 402-byte settings header and per-slice records.", "0.1.0"),
ev("flp.slicex.trest", Level::Debug, Category::FlpSlicex,
   "{body}", &["body"],
   "Slicex trest region records and region-param block.", "0.1.0"),
ev("flp.slicex.env", Level::Debug, Category::FlpSlicex,
   "{body}", &["body"],
   "Slicex amp envelope points and ADSR decomposition.", "0.1.0"),
ev("flp.slicex.computed", Level::Debug, Category::FlpSlicex,
   "{body}", &["body"],
   "Slicex derived values: base key, key layout, timing.", "0.1.0"),
ev("flp.slicex.application", Level::Debug, Category::FlpSlicex,
   "{body}", &["body"],
   "Which Slicex parameter groups were applied and their values.", "0.1.0"),
ev("flp.slicex.speed_env", Level::Debug, Category::FlpSlicex,
   "{body}", &["body"],
   "Slicex SPEED envelope points.", "0.1.0"),
ev("flp.slicex.stretch", Level::Debug, Category::FlpSlicex,
   "{body}", &["body"],
   "Tempo-stretch decision: sample BPM source, project BPM, factor.", "0.1.0"),

// ── MIDI ───────────────────────────────────────────────────────────
ev("midi.load_failed", Level::Error, Category::Midi,
   "midi: skipping {path} (load failed: {err})", &["path", "err"],
   "A MIDI file could not be parsed.", "0.1.0"),

// ── Deps ───────────────────────────────────────────────────────────
ev("deps.fetch.step_failed", Level::Error, Category::Deps,
   "fetch {name}: {err}", &["name", "err"],
   "An acquire.sh step for a missing dependency failed.", "0.1.0"),
ev("deps.fetch.incomplete", Level::Warn, Category::Deps,
   "fetch {name}: step done but {provides} still missing", &["name", "provides"],
   "An acquire step reported success but its declared output is absent.", "0.1.0"),

// ── TUI ────────────────────────────────────────────────────────────
ev("tui.settings.parse_failed", Level::Warn, Category::Tui,
   "player_settings {path}: {err}", &["path", "err"],
   "player_settings.json could not be parsed.", "0.1.0"),
ev("tui.settings.preset_failed", Level::Warn, Category::Tui,
   "player_settings: {err}", &["err"],
   "A theme preset name could not be applied.", "0.1.0"),
ev("tui.settings.unknown_token", Level::Warn, Category::Tui,
   "player_settings: unknown token `{token}`", &["token"],
   "A theme override key is not recognized.", "0.1.0"),
ev("tui.settings.bad_color", Level::Warn, Category::Tui,
   "player_settings: bad color `{value}` for {token}", &["value", "token"],
   "A color value in player_settings.json could not be parsed.", "0.1.0"),
ev("tui.cover.read_failed", Level::Warn, Category::Tui,
   "cover: read {path} failed: {err}", &["path", "err"],
   "An album-art image could not be read or decoded.", "0.1.0"),
ev("tui.debug", Level::Debug, Category::Tui,
   "{body}", &["body"],
   "Freeform TUI event trace (TUI_DEBUG=1).", "0.1.0"),
   ev("tui.menu.dump_started", Level::Info, Category::Tui,
      "Dumping {path}...", &["path"],
      "User chose Dump from the home menu.", "0.1.0"),
ev("tui.menu.dump_ok", Level::Info, Category::Tui,
   "Dump successful: {path}", &["path"],
   "Dump finished cleanly.", "0.1.0"),
ev("tui.menu.dump_failed", Level::Error, Category::Tui,
   "Dump failed: {err}", &["err"],
   "Dump errored.", "0.1.0"),
ev("tui.menu.export_started", Level::Info, Category::Tui,
   "Exporting {path} to trackscript directory...", &["path"],
   "User chose Export from the home menu.", "0.1.0"),
ev("tui.menu.export_ok", Level::Info, Category::Tui,
   "Export successful: {path}", &["path"],
   "Export finished cleanly.", "0.1.0"),
ev("tui.menu.export_failed", Level::Error, Category::Tui,
   "Export failed: {err}", &["err"],
   "Export errored.", "0.1.0"),

// ── FFI stderr ─────────────────────────────────────────────────────
ev("ffi.stderr", Level::Warn, Category::Ffi,
   "{line}", &["line"],
   "A line written to fd 2 by a C++ plugin.", "0.1.0"),
];

pub fn lookup(key: &str) -> Option<&'static LogEvent> {
    LOG_EVENTS.iter().find(|e| e.key == key)
}

pub fn render(spec: &LogEvent, values: &[(&str, String)]) -> String {
    let mut out = String::with_capacity(spec.template.len() + 64);
    let mut rest = spec.template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let inner = &after[..end];
        let (name, prec) = match inner.split_once(':') {
            Some((n, f)) => (n, f.strip_prefix('.').and_then(|p| p.parse::<usize>().ok())),
            None => (inner, None),
        };
        if let Some((_, v)) = values.iter().find(|(k, _)| *k == name) {
            match prec {
                Some(p) => match v.parse::<f64>() {
                    Ok(f) => { let _ = write!(out, "{:.*}", p, f); }
                    Err(_) => out.push_str(v),
                },
                None => out.push_str(v),
            }
        } else {
            out.push('{'); out.push_str(inner); out.push('}');
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

pub fn render_markdown() -> String {
    let mut out = String::new();
    out.push_str("# Log reference\n\n");
    out.push_str("<!-- Generated from src/log/spec.rs. Do not edit by hand. -->\n\n");
    out.push_str(
        "Every log Circolate can emit, grouped by category.  \
Call sites name a `key` and supply field values; the text and \
level live only in this table.\n\n",
    );

    for cat in CATEGORIES {
        let rows: Vec<&LogEvent> =
        LOG_EVENTS.iter().filter(|e| e.category == *cat).collect();
        if rows.is_empty() { continue; }
        let _ = writeln!(out, "## `{}`\n", cat.slug());
        for e in rows {
            let _ = writeln!(out, "### `{}` — {}\n", e.key, e.level.label());
            let _ = writeln!(out, "```\n{}\n```\n", e.template);
            if !e.fields.is_empty() {
                let names: Vec<String> =
                e.fields.iter().map(|f| format!("`{}`", f)).collect();
                let _ = writeln!(out, "- fields: {}\n", names.join(", "));
            }
            let _ = writeln!(out, "- since {}\n", e.since);
            let _ = writeln!(out, "- {}\n", e.doc);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_template_renders_without_leftover_placeholders() {
        for e in LOG_EVENTS {
            let values: Vec<(&str, String)> = e.fields.iter()
            .map(|f| (*f, format!("<{}>", f)))
            .collect();
            let rendered = render(e, &values);
            assert!(!rendered.contains('{'),
                    "template for {:?} left a placeholder: {}", e.key, rendered);
        }
    }

    #[test]
    fn keys_are_unique_and_slug_shaped() {
        let mut seen = HashSet::new();
        for e in LOG_EVENTS {
            assert!(seen.insert(e.key), "duplicate log key {:?}", e.key);
            assert!(
                e.key.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_'),
                    "log key {:?} is not slug-shaped", e.key);
        }
    }

    #[test]
    fn templates_mention_every_declared_field() {
        for e in LOG_EVENTS {
            for f in e.fields {
                assert!(
                    e.template.contains(&format!("{{{}}}", f))
                    || e.template.contains(&format!("{{{}:", f)),
                        "log {:?}: field {:?} declared but never in template {:?}",
                        e.key, f, e.template);
            }
        }
    }

    #[test]
    fn every_category_has_at_least_one_event() {
        for cat in CATEGORIES {
            assert!(
                LOG_EVENTS.iter().any(|e| e.category == *cat),
                    "category {:?} has no events", cat);
        }
    }

    #[test]
    fn slugs_round_trip() {
        for e in LOG_EVENTS {
            let _ = Level::parse(e.level.slug())
            .unwrap_or_else(|| panic!("level slug {:?} does not parse", e.level.slug()));
            let _ = Category::parse(e.category.slug())
            .unwrap_or_else(|| panic!("category slug {:?} does not parse", e.category.slug()));
        }
    }

    #[test]
    fn logs_md_is_up_to_date() {
        let Ok(dir) = std::env::var("CARGO_MANIFEST_DIR") else { return };
        let path = std::path::Path::new(&dir).join("docs").join("LOGS.md");
        let fresh = render_markdown();
        if std::env::var_os("UPDATE_DOCS").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &fresh).unwrap();
            return;
        }
        let on_disk = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(on_disk == fresh,
                "docs/LOGS.md is stale; run UPDATE_DOCS=1 cargo test logs_md");
    }
}
