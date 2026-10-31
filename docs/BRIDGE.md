# CHIP.md
# Circolate file formats (v2)

Three text file kinds, one tiny parser:

| File | Role | Analogy |
|---|---|---|
| `.chip` | **Backend model.** Advertises capabilities, implements the primitive wiring for one chip, declares bus/registers if any. | an audio driver |
| `.patch` | **Sound.** What a note does: source, pitch, amp, optional filter/mod. Portable. | SF2 preset, VST preset |
| `.bank` | **Collection.** Patches + samples + license + provenance + preferred chip binding. | SF2 soundfont |

The inversion from v1: **sounds are authored as `.patch`, not as register traces in
`.chip`.** A `.chip` is a backend a patch *runs on*. The adapter is the only place the two
meet.

```
.vgm --lift--> .bank (.patch files + samples)
                    |
                    v
              Trackscript <--> Timeline <--adapter--> .chip core --> audio
```

Design rules:

1. `.chip` is pure data plus primitive wiring. It contains no sounds.
2. `.patch` is portable. It states `requires`; the backend satisfies them or the adapter
   logs a rendition.
3. No register addresses appear outside a `.chip` field table. Everything else speaks in
   capability names or native field names.
4. Lifting produces `.patch` files with `amp steps` envelopes and, when bit-exactness is
   needed, `native` blocks.

---

## 1. Lexical rules

```
comment  := '#' .* EOL
int      := [0-9]+ | '0x' [0-9a-fA-F]+ | '0b' [01]+
num      := int | [0-9]+ '.' [0-9]+
ident    := [A-Za-z_][A-Za-z0-9_]*
string   := '"' ... '"'
range    := int '..' int
expr     := arithmetic over num, ident (clock, n, fnum, block, i), + - * / ^ ( )
```

A **directive** starts at column 0. Lines indented under it belong to it (a block). Blank
lines and comments are ignored. Unknown directives are an error, not skipped, so typos
surface. Unknown *keys* under a known directive are also errors.

Each file kind declares its version on the first directive: `chip 2`, `patch 1`, `bank 1`.
Version bumps are reserved for incompatible changes; new directives and new capability
vocabulary entries are additive and don't bump.

---

## 2. `.chip` grammar (backend model)

First directive must be `chip 2`. A chip file is self-contained: it does not `use` another
file.

```
chip 2
name      <ident>
title     <string>
family    fm | psg | fmhybrid | ...
clock     <int>                       # default clock in Hz
step      <expr>                      # one core step, e.g. clock / 16
out       unipolar | bipolar          # 0..1 or -1..1; host resamples & DC-blocks

variant <ident> [<key> <value>]...    # named flavor; keys passed to the core
default_variant <ident>

behavior                              # adapter instructions (not capabilities)
  key     none | reg <field>
  noteon  <step>...                   # step: patch | pitch | vol | keyon | pan
  noteoff <step>...                   # step: keyoff | vol_off

voices                                # voice kinds the adapter can allocate
  <name> <kind>                       # e.g. tone1 tone

provides                              # capability contract; see §5
  <capability>
    <key> <value>...
    dependency <expr>                 # optional, e.g. noise_rate=3 consumes voice tone3

reg <name> voices|global <field-list> width <n> reset <int>
field <name> reg <reg> bits <n> | <lo>..<hi> role pitch|param|volume|key
enum <field> <name>=<int> ...

bus <protocol>                        # latch_data | addr_data; omit for fantasy chips
  ...                                 # see §2.3
core
  ...                                 # primitive wiring; see §2.4
vgm                                   # only on chips that match real hardware
  header <name> <offset> [min_version <v>] [dual_bit <n>]
  op     <opcode> <port>
  op2    <opcode> <port>              # second chip, for dual
```

### 2.1 Voice declaration vs. capability sources

- `voices` says which voices exist and of what kind. The adapter allocates from this.
- `provides ... sources` says which of those voices can satisfy a capability. A voice may
  serve one capability; a capability may be served by several voices.

A fantasy chip may declare `voices` and `provides` without any `bus` or `reg` block; fields
are then parameters, not registers.

### 2.2 Field table

`reg` declares logical registers. `field` gives each a named view with a `role`:

| Role | Meaning | Addressable as parameter? |
|---|---|---|
| `pitch` | part of the pitch law | no, use semantic `pitch` |
| `param` | free parameter (mul, ar, alg, noise_rate) | yes |
| `volume` | a field the volume law adds to | yes, composed (BRIDGE.md §3.4) |
| `key` | key-on/off | no, implied by note span |

Several fields may share a register byte. Writes always emit the whole byte composed from
shadow, never a partial update. Field names must not end in a digit (BRIDGE.md §3.2).

### 2.3 Bus (for real-hardware chips)

Two protocols are implemented in code; everything else is a fantasy chip with no bus.

**`latch_data`** (SN76489): byte stream. A latch byte selects channel and type and carries
the low bits; a data byte carries the high bits of a tone register and is a no-op for
noise/attenuation.

```
bus latch_data
  flag  bit 7
  chan  bits 5..6
  type  bit 4
  lo    bits 0..3
  hi    bits 0..5

  route type=0  tone voices -> div
  route type=0  noise voice -> noise
  route type=1              -> att

  latch div     = (reg & ~0x0F) | (lo & 0x0F)   # latch writes low 4 bits
  latch other   = lo & mask(reg)                # noise/att get full value
  data  div     = (reg & 0x0F) | (hi << 4)      # data writes high 6 bits
  data  other   = ignore                        # data to noise/att is a no-op

  on_write noise -> nz.reset
```

**`addr_data`** (most FM chips): write `(port, reg, value)`. Register address is
`field.reg + (ch % chan_per_port) * chan_stride + op_offset[op]`; port is
`ch / chan_per_port`.

```
bus addr_data
  ports          <n>
  chan_per_port  <n>
  chan_stride    <n>
  op_offset      <op>:<offset>...
```

### 2.4 Core wiring

The core block wires **primitives** implemented once, in code, shared by every chip. A
`.chip` file never contains Rust; it names primitives and gives their parameters.

```
core
  divider <id>  n=<expr>  [dc_below=<int>]
  lfsr    <id>  clock=<id>.rise  taps=<expr>  width=<expr>  mode=<expr>
  table   <id>[<len>] = <expr>
  mix     <expr>
```

Primitive semantics (all implemented in code):

```
divider:  each step cnt -= 1; if cnt <= 0 { cnt = n; q ^= 1 }
          q is 0/1; rise is a one-step pulse on 0 -> 1.
          dc_below m: n < m holds q at 1.

lfsr:     on each `clock` pulse
            fb  = mode ? parity(reg & taps) : (reg & 1)
            reg = (reg >> 1) | (fb << (width - 1)); q = reg & 1
            reset: reg = 1 << (width - 1)

table:    precomputed lookup; expressions may use i (index)

pick:     pick(sel, v0, v1, ...) -> v[sel], 0-based (usable in any <expr>)

mix:      final per-step output expression
```

Tables live in the `.chip` file (§2.5), not in code. Correcting a table is a file edit.

### 2.5 What "tables live in the file" means

`table level[16] = (i == 15) ? 0 : 10 ^ (-i / 10)` — the right-hand side is evaluated once
at load. For the SN76489 that's a one-line formula; for the YM2612 it will be the log-sine
and exp tables. Both are data. If a table turns out to be wrong against a capture, edit the
file, not the code.

The tradeoff: a `.chip` file for a complex FM chip becomes large (10–20× the SN76489). That
is the correct trade — tables are the least interesting lines in the file, and keeping them
out of code is what makes accuracy-later a file edit rather than a recompile.

### 2.6 `provides` block

The capability vocabulary is defined in §5. Each `provides` entry advertises that this
backend can satisfy the capability, with constraints. Keys under a capability are the
contract — the adapter checks a patch's `requires` against these.

`dependency <expr>` declares a runtime constraint the adapter must honor. The SN76489's
`noise_rate=3 consumes voice tone3` is one: a noise note with rate 3 blocks `tone3` for its
span. The adapter reads this; the core does not.

---

## 3. `.patch` grammar (sound)

First directive must be `patch 1`. A patch is portable: it says what a note does and what
it requires. It does not name chips or registers.

```
patch 1

name    <string>
author  <string>
license <string>
origin  <string>                     # "vgm:<hash> track:<n> voice:<name>" | "handwritten" | ...
parent  <name>                       # optional; see §3.4

requires <capability>                # repeated, conjunctive
  <key> <value>...

source  <kind>                       # one; opens a block
  <key> <value>...

pitch
  transpose <num>                    # semitones
  detune    <num>                    # cents
  glide     <num>                    # seconds, 0 = none

amp     <form>                       # adsr | steps; see §3.2
  ...

filter  <kind>                       # optional; reserved for v2
  <key> <value>...

mod     <kind>                       # optional; reserved for v2
  <key> <value>...

native  <chip-model>                 # optional, repeated; bit-exact overrides
  <field> <value>...
```

### 3.1 `requires` and `source`

`requires` lists the capabilities the patch needs. The patch plays on a backend iff every
listed capability is advertised by the backend's `provides` block, with constraints
satisfied. Constraints are *binding* in strict mode (reject) and *advisory* in rendition
mode (log a warning, proceed).

`source` selects the sound source and sets its parameters:

```
source tone.square

source noise.lfsr
  rate 0
  fb   white
```

Abstract keys (`rate`, `fb`) are mapped to native fields by the backend's `provides` block
(`key rate -> noise_rate`, etc.).

### 3.2 Envelope forms

Two forms, both first-class. A hand-written patch uses `amp adsr`; a lifted patch uses
`amp steps`.

```
amp adsr
  a 0.005      # attack, seconds
  d 0.120      # decay, seconds
  s 0.0        # sustain, 0..1
  r 0.040      # release, seconds
  curve lin    # lin | exp | log

amp steps
  values 0 2 4 6 8 10 13 15   # attenuation steps; 0 = loudest, last = off
  loop   4                     # step index to loop from while held; omitted = one-shot
  rate   60                    # steps per second
```

Behavior at note-off: jump to the last step. For a lifted SN76489 patch that's `15` (off);
for a drone the last step can be non-off and the note continues to sound. Both are valid.

The adapter converts: `amp adsr` becomes a step sequence at the backend's envelope
resolution (whatever its `amp.*` capability provides); `amp steps` plays exactly if the
backend provides `amp.attenuation` with matching step count and step_db, or is resampled
with a logged error bound.

### 3.3 `native` block

Escape hatch for bit-exactness on a specific backend.

```
native sn76489
  div 200
  att 5
```

When rendering on a `.chip` whose `name` matches, the adapter prefers these values over what
the laws would produce. On any other backend they are ignored. This is how a lifted patch
stays exact without contaminating the portable layer.

### 3.4 Inheritance

`parent <name>` resolves against sibling patches in the same bank. This patch's fields
override its parent's; `requires` blocks are unioned. Cycles and unknown parents are errors.

---

## 4. `.bank` grammar (collection)

First directive must be `bank 1`. A bank is a manifest of patches, samples, and provenance.

```
bank 1
name     <string>
author   <string>
license  <string>
chip     <chip-model> [variant <ident>] [clock <int>]   # preferred binding

patch    <path>                      # add a patch by file path (relative to the bank)
alias    <name> <name>               # rename a patch locally

map      program <n> -> <patch-name> # selection by MIDI program
zone     <lo>..<hi>  <patch-name>    # selection by key range; lowest matching zone wins

sample   <name> <path> [rate <hz>] [depth <bits>] [root <key>]
                                     # sample data; only valid for backends
                                     # advertising sample.pcm
```

Patches within a bank are addressed by the basename of their file path, or by `alias`.
Multiple patches of the same name is an error.

---

## 5. Capability vocabulary (v1)

The interop boundary. Each entry is a promise: "a backend that provides X can be asked to do
Y in a standardized way." Extending it is a versioned change to the format, not a per-file
choice.

| Capability | Provides | Constraint keys |
|---|---|---|
| `tone.square` | a square-wave tone | — |
| `tone.pulse` | a pulse-wave tone | `duty` (list) — reserved |
| `tone.triangle` | a triangle tone | — reserved |
| `tone.saw` | a sawtooth tone | — reserved |
| `tone.sine` | a sine tone | — reserved |
| `tone.wavetable` | a wavetable tone | `tables` (count) — reserved |
| `noise.lfsr` | an LFSR noise source | `width` (list), `taps`, `fb` (`periodic` `white`) |
| `noise.periodic` | a fixed periodic pulse | — reserved |
| `sample.pcm` | a PCM sample source | `rate` (Hz), `depth` (bits) — reserved |
| `fm.operator` | an FM operator chain | `operators` (int), `feedback` (bool) — reserved |
| `pitch.divider` | `n = clock/(32·f)` | `range` (`lo..hi`), `clock` (Hz) |
| `pitch.fnum_block` | `fnum · (clock/144) / 2^(21−block)` | `block` (0..7), `clock` (Hz) — reserved |
| `pitch.linear` | `n = clock/f` | `range`, `clock` — reserved |
| `amp.attenuation` | attenuation steps | `steps` (int), `step_db` (num), `off` (int) |
| `amp.linear` | linear amplitude | `resolution` (bits) — reserved |
| `amp.envelope` | rate-table envelope | `rate_table` (string) — reserved |
| `filter.one_pole` | 1-pole lowpass | `cutoff` (Hz), `slope` (dB/oct) — reserved |
| `filter.biquad` | biquad filter | `type` (`lp` `hp` `bp`) — reserved |
| `mod.lfo` | LFO | `rate` (Hz), `target` (param name) — reserved |

v1 ships `tone.square`, `noise.lfsr`, `pitch.divider`, `amp.attenuation` — exactly what the
SN76489 provides. The rest are named now so the grammar is stable and the field can be
extended without a format bump.

---

## 6. Voice allocation

A note takes a free voice of the right kind:

- `tone.square` → any voice in `provides tone.square sources`.
- `noise.lfsr` → any voice in `provides noise.lfsr sources`.
- `sample.pcm` → a `dac`/sample voice if the backend has one.
- `fm.operator` → an FM channel.

If none are free, the bank's `steal` policy applies (bank-level; default `oldest`). Voice
count is real, so polyphony limits are authentic.

`behavior noteon` / `noteoff` define write ordering. For the YM2612, key-off precedes patch
writes so retriggers are clean.

Capability `dependency` expressions constrain allocation: a noise note with rate 3 on the
SN76489 consumes `tone3` for its span and no other note may use that voice meanwhile.

---

## 7. Pipe interface (module boundary)

The chip crate touches nothing outside this interface:

```
in :  Write { t, backend, port, reg, val }     raw register write
      Note  { t, dur, patch, key, vel }        note event (0..127)
out:  PCM (f32, native rate) | Vec<Note> from lift(&[Write])
```

The adapter is between `Note` and `Write`: it reads the backend's `.chip` (field table,
laws, capability provides), the `.patch` (source, envelope, requires, native), and produces
register writes or parameter sets at the core's step rate.

---

## 8. Validation (loader must reject)

- Missing or unsupported `chip` / `patch` / `bank` version.
- Field name not in the chip's field table; value out of range for its bits.
- Field name ends in a digit (BRIDGE.md §3.2).
- `requires` naming a capability not in the vocabulary.
- `source` kind not in the vocabulary, or not listed as `requires`.
- `amp steps` with `loop` >= `values` length.
- `amp adsr` with a negative time.
- `native <chip>` for a chip not declared by the bank (warning, not error).
- Sample in a bank whose chip doesn't advertise `sample.pcm`.
- Reserved: `chan` low bits == 3 on `addr_data` addressing.
- Cyclic `parent` references.
- Duplicate patch names in a bank.

---

## 9. Not decided yet

- Cross-backend patch translation (a lifted SN76489 patch played on a YM2612 backend). Out
  of scope for v1; the `requires` block makes it explicit that it cannot play, which is the
  correct behavior.
- Whether user-written `.chip` files may add new chips without a core (yes: a `.chip` with
  no `bus`, only `provides` and `core`, is a fantasy chip; the primitives implement it).
- The precise interplay of `amp adsr` release with held-note polyphony when a voice must be
  stolen.
- Sample root-key and rate conventions (do they follow SF2's, or something simpler?).
