# CHIP.md
# Circolate file formats (v2.1)

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

### Changes in v2.1

v2.1 is **additive**. v2 files still load. Five extensions:

- **EXT-1** `scope op` restored on fields (v1 had it, v2 dropped it).
- **EXT-2** Multi-register fields: a field may span more than one register with per-register
  bit ranges. Required by the YM2612's 11-bit `fnum`, split across `fnum_lo` and `fnum_hi`.
- **EXT-3** `carriers` block: per-algorithm carrier list for FM chips, used by the volume
  law (carrier TL is what velocity modulates).
- **EXT-4** `addr` on `reg` declarations. v2's bus formula implied `field.reg` was a base
  address; that was a name, not a number. Fixed: `reg` carries the numeric base.
- **EXT-5** Format and vocabulary additions:
  - `quirk <name> <value>` as a variant key (e.g. `quirk dac_ladder on`).
  - `pan.lr` capability added to the vocabulary.
  - `sample.pcm` gains a `mode` key; `streaming` is the YM2612 DAC (write-driven) mode.
  - `amp.envelope` promoted from reserved to active with a `rate_table` key (YM2612 EG).
  - `vgm op <opcode> <target>` syntax: target is `bus`, `port <n>`, or a field name.
  - Key-on encoding: `behavior key reg <field>` names the field; for `addr_data` chips the
    adapter knows the chip-specific encoding (YM2612 0x28 packs `op_mask << 4 | chan_sel`).

Also corrected: **no `port_offset`**. The YM2612 uses the same register addresses on both
ports; port is derived as `ch / chan_per_port`. The earlier YM2612 draft wrongly introduced
per-field port offsets.

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
clock     <int>
step      <expr>
out       unipolar | bipolar

variant <ident> [<key> <value> | quirk <name> <value>]...
default_variant <ident>

behavior
  key     none | reg <field>
  noteon  <step>...
  noteoff <step>...

voices
  <name> <kind>

provides
  <capability>
    <key> <value>...
    dependency <expr>                # optional

reg <name> voices|global <field-list> [addr <int>] width <n> reset <int>
field <name> reg <regspec>... (bits <lo>..<hi> | bits <n> | bit <n>) role <role> [scope <scope>]
enum <field> <name>=<int> ...

carriers                             # FM only; see §2.7
  alg <n>: <op>...

bus <protocol>                       # latch_data | addr_data; omit for fantasy chips
  ...
core
  ...
vgm                                  # only for real-hardware-matching chips
  header <name> <offset> [min_version <v>] [dual_bit <n>]
  op  <opcode> <target>
  op2 <opcode> <target>
  ...

<regspec> := <reg> ['[' <rlo> '..' <rhi> ']']
<role>    := pitch | param | volume | key
<scope>   := global | voice | op        # default: voice
<target>  := bus | port <n> | <field-name>
```

### 2.1 Voice declaration vs. capability sources

- `voices` says which voices exist and of what kind. The adapter allocates from this.
- `provides ... sources` says which of those voices can satisfy a capability. A voice may
  serve one capability; a capability may be served by several voices.

A fantasy chip may declare `voices` and `provides` without any `bus` or `reg` block; fields
are then parameters, not registers.

### 2.2 Register table (EXT-4)

`reg` declares a logical register with a numeric base address and width. The address is
required for `addr_data` chips; ignored by `latch_data` chips (which route via the bus
block, not numeric addresses).

```
reg tl      voices ch1 ch2 ch3 ch4 ch5 ch6  addr 0x40  width 7  reset 0
reg lfo     global                            addr 0x22  width 8  reset 0
```

Registers are shared between channels/operators; the address formula (see §2.3) derives the
actual byte from the base + channel offset + operator offset.

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
  latch other   = lo & mask(reg)
  data  div     = (reg & 0x0F) | (hi << 4)      # data writes high 6 bits
  data  other   = ignore

  on_write noise -> nz.reset
```

**`addr_data`** (most FM chips): write `(port, reg, value)`.

```
bus addr_data
  ports          <n>
  chan_per_port  <n>
  chan_stride    <n>
  op_offset      <op>:<offset>...
```

Address and port derivation, for a voice-scoped field with channel `ch` (0-based) and
operator `op` (0-based):

```
addr = reg.addr + (ch mod chan_per_port) * chan_stride + op_offset[op + 1]
port = ch / chan_per_port
```

For global fields:

```
addr = reg.addr
port = 0
```

No per-field port override exists: **the same register addresses are used on all ports**,
distinguished only by the `port` value. (The earlier YM2612 draft's `port_offset` was a
mistake.) If a chip has a global register only accessible via port 1, that's a future
extension; the current spec doesn't support it.

Special case — **key-on encoding**. A `behavior key reg <field>` names a field. For
`addr_data` chips, the adapter writes `(port, field.addr, value)` where the field's own
value packs chip-specific channel/operator selectors. The YM2612 uses `op_mask << 4 |
channel_selector` at register 0x28, with `channel_selector = ch if ch < 3 else ch + 1`.
This encoding is adapter knowledge, not data; it's declared by convention, not by the file.
Chips with a uniform key-on field (a bit per voice, etc.) don't need the special case.

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

Primitive semantics (implemented in code):

```
divider:  each step cnt -= 1; if cnt <= 0 { cnt = n; q ^= 1 }
          q is 0/1; rise is a one-step pulse on 0 -> 1.
          dc_below m: n < m holds q at 1.

lfsr:     on each `clock` pulse
            fb  = mode ? parity(reg & taps) : (reg & 1)
            reg = (reg >> 1) | (fb << (width - 1)); q = reg & 1
            reset: reg = 1 << (width - 1)

table:    precomputed lookup; expressions may use i (index)

pick:     pick(sel, v0, v1, ...) -> v[sel], 0-based

mix:      final per-step output expression
```

Tables live in the `.chip` file (§2.5), not in code. Correcting a table is a file edit.
FM routing (algorithm wiring) lives in code, not in the `.chip` file; only the carrier
list is data (§2.7).

### 2.5 Tables

`table level[16] = (i == 15) ? 0 : 10 ^ (-i / 10)` — the right-hand side is evaluated once
at load. For the SN76489 that's a one-liner; for the YM2612 the log-sine and exp tables are
larger but the same principle. If a table turns out wrong against a capture, edit the file,
not the code.

### 2.6 `provides` block

The capability vocabulary is in §5. Each `provides` entry advertises that this backend can
satisfy the capability, with constraints. `dependency <expr>` declares a runtime constraint
the adapter must honor. The SN76489's `noise_rate=3 consumes voice tone3` is one: a noise
note with rate 3 blocks `tone3` for its span.

`quirk <name> <value>` under `variant` records differences between silicon revisions. The
value is free-form; the core reads it as a named boolean or number. Quirk names are listed
in the chip file header, not in this spec.

### 2.7 `carriers` block (EXT-3)

For FM chips, each algorithm has a set of carrier operators. The volume law adds
velocity-derived attenuation to carrier TL only; modulators are never touched. The list
lives in the `.chip` file so the volume law and the adapter share it.

```
carriers
  alg <n>: <op> <op> ...
```

Operators are **1-based** in text (matching datasheets and BRIDGE.md §3.2). Algorithms are
**1-based** in text. A chip with N algorithms must list exactly N entries; the adapter
validates this.

The routing itself (which operator modulates which, in which order) lives in the core code,
not in this block. Only the *outputs* are declared here.

---

## 3. `.patch` grammar (sound)

First directive must be `patch 1`. A patch is portable: it says what a note does and what
it requires.

```
patch 1

name    <string>
author  <string>
license <string>
origin  <string>
parent  <name>

requires <capability>
  <key> <value>...

source  <kind>
  <key> <value>...

pitch
  transpose <num>
  detune    <num>
  glide     <num>

amp     <form>
  ...

filter  <kind>
  ...
mod     <kind>
  ...

native  <chip-model>
  <field> <value>...
```

### 3.1 `requires` and `source`

`requires` lists the capabilities the patch needs. The patch plays on a backend iff every
listed capability is advertised by the backend's `provides` block, with constraints
satisfied. Constraints are *binding* in strict mode and *advisory* in rendition mode.

`source` selects the sound source:

```
source tone.square

source noise.lfsr
  rate 0
  fb   white
```

Abstract keys (`rate`, `fb`) are mapped to native fields by the backend's `provides` block.

### 3.2 Envelope forms

```
amp adsr
  a 0.005
  d 0.120
  s 0.0
  r 0.040
  curve lin

amp steps
  values 0 2 4 6 8 10 13 15
  loop   4
  rate   60
```

Behavior at note-off: jump to the last step. The adapter converts `amp adsr` to whatever
the backend's `amp.*` capability provides; `amp steps` plays exactly if the backend matches,
or is resampled with a logged error bound.

### 3.3 `native` block

```
native sn76489
  div 200
  att 5
```

When rendering on a `.chip` whose `name` matches, the adapter prefers these values over
what the laws produce. On other backends, ignored.

### 3.4 Inheritance

`parent <name>` resolves against sibling patches in the same bank. Fields override; `requires`
unions. Cycles and unknown parents are errors.

---

## 4. `.bank` grammar (collection)

```
bank 1
name     <string>
author   <string>
license  <string>
chip     <chip-model> [variant <ident>] [clock <int>]

patch    <path>
alias    <name> <name>

map      program <n> -> <patch-name>
zone     <lo>..<hi>  <patch-name>

sample   <name> <path> [rate <hz>] [depth <bits>] [root <key>]
```

Patches are addressed by basename or `alias`. Duplicate names are errors.

---

## 5. Capability vocabulary (v2.1)

Each entry is a promise: "a backend that provides X can be asked to do Y in a standardized
way." Extending the vocabulary is a versioned change, not a per-file choice.

| Capability | Provides | Constraint keys | Status |
|---|---|---|---|
| `tone.square` | square-wave tone | — | **active** |
| `noise.lfsr` | LFSR noise source | `width` (list), `taps`, `fb` (`periodic` `white`) | **active** |
| `pitch.divider` | `n = clock/(32·f)` | `range`, `clock` | **active** |
| `pitch.fnum_block` | `fnum · (clock/144) / 2^(21−block)` | `block`, `fnum`, `clock` | **active** |
| `amp.attenuation` | attenuation steps | `steps`, `step_db`, `off` | **active** |
| `amp.envelope` | rate-table envelope (EXT-5) | `rate_table`, `resolution`, `key_scaling` | **active** |
| `pan.lr` | left/right pan (EXT-5) | `values` (`off l r lr`) | **active** |
| `sample.pcm` | PCM sample source | `rate` (Hz), `depth` (bits), `mode` (`streaming` — EXT-5) | **active** |
| `fm.operator` | FM operator chain | `operators` (int), `algorithms` (int), `feedback` (bool) | **active** |
| `tone.pulse` | pulse-wave tone | `duty` (list) | reserved |
| `tone.triangle` | triangle tone | — | reserved |
| `tone.saw` | sawtooth tone | — | reserved |
| `tone.sine` | sine tone | — | reserved |
| `tone.wavetable` | wavetable tone | `tables` (count) | reserved |
| `noise.periodic` | fixed periodic pulse | — | reserved |
| `pitch.linear` | `n = clock/f` | `range`, `clock` | reserved |
| `amp.linear` | linear amplitude | `resolution` (bits) | reserved |
| `filter.one_pole` | 1-pole lowpass | `cutoff` (Hz), `slope` (dB/oct) | reserved |
| `filter.biquad` | biquad filter | `type` (`lp` `hp` `bp`) | reserved |
| `mod.lfo` | LFO | `rate` (Hz), `target` | reserved |

The active set covers both chips specced so far: the SN76489 uses `tone.square`,
`noise.lfsr`, `pitch.divider`, `amp.attenuation`; the YM2612 uses `fm.operator`,
`pitch.fnum_block`, `amp.envelope`, `pan.lr`, `sample.pcm streaming`.

---

## 6. Voice allocation

A note takes a free voice of the right kind. `sample.pcm` on a voice that is also
`fm.operator` (the YM2612's ch6 DAC case) declares a `dependency` on the DAC enable field
so allocation knows the voice is unavailable for FM notes while the DAC is active.

If no voice is free, the bank's `steal` policy applies (default `oldest`).

`behavior noteon` / `noteoff` define write ordering.

---

## 7. Pipe interface (module boundary)

```
in :  Write { t, backend, port, reg, val }
      Note  { t, dur, patch, key, vel }
out:  PCM (f32, native rate) | Vec<Note> from lift(&[Write])
```

The adapter reads the backend's `.chip` (field table, laws, provides), the `.patch` (source,
envelope, requires, native), and produces register writes or parameter sets.

---

## 8. Validation (loader must reject)

- Missing or unsupported `chip` / `patch` / `bank` version.
- Field name not in the chip's field table; value out of range for its bits.
- Field name ends in a digit (BRIDGE.md §3.2).
- `scope op` used on a chip with no `ops` block (see §2.1).
- Multi-register field whose per-register bit widths don't sum to the declared total width.
- `requires` naming a capability not in the vocabulary.
- `source` kind not in the vocabulary, or not listed in `requires`.
- `amp steps` with `loop >= len(values)`.
- `amp adsr` with a negative time.
- `native <chip>` naming a chip not declared by the bank (warning, not error).
- `sample` in a bank whose chip doesn't advertise `sample.pcm`.
- `carriers`: count of algs doesn't match the chip's `algorithms` count; op index out of
  range for the chip's `operators` count.
- `reg` on an `addr_data` chip missing `addr`.
- Reserved: `chan` low bits == 3 on `addr_data` addressing.
- Cyclic `parent` references.
- Duplicate patch names in a bank.

---

## 9. Not decided yet

- Cross-backend patch translation (SN76489 patch played on YM2612): out of scope, and the
  `requires` block makes the incompatibility explicit.
- Whether global registers accessible only via non-zero ports need a `port <n>` override.
- The precise release behavior of `amp adsr` under voice stealing.
- Sample root-key and rate conventions (SF2's, or something simpler?).
- `quirk` values' type semantics (`on|off` vs numeric — currently free-form).
