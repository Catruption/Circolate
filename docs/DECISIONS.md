# Design decisions

One line per decision, plain English. Update as decisions change.

## Format identity

- Halotext is the source of truth for: song structure, arrangement, notes, instrument references, and FX chain structure.
- Halotext is NOT the source of truth for: audio content, VST state blobs, images, soundfonts.
- Binary content stays as files: `.wav`, `.mp3`, `.flac`, `.sf2`, `.state`, `.jpg`, `.png`.

## Sections and brackets

- `{ }` = declaration sections (`Config`, `Fallbacks`, `BlockArray`). Line-oriented.
- `[ ]` = measure-sequential sections (`BlockList`, part bodies, `Lyrics`). Each line is one measure of song time.
- Gutter in the editor: line numbers inside `{ }`, cumulative measure numbers inside `[ ]`.

## Sample and instrument references

- Sample files use `.wav` extension but may be FLAC, MP3, or OGG internally. The extension is advisory; the decoder content-sniffs.
- VST reference form: `ƒ'<sidecar_stem>.vst3'` where `<sidecar_stem>` is the `.instrument` file's stem (e.g., `MODO_BASS.instrument` → `ƒ'MODO_BASS.vst3'`). This is the **intent name** — what the part was originally meant to play.
- VST sidecar lookup: `MODO_BASS.instrument` carries `Plugin: 'TAL-NoiseMaker'` meaning TAL-NoiseMaker actually loads in its place. The loader aliases `MODO_BASS.vst3` → sidecar → `plugin_path`, so playback uses TAL-NoiseMaker.
- SF2 reference form: `ƒ'instruments/X.instrument'` (sidecar form) **for now**. Flattening to `ƒ'X.sf2'(preset)` deferred — loader needs preset-aware lookup by basename.
- Sample reference form: `ƒ'X.wav'` (bare filename). Loader aliases bare filenames to the corresponding `samples.json` entry, so both `ƒ'X.wav'` and `ƒ'samples/X.wav'` resolve to the same sample during migration.

## Fallbacks

- `share/fallbacks.json` is NOT a sidecar. It's a global registry with download/install scripts. It survives.
- `requires.json` is a per-project *export* of that registry. It gets replaced by the `Fallbacks { }` section.
- `Fallbacks { }` keys match reference keys (`<sidecar_stem>.vst3`). Values are the substitute plugin (`<plugin>.vst3`).
- `Gain:` on a fallback is a loudness compensation for the substitution, not the original plugin's gain.
- **Gap (deferred)**: loader doesn't consult `Fallbacks` when a sidecar's `plugin_path` fails. If the substituted plugin isn't installed, playback fails. VST absorption project will fix.
- **Gap (deferred)**: original plugin preference. Even if `MODO_BASS` is installed, the sidecar's `plugin_path` (`TAL-NoiseMaker`) is used. VST absorption project will store intent separately and prefer original if installed.

## Sample file format

- `.wav` extension is advisory. Content-sniffing handles FLAC/MP3/OGG.
- Loop points are stored in the halotext (`loop: A..B` on the `ƒ` reference) because FLAC has no standard loop metadata and pack-exported WAVs often lack `smpl` chunks.
- Loop migration: deferred. `samples.json` carries loop data until the `loop:` halotext field ships.

## Effects

- Shims are attached to calls today. Moving them to definitions is a later refactor.
- Empty shims (`@<ƒEQ2()>`) mean "there's an EQ in the chain, dial it." Not malformed; settings are unknowable for Fruity natives.
- Comma-chained shims (`@<ƒEQ2(), ƒReverb2()>`) are equivalent to consecutive `@<...>@<...>`. Both parse; the comma form is preferred for readability.

## Soundfont presets

- Reference form: `ƒ'X.sf2'(preset)`, or `ƒ'X.sf2'(bank, preset)` when bank ≠ 0.
- A `root:` named arg overrides the sf2's declared sample root: `ƒ'X.sf2'(0, root: 48)`.

## Chips

- `.chip` and `.patch` are NOT halotext. They stay their own format.
- Chip semantic parameters (`pitch`, `amp`, `filter`) are an additive layer on the existing format, not a redesign.
- Patches can be semantic, native, or hybrid. Semantic patches are portable across chips.

## Deferred

- SF2 reference flattening (`ƒ'instruments/X.instrument'` → `ƒ'X.sf2'(preset)`).
- Sample loop migration (`samples.json` loops → `loop: A..B` in halotext).
- Filesystem walk for sample discovery (replaces `samples.json` reading).
- VST absorption (`Instruments {}` section, `.instrument` files gone).
- `fidelity.json` absorption (project-dependent, `Carol Roll` needs per-channel state preserved).
- `Include` for multi-file projects — designed, not implemented.
- Cross-measure "move scattered notes" — cut-paste-fix until painful.
- Operators (`+ - * ± ^ :count`) — format supports, playback doesn't.
- Format-on-save — command exists (`Ctrl+Shift+I`), not auto-on-save.

## What deletion means

- "Drop X" = X doesn't ship. Not migrated to a new form, just gone.
- `instruments/*.instrument`, `inserts/*.instrument`, `samples.json`, `requires.json`: dropped after their replacements ship.
- `fidelity.json`: dropped after halotext carries its data.

## Editor Integrations & UX (The LSP Blueprint)

*Status: Planned. The parser is stable, but tooling is needed to make editing pleasant.*

### Architecture
- **One Server, Two Clients:** We use a Language Server Protocol (LSP) written in Rust (reusing `spec.rs` and `formats::halotext`). Kate and VS Code get thin plugins (~300 lines each) that just map keystrokes to LSP `workspace/executeCommand` calls.
- **Scope:** The LSP strictly scopes structural queries using the `[ ]` brackets. Commands like "select all C5s" default to the current `[ ]` part body.

### Glyph Input (The Leader Key)
Since Halotext uses non-standard Unicode glyphs (`§, ƒ, ¶, ₨, ¥, ₥, ¢, α, ♪, ♫`), typing them requires a plugin solution.
- **Leader Mode:** `Ctrl+\` (configurable) opens a one-shot glyph input mode.
- **Behavior:** The status bar flashes "GLYPH". The next keypress maps to a glyph (`S` → `§`, `F` → `ƒ`). Unmapped keys cancel the mode. `Esc` cancels. 
- **Discovery:** `Ctrl+Space` opens a standard completion popup listing all fields and their glyphs.
- **Scope:** Only active in `.halo.txt` / `.halotext` files.

### Keybindings & Operations (LSP Custom Commands)
These apply to either the text selection or multi-cursors. 
- **Note Edits:** `Alt+Up/Down` (transpose semitone), `Alt+Shift+Up/Down` (transpose octave).
- **Time Edits:** `Alt+Left/Right` (nudge subdivision), `Alt+./,` (extend/shrink duration).
- **Mix Edits:** `Alt+]/[` (volume ±5%), `Alt+; / '` (pan).
- **Duplication:** `Ctrl+D` (duplicate note in place), `Ctrl+Shift+D` (duplicate after).
- **Macro/Refactor:** `Ctrl+Shift+M` (Extract selection to a new `ƒmotif` definition and replace selection with a call).
- **Formatting:** `Ctrl+Shift+I` (Format-on-save: aligns commas, normalizes whitespace, orders fields. Manual trigger, not automatic).

### Visualizations & Audio Feedback
- **Gutter Numbers:** The editor must override standard line numbers. Inside `[ ]` blocks (BlockList / part bodies), the gutter shows the **measure number**, not the line number. 
- **Absolute Positioning:** Hovering a line inside a part body shows a tooltip: "Part measure 3. Plays at song measures: 4, 12, 36." (Via LSP Hover).
- **The Playhead:** The Circolate player broadcasts `active_notes` via `/tmp/circolate.sock`. The editor highlights the active lines in cyan.
- **Terminal 2-for-1:** Use Kate's built-in terminal panel to run `circolate play`. The playhead runs upward via socket, the terminal handles the live logs.
- **Scroll-on-Value (Kate Only):** Mouse wheel over `%78.1` changes it. (VS Code extensions cannot intercept native mouse events; VS Code uses `Alt+]/[` exclusively).

### Known Limitations
- **VS Code Mouse:** Extensions cannot intercept mouse wheel or click-drag in the Monaco editor. Features like "Alt+Click to select field" and "Scroll wheel to change value" are **Kate-exclusive**.
- **Multi-Cursor:** The LSP will iterate over all active cursors provided by the editor. If Kate's multi-cursor API doesn't expose column positions cleanly, scattered note edits may be limited to VS Code.
- **Current Playhead:** The existing `trackscript_playhead.py` uses PyKDE4 and must be ported to PyQt5/PyQt6 before it works on modern Kate.

### What not to do
- **No Piano Roll overlay:** Do not build a graphical grid. The text *is* the grid. 
- **No Parallel Model:** The plugin must never maintain a hidden list of notes. It reads text, modifies text, writes text. Undo/Redo must be native text undo.
- **No Drag and Drop:** There is no spatial representation of time.
