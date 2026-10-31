//! `.chip` parser and model (CHIP.md v2.1 §2).

use std::collections::BTreeMap;

#[derive(Clone, Debug, Default)]
pub struct Chip {
    pub name: String,
    pub title: String,
    pub family: String,
    pub clock: u64,
    pub step_expr: String,       // e.g. "clock / 16"
    pub out: Out,
    pub variants: BTreeMap<String, Variant>,
    pub default_variant: Option<String>,
    pub behavior: Behavior,
    pub voices: Vec<Voice>,
    pub provides: BTreeMap<String, Capability>,
    pub registers: BTreeMap<String, Register>,
    pub fields: BTreeMap<String, Field>,
    pub enums: BTreeMap<String, BTreeMap<String, i32>>,
    pub carriers: Option<Carriers>,
    pub bus: Option<Bus>,
    pub core: Option<Core>,
    pub vgm: Option<Vgm>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Out { #[default] Unipolar, Bipolar }

#[derive(Clone, Debug, Default)]
pub struct Variant {
    /// Named quirk or key/value pairs, e.g. `taps=0x0009`, `width=16`.
    pub keys: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default)]
pub struct Behavior {
    /// `key reg <field>` → Some(field-name); `key none` → None.
    pub key_field: Option<String>,
    pub noteon: Vec<String>,
    pub noteoff: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Voice { pub name: String, pub kind: String }

#[derive(Clone, Debug, Default)]
pub struct Capability {
    /// Ordered KV pairs. `sources`, `voices`, `width`, `taps`, `fb` etc.
    pub keys: BTreeMap<String, CapValue>,
    pub dependency: Option<String>,
}

#[derive(Clone, Debug)]
pub enum CapValue {
    /// Space-separated tokens: `sources tone1 tone2 tone3`.
    List(Vec<String>),
    /// Everything after the key, kept raw: `law clock / (32 * n)`.
    Raw(String),
}

#[derive(Clone, Debug, Default)]
pub struct Register {
    pub name: String,
    pub scope: RegScope,          // voices | global
    pub voices: Vec<String>,
    pub addr: Option<u32>,
    pub width: u8,
    pub reset: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RegScope { #[default] Voices, Global }

#[derive(Clone, Debug, Default)]
pub struct Field {
    pub name: String,
    /// One entry for v2.1 single-register; N for EXT-2 multi-register.
    pub parts: Vec<FieldPart>,
    pub role: Role,
    pub scope: Scope,
    /// True if declared as `field X reg Y bits 0..7` (contiguous slot, for enums).
    pub enum_hint: Option<String>,
}

#[derive(Clone, Debug)]
pub struct FieldPart {
    pub reg: String,
    pub bits_lo: u8,
    pub bits_hi: u8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Role { #[default] Param, Pitch, Volume, Key }

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scope { #[default] Voice, Global, Op }

#[derive(Clone, Debug, Default)]
pub struct Carriers {
    /// alg number (1-based) → operator numbers (1-based).
    pub algs: BTreeMap<u8, Vec<u8>>,
}

#[derive(Clone, Debug, Default)]
pub struct Bus {
    pub protocol: Protocol,
    pub ports: u8,               // addr_data
    pub chan_per_port: u8,       // addr_data
    pub chan_stride: u8,         // addr_data
    pub op_offset: BTreeMap<u8, u8>,  // addr_data
    /// Opaque per-chip bus DSL lines. Adapter interprets them per-protocol.
    pub raw: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Protocol { #[default] LatchData, AddrData }

#[derive(Clone, Debug, Default)]
pub struct Core {
    /// `divider <id> n=<expr> [dc_below=<n>]`
    pub dividers: BTreeMap<String, Divider>,
    /// `lfsr <id> clock=<id>.rise taps=<expr> width=<expr> mode=<expr>`
    pub lfsrs: BTreeMap<String, Lfsr>,
    /// `table <id>[<len>] = <expr>`
    pub tables: BTreeMap<String, Table>,
    /// `mix <expr>` (may span multiple lines).
    pub mix: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Divider { pub n_expr: String, pub dc_below: Option<u32> }
#[derive(Clone, Debug, Default)]
pub struct Lfsr {
    pub clock: String,  // "<id>.rise"
    pub taps: String,
    pub width: String,
    pub mode: String,
}
#[derive(Clone, Debug, Default)]
pub struct Table { pub len: u32, pub expr: String }

#[derive(Clone, Debug, Default)]
pub struct Vgm {
    pub headers: BTreeMap<String, String>,
    pub ops: BTreeMap<u8, OpTarget>,
}

#[derive(Clone, Debug, Default)]
pub enum OpTarget { #[default] Bus, Port(u8) }

// ── lexing ────────────────────────────────────────────────────────────────

fn strip_comment(line: &str) -> &str {
    let mut in_str = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_str = !in_str,
            '#' if !in_str => return &line[..i],
            _ => {}
        }
    }
    line
}

fn strip_quotes(s: &str) -> String {
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// Split `head rest…` — head is the first whitespace-delimited token.
fn split_head(line: &str) -> (&str, &str) {
    match line.find(char::is_whitespace) {
        Some(i) => (&line[..i], &line[i..]),
        None => (line.trim(), ""),
    }
}

fn indent_of(line: &str) -> usize {
    line.chars().take_while(|c| c.is_whitespace()).count()
}

/// Consume all consecutive lines with indent > `parent_indent` (blank and
/// comment-only lines pass through). Returns the raw slice; caller re-parses.
fn take_block<'a, I>(lines: &mut std::iter::Peekable<I>) -> Vec<(usize, String)>
where
I: Iterator<Item = (usize, &'a str)>,
{
    let mut out = Vec::new();
    while let Some(&(idx, raw)) = lines.peek() {
        let stripped = strip_comment(raw);
        if stripped.trim().is_empty() { lines.next(); continue; }
        if indent_of(raw) == 0 { break; }
        out.push((idx, stripped.to_string()));
        lines.next();
    }
    out
}

fn parse_int(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix("0x") {
        u64::from_str_radix(h, 16).map_err(|e| format!("int `{}`: {}", s, e))
    } else if let Some(b) = s.strip_prefix("0b") {
        u64::from_str_radix(b, 2).map_err(|e| format!("int `{}`: {}", s, e))
    } else {
        s.parse().map_err(|e| format!("int `{}`: {}", s, e))
    }
}

/// Split `key=value key=value …` respecting () [] depth so an expression
/// like `pick(a, 16, 32)` stays one token.
fn split_kv_pairs(s: &str) -> Vec<(String, String)> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        while i < chars.len() && chars[i].is_whitespace() { i += 1; }
        if i >= chars.len() { break; }
        let kstart = i;
        while i < chars.len() && chars[i] != '=' && !chars[i].is_whitespace() { i += 1; }
        let key: String = chars[kstart..i].iter().collect();
        if i >= chars.len() || chars[i] != '=' {
            out.push((key, String::new()));
            continue;
        }
        i += 1;
        let vstart = i;
        let mut depth: i32 = 0;
        while i < chars.len() {
            match chars[i] {
                '(' | '[' => depth += 1,
                ')' | ']' => depth -= 1,
                c if c.is_whitespace() && depth == 0 => break,
                _ => {}
            }
            i += 1;
        }
        let value: String = chars[vstart..i].iter().collect();
        out.push((key, value));
    }
    out
}

// ── entry point ───────────────────────────────────────────────────────────

pub fn parse(text: &str) -> Result<Chip, String> {
    let mut chip = Chip::default();
    let mut lines = text.lines().enumerate().peekable();
    let mut seen_header = false;

    while let Some((idx, raw)) = lines.next() {
        let lineno = idx + 1;
        let line = strip_comment(raw);
        if line.trim().is_empty() { continue; }
        if indent_of(raw) > 0 {
            return Err(format!("line {}: unexpected indent at column 0", lineno));
        }
        let (head, rest) = split_head(line);
        let rest = rest.trim();

        if !seen_header {
            if head != "chip" || rest != "2" {
                return Err(format!(
                    "line {}: first directive must be `chip 2`, got `{}`",
                    lineno, line,
                ));
            }
            seen_header = true;
            continue;
        }

        match head {
            "name"    => chip.name = strip_quotes(rest),
            "title"   => chip.title = strip_quotes(rest),
            "family"  => chip.family = rest.to_string(),
            "clock"   => chip.clock = parse_int(rest)?,
            "step"    => chip.step_expr = rest.to_string(),
            "out" => match rest {
                "unipolar" => chip.out = Out::Unipolar,
                "bipolar"  => chip.out = Out::Bipolar,
                other => return Err(format!("line {}: out must be unipolar|bipolar, got `{}`", lineno, other)),
            },
            "variant"         => parse_variant(rest, &mut chip)?,
            "default_variant" => chip.default_variant = Some(rest.to_string()),
            "behavior" => {
                let block = take_block(&mut lines);
                parse_behavior(&block, &mut chip)?;
            }
            "voices" => {
                let block = take_block(&mut lines);
                parse_voices(&block, &mut chip)?;
            }
            "provides" => {
                let block = take_block(&mut lines);
                parse_provides(&block, &mut chip)?;
            }
            "reg"   => parse_reg(rest, &mut chip)?,
            "field" => parse_field(rest, &mut chip)?,
            "enum"  => parse_enum(rest, &mut chip)?,
            "carriers" => {
                let block = take_block(&mut lines);
                parse_carriers(&block, &mut chip)?;
            }
            "bus" => {
                let proto = rest;
                let block = take_block(&mut lines);
                parse_bus(proto, &block, &mut chip)?;
            }
            "core" => {
                let block = take_block(&mut lines);
                parse_core(&block, &mut chip)?;
            }
            "vgm" => {
                let block = take_block(&mut lines);
                parse_vgm(&block, &mut chip)?;
            }
            other => return Err(format!("line {}: unknown directive `{}`", lineno, other)),
        }
    }

    if !seen_header {
        return Err("empty chip file: missing `chip 2`".to_string());
    }
    Ok(chip)
}

// ── directive parsers ─────────────────────────────────────────────────────

fn parse_variant(rest: &str, chip: &mut Chip) -> Result<(), String> {
    // variant <ident> [<key> <value> | quirk <name> <value>]...
    let mut it = rest.split_whitespace();
    let name = it.next().ok_or("variant missing name")?.to_string();
    let mut keys = BTreeMap::new();
    let toks: Vec<&str> = it.collect();
    let mut i = 0;
    while i < toks.len() {
        let k = toks[i];
        if k == "quirk" {
            let qname = toks.get(i + 1).ok_or("quirk missing name")?;
            let qval  = toks.get(i + 2).ok_or("quirk missing value")?;
            keys.insert(format!("quirk.{}", qname), qval.to_string());
            i += 3;
        } else {
            let v = toks.get(i + 1).ok_or_else(|| format!("variant key `{}` missing value", k))?;
            keys.insert(k.to_string(), v.to_string());
            i += 2;
        }
    }
    chip.variants.insert(name, Variant { keys });
    Ok(())
}

fn parse_behavior(block: &[(usize, String)], chip: &mut Chip) -> Result<(), String> {
    for (idx, raw) in block {
        let lineno = idx + 1;
        let (k, rest) = split_head(raw.trim());
        let rest = rest.trim();
        match k {
            "key" => chip.behavior.key_field = match rest {
                "none" => None,
                r => {
                    let f = r.strip_prefix("reg ").ok_or_else(|| {
                        format!("line {}: key must be `none` or `reg <field>`", lineno)
                    })?;
                    Some(f.to_string())
                }
            },
            "noteon"  => chip.behavior.noteon  = rest.split_whitespace().map(String::from).collect(),
            "noteoff" => chip.behavior.noteoff = rest.split_whitespace().map(String::from).collect(),
            other => return Err(format!("line {}: unknown behavior key `{}`", lineno, other)),
        }
    }
    Ok(())
}

fn parse_voices(block: &[(usize, String)], chip: &mut Chip) -> Result<(), String> {
    for (idx, raw) in block {
        let (name, kind) = split_head(raw.trim());
        if kind.trim().is_empty() {
            return Err(format!("line {}: voices entry needs `<name> <kind>`", idx + 1));
        }
        chip.voices.push(Voice { name: name.into(), kind: kind.trim().into() });
    }
    Ok(())
}

fn parse_provides(block: &[(usize, String)], chip: &mut Chip) -> Result<(), String> {
    // Two-level indented block. `provides` children are capability names;
    // their children are KV pairs / `dependency`.
    let base = block.iter().map(|(_, r)| indent_of(r)).min().unwrap_or(2);
    let mut current: Option<String> = None;
    let mut current_cap = Capability::default();

    for (idx, raw) in block {
        let _lineno = idx + 1;
        let stripped = raw.trim();
        if stripped.is_empty() { continue; }
        let depth = indent_of(raw);

        if depth == base {
            // Flush previous capability
            if let Some(name) = current.take() {
                chip.provides.insert(name, std::mem::take(&mut current_cap));
            }
            current = Some(stripped.to_string());
        } else {
            // Child line — belongs to current capability
            let (k, rest) = split_head(stripped);
            let rest = rest.trim();
            match k {
                "dependency" => current_cap.dependency = Some(rest.to_string()),
                "sources" | "voices" | "width" | "taps" | "fb" | "values"
                if k == "sources" || k == "voices" || k == "values" =>
                {
                    current_cap.keys.insert(
                        k.into(),
                                            CapValue::List(rest.split_whitespace().map(String::from).collect()),
                    );
                }
                other => {
                    current_cap.keys.insert(other.into(), CapValue::Raw(rest.to_string()));
                }
            }
        }
    }
    if let Some(name) = current.take() {
        chip.provides.insert(name, current_cap);
    }
    Ok(())
}

fn parse_reg(rest: &str, chip: &mut Chip) -> Result<(), String> {
    // reg <name> voices|global <voice-list> [addr <int>] width <n> reset <int>
    let toks: Vec<&str> = rest.split_whitespace().collect();
    let name = toks.get(0).ok_or("reg missing name")?.to_string();
    let scope = match toks.get(1) {
        Some(&"voices") => RegScope::Voices,
        Some(&"global") => RegScope::Global,
        _ => return Err(format!("reg `{}`: scope must be voices|global", name)),
    };
    let mut i = 2;
    let mut voices = Vec::new();
    while let Some(&t) = toks.get(i) {
        if matches!(t, "addr" | "width" | "reset") { break; }
        voices.push(t.to_string());
        i += 1;
    }
    let mut addr = None;
    let mut width = 8;
    let mut reset = 0;
    while i < toks.len() {
        match toks[i] {
            "addr"  => { addr = Some(parse_int(toks.get(i + 1).ok_or("reg addr missing value")?)? as u32); i += 2; }
            "width" => { width = parse_int(toks.get(i + 1).ok_or("reg width missing value")?)? as u8; i += 2; }
            "reset" => { reset = parse_int(toks.get(i + 1).ok_or("reg reset missing value")?)? as u32; i += 2; }
            other => return Err(format!("reg `{}`: unknown key `{}`", name, other)),
        }
    }
    if scope == RegScope::Voices && addr.is_none() {
        // Allowed: latch_data chips don't use numeric addresses.
    }
    chip.registers.insert(name.clone(), Register { name, scope, voices, addr, width, reset });
    Ok(())
}

fn parse_field(rest: &str, chip: &mut Chip) -> Result<(), String> {
    // Single:  field <name> reg <reg> bits <lo>..<hi> role <r> [scope <s>]
    //          field <name> reg <reg> bit  <n>      role <r> [scope <s>]
    // Multi:   field <name> reg <reg> bits <lo>..<hi> reg <reg> bits <lo>..<hi> role <r>
    let toks: Vec<&str> = rest.split_whitespace().collect();
    let name = toks.get(0).ok_or("field missing name")?.to_string();
    let mut i = 1;
    let mut parts = Vec::new();
    let mut role = Role::Param;
    let mut scope = Scope::Voice;

    while i < toks.len() {
        match toks[i] {
            "reg" => {
                let reg = toks.get(i + 1).ok_or("field reg missing name")?.to_string();
                let spec = toks.get(i + 2).ok_or("field reg missing bits|bit")?;
                let (lo, hi) = match *spec {
                    "bits" => {
                        let r = toks.get(i + 3).ok_or("field bits missing range")?;
                        let (lo, hi) = r.split_once("..").ok_or("field bits expects lo..hi")?;
                        (lo.parse::<u8>().map_err(|e| format!("bits lo: {}", e))?,
                         hi.parse::<u8>().map_err(|e| format!("bits hi: {}", e))?)
                    }
                    "bit" => {
                        let b: u8 = toks.get(i + 3).ok_or("field bit missing n")?
                        .parse().map_err(|e| format!("bit: {}", e))?;
                        (b, b)
                    }
                    other => return Err(format!("field `{}`: unknown `{}` after reg", name, other)),
                };
                parts.push(FieldPart { reg, bits_lo: lo, bits_hi: hi });
                i += 4;
            }
            "role" => {
                role = match toks.get(i + 1).copied() {
                    Some("pitch")  => Role::Pitch,
                    Some("param")  => Role::Param,
                    Some("volume") => Role::Volume,
                    Some("key")    => Role::Key,
                    Some(o) => return Err(format!("field `{}`: unknown role `{}`", name, o)),
                    None => return Err(format!("field `{}`: role missing", name)),
                };
                i += 2;
            }
            "scope" => {
                scope = match toks.get(i + 1).copied() {
                    Some("voice")  => Scope::Voice,
                    Some("global") => Scope::Global,
                    Some("op")     => Scope::Op,
                    Some(o) => return Err(format!("field `{}`: unknown scope `{}`", name, o)),
                    None => return Err(format!("field `{}`: scope missing", name)),
                };
                i += 2;
            }
            other => return Err(format!("field `{}`: unknown key `{}`", name, other)),
        }
    }

    if parts.is_empty() {
        return Err(format!("field `{}`: no reg/bits", name));
    }
    chip.fields.insert(name.clone(), Field {
        name, parts, role, scope, enum_hint: None,
    });
    Ok(())
}

fn parse_enum(rest: &str, chip: &mut Chip) -> Result<(), String> {
    // enum <field> <name>=<int> ...
    let toks: Vec<&str> = rest.split_whitespace().collect();
    let field = toks.get(0).ok_or("enum missing field name")?.to_string();
    let mut map = BTreeMap::new();
    for t in &toks[1..] {
        let (k, v) = t.split_once('=').ok_or_else(|| format!("enum entry `{}`: expected name=val", t))?;
        let n: i32 = v.parse().map_err(|e| format!("enum val `{}`: {}", v, e))?;
        map.insert(k.to_string(), n);
    }
    chip.enums.insert(field, map);
    Ok(())
}

fn parse_carriers(block: &[(usize, String)], chip: &mut Chip) -> Result<(), String> {
    let mut carriers = Carriers::default();
    for (idx, raw) in block {
        let lineno = idx + 1;
        let line = raw.trim();
        let (key, rest) = line.split_once(':')
        .ok_or_else(|| format!("line {}: carriers expects `alg <n>: <ops…>`", lineno))?;
        let alg: u8 = key.trim().strip_prefix("alg ")
        .ok_or_else(|| format!("line {}: carriers key must be `alg <n>`", lineno))?
        .trim().parse().map_err(|e| format!("line {}: alg number: {}", lineno, e))?;
        let ops: Vec<u8> = rest.split_whitespace()
        .map(|s| s.parse::<u8>().map_err(|e| format!("line {}: op number: {}", lineno, e)))
        .collect::<Result<_, _>>()?;
        carriers.algs.insert(alg, ops);
    }
    chip.carriers = Some(carriers);
    Ok(())
}

fn parse_bus(proto: &str, block: &[(usize, String)], chip: &mut Chip) -> Result<(), String> {
    let mut bus = Bus::default();
    bus.protocol = match proto {
        "latch_data" => Protocol::LatchData,
        "addr_data"  => Protocol::AddrData,
        other => return Err(format!("bus protocol `{}` not supported", other)),
    };

    for (idx, raw) in block {
        let lineno = idx + 1;
        let line = raw.trim();
        let (k, rest) = split_head(line);
        let rest = rest.trim();
        match k {
            "ports"          => bus.ports = parse_int(rest)? as u8,
            "chan_per_port"  => bus.chan_per_port = parse_int(rest)? as u8,
            "chan_stride"    => bus.chan_stride = parse_int(rest)? as u8,
            "op_offset" => {
                for entry in rest.split_whitespace() {
                    let (op, off) = entry.split_once(':').ok_or_else(|| format!(
                        "line {}: op_offset entry `{}` needs `<op>:<offset>`", lineno, entry,
                    ))?;
                    let op: u8 = op.parse().map_err(|e| format!("line {}: op `{}`: {}", lineno, op, e))?;
                    let off = parse_int(off)? as u8;
                    bus.op_offset.insert(op, off);
                }
            }
            // latch_data protocol DSL: preserve verbatim, interpreted by adapter.
            "flag" | "chan" | "type" | "lo" | "hi"
            | "route" | "latch" | "data" | "on_write" => {
                bus.raw.push(line.to_string());
            }
            other => return Err(format!("line {}: unknown bus key `{}`", lineno, other)),
        }
    }
    chip.bus = Some(bus);
    Ok(())
}

fn parse_core(block: &[(usize, String)], chip: &mut Chip) -> Result<(), String> {
    let mut core = Core::default();
    let mut pending_mix = String::new();
    let mut in_mix = false;

    for (idx, raw) in block {
        let lineno = idx + 1;
        let line = raw.trim();
        if line.is_empty() { continue; }
        let (head, rest) = split_head(line);
        let rest = rest.trim();

        if in_mix && !matches!(head, "divider" | "lfsr" | "table" | "mix") {
            pending_mix.push(' ');
            pending_mix.push_str(line);
            continue;
        }

        match head {
            "divider" => {
                let (id, params) = rest.split_once(char::is_whitespace)
                .ok_or_else(|| format!("line {}: divider needs `<id> n=<expr>`", lineno))?;
                let mut div = Divider::default();
                for (k, v) in split_kv_pairs(params) {
                    match k.as_str() {
                        "n"        => div.n_expr = v,
                        "dc_below" => div.dc_below = Some(parse_int(&v)? as u32),
                        other      => return Err(format!("line {}: divider unknown key `{}`", lineno, other)),
                    }
                }
                core.dividers.insert(id.to_string(), div);
            }
            "lfsr" => {
                let (id, params) = rest.split_once(char::is_whitespace)
                .ok_or_else(|| format!("line {}: lfsr needs `<id> k=v…`", lineno))?;
                let mut lfsr = Lfsr::default();
                for (k, v) in split_kv_pairs(params) {
                    match k.as_str() {
                        "clock" => lfsr.clock = v,
                        "taps"  => lfsr.taps  = v,
                        "width" => lfsr.width = v,
                        "mode"  => lfsr.mode  = v,
                        other   => return Err(format!("line {}: lfsr unknown key `{}`", lineno, other)),
                    }
                }
                core.lfsrs.insert(id.to_string(), lfsr);
            }
            "table" => {
                let (lhs, rhs) = rest.split_once('=').ok_or_else(|| format!(
                    "line {}: table needs `<id>[<len>] = <expr>`", lineno,
                ))?;
                let lhs = lhs.trim();
                let (id, len_s) = lhs.split_once('[').ok_or_else(|| format!(
                    "line {}: table name needs `[<len>]`", lineno,
                ))?;
                let len: u32 = len_s.trim_end_matches(']')
                .parse().map_err(|e| format!("line {}: table len: {}", lineno, e))?;
                core.tables.insert(id.to_string(), Table { len, expr: rhs.trim().to_string() });
            }
            "mix" => {
                in_mix = true;
                pending_mix = rest.to_string();
            }
            other => return Err(format!("line {}: unknown core primitive `{}`", lineno, other)),
        }
    }

    if !pending_mix.is_empty() {
        core.mix = Some(pending_mix);
    }
    chip.core = Some(core);
    Ok(())
}

fn parse_vgm(block: &[(usize, String)], chip: &mut Chip) -> Result<(), String> {
    let mut vgm = Vgm::default();
    for (idx, raw) in block {
        let lineno = idx + 1;
        let line = raw.trim();
        let (head, rest) = split_head(line);
        let rest = rest.trim();
        match head {
            "header" => {
                // header <k> <v> <k> <v> …
                let toks: Vec<&str> = rest.split_whitespace().collect();
                let mut i = 0;
                while i + 1 < toks.len() {
                    vgm.headers.insert(toks[i].to_string(), toks[i + 1].to_string());
                    i += 2;
                }
            }
            "op" | "op2" => {
                // op <opcode> bus | port <n> | <field-name>
                let mut it = rest.split_whitespace();
                let opcode_s = it.next().ok_or_else(|| format!("line {}: op missing opcode", lineno))?;
                let opcode: u8 = parse_int(opcode_s)? as u8;
                let target = match it.next() {
                    Some("bus")  => OpTarget::Bus,
                    Some("port") => {
                        let n: u8 = it.next().ok_or_else(|| format!(
                            "line {}: port needs a number", lineno,
                        ))?.parse().map_err(|e| format!("line {}: port: {}", lineno, e))?;
                        OpTarget::Port(n)
                    }
                    Some(other) => return Err(format!(
                        "line {}: vgm op target `{}` not supported (must be bus|port <n>)", lineno, other,
                    )),
                    None => return Err(format!("line {}: vgm op missing target", lineno)),
                };
                vgm.ops.insert(opcode, target);
            }
            other => return Err(format!("line {}: unknown vgm key `{}`", lineno, other)),
        }
    }
    chip.vgm = Some(vgm);
    Ok(())
}

// ── queries ───────────────────────────────────────────────────────────────

impl Chip {
    pub fn field(&self, name: &str) -> Option<&Field> { self.fields.get(name) }
    pub fn reg(&self, name: &str)   -> Option<&Register> { self.registers.get(name) }
    pub fn provide(&self, cap: &str) -> Option<&Capability> { self.provides.get(cap) }
    pub fn variant(&self) -> Option<&Variant> {
        self.default_variant.as_deref().and_then(|n| self.variants.get(n))
    }
}
