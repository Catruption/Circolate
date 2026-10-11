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

pub const FIELD_POSITION: &str = "§";
pub const FIELD_PITCH:    &str = "#";
pub const FIELD_DURATION: &str = "~";
pub const FIELD_VOLUME:   &str = "%";
pub const FIELD_PAN:      &str = "¶";
pub const FIELD_SOURCE:   &str = "ƒ";

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
///
/// `events` may contain newlines; each line is indented four spaces to
/// line up under the opening bracket.  Callers that want one event per
/// line should join with `",\n"` rather than `", "`.
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
    sym(FIELD_PITCH, "pitch", "Note name (`C5`, `A#4`) or a number in Hz.", "1.10", Done),
    sym(FIELD_DURATION, "duration", "Length in beats. May run past the end of the measure.", "1.10", Done),
    sym(FIELD_VOLUME, "volume", "Percent. A leading `+` is tolerated.", "1.10", Done),
    sym(FIELD_PAN, "pan", "-1 (full left) to 1 (full right).", "1.10", Done),
    sym(FIELD_SOURCE, "source / call", "`ƒ'path'` overrides the sample. `ƒName` calls a defined part or block.", "1.10", Done),
];

pub const EVENT_PREFIXES: &[Symbol] = &[
    sym(EV_NOTE, "note", "A note-on.", "1.10", Done),
    sym(EV_WAYPOINT, "waypoint", "Changes properties of the preceding note at a later position instead of starting a new note. Waypoints cross section boundaries; a section header between them is a layout marker, not a break.", "1.10", Done),
    sym(EV_UNDOC_EQ, "(undocumented)", "Recognised by the parser; its meaning is not defined in trackscript.rs.", "1.10", Done),
    sym(EV_UNDOC_TRI, "(undocumented)", "Recognised by the parser; its meaning is not defined in trackscript.rs.", "1.10", Done),
    sym(EV_UNDOC_NOTE, "(undocumented)", "Recognised by the parser; its meaning is not defined in trackscript.rs.", "1.10", Done),
    sym(EV_SHIM, "shim", "Applies mix automation to a named target. The last field of the `@<...>` list must be a reserved target name.", "1.11", Done),
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
`%`/`¶`/`~` supply defaults notes may override.",
status: Done,
},
Context {
    name: "composite",
    syntax: "ƒName(ƒA, ƒB, ...);",
    doc: "Defines a block as a list of calls. No body; the arguments are the body. \
Distinguished from a part by the args not starting with `<`.",
status: Done,
},
Context {
    name: "call",
    syntax: "ƒName(<§offset, %vol, ¶pan, ~dur, ƒsource>);",
    doc: "Calls a definition. Always parenthesized; empty parens are `ƒName();`. \
Arguments override the definition's defaults for this call. \
`§offset` starts the call partway into the parent measure. \
`~dur` sets how long the call plays: shorter cuts voices off at the call end, \
longer repeats the definition until the time is filled.",
status: Plan,
},
Context {
    name: "shim",
    syntax: "@<§pos, %expr, ƒTarget>;",
    doc: "Mix automation on a BlockList line. `§pos` is where the curve starts \
(local to the line's measure). The value field carries a starting \
value and an operator expression. The trailing `ƒName` must be one \
of the reserved shim targets listed below.",
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
        call: "offset within the measure",
        shim: "start row of the curve",
    },
FieldContextUse {
    glyph: FIELD_PITCH, name: "pitch",
    note: "this note's pitch",
    definition: "default pitch",
    call: "pitch override",
    shim: "the value being ramped (for pitch-shaped targets)",
},
FieldContextUse {
    glyph: FIELD_DURATION, name: "duration",
    note: "this note's length, in beats",
    definition: "default length",
    call: "how long this call plays: <1 truncates, >1 repeats",
    shim: "ignored (the operator's rate carries it)",
},
FieldContextUse {
    glyph: FIELD_VOLUME, name: "volume",
    note: "this note's volume",
    definition: "default volume",
    call: "volume override",
    shim: "the value being ramped (for percent-shaped targets)",
},
FieldContextUse {
    glyph: FIELD_PAN, name: "pan",
    note: "this note's pan",
    definition: "default pan",
    call: "pan override",
    shim: "the value being ramped (for pan-shaped targets)",
},
FieldContextUse {
    glyph: FIELD_SOURCE, name: "source / call",
    note: "sample override for this note",
    definition: "the part's sample (required)",
    call: "sample override, or a call target",
    shim: "target shim name (required)",
},
];

pub const DEFAULTS_NOTE: &str =
"A field omitted from a note inherits the definition's default for that field. \
A field omitted from a definition falls back to the engine default: 100% volume, \
0 pan, 1-beat duration.";

    pub const CALL_LENGTH_SEMANTICS: &str =
    "Call length: shorter than the definition cuts voices off at the call \
end; equal plays once; longer repeats the definition until the time is \
filled, truncating the final iteration.  Omitted means play once.";

pub const SHIM_SEMANTICS: &str =
"A shim is a global call.  It carries a starting value and an operator \
expression, and applies that curve to a named target for the rest of \
the song (until another shim for the same target takes over).  \
Multiple shims for the same target do not multiply: the later one \
overrides.  The target's property determines which field carries the \
value: `%` for percent-shaped targets, `¶` for pan, `#` for pitch.";

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
    doc: "One line per measure. Each line is a sequence of `ƒName(...);` calls and `@<...>;` shim events. \
A call's `~dur` argument may truncate or repeat the definition it calls.",
},
Section {
    name: "BlockArray { ... }",
    doc: "Definitions. `ƒName(<|N/D, ƒsource, ...>)[ events ];` for a part, `ƒName(ƒA, ƒB);` for a composite block.",
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
Call `~dur` truncates or repeats the called definition. \
Shim events (`@<...>`) apply mix automation to reserved global targets. \
Sections accumulate row offsets, so a waypoint chain crosses them. \
Inline operators (`+ - * ±`, `^`, `:count`). \
The 1.10 `Tempo { ... }` section becomes the `ƒTempo` shim.",
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
Example { title: "Operator: volume rises 4 times", status: Plan, src: "<§1/1,#D5,~4,%15+0.1^0/1:4>" },
Example { title: "Operator: pitch oscillates", status: Plan, src: "<§1/1,#C5,~4,#C5±0.25^0/2:4>" },
Example { title: "Shim: end fade-out", status: Plan, src: "@<§1/1,%100-0.78125^0/2:8, ƒGlobalVolume>" },
Example { title: "Shim: tempo", status: Done, src: "@<§1/1,#110, ƒTempo>" },
Example { title: "Definition with defaults", status: Done, src: "ƒHARP(<|4/4, ƒ'samples/harp.wav', %80, ¶0.5>)[\n    <§1/1,#D5>\n    <§1/2,#E5,%100>\n];" },
Example { title: "Composite call", status: Done, src: "ƒLAYER(ƒKICK, ƒHARP);" },
Example { title: "Call with overrides", status: Plan, src: "ƒHARP(<%60, §1/3>);" },
Example { title: "Call with offset and length", status: Plan, src: "ƒDRUMS(<§1/3, ~2>);" },
Example { title: "Call that repeats a two-beat part four times", status: Plan, src: "ƒHIT(<~8>);   // definition is |2/4, so ~8 plays it four times" },
Example { title: "Bare call", status: Done, src: "ƒKICK();" },
];

pub fn wrap_example(src: &str) -> String {
    format!(
        "BlockList()\n{{\n    ƒExample();\n}}\n\nBlockArray()\n{{\n    {}\n}}\n",
            part_definition("Example", &source_arg("samples/example.wav", None), 4, 4, src)
    )
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
            let file = crate::sources::trackscript::parse_str(&wrap_example(e.src))
            .unwrap_or_else(|err| panic!("example {:?} failed: {:?}", e.title, err));
            let part = file
            .parts
            .get("Example")
            .unwrap_or_else(|| panic!("example {:?}: part not found", e.title));
            assert!(!part.events.is_empty(), "example {:?}: no events", e.title);
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
    fn call_semantics_documented() {
        let md = render_markdown();
        assert!(md.contains("repeats the definition"));
    }

    #[test]
    fn shim_semantics_documented() {
        let md = render_markdown();
        assert!(md.contains("global call"));
        assert!(md.contains("GlobalVolume"));
        assert!(md.contains("overrides"));
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
