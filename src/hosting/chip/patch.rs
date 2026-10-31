//! `.patch` parser and model (CHIP.md v2.1 §3).

use std::collections::BTreeMap;

use super::expr::Expr;

// ───────────────────────── Model ─────────────────────────

#[derive(Clone, Debug)]
pub struct Patch {
    pub name: String,
    pub author: String,
    pub license: String,
    pub origin: String,
    pub parent: Option<String>,
    pub requires: Vec<Requirement>,
    pub source: SourceDecl,
    pub pitch: PitchDecl,
    pub amp: Envelope,
    pub filter: Option<FilterDecl>,
    pub r#mod: Option<ModDecl>,
    pub native: BTreeMap<String, BTreeMap<String, i32>>,
}

#[derive(Clone, Debug)]
pub struct Requirement {
    pub capability: String,
    pub constraints: BTreeMap<String, String>,
}

#[derive(Clone, Debug)]
pub struct SourceDecl {
    pub kind: String,
    pub keys: BTreeMap<String, String>,
}

#[derive(Clone, Debug)]
pub struct PitchDecl {
    pub transpose: f32,
    pub detune: f32,
    pub glide: f32,
}

impl Default for PitchDecl {
    fn default() -> Self { PitchDecl { transpose: 0.0, detune: 0.0, glide: 0.0 } }
}

#[derive(Clone, Debug)]
pub enum Envelope {
    /// No `amp` block in the patch. Adapter falls back to `native.att`
    /// or the velocity-derived default.
    None,
    Adsr {
        a: f32, d: f32, s: f32, r: f32,
        curve: CurveKind,
    },
    Steps {
        values: Vec<i32>,
        loop_at: Option<usize>,
        rate: f32,
    },
}

impl Envelope {
    pub fn rate(&self) -> f32 {
        match self {
            Envelope::None => 0.0,
            Envelope::Adsr { .. } => 60.0,
            Envelope::Steps { rate, .. } => *rate,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CurveKind { Lin, Exp, Log }

#[derive(Clone, Debug)]
pub struct FilterDecl {
    pub kind: String,
    pub keys: BTreeMap<String, String>,
}

#[derive(Clone, Debug)]
pub struct ModDecl {
    pub kind: String,
    pub keys: BTreeMap<String, String>,
}

// ───────────────────────── Parser ─────────────────────────

struct RawBlock {
    directive: String,
    inner: Vec<(usize, String)>,
}

fn strip_comment(line: &str) -> &str {
    match line.find('#') { Some(i) => &line[..i], None => line }
}

fn collect_blocks(text: &str) -> Vec<RawBlock> {
    let mut blocks: Vec<RawBlock> = Vec::new();
    let mut cur: Option<RawBlock> = None;
    for raw in text.lines() {
        let no_comment = strip_comment(raw);
        if no_comment.trim().is_empty() { continue; }
        let indent = no_comment.chars().take_while(|c| *c == ' ' || *c == '\t').count();
        let body = no_comment.trim();
        if indent == 0 {
            if let Some(b) = cur.take() { blocks.push(b); }
            cur = Some(RawBlock { directive: body.to_string(), inner: Vec::new() });
        } else if let Some(ref mut b) = cur {
            b.inner.push((indent, body.to_string()));
        }
    }
    if let Some(b) = cur { blocks.push(b); }
    blocks
}

fn rest_after<'a>(line: &'a str, kw: &str) -> String {
    line[kw.len()..].trim().to_string()
}

fn strip_quotes(s: String) -> String {
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        s[1..s.len() - 1].to_string()
    } else { s }
}

pub fn parse(text: &str) -> Result<Patch, String> {
    let blocks = collect_blocks(text);
    let mut first = true;
    let mut p: Option<Patch> = None;

    for b in blocks {
        if first {
            if b.directive.trim() != "patch 1" {
                return Err(format!("first directive must be `patch 1`, got `{}`", b.directive));
            }
            p = Some(Patch {
                name: String::new(), author: String::new(),
                license: String::new(), origin: "handwritten".into(),
                parent: None, requires: Vec::new(),
                source: SourceDecl { kind: String::new(), keys: BTreeMap::new() },
                pitch: PitchDecl::default(),
                amp: Envelope::None,
                filter: None, r#mod: None, native: BTreeMap::new(),
            });
            first = false;
            continue;
        }

        let patch = p.as_mut().unwrap();
        let head = b.directive.split_whitespace().next().unwrap_or("");
        match head {
            "name"    => patch.name = strip_quotes(rest_after(&b.directive, "name")),
            "author"  => patch.author = strip_quotes(rest_after(&b.directive, "author")),
            "license" => patch.license = strip_quotes(rest_after(&b.directive, "license")),
            "origin"  => patch.origin = rest_after(&b.directive, "origin"),
            "parent"  => patch.parent = Some(rest_after(&b.directive, "parent")),
            "requires" => {
                let cap = rest_after(&b.directive, "requires");
                let mut req = Requirement { capability: cap, constraints: BTreeMap::new() };
                for (_, line) in &b.inner {
                    let w: Vec<&str> = line.splitn(2, char::is_whitespace).collect();
                    if w.is_empty() { continue; }
                    let k = w[0].to_string();
                    let v = w.get(1).map(|s| s.trim().to_string()).unwrap_or_default();
                    req.constraints.insert(k, v);
                }
                patch.requires.push(req);
            }
            "source" => {
                let kind = rest_after(&b.directive, "source");
                let mut src = SourceDecl { kind, keys: BTreeMap::new() };
                for (_, line) in &b.inner {
                    let w: Vec<&str> = line.splitn(2, char::is_whitespace).collect();
                    if w.is_empty() { continue; }
                    let k = w[0].to_string();
                    let v = w.get(1).map(|s| s.trim().to_string()).unwrap_or_default();
                    src.keys.insert(k, v);
                }
                patch.source = src;
            }
            "pitch" => patch.pitch = parse_pitch(&b.inner)?,
            "amp"   => {
                let form = rest_after(&b.directive, "amp");
                patch.amp = parse_amp(&form, &b.inner)?;
            }
            "filter" => {
                let kind = rest_after(&b.directive, "filter");
                let mut f = FilterDecl { kind, keys: BTreeMap::new() };
                for (_, line) in &b.inner {
                    let w: Vec<&str> = line.splitn(2, char::is_whitespace).collect();
                    if w.is_empty() { continue; }
                    f.keys.insert(w[0].to_string(),
                        w.get(1).map(|s| s.trim().to_string()).unwrap_or_default());
                }
                patch.filter = Some(f);
            }
            "mod" => {
                let kind = rest_after(&b.directive, "mod");
                let mut m = ModDecl { kind, keys: BTreeMap::new() };
                for (_, line) in &b.inner {
                    let w: Vec<&str> = line.splitn(2, char::is_whitespace).collect();
                    if w.is_empty() { continue; }
                    m.keys.insert(w[0].to_string(),
                        w.get(1).map(|s| s.trim().to_string()).unwrap_or_default());
                }
                patch.r#mod = Some(m);
            }
            "native" => {
                let chip = rest_after(&b.directive, "native");
                let mut fields = BTreeMap::new();
                for (_, line) in &b.inner {
                    let w: Vec<&str> = line.splitn(2, char::is_whitespace).collect();
                    if w.is_empty() { continue; }
                    let k = w[0].to_string();
                    let v: i32 = w.get(1).and_then(|s| s.trim().parse().ok())
                        .ok_or_else(|| format!("native `{}`: bad integer", k))?;
                    fields.insert(k, v);
                }
                patch.native.insert(chip, fields);
            }
            other => return Err(format!("unknown patch directive `{}`", other)),
        }
    }

    let patch = p.ok_or("empty patch file")?;
    if patch.name.is_empty() { return Err("missing `name`".into()); }
    if patch.source.kind.is_empty() { return Err("missing `source`".into()); }
    if patch.requires.is_empty() {
        // Best-effort: derive from source.kind if user omitted `requires`.
        // The example patches include them explicitly; skip this fallback.
    }
    Ok(patch)
}

fn parse_pitch(inner: &[(usize, String)]) -> Result<PitchDecl, String> {
    let mut p = PitchDecl::default();
    for (_, line) in inner {
        let w: Vec<&str> = line.split_whitespace().collect();
        if w.len() < 2 { continue; }
        let v: f32 = w[1].parse().map_err(|e| format!("pitch {}: {}", w[0], e))?;
        match w[0] {
            "transpose" => p.transpose = v,
            "detune"    => p.detune = v,
            "glide"     => p.glide = v,
            other => return Err(format!("unknown pitch key `{}`", other)),
        }
    }
    Ok(p)
}

fn parse_amp(form: &str, inner: &[(usize, String)]) -> Result<Envelope, String> {
    match form {
        "adsr" => {
            let mut a = 0.0; let mut d = 0.0; let mut s = 0.0; let mut r = 0.0;
            let mut curve = CurveKind::Lin;
            for (_, line) in inner {
                let w: Vec<&str> = line.split_whitespace().collect();
                if w.len() < 2 { continue; }
                match w[0] {
                    "a" => a = w[1].parse().map_err(|e| format!("adsr a: {}", e))?,
                    "d" => d = w[1].parse().map_err(|e| format!("adsr d: {}", e))?,
                    "s" => s = w[1].parse().map_err(|e| format!("adsr s: {}", e))?,
                    "r" => r = w[1].parse().map_err(|e| format!("adsr r: {}", e))?,
                    "curve" => curve = match w[1] {
                        "lin" => CurveKind::Lin,
                        "exp" => CurveKind::Exp,
                        "log" => CurveKind::Log,
                        other => return Err(format!("adsr curve `{}`", other)),
                    },
                    other => return Err(format!("unknown adsr key `{}`", other)),
                }
            }
            Ok(Envelope::Adsr { a, d, s, r, curve })
        }
        "steps" => {
            let mut values: Vec<i32> = Vec::new();
            let mut loop_at: Option<usize> = None;
            let mut rate = 60.0;
            for (_, line) in inner {
                let w: Vec<&str> = line.split_whitespace().collect();
                if w.is_empty() { continue; }
                match w[0] {
                    "values" => {
                        values = w[1..].iter().map(|s| s.parse::<i32>()
                            .map_err(|e| format!("steps value `{}`: {}", s, e)))
                            .collect::<Result<Vec<_>, _>>()?;
                    }
                    "loop" => loop_at = Some(w[1].parse()
                        .map_err(|e| format!("steps loop: {}", e))?),
                    "rate" => rate = w[1].parse()
                        .map_err(|e| format!("steps rate: {}", e))?,
                    other => return Err(format!("unknown steps key `{}`", other)),
                }
            }
            if values.is_empty() { return Err("amp steps needs `values`".into()); }
            if let Some(l) = loop_at {
                if l >= values.len() { return Err("steps loop out of range".into()); }
            }
            Ok(Envelope::Steps { values, loop_at, rate })
        }
        other => Err(format!("unknown amp form `{}`", other)),
    }
}

// Expr is used to pre-evaluate the `.chip` file; patches don't currently use
// it, but the type is re-exported for future `native` expression fields.
#[allow(dead_code)]
fn _unused(_: &Expr) {}

// ───────────────────────── Tests ─────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_handwritten_pluck() {
        let p = parse(include_str!("../../../share/chip/examples/pluck.patch.txt"))
            .expect("parse");
        assert_eq!(p.name, "pluck");
        assert_eq!(p.source.kind, "tone.square");
        assert!(p.requires.iter().any(|r| r.capability == "tone.square"));
        assert!(matches!(p.amp, Envelope::Adsr { .. }));
    }

    #[test]
    fn parses_lifted_pluck() {
        let p = parse(include_str!("../../../share/chip/examples/pluck_sms_01.patch.txt"))
            .expect("parse");
        assert!(matches!(p.amp, Envelope::Steps { .. }));
        if let Envelope::Steps { ref values, loop_at, .. } = p.amp {
            assert_eq!(values.len(), 8);
            assert_eq!(loop_at, Some(4));
        }
    }

    #[test]
    fn parses_hat() {
        let p = parse(include_str!("../../../share/chip/examples/hat_sms_01.patch.txt"))
            .expect("parse");
        assert_eq!(p.source.kind, "noise.lfsr");
    }
}
