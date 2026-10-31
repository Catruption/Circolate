//! `.bank` parser and model (CHIP.md v2.1 §4).

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Bank {
    pub name: String,
    pub author: String,
    pub license: String,
    pub chip: Option<ChipBinding>,
    pub patch_paths: Vec<PathBuf>,
    pub aliases: BTreeMap<String, String>,
    pub maps: Vec<MapEntry>,
    pub zones: Vec<ZoneEntry>,
    pub samples: Vec<SampleEntry>,
}

#[derive(Clone, Debug)]
pub struct ChipBinding {
    pub model: String,
    pub variant: Option<String>,
    pub clock: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct MapEntry   { pub program: u8, pub patch_name: String }
#[derive(Clone, Debug)]
pub struct ZoneEntry  { pub lo: u8, pub hi: u8, pub patch_name: String }
#[derive(Clone, Debug)]
pub struct SampleEntry {
    pub name: String,
    pub path: PathBuf,
    pub rate: Option<u32>,
    pub depth: Option<u8>,
    pub root: Option<u8>,
}

// ── lexical helpers ──────────────────────────────────────────────────────────

/// Strip a `#`-comment, but only when the `#` is outside a quoted string.
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

/// Split a directive line into tokens, honouring `"…"` quotes.
/// Quotes are stripped from the returned tokens.
fn tokenize(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut it = line.chars().peekable();
    while let Some(c) = it.next() {
        if c.is_whitespace() { continue; }
        if c == '"' {
            let mut s = String::new();
            while let Some(&nc) = it.peek() {
                if nc == '"' { it.next(); break; }
                s.push(it.next().unwrap());
            }
            out.push(s);
        } else {
            let mut s = String::new();
            s.push(c);
            while let Some(&nc) = it.peek() {
                if nc.is_whitespace() { break; }
                s.push(it.next().unwrap());
            }
            out.push(s);
        }
    }
    out
}

/// For `name` / `author` / `license` — take everything after the keyword,
/// then strip one layer of quotes if present.
fn rest_after<'a>(line: &'a str, kw: &str) -> String {
    let tail = line[kw.len()..].trim();
    if tail.len() >= 2 && tail.starts_with('"') && tail.ends_with('"') {
        tail[1..tail.len() - 1].to_string()
    } else {
        tail.to_string()
    }
}

fn parse_range(s: &str) -> Result<(u8, u8), String> {
    let (a, b) = s.split_once("..")
        .ok_or_else(|| format!("zone range needs `<lo>..<hi>`, got `{}`", s))?;
    let lo: u8 = a.parse().map_err(|e| format!("zone lo `{}`: {}", a, e))?;
    let hi: u8 = b.parse().map_err(|e| format!("zone hi `{}`: {}", b, e))?;
    if lo > hi { return Err(format!("zone range inverted: {}..{}", lo, hi)); }
    Ok((lo, hi))
}

/// Patch name = file stem of `patch <path>` (no `.patch` extension), per
/// §4 "addressed by the basename of their file path". Aliases share this
/// namespace and are `ident`s, so the extension must be stripped.
fn patch_name_from_path(p: &Path) -> String {
    p.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string()
}

// ── parser ──────────────────────────────────────────────────────────────────

pub fn parse(text: &str) -> Result<Bank, String> {
    let mut bank: Option<Bank> = None;
    let mut seen_patches: HashSet<String> = HashSet::new();

    for (idx, raw) in text.lines().enumerate() {
        let lineno = idx + 1;
        let line = strip_comment(raw).trim();
        if line.is_empty() { continue; }

        if bank.is_none() {
            let w: Vec<&str> = line.split_whitespace().collect();
            if w.as_slice() != ["bank", "1"] {
                return Err(format!(
                    "line {}: first directive must be `bank 1`, got `{}`",
                    lineno, line,
                ));
            }
            bank = Some(Bank {
                name: String::new(), author: String::new(), license: String::new(),
                chip: None, patch_paths: Vec::new(), aliases: BTreeMap::new(),
                maps: Vec::new(), zones: Vec::new(), samples: Vec::new(),
            });
            continue;
        }

        let b = bank.as_mut().unwrap();
        let head = line.split_whitespace().next().unwrap_or("");

        match head {
            "name"    => b.name    = rest_after(line, "name"),
            "author"  => b.author  = rest_after(line, "author"),
            "license" => b.license = rest_after(line, "license"),

            "chip" => {
                let w = tokenize(line);
                let model = w.get(1)
                    .ok_or_else(|| format!("line {}: bank chip missing model", lineno))?
                    .clone();
                let mut binding = ChipBinding { model, variant: None, clock: None };
                let mut i = 2;
                while i < w.len() {
                    match w[i].as_str() {
                        "variant" => {
                            binding.variant = Some(w.get(i + 1)
                                .ok_or_else(|| format!("line {}: chip variant missing value", lineno))?
                                .clone());
                            i += 2;
                        }
                        "clock" => {
                            binding.clock = Some(w.get(i + 1)
                                .ok_or_else(|| format!("line {}: chip clock missing value", lineno))?
                                .parse().map_err(|e| format!("line {}: chip clock: {}", lineno, e))?);
                            i += 2;
                        }
                        other => return Err(format!("line {}: unknown chip key `{}`", lineno, other)),
                    }
                }
                b.chip = Some(binding);
            }

            "patch" => {
                let p = PathBuf::from(rest_after(line, "patch"));
                let name = patch_name_from_path(&p);
                if name.is_empty() {
                    return Err(format!("line {}: patch path has no file stem", lineno));
                }
                if !seen_patches.insert(name.clone()) {
                    return Err(format!("line {}: duplicate patch name `{}`", lineno, name));
                }
                b.patch_paths.push(p);
            }

            "alias" => {
                let w = tokenize(line);
                if w.len() != 3 {
                    return Err(format!(
                        "line {}: alias needs `<from> <to>`, got `{}`", lineno, line,
                    ));
                }
                b.aliases.insert(w[1].clone(), w[2].clone());
            }

            "map" => {
                // map program <n> -> <patch-name>
                let w = tokenize(line);
                if w.len() != 5 || w[1] != "program" || w[3] != "->" {
                    return Err(format!(
                        "line {}: map syntax: `map program <n> -> <patch>`, got `{}`",
                        lineno, line,
                    ));
                }
                let program: u8 = w[2].parse()
                    .map_err(|e| format!("line {}: map program `{}`: {}", lineno, w[2], e))?;
                b.maps.push(MapEntry { program, patch_name: w[4].clone() });
            }

            "zone" => {
                // zone <lo>..<hi> <patch-name>
                let w = tokenize(line);
                if w.len() != 3 {
                    return Err(format!(
                        "line {}: zone syntax: `zone <lo>..<hi> <patch>`, got `{}`",
                        lineno, line,
                    ));
                }
                let (lo, hi) = parse_range(&w[1])
                    .map_err(|e| format!("line {}: {}", lineno, e))?;
                b.zones.push(ZoneEntry { lo, hi, patch_name: w[2].clone() });
            }

            "sample" => {
                // sample <name> <path> [rate <hz>] [depth <bits>] [root <key>]
                let w = tokenize(line);
                if w.len() < 3 {
                    return Err(format!(
                        "line {}: sample syntax: `sample <name> <path> [rate <hz>] [depth <bits>] [root <key>]`, got `{}`",
                        lineno, line,
                    ));
                }
                let mut s = SampleEntry {
                    name: w[1].clone(),
                    path: PathBuf::from(&w[2]),
                    rate: None, depth: None, root: None,
                };
                let mut i = 3;
                while i < w.len() {
                    match w[i].as_str() {
                        "rate" => {
                            s.rate = Some(w.get(i + 1)
                                .ok_or_else(|| format!("line {}: sample rate missing value", lineno))?
                                .parse().map_err(|e| format!("line {}: sample rate: {}", lineno, e))?);
                            i += 2;
                        }
                        "depth" => {
                            s.depth = Some(w.get(i + 1)
                                .ok_or_else(|| format!("line {}: sample depth missing value", lineno))?
                                .parse().map_err(|e| format!("line {}: sample depth: {}", lineno, e))?);
                            i += 2;
                        }
                        "root" => {
                            s.root = Some(w.get(i + 1)
                                .ok_or_else(|| format!("line {}: sample root missing value", lineno))?
                                .parse().map_err(|e| format!("line {}: sample root: {}", lineno, e))?);
                            i += 2;
                        }
                        other => return Err(format!("line {}: unknown sample key `{}`", lineno, other)),
                    }
                }
                b.samples.push(s);
            }

            other => return Err(format!("line {}: unknown directive `{}`", lineno, other)),
        }
    }

    bank.ok_or_else(|| "empty bank file: missing `bank 1` directive".to_string())
}

// ── queries ─────────────────────────────────────────────────────────────────

impl Bank {
    /// Follow `alias` chains (cycle-bounded) to a non-aliased name.
    pub fn resolve_alias<'a>(&'a self, name: &'a str) -> &'a str {
        let mut cur: &'a str = name;
        for _ in 0..64 {
            match self.aliases.get(cur) {
                Some(next) => cur = next.as_str(),
                None => return cur,
            }
        }
        cur
    }

    /// First `map program` entry for `program` (§4 — spec doesn't define
    /// behaviour for duplicate programs; first-declared wins).
    pub fn patch_for_program(&self, program: u8) -> Option<&str> {
        self.maps.iter().find(|m| m.program == program).map(|m| m.patch_name.as_str())
    }

    /// First declared `zone` containing `note`. Spec §4 says "lowest matching
    /// zone wins" — I read that as first-declared (declaration order = priority).
    /// If "lowest" instead means lowest `lo`, sort by `lo` here.
    pub fn patch_for_zone(&self, note: u8) -> Option<&str> {
        self.zones.iter()
            .find(|z| z.lo <= note && note <= z.hi)
            .map(|z| z.patch_name.as_str())
    }
}

/// Read + parse, rebasing relative `patch` / `sample` paths onto the bank's
/// directory. Cross-file validation (§8 — e.g. samples vs. `sample.pcm`)
/// is not done here; see `Bank::validate_against`.
pub fn load(path: impl AsRef<Path>) -> Result<Bank, String> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("read {}: {}", path.display(), e))?;
    let mut b = parse(&text)?;
    let base = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    for p in &mut b.patch_paths {
        if p.is_relative() { *p = base.join(&*p); }
    }
    for s in &mut b.samples {
        if s.path.is_relative() { s.path = base.join(&s.path); }
    }
    Ok(b)
}