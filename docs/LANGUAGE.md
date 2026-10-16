# Trackscript language reference

<!-- Generated from src/spec.rs. Do not edit by hand. -->

## File sections

- `Config { ... }`: `Key: value;` entries. Static project header.
- `BlockList { ... }`: A cursor over song time.  Each line places its calls starting at the current cursor position; a call occupies as many measures as its selections sum to, and the cursor advances past it.  A line's duration is the maximum duration of its calls, so parallel calls line up on the same measure.  An empty line (`;`) advances the cursor by one measure.  Calls may select specific measures from a definition (`ƒName(1,2,3)`) and may apply per-measure overrides (`ƒName(1<%50>, 2<%75>)`); see the `call` context below.  Effect shims (`@<...>`) attach effects to windows of song time and may share the line with the calls they modify.
- `BlockArray { ... }`: Definitions. `ƒName(<|N/D, ƒsource, ...>)[ events ];` for a part, `ƒName(ƒA, ƒB);` for a composite block.  A part's body may contain multiple `|N/D[...]` sections; each section is one measure, laid end to end.

## Contexts

| Context | Syntax | Meaning |
|---|---|---|
| note | `<§pos, #pitch, ~dur, %vol, ¶pan, ƒsource>` | A note-on inside a part body. Fields describe this one note. |
| definition | `ƒName(<\|N/D, ƒsource, %vol, ¶pan, ~dur>)[ events ];` | Defines a part. The argument list is a note-shaped default: `\|N/D` sets the section frame, `ƒ'path'` names the sample, `%`/`¶`/`~` supply defaults notes may override.  A definition's body may contain multiple `\|N/D[...]` sections; each section is one measure, and they accumulate left-to-right in the part's timeline. |
| composite | `ƒName(ƒA, ƒB, ...);` | Defines a block as a list of calls. No body; the arguments are the body. Distinguished from a part by the args not starting with `<`.  A composite's measure timeline is the concatenation of its calls' timelines. |
| call *(planned)* | `ƒName();  ƒName(1,2,3);  ƒName(1<%50>, 2<%75>);` | Calls a definition from a BlockList line.  An empty argument list plays every measure of the definition in its natural order.  Bare integers select specific measures and set the order they play in — `ƒName(3,1,2)` plays measure 3, then 1, then 2.  A `<...>` override block after a measure number applies to just that measure; the override fields are `%vol`, `¶pan`, `~dur`, and `ƒsource`, matching the field shapes used on a note.  A call's duration is the sum of the durations of its selected measures; the BlockList cursor advances by that amount.  Some fields with a note-shaped meaning (position, pitch) have no place in a call's override block and are ignored if present. |
| shim | `@<§pos, ~len, ◊mix, %gain, ¶pan, ƒName(args)>;` | Attaches an effect to a window of song time. The target (`ƒName`) is either an effect function or a reserved global (see Shim targets below). Universal shim fields are `§` start, `~` length, `◊` dry/wet, `%` output gain, and `¶` output pan; waypoints (`&`) ramp any of them. Effect parameters are positional: the slot index is a bijective base-10 run of Greek letters (`α1` is slot 1, `αδ6.0` is slot 14) followed by the value. Parameter names and slot assignments live in the plugin registry, not in the file. Removing a shim is abrupt unless the writer closes it with a waypoint to `◊0`. |

## Fields by context

A field omitted from a note inherits the definition's default for that field. A field omitted from a definition falls back to the engine default: 100% volume, 0 pan, 1-beat duration.

| Field | Name | Note | Definition | Call | Shim |
|---|---|---|---|---|---|
| `§` | position | start row of this note | ignored | ignored (measure selection replaces it) | start row of the effect window |
| `#` | pitch | this note's pitch | default pitch | ignored | the value being ramped (for pitch-shaped targets) |
| `~` | duration | this note's length, in beats | default length | how long the selected measures play | length of the effect window |
| `%` | volume | this note's volume | default volume | volume override, per selected measure | output gain of the effect |
| `¶` | pan | this note's pan | default pan | pan override, per selected measure | output pan of the effect |
| `ƒ` | source / call | sample override for this note | the part's sample (required) | sample override, per selected measure | target: effect function or reserved global |
| `¢` | fine pitch | cents offset for this note | default cents offset | ignored | ignored |
| `₨` | release | release velocity delta for this note | default release delta | ignored | ignored |
| `₥` | mod X | mod X delta for this note | default mod X delta | ignored | ignored |
| `¥` | mod Y | mod Y delta for this note | default mod Y delta | ignored | ignored |
| `◊` | mix | ignored | ignored | ignored | dry/wet mix of the effect, 0.0..1.0 |

## Call length

A call's duration is the sum of the durations of the measures it selects.  With no arguments, that is the definition's full length.  With measure selection (`ƒName(1,2,3)`), it is the sum of the chosen measures in the order listed.  The BlockList cursor advances by a call's duration, so calls stack vertically in a BlockList line the way notes stack horizontally in a measure.  An empty BlockList line (`;`) advances by one measure of the song's time signature.

## Note fields

| Symbol | Name | Meaning | Since |
|---|---|---|---|
| `§` | position | `§beat/subdivision`, both 1-based; the subdivision may be fractional (`§1/1.5`). Local to the measure the part is placed in. | 1.10 |
| `#` | pitch | Note name (`C5`, `A#4`) or a number in Hz. | 1.10 |
| `~` | duration | Length in beats. May run past the end of the measure. | 1.10 |
| `%` | volume | Percent. A leading `+` is tolerated. | 1.10 |
| `¶` | pan | -1 (full left) to 1 (full right). | 1.10 |
| `ƒ` | source / call | `ƒ'path'` overrides the sample. `ƒName` calls a defined part or block. | 1.10 |
| `¢` | fine pitch | Cents offset from equal temperament, as a signed delta from 0. Slides glide to the equal-tempered pitch of the target note and ignore this field. | 1.11 |
| `₨` | release | Release velocity, as a signed delta from the engine default of 64. Shapes the amplitude envelope's release stage when the note-off fires. | 1.11 |
| `₥` | mod X | Modulation X, as a signed delta from the engine default of 128. Routes to the plugin's X destination (filter cutoff on simple plugins). | 1.11 |
| `¥` | mod Y | Modulation Y, as a signed delta from the engine default of 128. Routes to the plugin's Y destination (resonance on simple plugins). | 1.11 |
| `◊` | mix | Dry/wet mix of an effect shim. 0 is fully dry, 1 is fully wet. Rampable with waypoints. | 1.11 |

## Event prefixes

| Symbol | Name | Meaning | Since |
|---|---|---|---|
| `` | note | A note-on. | 1.10 |
| `&` | waypoint | Changes properties of the preceding note at a later position instead of starting a new note. Waypoints cross section boundaries; a section header between them is a layout marker, not a break. | 1.10 |
| `=` | (undocumented) | Recognised by the parser; its meaning is not defined in trackscript.rs. | 1.10 |
| `▲` | (undocumented) | Recognised by the parser; its meaning is not defined in trackscript.rs. | 1.10 |
| `♪` | (undocumented) | Recognised by the parser; its meaning is not defined in trackscript.rs. | 1.10 |
| `@` | shim | Attaches an effect or reserved-global automation to a window. The last field of `@<...>` is `ƒName(args)`; Name is an effect function or a reserved global. | 1.11 |

## Operators

| Symbol | Name | Meaning | Since |
|---|---|---|---|
| `+` | increase *(planned)* | Add `amount` each interval. | 1.11 |
| `-` | decrease *(planned)* | Subtract `amount` each interval. | 1.11 |
| `*` | multiply *(planned)* | Multiply by `amount` each interval. | 1.11 |
| `±` | oscillate *(planned)* | Alternate down/up by `amount` (triangle wave). `:count` counts direction changes. | 1.11 |
| `^` | rate *(planned)* | Introduces the interval between changes, in beat/subdivision format. | 1.11 |
| `:` | count *(planned)* | How many times the operation applies. Default 1. | 1.11 |

## Shim targets

An effect shim attaches an effect to a window of song time.  Its target is the last field: `ƒName(args)`, where Name is either an effect function or a reserved global.  The universal shim fields are position (`§`), length (`~`), dry/wet mix (`◊`), output gain (`%`), and pan (`¶`); waypoints (`&`) ramp any of them, in the block's own coordinate frame.

Effect parameters are positional.  The slot index is a run of Greek letters read as bijective base-10 (α=1 … ι=9, κ=10; no zero digit), and the value follows immediately after: `α1` is slot 1, `αδ6.0` is slot 14, `κα8` is slot 101.  Slot names live in the plugin registry.

A shim placed on a BlockList line applies to the calls on that line.  A shim placed on its own line applies from the cursor's current position.  When a shim's window ends, its effect is removed abruptly unless the writer supplies a closing waypoint to `◊0`.  There is no implicit fade-out.

| Target | Scope | Property | Default | Unit | Meaning |
|---|---|---|---|---|---|
| `ƒTempo` | song | `#` | 120 | bpm | Song tempo. |
| `ƒGlobalVolume` *(planned)* | song | `%` | 100% | percent | Song-wide volume multiplier over time. |
| `ƒGlobalPan` *(planned)* | song | `¶` | 0 | pan | Song-wide stereo pan. |
| `ƒGlobalTuning` *(planned)* | song | `#` | 0 | cents | Song-wide fine tuning. |

## Config keys

| Key | Type | Default | Meaning | Parser |
|---|---|---|---|---|
| `TrackscriptVersion` | number | - | Format version the file was written for. | stores only |
| `BeatsPerMinute` | number | 120 | Starting tempo. Time-varying tempo is the `ƒTempo` shim. | reads |
| `TimeSignature` | a/b | 4/4 | Time signature. | reads |
| `Subdivisions` | integer | 4 | Rows per beat. | reads |
| `SongVolume` | percent | 100% | Static master volume. | reads |
| `PanningLaw` | linear \| equal_power | equal_power | How pan maps to left/right gain. | reads |
| `Overlap` | word | - | Written by the converter (`stack`); not read by the parser. | stores only |
| `PitchDefault` | word | - | Written by the converter (`hz`); not read by the parser. | stores only |

## Reserved names

These cannot be used for parts, blocks or instruments.

- `ƒTempo` — see Shim targets.
- `ƒGlobalVolume` *(planned)* — see Shim targets.
- `ƒGlobalPan` *(planned)* — see Shim targets.
- `ƒGlobalTuning` *(planned)* — see Shim targets.

## Examples

### A note

```
<§1/1,#D5,~4,%50>
```

### A note with a waypoint

```
<§1/1,#D5,~4,%50>&<§1/2,%43,~0.33>
```

### Pan and a per-note sample

```
<§1/1,#C6,~2,¶-1,ƒ'samples/other.wav'>
```

### Fine pitch, release, and mod tweaks

```
<§1/1,#C5,~2,¢+5,₨-32,₥-64,¥+64>
```

### Operator: volume rises 4 times *(planned)*

```
<§1/1,#D5,~4,%15+0.1^0/1:4>
```

### Operator: pitch oscillates *(planned)*

```
<§1/1,#C5,~4,#C5±0.25^0/2:4>
```

### Shim: end fade-out *(planned)*

```
@<§1/1,%100-0.78125^0/2:8, ƒGlobalVolume>
```

### Shim: tempo

```
@<§1/1,#110, ƒTempo>
```

### Definition with defaults

```
ƒHARP(<|4/4, ƒ'samples/harp.wav', %80, ¶0.5>)[
    <§1/1,#D5>
    <§1/2,#E5,%100>
];
```

### Multi-measure definition (two sections)

```
ƒMELODY(<|4/4, ƒ'samples/harp.wav'>)[
    <§1/1,#D5,~2>
]
|4/4[
    <§1/1,#E5,~2>
];
```

### Composite call

```
ƒLAYER(ƒKICK, ƒHARP);
```

### Bare call

```
ƒKICK();
```

### Measure selection: play measure 2 only *(planned)*

```
ƒMELODY(2);
```

### Measure selection: reorder *(planned)*

```
ƒMELODY(3,1,2);
```

### Measure selection: per-measure volume overrides *(planned)*

```
ƒMELODY(1<%50>, 2<%75>);
```

### Measure selection: some measures overridden, some not *(planned)*

```
ƒMELODY(1<%50>, 2, 3<%75>);
```

### Effect shim: reverb over a block *(planned)*

```
@<§1/1, ~8, ◊0, ƒReverb.Hall(α1, γ8)>;
&<§1/3, ◊0.4>;
&<§1/8, ◊0>;
```

### Effect shim: paired with a call *(planned)*

```
ƒpianoer(2);
@<§1/1, ~8, ◊0, ƒReverb.Hall(αδ6.0)>;
```

### Shim: reserved target (tempo)

```
@<§1/1,#110, ƒTempo>;
```

## Changelog

- **1.11 (planned)**: Unified `ƒName(<|N/D, args>)[ events ];` syntax for definitions and calls. Section opener becomes `|`, freeing `@` for shim events. The definition's argument list is a note-shaped default wrapped in `<...>`. Multi-section part bodies: a part may declare `|N/D[...]` sections end to end, one per measure. Measure selection in calls: `ƒName(1,2,3)` plays just those measures, in that order; `ƒName(1<%50>, 2<%75>)` applies per-measure overrides.  A call's duration is the sum of its selected measures and advances the BlockList cursor. Shim events (`@<...>`) attach an effect or reserved-global automation to a window of song time. Sections accumulate row offsets, so a waypoint chain crosses them. Inline operators (`+ - * ±`, `^`, `:count`). Per-note extended fields: `¢` fine pitch (cents delta), `₨` release (velocity delta), `₥` mod X and `¥` mod Y (both deltas from 128).  Omitted when at engine default. The 1.10 `Tempo { ... }` section becomes the `ƒTempo` shim. Effect shims: `@<§pos, ~len, ◊mix, %gain, ¶pan, ƒName(args)>;` attaches an effect to a window of song time.  `◊` is a new universal field for dry/wet mix; `~` in a shim is the effect's length.  Effect parameters are positional, indexed by a bijective base-10 run of Greek letters (`α1` = slot 1, `αδ6.0` = slot 14).  Parameter names live in the plugin registry.
- **1.10**: Current format: `@4/4[` headers, `ƒ'path'(offset)` sources, waypoints with `&`.
