//! Halotext emitter. The only module that writes halotext syntax.

use crate::formats::halotext::model::*;
use crate::formats::halotext::spec::{
    slot_to_greek, FIELD_DURATION, FIELD_MIX, FIELD_PAN, FIELD_SOURCE, FIELD_VOLUME,
};
use std::collections::{HashMap, HashSet};

const GLYPH_PART: &str = "ƒ";
const GLYPH_VOLUME: &str = "%";
const GLYPH_PAN: &str = "¶";
const GLYPH_DURATION: &str = "~";
const GLYPH_KEY: &str = "#";
const GLYPH_SELECTOR: &str = "§";
const GLYPH_FINE: &str = "¢";
const GLYPH_RELEASE: &str = "₨";
const GLYPH_MOD_X: &str = "₥";
const GLYPH_MOD_Y: &str = "¥";
const GLYPH_SRC_BPM: &str = "|";
const GLYPH_PART_SIGIL: &str = "|";
const GLYPH_ENV: &str = "∂";
const GLYPH_SLICES: &str = "γ";
const GLYPH_ENV_REFS: &str = "ε";

pub fn emit_document(f: &HalotextFile) -> String {
    let mut out = String::new();
    emit_config(&mut out, &f.config);
    out.push('\n');
    if !f.fallbacks.is_empty() {
        emit_fallbacks(&mut out, &f.fallbacks);
        out.push('\n');
    }
    if !f.lyrics.is_empty() {
        emit_lyrics(&mut out, &f.lyrics);
        out.push('\n');
    }
    emit_block_list(&mut out, &f.block_list, &f.shims, f.config.subdivisions);
    out.push('\n');
    emit_block_array(&mut out, f);
    out
}

pub fn emit_document_normalized(f: &mut HalotextFile) -> String {
    hoist_shims(f);
    emit_document(f)
}

// ── shim hoisting ──────────────────────────────────────────────────────

fn shim_key(s: &ShimEvent) -> String {
    format!(
        "{}|{:?}|{}|{}|{}|{:?}|{}|{}|{}|{:?}",
        s.target, s.params, s.mix, s.gain, s.pan, s.length_rows, s.row, s.property,
        s.start_value, s.waypoints
    )
}

pub fn hoist_shims(f: &mut HalotextFile) {
    let mut by_call: HashMap<(usize, usize), Vec<usize>> = HashMap::new();
    for (i, s) in f.shims.iter().enumerate() {
        if let Some(ci) = s.attached_call {
            by_call.entry((s.line, ci)).or_default().push(i);
        }
    }

    const BYPASS_KEY: &str = "<bypass>";

    struct Info {
        line: usize,
        ci: usize,
        part: String,
        key: String,
    }

    let mut infos: Vec<Info> = Vec::new();
    let mut chains: HashMap<(String, String), Vec<ShimEvent>> = HashMap::new();

    for (li, line) in f.block_list.iter().enumerate() {
        for (ci, call) in line.iter().enumerate() {
            let idxs = by_call.get(&(li, ci));
            let key = if call.bypass_shims {
                BYPASS_KEY.to_string()
            } else {
                idxs.map(|v| {
                    v.iter().map(|&i| shim_key(&f.shims[i])).collect::<Vec<_>>().join(";")
                })
                .unwrap_or_default()
            };
            if let Some(v) = idxs {
                chains.entry((call.name.clone(), key.clone())).or_insert_with(|| {
                    v.iter()
                    .map(|&i| {
                        let mut c = f.shims[i].clone();
                        c.line = 0;
                        c.attached_call = None;
                        c
                    })
                    .collect()
                });
            }
            infos.push(Info { line: li, ci, part: call.name.clone(), key });
        }
    }

    let mut counts: HashMap<&str, HashMap<&str, usize>> = HashMap::new();
    for inf in &infos {
        if inf.key == BYPASS_KEY { continue; }
        *counts.entry(inf.part.as_str()).or_default()
        .entry(inf.key.as_str()).or_insert(0) += 1;
    }

    let mut winners: HashMap<String, String> = HashMap::new();
    for (part, keys) in &counts {
        match f.parts.get(*part) {
            Some(p) if p.shims.is_empty() => {}
            _ => continue,
        }
        let best = keys.iter().max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
        .map(|(k, _)| k.to_string());
        if let Some(k) = best {
            if !k.is_empty() { winners.insert(part.to_string(), k); }
        }
    }

    if winners.is_empty() { return; }

    let mut drop: HashSet<usize> = HashSet::new();
    for inf in &infos {
        let Some(win) = winners.get(&inf.part) else { continue };
        if &inf.key == win {
            if let Some(v) = by_call.get(&(inf.line, inf.ci)) {
                drop.extend(v.iter().copied());
            }
        } else if inf.key.is_empty() {
            f.block_list[inf.line][inf.ci].bypass_shims = true;
        }
    }
    for (part, win) in &winners {
        if let (Some(p), Some(chain)) = (
            f.parts.get_mut(part),
                                         chains.get(&(part.clone(), win.clone())),
        ) {
            p.shims = chain.clone();
        }
    }
    let mut i = 0usize;
    f.shims.retain(|_| {
        let keep = !drop.contains(&i);
        i += 1;
        keep
    });
}

// ── config / fallbacks / lyrics ────────────────────────────────────────

fn emit_config(out: &mut String, c: &Config) {
    out.push_str("Config\n{\n");
    out.push_str("    TrackscriptVersion: 1.10;\n");
    out.push_str(&format!("    BeatsPerMinute: {};\n", fmt_num(c.bpm)));
    out.push_str(&format!("    TimeSignature: {}/{};\n", c.time_signature.0, c.time_signature.1));
    out.push_str(&format!("    Subdivisions: {};\n", c.subdivisions));
    out.push_str(&format!("    SongVolume: {}%;\n", fmt_num(c.song_volume)));
    out.push_str(&format!("    PanningLaw: {};\n", c.panning_law));
    if (c.stereo_width - 1.0).abs() > f32::EPSILON {
        out.push_str(&format!("    StereoWidth: {};\n", fmt_num(c.stereo_width as f64)));
    }
    out.push_str("    Overlap: stack;\n");
    out.push_str("    PitchDefault: names;\n");
    out.push_str("}\n");
}

fn sanitize_ref(s: &str) -> String {
    if s.ends_with(".vst3") || s.ends_with(".sf2") {
        let mut out = String::new();
        let mut last_underscore = false;
        for c in s.chars() {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '/' {
                out.push(c);
                last_underscore = false;
            } else if !last_underscore {
                out.push('_');
                last_underscore = true;
            }
        }
        out.trim_matches('_').to_string()
    } else {
        s.to_string()
    }
}

fn emit_fallbacks(out: &mut String, fb: &HashMap<String, FallbackSpec>) {
    out.push_str("Fallbacks {\n");
    let mut keys: Vec<&String> = fb.keys().collect();
    keys.sort();
    for k in keys {
        let v = &fb[k];
        let safe_k = sanitize_ref(k);
        let safe_use = sanitize_ref(&v.use_);
        match v.gain {
            Some(gain) if (gain - 1.0).abs() > f64::EPSILON => {
                out.push_str(&format!("    '{}': {{ Use: '{}'; Gain: {}; }};\n",
                                      safe_k, safe_use, fmt_num(gain)));
            }
            _ => out.push_str(&format!("    '{}': {{ Use: '{}'; }};\n", safe_k, safe_use)),
        }
    }
    out.push_str("}\n");
}

fn emit_lyrics(out: &mut String, lyrics: &[LyricEvent]) {
    out.push_str("Lyrics {\n");
    for ly in lyrics {
        let mut line = String::new();
        for seg in &ly.segments {
            line.push_str(&format!("<{}{},{}>,", GLYPH_SELECTOR, fmt_num(seg.row), seg.text));
        }
        out.push_str(&line);
        out.push('\n');
    }
    out.push_str("}\n");
}

// ── shim emission ──────────────────────────────────────────────────────

fn shim_target(s: &ShimEvent) -> String {
    let args: Vec<String> = s.params.iter()
    .map(|p| format!("{}{}", slot_to_greek(p.slot), fmt_num(p.value)))
    .collect();
    format!("{}{}({})", FIELD_SOURCE, s.target, args.join(", "))
}

fn shim_universal(s: &ShimEvent, subs: u32, default_mix: f64) -> Vec<String> {
    let mut v = Vec::new();
    if s.row.abs() > 1e-9 { v.push(pos_str(s.row, subs)); }
    if let Some(l) = s.length_rows {
        v.push(format!("{}{}", FIELD_DURATION, fmt_num(l / subs.max(1) as f64)));
    }
    if (s.mix - default_mix).abs() > 1e-9 {
        v.push(format!("{}{}", FIELD_MIX, fmt_num(s.mix)));
    }
    if !s.property.is_empty() {
        v.push(format!("{}{}", s.property, fmt_num(s.start_value)));
    } else {
        if (s.gain - 100.0).abs() > 1e-9 {
            v.push(format!("{}{}", FIELD_VOLUME, fmt_num(s.gain)));
        }
        if s.pan.abs() > 1e-9 {
            v.push(format!("{}{}", FIELD_PAN, fmt_num(s.pan)));
        }
    }
    v
}

fn emit_shim_waypoint(w: &ShimWaypoint, subs: u32) -> String {
    let glyph = match w.field {
        ShimField::Mix => FIELD_MIX,
        ShimField::Gain => FIELD_VOLUME,
        ShimField::Pan => FIELD_PAN,
        ShimField::Pitch => return String::new(),
    };
    let mut fields = vec![pos_str(w.row, subs), format!("{}{}", glyph, fmt_num(w.value))];
    if let Some(r) = w.ramp_rows {
        fields.push(format!("{}{}", FIELD_DURATION, fmt_num(r / subs.max(1) as f64)));
    }
    format!("&<{}>", fields.join(", "))
}

fn emit_shim_groups(shims: &[&ShimEvent], subs: u32, default_mix: f64) -> String {
    let mut out = String::new();
    let mut i = 0usize;
    while i < shims.len() {
        let uni = shim_universal(shims[i], subs, default_mix);
        let mut j = i + 1;
        while j < shims.len()
            && shims[j - 1].waypoints.is_empty()
            && shim_universal(shims[j], subs, default_mix) == uni
            {
                j += 1;
            }
            let mut fields = uni;
        for s in &shims[i..j] {
            fields.push(shim_target(s));
        }
        out.push_str("@<");
        out.push_str(&fields.join(", "));
        out.push('>');
        for w in &shims[j - 1].waypoints {
            out.push_str(&emit_shim_waypoint(w, subs));
        }
        i = j;
    }
    out
}

fn emit_selection(s: &MeasureSelection) -> String {
    let mut out = (s.measure + 1).to_string();
    if let Some(ov) = &s.overrides {
        let mut fields: Vec<String> = Vec::new();
        if let Some(v) = ov.vol { fields.push(format!("{}{}", FIELD_VOLUME, fmt_num(v))); }
        if let Some(p) = ov.pan { fields.push(format!("{}{}", FIELD_PAN, fmt_num(p))); }
        if let Some(d) = ov.dur { fields.push(format!("{}{}", FIELD_DURATION, fmt_num(d))); }
        if let Some(src) = &ov.source { fields.push(format!("{}'{}'", FIELD_SOURCE, src)); }
        if !fields.is_empty() {
            out.push('<');
            out.push_str(&fields.join(", "));
            out.push('>');
        }
    }
    out
}

pub fn emit_call(call: &BlockCall) -> String {
    let sel: Vec<String> = call.selections.iter().map(emit_selection).collect();
    format!("{}{}({})", GLYPH_PART, call.name, sel.join(", "))
}

fn emit_block_list(out: &mut String, lines: &[Vec<BlockCall>], shims: &[ShimEvent], subs: u32) {
    let mut shims_by_line: HashMap<usize, Vec<&ShimEvent>> = HashMap::new();
    for s in shims {
        shims_by_line.entry(s.line).or_default().push(s);
    }

    out.push_str("BlockList()\n[\n");
    for (i, line) in lines.iter().enumerate() {
        let line_shims: &[&ShimEvent] = shims_by_line.get(&i).map(|v| v.as_slice()).unwrap_or(&[]);
        if line.is_empty() && line_shims.is_empty() {
            out.push_str("    ;\n");
            continue;
        }
        out.push_str("    ");
        let mut wrote_anything = false;
        for (ci, call) in line.iter().enumerate() {
            if wrote_anything { out.push(' '); }
            wrote_anything = true;
            out.push_str(&format!("{}{}(", GLYPH_PART, call.name));
            let sel: Vec<String> = call.selections.iter().map(emit_selection).collect();
            out.push_str(&sel.join(", "));
            out.push(')');

            let call_shims: Vec<&ShimEvent> = line_shims.iter()
            .filter(|s| s.attached_call == Some(ci)).copied().collect();
            if call.bypass_shims && call_shims.is_empty() {
                out.push_str("@<>");
            } else {
                out.push_str(&emit_shim_groups(&call_shims, subs, 1.0));
            }
            out.push(';');
        }
        let standalone: Vec<&ShimEvent> = line_shims.iter()
        .filter(|s| s.attached_call.is_none()).copied().collect();
        if !standalone.is_empty() {
            if wrote_anything { out.push(' '); }
            out.push_str(&emit_shim_groups(&standalone, subs, 0.0));
            out.push(';');
        }
        out.push('\n');
    }
    out.push_str("]\n");
}

// ── parts ──────────────────────────────────────────────────────────────

fn emit_block_array(out: &mut String, f: &HalotextFile) {
    out.push_str("BlockArray()\n{\n");
    let mut names: Vec<&String> = f.parts.keys().collect();
    names.sort();
    for name in names {
        let p = &f.parts[name];
        emit_block_def(out, p);
        out.push_str("\n    ;\n\n");
    }
    out.push_str("}\n");
}

fn emit_block_def(out: &mut String, p: &HalotextPart) {
    out.push_str("    ");

    let mut args: Vec<String> = vec![format!(
        "{sigil}{beats}/{subs}",
        sigil = GLYPH_PART_SIGIL,
        beats = p.beats_per_measure,
        subs  = p.subs_per_beat,
    )];

    if !p.sample_path.is_empty() {
        let path = sanitize_ref(&p.sample_path);
        let source_ref = if path.ends_with(".sf2") {
            let preset = p.preset.unwrap_or(0);
            let bank = p.bank.unwrap_or(0);
            if bank == 0 {
                format!("{part}'{path}'({preset})", part = GLYPH_PART, path = path)
            } else {
                format!("{part}'{path}'({bank}, {preset})", part = GLYPH_PART, path = path)
            }
        } else {
            format!("{part}'{path}'", part = GLYPH_PART, path = path)
        };
        args.push(source_ref);
    }

    if let Some(sx) = &p.slicex {
        if !sx.slices.is_empty() {
            let gamma: String = sx.slices.iter().map(|s| format!(
                "{}:{}:{}:{}:{}:{}",
                s.frame_start, s.frame_end, s.key, s.pan, s.atten, s.cutoff
            )).collect::<Vec<_>>().join(";");
            args.push(format!("{}'{}'", GLYPH_SLICES, gamma));

            let mut eps: Vec<String> = Vec::new();
            if let Some(r) = &sx.amp   { eps.push(format!("amp:{}:{}",   r.part, fmt_num(r.amount))); }
            if let Some(r) = &sx.speed { eps.push(format!("speed:{}:{}", r.part, fmt_num(r.amount))); }
            if let Some(r) = &sx.cut   { eps.push(format!("cut:{}:{}",   r.part, fmt_num(r.amount))); }
            if let Some(r) = &sx.lfo   { eps.push(format!("lfo:{}:{}",   r.part, fmt_num(r.amount))); }
            if !eps.is_empty() {
                args.push(format!("{}'{}'", GLYPH_ENV_REFS, eps.join(";")));
            }
        }
    }

    out.push_str(&format!("{}{}(<{}>)", GLYPH_PART, p.name, args.join(", ")));
    if !p.shims.is_empty() {
        let refs: Vec<&ShimEvent> = p.shims.iter().collect();
        out.push_str(&emit_shim_groups(&refs, p.subs_per_beat, 1.0));
    }
    out.push_str("[\n");
    for measure in &p.measures {
        emit_measure(out, measure, p.subs_per_beat);
    }
    out.push_str("    ]");
}

fn emit_measure(out: &mut String, events: &[HalotextEvent], subs: u32) {
    if events.is_empty() {
        out.push_str("        ;\n");
        return;
    }
    out.push_str("        ");
    let mut first = true;
    for ev in events {
        if !first && !ev.is_waypoint() {
            out.push(',');
        }
        first = false;
        out.push_str(&emit_event(ev, subs));
    }
    out.push('\n');
}

fn emit_event(ev: &HalotextEvent, subs: u32) -> String {
    let mut fields: Vec<String> = Vec::new();
    fields.push(pos_str(ev.row, subs));

    if ev.is_waypoint() {
        if let Some(v) = ev.vol { fields.push(format!("{}{}", GLYPH_VOLUME, fmt_num(v))); }
        if let Some(k) = ev.pitch_midi { fields.push(format!("{}{}", GLYPH_KEY, note_name(k))); }
        if let Some(d) = ev.dur_beats { fields.push(format!("{}{}", GLYPH_DURATION, fmt_num(d))); }
        if let Some(p) = ev.pan { fields.push(format!("{}{}", GLYPH_PAN, fmt_num(p))); }
    } else {
        // Slicex envelope point — the *only* field on an envelope event.
        // Emit it first so it reads as the primary value.
        if let Some(v) = ev.env_value {
            let tension = ev.env_tension.unwrap_or(0.0);
            let kind = ev.env_kind.unwrap_or(0);
            if tension.abs() < 1e-6 && kind == 0 {
                fields.push(format!("{}{}", GLYPH_ENV, fmt_num(v)));
            } else {
                fields.push(format!(
                    "{}{}:{}:{}",
                    GLYPH_ENV, fmt_num(v), fmt_num(tension), kind
                ));
            }
        }

        if let Some(k) = ev.pitch_midi { fields.push(format!("{}{}", GLYPH_KEY, note_name(k))); }
        if let Some(d) = ev.dur_beats { fields.push(format!("{}{}", GLYPH_DURATION, fmt_num(d))); }
        if let Some(v) = ev.vol { fields.push(format!("{}{}", GLYPH_VOLUME, fmt_num(v))); }
        if let Some(p) = ev.pan { fields.push(format!("{}{}", GLYPH_PAN, fmt_num(p))); }
        if let Some(f) = ev.fine_pitch { fields.push(format!("{}{:+}", GLYPH_FINE, f)); }
        if let Some(r) = ev.release { fields.push(format!("{}{:+}", GLYPH_RELEASE, r)); }
        if let Some(m) = ev.mod_x { fields.push(format!("{}{:+}", GLYPH_MOD_X, m)); }
        if let Some(m) = ev.mod_y { fields.push(format!("{}{:+}", GLYPH_MOD_Y, m)); }
        if let Some(b) = ev.src_bpm { fields.push(format!("{}{}", GLYPH_SRC_BPM, fmt_num(b))); }
        if let (Some(src), Some(off)) = (&ev.sample_override, ev.offset) {
            fields.push(format!("{}{}{}{}({})", GLYPH_PART, "'", sanitize_ref(src), "'", off));
        }
    }

    let mut out = String::new();
    if ev.is_waypoint() { out.push('&'); }
    out.push('<');
    out.push_str(&fields.join(","));
    out.push('>');
    out
}

fn pos_str(row: f64, subs: u32) -> String {
    let m = (row / subs as f64).floor() + 1.0;
    let s = row % subs as f64 + 1.0;
    if (s - s.round()).abs() < 1e-9 {
        format!("{}{}/{}", GLYPH_SELECTOR, m as i64, s as i64)
    } else {
        format!("{}{}/{}", GLYPH_SELECTOR, m as i64, (s * 1000.0).round() / 1000.0)
    }
}

fn fmt_num(v: f64) -> String {
    let rounded = (v * 10000.0).round() / 10000.0;
    if rounded.fract().abs() < 1e-9 {
        format!("{}", rounded as i64)
    } else {
        format!("{}", rounded)
    }
}

fn note_name(midi: i32) -> String {
    const NAMES: [&str; 12] = ["C","C#","D","D#","E","F","F#","G","G#","A","A#","B"];
    let octave = midi / 12;
    let n = midi.rem_euclid(12) as usize;
    format!("{}{}", NAMES[n], octave)
}
