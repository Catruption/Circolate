//! `trackscript-player test` — pipeline diagnostics.
//!
//! Without flags: walks the load pipeline and prints the first
//! checkpoint that comes back "no" for each part.
//!
//! With `--smoke`: additionally instantiates every VST instrument,
//! loads it with the state blob from the sidecar, plays one note for
//! one second, and reports the peak output level.  A plugin that
//! loads but produces silence tells you the state blob wasn't
//! accepted (or is empty, or points at a file the plugin can't find).
//!
//! Output is deliberately verbose and unstructured: it's a debugging
//! tool, not a UI.  Pipe through `grep` to narrow.

use anyhow::Result;
use std::path::Path;

use crate::player::{load_project, Project};
use crate::sources::trackscript::TrackscriptEvent;
use crate::vst_host_ffi::VstHost;

const TEST_SAMPLE_RATE: u32 = 48000;
const SMOKE_SR: u32 = 44100;
const SMOKE_BLOCK: usize = 512;
const SMOKE_SECONDS: usize = 1;

pub fn run(rest: &[String]) -> Result<()> {
    let mut smoke = false;
    let mut file_arg: Option<&str> = None;
    for a in rest {
        if a == "--smoke" {
            smoke = true;
        } else if !a.starts_with("--") && file_arg.is_none() {
            file_arg = Some(a);
        }
    }

    let Some(file) = file_arg else {
        anyhow::bail!("test: missing <file>  (use --smoke to also load VST instruments)");
    };
    let path = Path::new(file);
    if !path.exists() {
        anyhow::bail!("test: no such file: {}", path.display());
    }

    let (project, _cache) = load_project(path, TEST_SAMPLE_RATE, false, false)?;

    print_summary(&project);
    print_sf2_keys(&project);
    print_vst_keys(&project);
    print_multi_sample_keys(&project);
    print_path_kinds(&project);
    print_per_part_trace(&project);

    if smoke {
        smoke_vst_instruments(&project, path)?;
    }

    print_footer(smoke);
    Ok(())
}

// ── summary ────────────────────────────────────────────────────────────

fn print_summary(p: &Project) {
    println!("\n=== project ===");
    println!("  title:              {:?}", p.title);
    println!("  parts:              {}", p.parts.len());
    println!("  samples:            {}", p.samples.len());
    println!("  path_to_id entries: {}", p.path_to_id.len());
    println!("  sf2_instruments:    {}", p.sf2_instruments.len());
    println!("  vst_instruments:    {}", p.vst_instruments.len());
    println!("  multi_sample:       {}", p.multi_sample_instruments.len());
    println!("  lines:              {}", p.lines.len());
    println!("  voice_count:        {}", p.voice_count);
    println!("  bpm:                {}", p.initial_bpm);
    println!(
        "  time_signature:     {}/{}",
        p.time_signature.0, p.time_signature.1
    );
}

fn print_sf2_keys(p: &Project) {
    println!("\n=== sf2_instruments ===");
    if p.sf2_instruments.is_empty() {
        println!("  (none)");
        return;
    }
    let mut keys: Vec<_> = p.sf2_instruments.keys().cloned().collect();
    keys.sort();
    for k in &keys {
        let inst = &p.sf2_instruments[k];
        let zones = inst.zone_count_hint();
        let first = inst.first_zone_sample_id();
        let first_ok = first
            .map(|sid| p.samples.contains_key(&sid))
            .unwrap_or(false);
        println!(
            "  {:<50} zones={:<4} first_sid={:?} in_samples={}",
            k, zones, first, first_ok
        );
    }
}

fn print_vst_keys(p: &Project) {
    println!("\n=== vst_instruments ===");
    if p.vst_instruments.is_empty() {
        println!("  (none)");
        return;
    }
    let mut keys: Vec<_> = p.vst_instruments.keys().cloned().collect();
    keys.sort();
    for k in &keys {
        let inst = &p.vst_instruments[k];
        println!(
            "  {:<50} name={:<14} state={:<6} fallback={} path={}",
            k,
            format!("{:?}", inst.name),
            inst.state_bytes.len(),
            inst.fallback,
            inst.plugin_path.display()
        );
    }
}

fn print_multi_sample_keys(p: &Project) {
    println!("\n=== multi_sample_instruments ===");
    if p.multi_sample_instruments.is_empty() {
        println!("  (none)");
        return;
    }
    let mut keys: Vec<_> = p.multi_sample_instruments.keys().cloned().collect();
    keys.sort();
    for k in &keys {
        println!("  {}", k);
    }
}

// ── per-part path classification ──────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathKind {
    Multi,
    Wav,
    Vst,
    Sf2,
    Sf2Empty,
    Sf2MissingSamples,
    None,
}

fn classify_path(p: &Project, sample_path: &str) -> PathKind {
    if p.multi_sample_instruments.contains_key(sample_path) {
        return PathKind::Multi;
    }
    if let Some(id) = p.path_to_id.get(sample_path) {
        if p.samples.contains_key(id) {
            return PathKind::Wav;
        }
    }
    if p.vst_instruments.contains_key(sample_path) {
        return PathKind::Vst;
    }
    if let Some(inst) = p.sf2_instruments.get(sample_path) {
        if inst.zone_count_hint() == 0 {
            return PathKind::Sf2Empty;
        }
        let first_ok = inst
            .first_zone_sample_id()
            .map(|sid| p.samples.contains_key(&sid))
            .unwrap_or(false);
        if !first_ok {
            return PathKind::Sf2MissingSamples;
        }
        return PathKind::Sf2;
    }
    PathKind::None
}

fn print_path_kinds(p: &Project) {
    println!("\n=== part path classification ===");
    let mut names: Vec<_> = p.parts.keys().cloned().collect();
    names.sort();
    for name in &names {
        let part = &p.parts[name];
        let notes = part.events.iter().filter(|e| !e.is_waypoint()).count();
        if notes == 0 {
            continue;
        }
        let kind = classify_path(p, &part.sample_path);
        println!("  {:<40} notes={:<5} kind={:?}", name, notes, kind);
    }
}

// ── deep per-part trace ───────────────────────────────────────────────

fn print_per_part_trace(p: &Project) {
    println!("\n=== per-part trace ===");
    let mut names: Vec<_> = p.parts.keys().cloned().collect();
    names.sort();
    for name in &names {
        let part = &p.parts[name];
        let notes: Vec<&TrackscriptEvent> = part
            .events
            .iter()
            .filter(|e| !e.is_waypoint())
            .collect();
        if notes.is_empty() {
            continue;
        }

        println!("\n── part {:?} ──", name);
        println!("   sample_path:   {:?}", part.sample_path);
        println!("   subs_per_beat: {}", part.subs_per_beat);
        println!(
            "   events:        {} ({} notes)",
            part.events.len(),
            notes.len()
        );

        let kind = classify_path(p, &part.sample_path);
        println!("   path_kind:     {:?}", kind);

        match kind {
            PathKind::None => {
                println!("   >> no instrument matches this sample_path");
            }
            PathKind::Sf2Empty => {
                println!("   >> sf2 sidecar loaded but has zero zones");
            }
            PathKind::Sf2MissingSamples => {
                println!("   >> sf2 zones exist but their sample ids");
                println!("   >> are not present in project.samples");
            }
            PathKind::Sf2 => trace_sf2(p, name, &notes),
            PathKind::Vst => {
                let inst = &p.vst_instruments[&part.sample_path];
                println!("   vst iid:       {}", inst.iid);
                println!("   vst name:      {:?}", inst.name);
                println!("   vst path:      {}", inst.plugin_path.display());
                println!("   vst state:     {} bytes", inst.state_bytes.len());
                println!("   fallback:      {}", inst.fallback);
                println!("   gain:          {}", inst.gain);
                if let Some(first) = notes.first() {
                    if let Some(k) = first.pitch_midi {
                        println!("   first note:    key={} vol={:?}", k, first.vol);
                    }
                }
            }
            PathKind::Wav => {
                let id = p.path_to_id[&part.sample_path];
                let smp = &p.samples[&id];
                println!("   sample id:     {}", id);
                println!(
                    "   sample:        {} frames @ {} Hz",
                    smp.len(),
                    smp.rate
                );
                println!("   stereo:        {}", smp.stereo);
                println!("   loop_pts:      {:?}", smp.loop_pts);
            }
            PathKind::Multi => {
                println!("   multi-sample instrument (routed via fidelity.json)");
            }
        }
    }
}

fn trace_sf2(p: &Project, part_name: &str, notes: &[&TrackscriptEvent]) {
    let part = &p.parts[part_name];
    let inst = &p.sf2_instruments[&part.sample_path];

    let n_check = notes.len().min(8);
    println!("   resolving first {} note(s):", n_check);
    let mut resolved = 0usize;
    let mut in_map = 0usize;
    for (i, ev) in notes.iter().take(n_check).enumerate() {
        let key = ev.pitch_midi.unwrap_or(60).clamp(0, 127) as u8;
        let vel = ev
            .vol
            .map(|v| (v * 127.0 / 100.0).clamp(0.0, 127.0) as u8)
            .unwrap_or(100);
        let fp_cents = ev.fine_pitch.unwrap_or(0) as f64;

        match inst.resolve(key, vel, fp_cents) {
            Some(spec) => {
                let ok = p.samples.contains_key(&spec.sample_id);
                resolved += 1;
                if ok {
                    in_map += 1;
                }
                println!(
                    "     [{:>2}] key={:<3} vel={:<3} -> sid={:<6} hz={:>8.2} in_samples={}",
                    i, key, vel, spec.sample_id, spec.pitch_hz, ok
                );
            }
            None => {
                println!(
                    "     [{:>2}] key={:<3} vel={:<3} -> resolve() returned None",
                    i, key, vel
                );
            }
        }
    }
    println!(
        "   summary:       {}/{} resolved, {}/{} samples present",
        resolved, n_check, in_map, n_check
    );
}

// ── VST smoke ─────────────────────────────────────────────────────────

/// Instantiate every VST instrument with its state blob, play a note
/// for one second, report peak.  A `SILENT` result means the plugin
/// loaded but produced no audio — usually because it rejected the
/// state blob (format mismatch, missing referenced file, or the state
/// is empty because the original plugin was a fallback substitution).
///
/// Chdirs into the project directory before instantiating so that any
/// relative paths inside the state (SF2 files, sample references) can
/// be resolved by the plugin.
fn smoke_vst_instruments(p: &Project, ts_path: &Path) -> Result<()> {
    let original_cwd = std::env::current_dir()?;
    let project_dir = ts_path.parent().unwrap_or(Path::new("."));
    std::env::set_current_dir(project_dir)?;

    println!("\n=== vst instrument smoke (--smoke) ===");
    println!("  sample_rate:   {}", SMOKE_SR);
    println!("  block_size:    {}", SMOKE_BLOCK);
    println!("  duration:      {} sec", SMOKE_SECONDS);
    println!("  cwd:           {}", project_dir.display());

    let mut keys: Vec<_> = p.vst_instruments.keys().cloned().collect();
    keys.sort();

    if keys.is_empty() {
        println!("  (no vst instruments to smoke)");
        let _ = std::env::set_current_dir(original_cwd);
        return Ok(());
    }

    let total = SMOKE_SR as usize * SMOKE_SECONDS;
    let blocks = total / SMOKE_BLOCK;

    for key in &keys {
        let inst = &p.vst_instruments[key];
        let state_len = inst.state_bytes.len();
        let plugin_path = inst.plugin_path.to_string_lossy().to_string();

        let mut host = match VstHost::load(&plugin_path, &inst.state_bytes, SMOKE_SR) {
            Ok(h) => h,
            Err(e) => {
                println!(
                    "  {:<50} LOAD FAILED: {:#}",
                    key, e
                );
                continue;
            }
        };

        host.note_on(60, 100);
        let mut l = vec![0f32; total];
        let mut r = vec![0f32; total];
        for b in 0..blocks {
            if b == 30 {
                host.note_off(60);
            }
            let off = b * SMOKE_BLOCK;
            host.process(
                &mut l[off..off + SMOKE_BLOCK],
                &mut r[off..off + SMOKE_BLOCK],
            );
        }

        let peak = l
            .iter()
            .chain(r.iter())
            .fold(0f32, |a, &b| a.max(b.abs()));
        let rms = (l.iter().chain(r.iter()).map(|&s| s * s).sum::<f32>()
            / (2 * total) as f32)
            .sqrt();

        let status = if peak < 1e-4 { "SILENT" } else { "ok" };
        println!(
            "  {:<50} state={:<6} peak={:.4} rms={:.4}  {}",
            key, state_len, peak, rms, status
        );
    }

    let _ = std::env::set_current_dir(original_cwd);
    Ok(())
}

// ── footer ────────────────────────────────────────────────────────────

fn print_footer(smoke: bool) {
    println!("\n=== legend ===");
    println!("  path_kind values:");
    println!("    Multi              routed via multi_sample_instruments");
    println!("    Wav                direct sample via path_to_id");
    println!("    Vst                VST3 sidecar");
    println!("    Sf2                SF2 loaded, zones present, samples in map");
    println!("    Sf2Empty           SF2 loaded but zero zones");
    println!("    Sf2MissingSamples  zones exist, sample ids not in project.samples");
    println!("    None               nothing matched the sample_path");
    if !smoke {
        println!();
        println!("  Re-run with --smoke to also instantiate each VST instrument");
        println!("  and report whether it produces audio from its state blob.");
    }
}