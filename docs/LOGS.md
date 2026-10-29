# Log reference

<!-- Generated from src/log/spec.rs. Do not edit by hand. -->

Every log Circolate can emit, grouped by category.  Call sites name a `key` and supply field values; the text and level live only in this table.

## `vst`

### `vst3.sdk.using` — WARN

```
Using VST3 SDK from {path}
```

- fields: `path`

- since 0.1.0

- build.rs fell back to the vendored SDK because VST3_SDK was unset or invalid.

### `vst3.sdk.source_missing` — WARN

```
Expected SDK source missing: {path}
```

- fields: `path`

- since 0.1.0

- The vendored VST3 SDK is missing a file build.rs expects.

### `vst3.sdk.invalid` — WARN

```
VST3_SDK set but does not look like a valid SDK: {path}
```

- fields: `path`

- since 0.1.0

- VST3_SDK pointed at a directory that doesn't contain the expected layout.

### `vst.load_failed` — ERROR

```
vst [{name}] failed to load: {err}
```

- fields: `name`, `err`

- since 0.1.0

- A VST failed to instantiate.  Fires from both sidecar resolution and shim install.

### `vst.shim.load_failed` — ERROR

```
shim: failed to load {target}: {err}
```

- fields: `target`, `err`

- since 0.1.0

- An effect shim's substitute VST failed to instantiate.

### `vst.smoke.silent` — ERROR

```
SILENT — plugin loaded but produced no audio: {path}
```

- fields: `path`

- since 0.1.0

- The plugin instantiated but its default patch rendered nothing.

## `sf2`

### `sf2.smoke.silent` — ERROR

```
SILENT — SF2 engine loaded but produced no audio
```

- since 0.1.0

- The SF2 engine loaded but its test render was silent.

## `flp`

### `flp.tempo.raw` — DEBUG

```
bpm_f64={bpm_f64:.6} as_u32={bpm_u32} round={bpm_round}
```

- fields: `bpm_f64`, `bpm_u32`, `bpm_round`

- since 0.1.0

- Fires where the tempo event is parsed; three candidate truncations.

### `flp.ev178.raw` — DEBUG

```
EV178 raw={raw} chan_idx={chan_idx}
```

- fields: `raw`, `chan_idx`

- since 0.1.0

- An EV178 event was decoded.

### `flp.classify.trial` — DEBUG

```
classify chan {iid}: trying {candidate}
```

- fields: `iid`, `candidate`

- since 0.1.0

- A candidate plugin/SF2 was tested against a channel.

### `flp.classify.native` — DEBUG

```
flp: chan {iid} native, no plugin
```

- fields: `iid`

- since 0.1.0

- A channel is native (no plugin).

### `flp.place.slide` — DEBUG

```
slide check chan {chan} key {key} flags {flags}
```

- fields: `chan`, `key`, `flags`

- since 0.1.0

- A Slicex slide note was classified.

### `flp.place.slides_kept` — DEBUG

```
flp: {bent} Slicex slide note(s) kept as pitch bends
```

- fields: `bent`

- since 0.1.0

- Summary of slide notes that became pitch bends.

### `flp.place.disabled` — DEBUG

```
flp: skipping disabled channels {chans}
```

- fields: `chans`

- since 0.1.0

- Channels disabled in the source project were dropped.

## `flp.slicex`

### `flp.slicex.header` — DEBUG

```
{body}
```

- fields: `body`

- since 0.1.0

- Slicex outer header + WAV1 chunks + WAV2 identity.

### `flp.slicex.settings` — DEBUG

```
{body}
```

- fields: `body`

- since 0.1.0

- Slicex 402-byte settings header and per-slice records.

### `flp.slicex.trest` — DEBUG

```
{body}
```

- fields: `body`

- since 0.1.0

- Slicex trest region records and region-param block.

### `flp.slicex.env` — DEBUG

```
{body}
```

- fields: `body`

- since 0.1.0

- Slicex amp envelope points and ADSR decomposition.

### `flp.slicex.computed` — DEBUG

```
{body}
```

- fields: `body`

- since 0.1.0

- Slicex derived values: base key, key layout, timing.

### `flp.slicex.application` — DEBUG

```
{body}
```

- fields: `body`

- since 0.1.0

- Which Slicex parameter groups were applied and their values.

### `flp.slicex.speed_env` — DEBUG

```
{body}
```

- fields: `body`

- since 0.1.0

- Slicex SPEED envelope points.

### `flp.slicex.stretch` — DEBUG

```
{body}
```

- fields: `body`

- since 0.1.0

- Tempo-stretch decision: sample BPM source, project BPM, factor.

## `midi`

### `midi.load_failed` — ERROR

```
midi: skipping {path} (load failed: {err})
```

- fields: `path`, `err`

- since 0.1.0

- A MIDI file could not be parsed.

## `player`

### `player.master_fx.missing` — WARN

```
master fx: missing {path}
```

- fields: `path`

- since 0.1.0

- A master FX state file was listed but not found on disk.

### `player.master_fx.state_read` — ERROR

```
master fx: cannot read state {path}: {err}
```

- fields: `path`, `err`

- since 0.1.0

- A master FX state file exists but could not be read.

### `player.master_fx.state_file_missing` — WARN

```
master fx: state file missing: {path}
```

- fields: `path`

- since 0.1.0

- A master_fx.json entry names a state file that does not exist.

### `player.master_fx.load_failed` — ERROR

```
master fx: failed to load {name}: {err}
```

- fields: `name`, `err`

- since 0.1.0

- The master FX chain referenced a plugin that failed to instantiate.

### `player.master_fx.chain_failed` — ERROR

```
master fx chain: {err}
```

- fields: `err`

- since 0.1.0

- EffectChain::load_default itself errored.

### `player.master_fx.loaded` — INFO

```
master fx: loaded {name}
```

- fields: `name`

- since 0.1.0

- A master FX plugin was loaded into the chain.

### `player.master_fx.non_utf8` — WARN

```
master fx: non-UTF8 path for {name}
```

- fields: `name`

- since 0.1.0

- A master FX entry resolved to a path the host cannot represent as UTF-8.

### `player.sample.decode_failed` — WARN

```
  sample {rel} failed to decode: {err}
```

- fields: `rel`, `err`

- since 0.1.0

- A sample referenced by samples.json could not be decoded.

### `player.embed.extract_failed` — WARN

```
  embedded plugin extraction failed: {err}
```

- fields: `err`

- since 0.1.0

- A plugin bundled inside a .tsz/.flp payload could not be extracted.

### `player.sf2.materialize_failed` — WARN

```
  SF2 materialize failed for {rel}: {err}
```

- fields: `rel`, `err`

- since 0.1.0

- A SoundFont referenced by a sidecar could not be copied into the project cache.

### `player.shims.no_line` — DEBUG

```
shims: no line {li}
```

- fields: `li`

- since 0.1.0

- A shim event referenced a BlockList line that has no call.

### `player.shims.debug` — DEBUG

```
{body}
```

- fields: `body`

- since 0.1.0

- Freeform shim-association trace (formerly SHIMS_DEBUG=1).

### `play.session.init_started` — INFO

```
Initializing VSTs (this may take a few seconds)...
```

- since 0.1.0

- Session load reached the VST install phase.

### `play.session.init_done` — INFO

```
VSTs initialized.
```

- since 0.1.0

- All VST hosts finished installing.

### `player.load.fidelity_summary` — INFO

```
  fidelity: {channels} channels, {automation} automation lanes, {playlist} playlist items, {inserts} insert buses, {multisample} multi-sample groups
```

- fields: `channels`, `automation`, `playlist`, `inserts`, `multisample`

- since 0.1.0

- One-line summary of fidelity.json contents when it parses cleanly.

### `player.load.fidelity_ignored` — WARN

```
  fidelity: {err}, ignoring
```

- fields: `err`

- since 0.1.0

- fidelity.json failed to parse; the project loads without it.

### `player.load.embed_extracted` — INFO

```
  extracted {count} embedded plugin bundle(s)
```

- fields: `count`

- since 0.1.0

- Bundled plugins materialized into the cache.

### `player.load.no_samples_json` — WARN

```
  (no samples.json in {source})
```

- fields: `source`

- since 0.1.0

- The project has no sample library; only SF2/VST instruments can play.

### `player.load.samples_count` — INFO

```
  loaded {count} samples
```

- fields: `count`

- since 0.1.0

- Count of decoded samples after loading samples.json.

### `player.load.multi_sample_count` — INFO

```
  multi-sample: {count} instrument(s)
```

- fields: `count`

- since 0.1.0

- Count of multi-sample instrument groups built from fidelity.json.

### `player.load.sidecar_unreadable` — WARN

```
  (sidecar unreadable: {rel})
```

- fields: `rel`

- since 0.1.0

- An `.instrument` sidecar exists but could not be read.

### `player.load.sf2_not_in_source` — WARN

```
  (SF2 {rel} not in source)
```

- fields: `rel`

- since 0.1.0

- A sidecar references a SoundFont that is absent from the source.

### `player.load.vst_not_installed` — WARN

```
  (VST {plugin} not installed, skipping; sidecar {sidecar})
```

- fields: `plugin`, `sidecar`

- since 0.1.0

- A sidecar's VST is not present on disk or in the local plugin scan.

### `player.load.fallback_trim` — INFO

```
  {count} stand-in plugins: trimmed {db:.1} dB each so they sit as backing
```

- fields: `count`, `db`

- since 0.1.0

- Fallback VSTs receive a per-instance trim so they don't dominate the mix.

### `player.load.summary` — INFO

```
  loaded {sf2s} sf2 instruments, {engines} sf2 engines, {vsts} vst instruments
```

- fields: `sf2s`, `engines`, `vsts`

- since 0.1.0

- End-of-sidecar-loop count.

### `player.load.requires_ignored` — WARN

```
  requires: {err}, ignoring
```

- fields: `err`

- since 0.1.0

- requires.json failed to parse; the dependency pre-flight is skipped.

### `player.load.insert_chains` — INFO

```
  loaded {chains} insert chain(s), {slots} slot(s) total
```

- fields: `chains`, `slots`

- since 0.1.0

- Mixer insert chains materialized from sidecars.

### `player.load.tempo_shims` — INFO

```
  tempo shims: {count} event(s)
```

- fields: `count`

- since 0.1.0

- Number of `ƒTempo` shims converted into tempo-map events.

### `player.load.parsed` — INFO

```
  parsed {parts} parts, {blocks} blocks, {lines} BlockList lines, {voices} voices
```

- fields: `parts`, `blocks`, `lines`, `voices`

- since 0.1.0

- End-of-parse count: how much was in the trackscript.

### `player.install.vst_not_found` — WARN

```
vst [{name}] not found (looked up by name {stem})
```

- fields: `name`, `stem`

- since 0.1.0

- A VST referenced by the project wasn't found in the plugin scan; the track is silent.

### `player.install.sf2_load_failed` — ERROR

```
sf2 [{name}]: failed to load {path}: {err}
```

- fields: `name`, `path`, `err`

- since 0.1.0

- A SoundFont engine failed to load during session setup.

### `player.install.sf2_debug` — DEBUG

```
{body}
```

- fields: `body`

- since 0.1.0

- Per-SF2-engine gain breakdown at install time (formerly FLP_DEBUG=1).

### `player.install.shims_no_share` — WARN

```
  shims: share/ not found; {count} shim(s) skipped
```

- fields: `count`

- since 0.1.0

- share/fallbacks.json is absent, so shim targets cannot be resolved.

### `player.install.shims_debug` — DEBUG

```
{body}
```

- fields: `body`

- since 0.1.0

- Freeform shim install trace (formerly SHIM_DEBUG=1).

### `player.install.shim_loaded` — INFO

```
  shim {id}: loaded '{target}'
```

- fields: `id`, `target`

- since 0.1.0

- A shim effect was installed on the mixer.

### `player.install.shims_summary` — INFO

```
  shims: {attached} attachment(s), {skipped} skipped, {unique} unique VST3 host(s) (from {total} shim event(s))
```

- fields: `attached`, `skipped`, `unique`, `total`

- since 0.1.0

- End-of-shims-install summary.

### `player.loader.voice_skipped` — DEBUG

```
flp: voice skipped: part={part} sample_id={sample_id} start_row={start_row} end_row={end_row} start={start} end={end} min_end={min_end} reason={reason}
```

- fields: `part`, `sample_id`, `start_row`, `end_row`, `start`, `end`, `min_end`, `reason`

- since 0.1.0

- A voice was dropped because its interval is empty or before min_end.  Formerly FLP_DEBUG=1.

### `player.loader.voice_dead` — DEBUG

```
flp: voice DEAD: part={part} sample_id={sample_id} sample is empty
```

- fields: `part`, `sample_id`

- since 0.1.0

- A voice references a sample that decoded to zero frames.

### `player.loader.voice_trace` — DEBUG

```
vtrace: scheduled sample {sample_id} start={start} end={end} ({len} samples, {secs:.3}s) offset={offset} pitch_hz={pitch_hz:.2} vol={vol}
```

- fields: `sample_id`, `start`, `end`, `len`, `secs`, `offset`, `pitch_hz`, `vol`

- since 0.1.0

- Per-voice trace for a specific sample_id, gated by VOICE_TRACE_SAMPLE=<id>.

### `player.render.vst_skipped` — WARN

```
  skipping VST [{name}]: plugin {stem} not installed
```

- fields: `name`, `stem`

- since 0.1.0

- A VST referenced by the project was not found on the local plugin scan.

### `player.render.vst_loaded` — INFO

```
  loaded VST [{iid}] {name} ({tag})
```

- fields: `iid`, `name`, `tag`

- since 0.1.0

- A VST host was installed for offline rendering.

### `player.render.vst_load_failed` — ERROR

```
  failed to load VST {name}: {err}
```

- fields: `name`, `err`

- since 0.1.0

- A VST host failed to instantiate during offline render.

### `player.render.vst_hosts_installed` — INFO

```
  installed {count} VST host(s)
```

- fields: `count`

- since 0.1.0

- Summary at end of the offline VST install loop.

### `player.render.vst_fallback_gain` — INFO

```
  VST fallback attenuation: {gain:.3} (VST_FALLBACK_GAIN)
```

- fields: `gain`

- since 0.1.0

- Emitted when VST_FALLBACK_GAIN is not 1.0.

### `player.render.sf2_load_failed` — ERROR

```
  sf2 engine failed to load {name}: {err}
```

- fields: `name`, `err`

- since 0.1.0

- A SoundFont engine failed to load during offline render.

### `player.render.sf2_debug` — DEBUG

```
{body}
```

- fields: `body`

- since 0.1.0

- Per-SF2-engine gain breakdown (formerly FLP_DEBUG=1).

### `player.render.sf2_engines_installed` — INFO

```
  installed {count} SF2 engine(s)
```

- fields: `count`

- since 0.1.0

- Summary at end of the offline SF2 install loop.

### `player.render.wav_clipped` — WARN

```
  warning: {clipped} output samples clip at peak {peak:.3}
```

- fields: `clipped`, `peak`

- since 0.1.0

- Offline render wrote samples outside [-1, 1].

## `deps`

### `deps.fetch.step_failed` — ERROR

```
fetch {name}: {err}
```

- fields: `name`, `err`

- since 0.1.0

- An acquire.sh step for a missing dependency failed.

### `deps.fetch.incomplete` — WARN

```
fetch {name}: step done but {provides} still missing
```

- fields: `name`, `provides`

- since 0.1.0

- An acquire step reported success but its declared output is absent.

## `tui`

### `tui.settings.parse_failed` — WARN

```
player_settings {path}: {err}
```

- fields: `path`, `err`

- since 0.1.0

- player_settings.json could not be parsed.

### `tui.settings.preset_failed` — WARN

```
player_settings: {err}
```

- fields: `err`

- since 0.1.0

- A theme preset name could not be applied.

### `tui.settings.unknown_token` — WARN

```
player_settings: unknown token `{token}`
```

- fields: `token`

- since 0.1.0

- A theme override key is not recognized.

### `tui.settings.bad_color` — WARN

```
player_settings: bad color `{value}` for {token}
```

- fields: `value`, `token`

- since 0.1.0

- A color value in player_settings.json could not be parsed.

### `tui.cover.read_failed` — WARN

```
cover: read {path} failed: {err}
```

- fields: `path`, `err`

- since 0.1.0

- An album-art image could not be read or decoded.

### `tui.debug` — DEBUG

```
{body}
```

- fields: `body`

- since 0.1.0

- Freeform TUI event trace (TUI_DEBUG=1).

### `tui.menu.dump_started` — INFO

```
Dumping {path}...
```

- fields: `path`

- since 0.1.0

- User chose Dump from the home menu.

### `tui.menu.dump_ok` — INFO

```
Dump successful: {path}
```

- fields: `path`

- since 0.1.0

- Dump finished cleanly.

### `tui.menu.dump_failed` — ERROR

```
Dump failed: {err}
```

- fields: `err`

- since 0.1.0

- Dump errored.

### `tui.menu.export_started` — INFO

```
Exporting {path} to trackscript directory...
```

- fields: `path`

- since 0.1.0

- User chose Export from the home menu.

### `tui.menu.export_ok` — INFO

```
Export successful: {path}
```

- fields: `path`

- since 0.1.0

- Export finished cleanly.

### `tui.menu.export_failed` — ERROR

```
Export failed: {err}
```

- fields: `err`

- since 0.1.0

- Export errored.

## `ffi`

### `ffi.stderr` — WARN

```
{line}
```

- fields: `line`

- since 0.1.0

- A line written to fd 2 by a C++ plugin.

