// src/spec.rs
//! Trackscript language reference: one place for the syntax tables.
//!
//! Everything here is plain data (std only).  Four things read it:
//!   * `render_markdown()` generates docs/LANGUAGE.md,
//!   * the parser and converter ask `is_reserved` / `avoid_reserved`,
//!   * the parser reads the glyph constants for regex construction,
//!   * the converters call the `*_definition` / `call` / `source_arg`
//!     helpers instead of writing syntax by hand.
//!
//! Regenerate the docs with:  UPDATE_DOCS=1 cargo test language_md

#![allow(dead_code)]

use std::fmt::Write as _;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Implemented,
    Planned,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Status::Implemented => "",
            Status::Planned => " *(planned)*",
        }
    }
}

// ── glyph constants ────────────────────────────────────────────────────

pub const PART_HEADER_SIGIL: &str = "|";

pub const FIELD_POSITION:   &str = "§";
pub const FIELD_PITCH:      &str = "#";
pub const FIELD_DURATION:   &str = "~";
pub const FIELD_VOLUME:     &str = "%";
pub const FIELD_PAN:        &str = "¶";
pub const FIELD_SOURCE:     &str = "ƒ";
pub const FIELD_FINE_PITCH: &str = "¢";
pub const FIELD_RELEASE:    &str = "₨";
pub const FIELD_MOD_X:      &str = "₥";
pub const FIELD_MOD_Y:      &str = "¥";
pub const FIELD_MIX:        &str = "◊";

// ── effect parameter slots ─────────────────────────────────────────────
//
// Effect parameters are positional.  The slot index is written as a run
// of Greek letters read as a bijective base-10 number:
//
//     α=1  β=2  γ=3  δ=4  ε=5  ζ=6  η=7  θ=8  ι=9  κ=10
//
// Bijective means no zero digit: every positive integer has exactly
// one spelling, and no position is wasted on a placeholder.
//
// A parameter argument is a slot run followed by a value: `αδ6.0` is
// slot 14, value 6.0.  The parser reads Greek letters greedily from
// the start of an argument; the first non-Greek character ends the
// slot and begins the value.
//
// Parameter names and their slot assignment live in the plugin
// registry (see `share/registry.json`), not in the file.

pub const GREEK_DIGITS: &[(char, u32)] = &[
    ('α', 1), ('β', 2), ('γ', 3), ('δ', 4), ('ε', 5),
    ('ζ', 6), ('η', 7), ('θ', 8), ('ι', 9), ('κ', 10),
];

/// Parse a bijective-base-10 Greek-digit run into a slot number.
/// Returns `(slot, bytes_consumed)`; slot is 0 when no digits are
/// present.
pub fn parse_greek_slot(s: &str) -> (u32, usize) {
    let mut n: u32 = 0;
    let mut consumed = 0;
    for (i, c) in s.char_indices() {
        let Some(&(_, digit)) = GREEK_DIGITS.iter().find(|(g, _)| *g == c) else {
            break;
        };
        n = n * 10 + digit;
        consumed = i + c.len_utf8();
    }
    (n, consumed)
}

pub const EV_NOTE:       &str = "";
pub const EV_WAYPOINT:   &str = "&";
pub const EV_UNDOC_EQ:   &str = "=";
pub const EV_UNDOC_TRI:  &str = "▲";
pub const EV_UNDOC_NOTE: &str = "♪";
pub const EV_SHIM:       &str = "@";

// ── emitters ───────────────────────────────────────────────────────────

/// `|N/D[` — a section header that also opens the bracket.
pub fn section_header(beats: u32, subs: u32) -> String {
    format!("{}{}/{}[", PART_HEADER_SIGIL, beats, subs)
}

/// `|N/D` — the section's coordinate frame, without the opening bracket.
pub fn section_frame(beats: u32, subs: u32) -> String {
    format!("{}{}/{}", PART_HEADER_SIGIL, beats, subs)
}

/// Full part definition:
///
///     ƒNAME(<|N/D, ƒsource, %vol, ¶pan, ~dur>)[
///         events
///     ];
pub fn part_definition(name: &str, args: &str, beats: u32, subs: u32, events: &str) -> String {
    let mut out = String::new();
    out.push_str(FIELD_SOURCE);
    out.push_str(name);
    out.push_str("(<");
    out.push_str(&section_frame(beats, subs));
    if !args.is_empty() {
        out.push_str(", ");
        out.push_str(args);
    }
    out.push_str(">)[");
    out.push('\n');
    if !events.is_empty() {
        for line in events.lines() {
            if line.is_empty() {
                out.push('\n');
            } else {
                out.push_str("    ");
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    out.push_str("];");
    out
}

/// `ƒNAME(args);` — a call in BlockList or a composite definition.
pub fn call(name: &str, args: &str) -> String {
    let mut out = String::new();
    out.push_str(FIELD_SOURCE);
    out.push_str(name);
    out.push('(');
    out.push_str(args);
    out.push_str(");");
    out
}

/// `ƒ'path'` or `ƒ'path'(offset)` — the source argument for a definition.
pub fn source_arg(path: &str, offset: Option<u32>) -> String {
    match offset {
        Some(n) if n != 0 => format!("{}'{}'({})", FIELD_SOURCE, path, n),
        _ => format!("{}'{}'", FIELD_SOURCE, path),
    }
}

/// `@<§pos, #bpm, ƒTempo>;` — a tempo shim event.
pub fn tempo_shim(pos: &str, bpm_text: &str) -> String {
    format!("@<{},#{}, {}Tempo>;", pos, bpm_text, FIELD_SOURCE)
}

/// `<§pos, #pitch, ~dur, %vol[, ƒsource]>`.
pub fn note(pos: &str, pitch: &str, dur: &str, vol: &str, source: &str) -> String {
    let mut s = String::with_capacity(64);
    s.push('<');
    s.push_str(pos);
    s.push(',');
    s.push_str(FIELD_PITCH);
    s.push_str(pitch);
    s.push(',');
    s.push_str(FIELD_DURATION);
    s.push_str(dur);
    s.push(',');
    s.push_str(FIELD_VOLUME);
    s.push_str(vol);
    if !source.is_empty() {
        s.push(',');
        s.push_str(source);
    }
    s.push('>');
    s
}

/// `@<[§pos,] [~len,] [◊mix,] [%gain,] [¶pan,] ƒTarget(params)>;`
/// The call-attached form is just this appended directly after a call.
pub fn shim_event(
    target: &str,
    params: &[(u32, f64)],
                  pos: Option<&str>,
                  len: Option<&str>,
                  mix: Option<f64>,
                  gain: Option<f64>,
                  pan: Option<f64>,
) -> String {
    let mut fields: Vec<String> = Vec::new();
    if let Some(p) = pos { fields.push(p.to_string()); }
    if let Some(l) = len { fields.push(format!("{}{}", FIELD_DURATION, l)); }
    if let Some(m) = mix { fields.push(format!("{}{}", FIELD_MIX, m)); }
    if let Some(g) = gain { fields.push(format!("{}{}", FIELD_VOLUME, g)); }
    if let Some(p) = pan { fields.push(format!("{}{}", FIELD_PAN, p)); }
    let mut target_str = format!("{}{}", FIELD_SOURCE, target);
    if !params.is_empty() {
        let param_strs: Vec<String> = params.iter()
        .map(|(slot, val)| format!("{}{}", slot_to_greek(*slot), val))
        .collect();
        target_str.push('(');
        target_str.push_str(&param_strs.join(", "));
        target_str.push(')');
    }
    fields.push(target_str);
    format!("@<{}>;", fields.join(", "))
}

/// Encode a slot number as bijective base-10 Greek.
pub fn slot_to_greek(mut slot: u32) -> String {
    if slot == 0 { return String::new(); }
    let mut out = String::new();
    while slot > 0 {
        let digit = ((slot - 1) % 10) + 1;
        let c = GREEK_DIGITS.iter().find(|(_, d)| *d == digit).map(|(c, _)| *c).unwrap_or('α');
        out.insert(0, c);
        slot = (slot - 1) / 10;
    }
    out
}

// ── tables ─────────────────────────────────────────────────────────────

pub struct Symbol {
    pub glyph: &'static str,
    pub name: &'static str,
    pub doc: &'static str,
    pub since: &'static str,
    pub status: Status,
}

const fn sym(
    glyph: &'static str,
    name: &'static str,
    doc: &'static str,
    since: &'static str,
    status: Status,
) -> Symbol {
    Symbol { glyph, name, doc, since, status }
}

use Status::{Implemented as Done, Planned as Plan};

pub const FIELD_SYMBOLS: &[Symbol] = &[
    sym(FIELD_POSITION, "position", "`§beat/subdivision`, both 1-based; the subdivision may be fractional (`§1/1.5`). Local to the measure the part is placed in.", "1.10", Done),
    sym(FIELD_PITCH, "pitch", "Note name (`C5`, `A#4`) or a number in Hz. In a shim, a pitch-shaped property for the target.", "1.10", Done),
    sym(FIELD_DURATION, "duration", "Length in beats. May run past the end of the measure. In a shim, the effect window length; when omitted on a call-attached shim, the call's duration applies.", "1.10", Done),
    sym(FIELD_VOLUME, "volume", "Percent. A leading `+` is tolerated. In a shim, output gain (default 100).", "1.10", Done),
    sym(FIELD_PAN, "pan", "-1 (full left) to 1 (full right). In a shim, output pan (default 0).", "1.10", Done),
    sym(FIELD_SOURCE, "source / call", "`ƒ'path'` overrides the sample. `ƒName` calls a defined part or block. In a shim, `ƒTarget(params)` names the effect target.", "1.10", Done),
    sym(FIELD_FINE_PITCH, "fine pitch", "Cents offset from equal temperament, as a signed delta from 0. Slides glide to the equal-tempered pitch of the target note and ignore this field.", "1.11", Done),
    sym(FIELD_RELEASE, "release", "Release velocity, as a signed delta from the engine default of 64. Shapes the amplitude envelope's release stage when the note-off fires.", "1.11", Done),
    sym(FIELD_MOD_X, "mod X", "Modulation X, as a signed delta from the engine default of 128. Routes to the plugin's X destination (filter cutoff on simple plugins).", "1.11", Done),
    sym(FIELD_MOD_Y, "mod Y", "Modulation Y, as a signed delta from the engine default of 128. Routes to the plugin's Y destination (resonance on simple plugins).", "1.11", Done),
    sym(FIELD_MIX, "mix", "Dry/wet mix of an effect shim. 0 is fully dry, 1 is fully wet. Default 1.0 for a call-attached shim. Rampable with waypoints.", "1.11", Done),
];

pub const EVENT_PREFIXES: &[Symbol] = &[
    sym(EV_NOTE, "note", "A note-on.", "1.10", Done),
    sym(EV_WAYPOINT, "waypoint", "Changes properties of the preceding note or shim at a later position instead of starting a new note. Waypoints cross section boundaries; a section header between them is a layout marker, not a break.", "1.10", Done),
    sym(EV_UNDOC_EQ, "(undocumented)", "Recognised by the parser; its meaning is not defined in trackscript.rs.", "1.10", Done),
    sym(EV_UNDOC_TRI, "(undocumented)", "Recognised by the parser; its meaning is not defined in trackscript.rs.", "1.10", Done),
    sym(EV_UNDOC_NOTE, "(undocumented)", "Recognised by the parser; its meaning is not defined in trackscript.rs.", "1.10", Done),
    sym(EV_SHIM, "shim", "Attaches an effect or reserved-global automation to a window. The last field of `@<...>` is `ƒName(args)`; Name is an effect function or a reserved global. See the *Shim targets* table for what is currently consumed.", "1.11", Done),
];

pub fn active_event_prefix_glyphs() -> Vec<&'static str> {
    EVENT_PREFIXES
    .iter()
    .filter(|s| !s.glyph.is_empty() && s.status == Status::Implemented)
    .map(|s| s.glyph)
    .collect()
}

pub struct Context {
    pub name: &'static str,
    pub syntax: &'static str,
    pub doc: &'static str,
    pub status: Status,
}

pub const CONTEXTS: &[Context] = &[
    Context {
        name: "note",
        syntax: "<§pos, #pitch, ~dur, %vol, ¶pan, ƒsource>",
        doc: "A note-on inside a part body. Fields describe this one note.",
        status: Done,
    },
Context {
    name: "definition",
    syntax: "ƒName(<|N/D, ƒsource, %vol, ¶pan, ~dur>)[ events ];",
    doc: "Defines a part. The argument list is a note-shaped default: \
`|N/D` sets the section frame, `ƒ'path'` names the sample, \
`%`/`¶`/`~` supply defaults notes may override.  A definition's body \
may contain multiple `|N/D[...]` sections; each section is one measure, \
and they accumulate left-to-right in the part's timeline.",
status: Done,
},
Context {
    name: "composite",
    syntax: "ƒName(ƒA, ƒB, ...);",
    doc: "Defines a block as a list of calls. No body; the arguments are the body. \
Distinguished from a part by the args not starting with `<`.  A composite's \
measure timeline is the concatenation of its calls' timelines.",
status: Done,
},
Context {
    name: "call",
    syntax: "ƒName();  ƒName(1,2,3);  ƒName(1<%50>, 2<%75>);",
    doc: "Calls a definition from a BlockList line.  An empty argument list \
plays every measure of the definition in its natural order.  Bare integers \
select specific measures and set the order they play in — `ƒName(3,1,2)` \
plays measure 3, then 1, then 2.  A `<...>` override block after a measure \
number applies to just that measure; the override fields are `%vol`, `¶pan`, \
`~dur`, and `ƒsource`, matching the field shapes used on a note.  A call's \
duration is the sum of the durations of its selected measures; the BlockList \
cursor advances by that amount.  Some fields with a note-shaped meaning \
(position, pitch) have no place in a call's override block and are ignored \
if present.",
    status: Done,
},
Context {
    name: "shim-syntax",
    syntax: "@<§pos, ~len, ◊mix, %gain, ¶pan, ƒName(params)>;",
    doc: "Syntax for attaching an effect or reserved-global automation to \
a window of song time.  Standalone form has its own BlockList line; \
call-attached form is `ƒName(args)@<ƒTarget(params)>;` and inherits the \
call's start position and duration when `§`/`~` are omitted.  The target \
(`ƒName`) is either an effect function or a reserved global (see *Shim \
targets*).  Universal shim fields are `§` start, `~` length, `◊` dry/wet, \
`%` output gain, and `¶` output pan; waypoints (`&`) ramp any of them.  \
Effect parameters are positional: the slot index is a bijective base-10 \
run of Greek letters (`α1` is slot 1, `αδ6.0` is slot 14) followed by the \
value.  Target names use the slug character set `[A-Za-z_][A-Za-z0-9_.-]*`, \
so `Fruity Reeverb 2` is written `Fruity_Reeverb_2` — matching the \
converter's sidecar naming.",
status: Done,
},
Context {
    name: "shim-effects",
    syntax: "call-attached:  ƒName(args)@<ƒTarget(params)>;  (parses; effect targets are not yet consumed by the player)",
    doc: "Effect targets on shims are parsed and stored in \
`TrackscriptFile.shims`, but the mixer does not yet route voices through \
them.  Only `ƒTempo` (via the legacy `Tempo { … }` section) currently \
drives audio.  Wiring the effect path is tracked as *shim-effects*.  See \
the *Shim targets* table below for the per-target status.",
status: Plan,
},
];

pub struct FieldContextUse {
    pub glyph: &'static str,
    pub name: &'static str,
    pub note: &'static str,
    pub definition: &'static str,
    pub call: &'static str,
    pub shim: &'static str,
}

pub const FIELD_CONTEXTS: &[FieldContextUse] = &[
    FieldContextUse {
        glyph: FIELD_POSITION, name: "position",
        note: "start row of this note",
        definition: "ignored",
        call: "ignored (measure selection replaces it)",
        shim: "start row of the effect window",
    },
FieldContextUse {
    glyph: FIELD_PITCH, name: "pitch",
    note: "this note's pitch",
    definition: "default pitch",
    call: "ignored",
    shim: "the value being ramped (for pitch-shaped targets)",
},
FieldContextUse {
    glyph: FIELD_DURATION, name: "duration",
    note: "this note's length, in beats",
    definition: "default length",
    call: "how long the selected measures play",
    shim: "length of the effect window; inherits from the call when omitted",
},
FieldContextUse {
    glyph: FIELD_VOLUME, name: "volume",
    note: "this note's volume",
    definition: "default volume",
    call: "volume override, per selected measure",
    shim: "output gain of the effect",
},
FieldContextUse {
    glyph: FIELD_PAN, name: "pan",
    note: "this note's pan",
    definition: "default pan",
    call: "pan override, per selected measure",
    shim: "output pan of the effect",
},
FieldContextUse {
    glyph: FIELD_SOURCE, name: "source / call",
    note: "sample override for this note",
    definition: "the part's sample (required)",
    call: "sample override, per selected measure",
    shim: "target: effect function or reserved global",
},
FieldContextUse {
    glyph: FIELD_FINE_PITCH, name: "fine pitch",
    note: "cents offset for this note",
    definition: "default cents offset",
    call: "ignored",
    shim: "ignored",
},
FieldContextUse {
    glyph: FIELD_RELEASE, name: "release",
    note: "release velocity delta for this note",
    definition: "default release delta",
    call: "ignored",
    shim: "ignored",
},
FieldContextUse {
    glyph: FIELD_MOD_X, name: "mod X",
    note: "mod X delta for this note",
    definition: "default mod X delta",
    call: "ignored",
    shim: "ignored",
},
FieldContextUse {
    glyph: FIELD_MOD_Y, name: "mod Y",
    note: "mod Y delta for this note",
    definition: "default mod Y delta",
    call: "ignored",
    shim: "ignored",
},
FieldContextUse {
    glyph: FIELD_MIX, name: "mix",
    note: "ignored",
    definition: "ignored",
    call: "ignored",
    shim: "dry/wet mix of the effect, 0.0..1.0",
},
];

pub const DEFAULTS_NOTE: &str =
"A field omitted from a note inherits the definition's default for that field. \
A field omitted from a definition falls back to the engine default: 100% volume, \
0 pan, 1-beat duration.  A field omitted from a call-attached shim falls back to: \
start = call's start, length = call's duration, mix = 1.0, gain = 100%, pan = 0.";

    pub const CALL_LENGTH_SEMANTICS: &str =
    "A call's duration is the sum of the durations of the measures it selects.  \
With no arguments, that is the definition's full length.  With measure selection \
(`ƒName(1,2,3)`), it is the sum of the chosen measures in the order listed.  \
The BlockList cursor advances by a call's duration, so calls stack vertically \
in a BlockList line the way notes stack horizontally in a measure.  \
An empty BlockList line (`;`) advances by one measure of the song's time \
signature.";

    pub const SHIM_SEMANTICS: &str =
    "**Implementation status.**  Shim *syntax* is parsed and stored in \
`TrackscriptFile.shims`, but effect targets are **not yet consumed by the \
player** — attaching a reverb shim currently has no audible effect.  Only \
`ƒTempo` via the legacy `Tempo { … }` section drives audio today.  See the \
*Shim targets* table below for per-target status.\n\
\n\
An effect shim attaches an effect to a window of song time.  Its target is the \
last field: `ƒName(params)`, where Name is either an effect function or a \
reserved global.  The universal shim fields are position (`§`), length (`~`), \
dry/wet mix (`◊`), output gain (`%`), and pan (`¶`); waypoints (`&`) ramp any \
of them, in the block's own coordinate frame.\n\
\n\
Effect parameters are positional.  The slot index is a run of Greek letters read \
as bijective base-10 (α=1 … ι=9, κ=10; no zero digit), and the value follows \
immediately after: `α1` is slot 1, `αδ6.0` is slot 14, `κα8` is slot 101.  Slot \
names live in the plugin registry.\n\
\n\
**Call-attached form.**  A shim written on the same line as a call \
(`ƒName(args)@<…>;`) applies to that call.  Its window defaults to the call's \
duration, `◊` defaults to 1.0 (fully wet), and `§` defaults to the call's \
start.  Standalone shims (own line) require `~` and `◊` defaults to 0.0 \
(dry).  Chaining two shims (`ƒName()@<A>@<B>;`) runs A then B on the signal \
captured from the call's voices; each shim's `◊` crossfades that stage against \
its input, and `%`/`¶` apply to its output.\n\
\n\
**Target names** use `[A-Za-z_][A-Za-z0-9_.-]*`.  Spaces become underscores — \
FL Studio's `Fruity Reeverb 2` becomes `Fruity_Reeverb_2`, matching the \
`.instrument` sidecar filename the FLP converter writes.\n\
\n\
When a shim's window ends, its effect is removed abruptly unless the writer \
supplies a closing waypoint to `◊0`.  There is no implicit fade-out.";

pub const OPERATORS: &[Symbol] = &[
    sym("+", "increase", "Add `amount` each interval.", "1.11", Plan),
    sym("-", "decrease", "Subtract `amount` each interval.", "1.11", Plan),
    sym("*", "multiply", "Multiply by `amount` each interval.", "1.11", Plan),
    sym("±", "oscillate", "Alternate down/up by `amount` (triangle wave). `:count` counts direction changes.", "1.11", Plan),
    sym("^", "rate", "Introduces the interval between changes, in beat/subdivision format.", "1.11", Plan),
    sym(":", "count", "How many times the operation applies. Default 1.", "1.11", Plan),
];

pub struct ConfigKey {
    pub key: &'static str,
    pub ty: &'static str,
    pub default: &'static str,
    pub doc: &'static str,
    pub parsed: bool,
}

pub const CONFIG_KEYS: &[ConfigKey] = &[
    ConfigKey { key: "TrackscriptVersion", ty: "number", default: "-", doc: "Format version the file was written for.", parsed: false },
ConfigKey { key: "BeatsPerMinute", ty: "number", default: "120", doc: "Starting tempo. Time-varying tempo is the `ƒTempo` shim.", parsed: true },
ConfigKey { key: "TimeSignature", ty: "a/b", default: "4/4", doc: "Time signature.", parsed: true },
ConfigKey { key: "Subdivisions", ty: "integer", default: "4", doc: "Rows per beat.", parsed: true },
ConfigKey { key: "SongVolume", ty: "percent", default: "100%", doc: "Static master volume.", parsed: true },
ConfigKey { key: "PanningLaw", ty: "linear | equal_power", default: "equal_power", doc: "How pan maps to left/right gain.", parsed: true },
ConfigKey { key: "Overlap", ty: "word", default: "-", doc: "Written by the converter (`stack`); not read by the parser.", parsed: false },
ConfigKey { key: "PitchDefault", ty: "word", default: "-", doc: "Written by the converter (`hz`); not read by the parser.", parsed: false },
];

pub struct Section {
    pub name: &'static str,
    pub doc: &'static str,
}

pub const SECTIONS: &[Section] = &[
    Section {
        name: "Config { ... }",
        doc: "`Key: value;` entries. Static project header.",
    },
Section {
    name: "BlockList { ... }",
    doc: "A cursor over song time.  Each line places its calls starting at the \
current cursor position; a call occupies as many measures as its selections \
sum to, and the cursor advances past it.  A line's duration is the maximum \
duration of its calls, so parallel calls line up on the same measure.  \
An empty line (`;`) advances the cursor by one measure.  Calls may select \
specific measures from a definition (`ƒName(1,2,3)`) and may apply per-measure \
overrides (`ƒName(1<%50>, 2<%75>)`).  Effect shims (`@<...>`) attach effects \
to windows of song time and may share the line with the calls they modify.",
},
Section {
    name: "BlockArray { ... }",
    doc: "Definitions. `ƒName(<|N/D, ƒsource, ...>)[ events ];` for a part, \
`ƒName(ƒA, ƒB);` for a composite block.  A part's body may contain multiple \
`|N/D[...]` sections; each section is one measure, laid end to end.",
},
];

pub struct Builtin {
    pub name: &'static str,
    pub scope: &'static str,
    pub property: &'static str,
    pub default: &'static str,
    pub unit: &'static str,
    pub doc: &'static str,
    pub status: Status,
}

pub const BUILTINS: &[Builtin] = &[
    Builtin {
        name: "Tempo", scope: "song", property: FIELD_PITCH,
        default: "120", unit: "bpm",
            doc: "Song tempo.", status: Done,
    },
Builtin {
    name: "GlobalVolume", scope: "song", property: FIELD_VOLUME,
        default: "100%", unit: "percent",
            doc: "Song-wide volume multiplier over time.", status: Plan,
},
Builtin {
    name: "GlobalPan", scope: "song", property: FIELD_PAN,
        default: "0", unit: "pan",
            doc: "Song-wide stereo pan.", status: Plan,
},
Builtin {
    name: "GlobalTuning", scope: "song", property: FIELD_PITCH,
        default: "0", unit: "cents",
            doc: "Song-wide fine tuning.", status: Plan,
},
];

pub fn is_reserved(name: &str) -> bool {
    BUILTINS.iter().any(|b| b.name == name)
}

pub fn avoid_reserved(base: &str) -> String {
    if is_reserved(base) {
        format!("{}_smp", base)
    } else {
        base.to_string()
    }
}

pub fn shim_default(name: &str) -> Option<&'static str> {
    BUILTINS.iter().find(|b| b.name == name).map(|b| b.default)
}

// ── shim target status ────────────────────────────────────────────────

pub struct ShimTarget {
    pub pattern: &'static str,
    pub scope: &'static str,
    pub note: &'static str,
    pub status: Status,
}

pub const SHIM_TARGETS: &[ShimTarget] = &[
    ShimTarget {
        pattern: "ƒTempo",
        scope: "song",
        note: "Drives tempo. Only the legacy `Tempo { … }` section currently \
reaches the engine; a `ƒTempo` BlockList shim parses but does not \
change tempo yet.",
status: Done,
    },
ShimTarget {
    pattern: "ƒGlobalVolume · ƒGlobalPan · ƒGlobalTuning",
    scope: "song",
    note: "Reserved names; parsed as `Plan` — no engine consumer.",
    status: Plan,
},
ShimTarget {
    pattern: "ƒ<EffectName>(params)",
    scope: "part/bus",
    note: "Effect target, e.g. `ƒFruity_Reeverb_2(α25, β5)`. Parsed and \
stored in `TrackscriptFile.shims`. **Not yet consumed by the \
mixer** — attaching a reverb has no audible effect today.",
status: Plan,
},
];

pub struct Change {
    pub version: &'static str,
    pub summary: &'static str,
}

pub const CHANGELOG: &[Change] = &[
    Change {
        version: "1.11 (planned)",
        summary: "Unified `ƒName(<|N/D, args>)[ events ];` syntax for definitions and calls. \
Section opener becomes `|`, freeing `@` for shim events. \
The definition's argument list is a note-shaped default wrapped in `<...>`. \
Multi-section part bodies: a part may declare `|N/D[...]` sections end to end, \
one per measure. \
Measure selection in calls: `ƒName(1,2,3)` plays just those measures, in that \
order; `ƒName(1<%50>, 2<%75>)` applies per-measure overrides.  A call's \
duration is the sum of its selected measures and advances the BlockList cursor. \
Shim events (`@<...>`) attach an effect or reserved-global automation to a \
window of song time. \
Sections accumulate row offsets, so a waypoint chain crosses them. \
Inline operators (`+ - * ±`, `^`, `:count`). \
Per-note extended fields: `¢` fine pitch (cents delta), `₨` release (velocity \
delta), `₥` mod X and `¥` mod Y (both deltas from 128).  Omitted when at \
engine default. \
The 1.10 `Tempo { ... }` section becomes the `ƒTempo` shim. \
Effect shims: `@<§pos, ~len, ◊mix, %gain, ¶pan, ƒName(args)>;` attaches \
an effect to a window of song time.  `◊` is a new universal field for \
dry/wet mix; `~` in a shim is the effect's length. Effect parameters \
are positional, indexed by a bijective base-10 run of Greek letters \
(`α1` = slot 1, `αδ6.0` = slot 14).  Parameter names live in the plugin \
registry.",
    },
Change {
    version: "1.10",
    summary: "Current format: `@4/4[` headers, `ƒ'path'(offset)` sources, waypoints with `&`.",
},
];

pub struct Example {
    pub title: &'static str,
    pub status: Status,
    pub src: &'static str,
}

pub const EXAMPLES: &[Example] = &[
    Example { title: "A note", status: Done, src: "<§1/1,#D5,~4,%50>" },
Example { title: "A note with a waypoint", status: Done, src: "<§1/1,#D5,~4,%50>&<§1/2,%43,~0.33>" },
Example { title: "Pan and a per-note sample", status: Done, src: "<§1/1,#C6,~2,¶-1,ƒ'samples/other.wav'>" },
Example { title: "Fine pitch, release, and mod tweaks", status: Done, src: "<§1/1,#C5,~2,¢+5,₨-32,₥-64,¥+64>" },
Example { title: "Operator: volume rises 4 times", status: Plan, src: "<§1/1,#D5,~4,%15+0.1^0/1:4>" },
Example { title: "Operator: pitch oscillates", status: Plan, src: "<§1/1,#C5,~4,#C5±0.25^0/2:4>" },
Example { title: "Shim: end fade-out", status: Plan, src: "@<§1/1,%100-0.78125^0/2:8, ƒGlobalVolume>" },
Example { title: "Shim: tempo", status: Done, src: "@<§1/1,#110, ƒTempo>" },
Example { title: "Definition with defaults", status: Done, src: "ƒHARP(<|4/4, ƒ'samples/harp.wav', %80, ¶0.5>)[\n    <§1/1,#D5>\n    <§1/2,#E5,%100>\n];" },
Example { title: "Multi-measure definition (two sections)", status: Done, src: "ƒMELODY(<|4/4, ƒ'samples/harp.wav'>)[\n    <§1/1,#D5,~2>\n]\n|4/4[\n    <§1/1,#E5,~2>\n];" },
Example { title: "Composite call", status: Done, src: "ƒLAYER(ƒKICK, ƒHARP);" },
Example { title: "Bare call", status: Done, src: "ƒKICK();" },
Example { title: "Measure selection: play measure 2 only", status: Plan, src: "ƒMELODY(2);" },
Example { title: "Measure selection: reorder", status: Plan, src: "ƒMELODY(3,1,2);" },
Example { title: "Measure selection: per-measure volume overrides", status: Plan, src: "ƒMELODY(1<%50>, 2<%75>);" },
Example { title: "Measure selection: some measures overridden, some not", status: Plan, src: "ƒMELODY(1<%50>, 2, 3<%75>);" },
Example { title: "Effect shim: call-attached reverb", status: Plan, src: "ƒpianoer(2)@<ƒFruity_Reeverb_2(α25, β5)>;" },
Example { title: "Effect shim: chained reverb + delay", status: Plan, src: "ƒpianoer(2)@<ƒFruity_Reeverb_2(α25, β5)>@<ƒFruity_Delay_3(α120)>;" },
Example { title: "Effect shim: standalone reverb over a song", status: Plan, src: "@<§1/1,~8,◊0,ƒFruity_Reeverb_2(α25)>;\n&<§1/3,◊0.4>;\n&<§1/8,◊0>;" },
Example { title: "Shim: reserved target (tempo)", status: Done, src: "@<§1/1,#110, ƒTempo>;" },
];

pub fn wrap_example(src: &str) -> String {
    let trimmed = src.trim_start();
    // Call-attached shim (`ƒName(args)@<…>`) is a BlockList line, not a
    // BlockArray entry — even though it starts with `ƒ`.
    let call_attached_shim = trimmed.starts_with('ƒ') && trimmed.contains("@<");
    if trimmed.starts_with('@') || trimmed.starts_with('&') || call_attached_shim {
        format!(
            "BlockList()\n{{\n    {}\n}}\n\nBlockArray()\n{{\n    {}\n}}\n",
                src,
                part_definition("Example", &source_arg("samples/example.wav", None), 4, 4, "<§1/1,#C5>")
        )
    } else if trimmed.starts_with('ƒ') {
        format!(
            "BlockList()\n{{\n    ƒExample();\n}}\n\nBlockArray()\n{{\n    {}\n\n    {}\n}}\n",
                part_definition("Example", &source_arg("samples/example.wav", None), 4, 4, "<§1/1,#C5>"),
                src
        )
    } else {
        format!(
            "BlockList()\n{{\n    ƒExample();\n}}\n\nBlockArray()\n{{\n    {}\n}}\n",
                part_definition("Example", &source_arg("samples/example.wav", None), 4, 4, src)
        )
    }
}

// ── docs ───────────────────────────────────────────────────────────────

fn cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

fn symbol_table(out: &mut String, title: &str, rows: &[Symbol]) {
    let _ = writeln!(out, "## {}\n", title);
    let _ = writeln!(out, "| Symbol | Name | Meaning | Since |");
    let _ = writeln!(out, "|---|---|---|---|");
    for r in rows {
        let _ = writeln!(
            out,
            "| `{}` | {}{} | {} | {} |",
            cell(r.glyph), cell(r.name), r.status.label(), cell(r.doc), r.since
        );
    }
    out.push('\n');
}

fn context_table(out: &mut String) {
    let _ = writeln!(out, "## Contexts\n");
    let _ = writeln!(out, "| Context | Syntax | Meaning |");
    let _ = writeln!(out, "|---|---|---|");
    for r in CONTEXTS {
        let _ = writeln!(
            out, "| {}{} | `{}` | {} |",
            cell(r.name), r.status.label(), cell(r.syntax), cell(r.doc)
        );
    }
    out.push('\n');
}

fn field_context_table(out: &mut String) {
    let _ = writeln!(out, "## Fields by context\n");
    let _ = writeln!(out, "{}\n", DEFAULTS_NOTE);
    let _ = writeln!(out, "| Field | Name | Note | Definition | Call | Shim |");
    let _ = writeln!(out, "|---|---|---|---|---|---|");
    for r in FIELD_CONTEXTS {
        let _ = writeln!(
            out, "| `{}` | {} | {} | {} | {} | {} |",
            cell(r.glyph), cell(r.name), cell(r.note),
                         cell(r.definition), cell(r.call), cell(r.shim)
        );
    }
    out.push('\n');
}

fn shim_table(out: &mut String) {
    let _ = writeln!(out, "## Shim targets\n");
    let _ = writeln!(out, "{}\n", SHIM_SEMANTICS);
    let _ = writeln!(out, "### Reserved globals\n");
    let _ = writeln!(out, "| Target | Scope | Property | Default | Unit | Meaning |");
    let _ = writeln!(out, "|---|---|---|---|---|---|");
    for b in BUILTINS {
        let _ = writeln!(
            out, "| `ƒ{}`{} | {} | `{}` | {} | {} | {} |",
            b.name, b.status.label(), b.scope, cell(b.property),
                         cell(b.default), cell(b.unit), cell(b.doc)
        );
    }
    out.push('\n');
    let _ = writeln!(out, "### Target support status\n");
    let _ = writeln!(out, "| Pattern | Scope | Status | Note |");
    let _ = writeln!(out, "|---|---|---|---|");
    for t in SHIM_TARGETS {
        let _ = writeln!(
            out, "| `{}`{} | {} | {} | {} |",
            cell(t.pattern), t.status.label(), cell(t.scope),
                         if t.status == Status::Implemented { "implemented" } else { "not yet implemented" },
                             cell(t.note)
        );
    }
    out.push('\n');
}

pub fn render_markdown() -> String {
    let mut out = String::new();
    out.push_str("# Trackscript language reference\n\n");
    out.push_str("<!-- Generated from src/spec.rs. Do not edit by hand. -->\n\n");

    let _ = writeln!(out, "## File sections\n");
    for s in SECTIONS {
        let _ = writeln!(out, "- `{}`: {}", s.name, s.doc);
    }
    out.push('\n');

    context_table(&mut out);
    field_context_table(&mut out);

    let _ = writeln!(out, "## Call length\n");
    let _ = writeln!(out, "{}\n", CALL_LENGTH_SEMANTICS);

    symbol_table(&mut out, "Note fields", FIELD_SYMBOLS);
    symbol_table(&mut out, "Event prefixes", EVENT_PREFIXES);
    symbol_table(&mut out, "Operators", OPERATORS);

    shim_table(&mut out);

    let _ = writeln!(out, "## Config keys\n");
    let _ = writeln!(out, "| Key | Type | Default | Meaning | Parser |");
    let _ = writeln!(out, "|---|---|---|---|---|");
    for k in CONFIG_KEYS {
        let _ = writeln!(
            out, "| `{}` | {} | {} | {} | {} |",
            k.key, cell(k.ty), cell(k.default), cell(k.doc),
                         if k.parsed { "reads" } else { "stores only" }
        );
    }
    out.push('\n');

    let _ = writeln!(out, "## Reserved names\n");
    let _ = writeln!(out, "These cannot be used for parts, blocks or instruments.\n");
    for b in BUILTINS {
        let _ = writeln!(out, "- `ƒ{}`{} — see Shim targets.",
                         b.name, b.status.label());
    }
    out.push('\n');

    let _ = writeln!(out, "## Examples\n");
    for e in EXAMPLES {
        let _ = writeln!(out, "### {}{}\n", e.title, e.status.label());
        let _ = writeln!(out, "```\n{}\n```\n", e.src);
    }

    let _ = writeln!(out, "## Changelog\n");
    for c in CHANGELOG {
        let _ = writeln!(out, "- **{}**: {}", c.version, c.summary);
    }
    out
}

// ── tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn implemented_examples_parse() {
        for e in EXAMPLES.iter().filter(|e| e.status == Status::Implemented) {
            let file = crate::formats::trackscript::parse_str(&wrap_example(e.src))
            .unwrap_or_else(|err| panic!("example {:?} failed: {:?}", e.title, err));

            let n_events: usize = file.parts.values().map(|p| p.events.len()).sum();
            let n_shims = file.shims.len();
            let n_calls: usize = file.block_list.iter().map(|l| l.len()).sum();

            assert!(
                n_events > 0 || n_shims > 0 || n_calls > 0,
                "example {:?}: parsed but produced no events, shims, or calls",
                e.title
            );
        }
    }

    #[test]
    fn glyphs_are_unique_per_table() {
        for table in [FIELD_SYMBOLS, EVENT_PREFIXES, OPERATORS] {
            let mut seen = HashSet::new();
            for s in table {
                assert!(seen.insert(s.glyph), "duplicate glyph {}", s.glyph);
            }
        }
    }

    #[test]
    fn note_delimiters_are_not_operators() {
        for s in FIELD_SYMBOLS.iter().chain(EVENT_PREFIXES).chain(OPERATORS) {
            for bad in ['<', '>', ','] {
                assert!(!s.glyph.contains(bad), "{} contains {}", s.glyph, bad);
            }
        }
    }

    #[test]
    fn reserved_names() {
        assert!(is_reserved("GlobalVolume"));
        assert!(is_reserved("Tempo"));
        assert!(!is_reserved("HARP"));
        assert_eq!(avoid_reserved("GlobalVolume"), "GlobalVolume_smp");
        assert_eq!(avoid_reserved("HARP"), "HARP");
    }

    #[test]
    fn every_field_appears_in_the_context_matrix() {
        for s in FIELD_SYMBOLS {
            let found = FIELD_CONTEXTS.iter().any(|f| f.glyph == s.glyph);
            assert!(found, "field {} missing from FIELD_CONTEXTS", s.glyph);
        }
    }

    #[test]
    fn every_builtin_property_is_a_field_glyph() {
        for b in BUILTINS {
            let known = FIELD_SYMBOLS.iter().any(|f| f.glyph == b.property);
            assert!(known, "target {} uses unknown property {}", b.name, b.property);
        }
    }

    #[test]
    fn greek_slot_parsing() {
        assert_eq!(parse_greek_slot("α1"), (1, 2));
        assert_eq!(parse_greek_slot("β0.5"), (2, 2));
        assert_eq!(parse_greek_slot("κ"), (10, 2));
        assert_eq!(parse_greek_slot("αδ6.0"), (14, 4));
        assert_eq!(parse_greek_slot("ικ"), (100, 4));
        assert_eq!(parse_greek_slot("κα"), (101, 4));
        assert_eq!(parse_greek_slot("6.0"), (0, 0));
    }

    #[test]
    fn greek_slot_round_trip() {
        for slot in [1u32, 2, 9, 10, 11, 20, 100, 101, 999] {
            let encoded = slot_to_greek(slot);
            let (decoded, _) = parse_greek_slot(&encoded);
            assert_eq!(slot, decoded, "round trip {} -> {:?}", slot, encoded);
        }
    }

    #[test]
    fn call_semantics_documented() {
        let md = render_markdown();
        assert!(md.contains("sum of the durations"));
    }

    #[test]
    fn shim_semantics_documented() {
        let md = render_markdown();
        assert!(md.contains("global"));
        assert!(md.contains("GlobalVolume"));
        assert!(md.contains("dry/wet"));
        assert!(md.contains("not yet consumed"));
        assert!(md.contains("Call-attached form") || md.contains("call-attached form"));
    }

    #[test]
    fn measure_selection_documented() {
        let md = render_markdown();
        assert!(md.contains("measure selection") || md.contains("Measure selection"));
        assert!(md.contains("BlockList cursor"));
    }

    #[test]
    fn part_definition_shape() {
        let s = part_definition("HARP", "ƒ'samples/harp.wav'", 4, 4, "<§1/1,#D5>");
        assert!(s.starts_with("ƒHARP(<|4/4, ƒ'samples/harp.wav'>)["));
        assert!(s.ends_with("];"));
        assert!(s.contains("    <§1/1,#D5>"));
    }

    #[test]
    fn part_definition_indents_multiline_events() {
        let s = part_definition("X", "ƒ'samples/x.wav'", 4, 4, "<a>,\n<b>");
        assert!(s.contains("    <a>,\n    <b>"));
    }

    #[test]
    fn call_shape_always_parenthesized() {
        assert_eq!(call("KICK", ""), "ƒKICK();");
        assert_eq!(call("HARP", "%60"), "ƒHARP(%60);");
    }

    #[test]
    fn tempo_shim_shape() {
        assert_eq!(tempo_shim("§1/1", "120"), "@<§1/1,#120, ƒTempo>;");
    }

    #[test]
    fn shim_emitter_shape() {
        let s = shim_event("Fruity_Reeverb_2", &[(1, 25.0), (2, 5.0)], None, None, None, None, None);
        assert_eq!(s, "@<ƒFruity_Reeverb_2(α25, β5)>;");
    }

    #[test]
    fn markdown_mentions_everything() {
        let md = render_markdown();
        for s in FIELD_SYMBOLS.iter().chain(EVENT_PREFIXES).chain(OPERATORS) {
            assert!(md.contains(s.name), "missing {}", s.name);
        }
        for k in CONFIG_KEYS {
            assert!(md.contains(k.key), "missing {}", k.key);
        }
        for b in BUILTINS {
            assert!(md.contains(b.name), "missing {}", b.name);
        }
        for c in CONTEXTS {
            assert!(md.contains(c.name), "missing {}", c.name);
        }
    }

    #[test]
    fn language_md_is_up_to_date() {
        let Ok(dir) = std::env::var("CARGO_MANIFEST_DIR") else { return };
        let path = std::path::Path::new(&dir).join("docs").join("LANGUAGE.md");
        let fresh = render_markdown();
        if std::env::var_os("UPDATE_DOCS").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &fresh).unwrap();
            return;
        }
        let on_disk = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            on_disk == fresh,
            "docs/LANGUAGE.md is stale; run UPDATE_DOCS=1 cargo test language_md"
        );
    }
}
