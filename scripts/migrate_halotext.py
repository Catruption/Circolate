#!/usr/bin/env python3
"""migrate_halotext.py — halotext 1.10 → 1.11 migration.

Usage:
  python migrate_halotext.py <project-dir>...              # dry run, writes .new
  python migrate_halotext.py <project-dir>... --in-place   # overwrite (.bak saved)
  python migrate_halotext.py <project-dir>... --check      # report only

Reads:
  *.halo.txt, instruments/*.instrument, fidelity.json

Transformations:
  BlockList() { }  → BlockList() [ ]
  Tempo { BeatsPerMinute: N; }  → @<§1/1,#N, ƒTempo>; shim
  ƒ'instruments/NAME.instrument' → ƒ'<sidecar_stem>.vst3' (+ Fallbacks) [VST]
  ƒ'samples/file.wav'            → ƒ'file.wav' [.wav/.mp3/.ogg]
  @<A>@<B>@<C>                   → @<A, B, C>
  Add Fallbacks { } after Config if any fallback substitutions exist

Does NOT:
  - Flatten SF2 refs (loader needs preset-aware lookup first)
  - Fill shim parameters (unknowable for FL natives)
  - Hoist shims to definitions (LSP-era refactor)
"""

import argparse
import json
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional


@dataclass
class Instrument:
    name: str
    kind: str = ""
    plugin: str = ""
    file: str = ""
    path: str = ""
    bank: int = 0
    preset: int = 0
    replaces: str = ""
    fallback: bool = False
    gain: float = 1.0
    via_default: bool = False
    original_plugin: str = ""
    slicex: bool = False        # NEW


@dataclass
class FallbackEntry:
    missing: str
    use: str
    gain: float = 1.0


@dataclass
class Project:
    dir: Path
    path: Path
    halotext: str
    instruments: dict = field(default_factory=dict)
    channels: dict = field(default_factory=dict)
    fallbacks: list = field(default_factory=list)
    warnings: list = field(default_factory=list)


# ── sidecar readers ────────────────────────────────────────────────────

def parse_instrument(path: Path) -> Optional[Instrument]:
    try:
        text = path.read_text(encoding="utf-8")
    except OSError:
        return None
    m = re.match(r"Instrument\s+(\S+)", text)
    if not m:
        return None
    inst = Instrument(name=m.group(1))
    body_m = re.search(r"\{([^}]*)\}", text)
    if not body_m:
        return None
    for line in body_m.group(1).split(";"):
        line = line.strip()
        if ":" not in line:
            continue
        key, val = line.split(":", 1)
        key = key.strip()
        val = val.strip().strip("'")
        try:
            if key == "Kind":            inst.kind = val
            elif key == "Plugin":        inst.plugin = val
            elif key == "File":          inst.file = val
            elif key == "Path":          inst.path = val
            elif key == "Bank":          inst.bank = int(val)
            elif key == "Preset":        inst.preset = int(val)
            elif key == "Replaces":      inst.replaces = val
            elif key == "Fallback":      inst.fallback = val.lower() == "true"
            elif key == "Gain":          inst.gain = float(val)
            elif key == "ViaDefault":    inst.via_default = val.lower() == "true"
            elif key == "OriginalPlugin": inst.original_plugin = val
            elif key == "Slicex":        inst.slicex = val.lower() == "true"   # NEW
        except ValueError:
            continue
    return inst


def load_instruments(proj: Project) -> None:
    d = proj.dir / "instruments"
    if not d.is_dir():
        return
    for p in list(d.glob("*.instrument")) + list(d.glob("*.instrument.txt")):
        inst = parse_instrument(p)
        if inst:
            proj.instruments[inst.name] = inst
        else:
            proj.warnings.append(f"could not parse {p.name}")


def load_channels(proj: Project) -> None:
    p = proj.dir / "fidelity.json"
    if not p.exists():
        return
    try:
        data = json.loads(p.read_text(encoding="utf-8"))
    except (json.JSONDecodeError, OSError) as e:
        proj.warnings.append(f"fidelity.json: {e}")
        return
    for iid, ch in data.get("channels", {}).items():
        try:
            proj.channels[int(iid)] = ch
        except ValueError:
            continue


# ── instrument → reference ────────────────────────────────────────────

def instrument_ref(inst: Instrument) -> str:
    """Reference text for `ƒ...`.

    VST: uses the sidecar stem (= `inst.name`). This is the intent
    identifier — it matches the Fallbacks key. What actually loads is
    determined by the sidecar's `Plugin:` field.

    SF2: uses filename + preset. Deferred; stay as sidecar form.
    """
    if inst.kind == "sf2":
        fname = Path(inst.file).name if inst.file else ""
        if not fname:
            return ""
        if inst.bank == 0:
            return f"'{fname}'({inst.preset})"
        return f"'{fname}'({inst.bank}, {inst.preset})"
    if not inst.name:
        return ""
    return f"'{inst.name}.vst3'"


def fallback_for(inst: Instrument) -> Optional[FallbackEntry]:
    """Missing → installed fallback. Key is the sidecar stem (= intent
    name, matches reference key). Value is the substituted plugin."""
    if not (inst.fallback and inst.plugin):
        return None
    if not inst.name:
        return None
    return FallbackEntry(f"{inst.name}.vst3", f"{inst.plugin}.vst3", inst.gain)


# ── bracket helpers ───────────────────────────────────────────────────

def find_section(text: str, keyword: str) -> Optional[tuple]:
    m = re.search(rf"\b{re.escape(keyword)}\s*(?:\([^)]*\))?\s*[\{{\[]", text)
    if not m:
        return None
    brace_at = m.end() - 1
    open_ch = text[brace_at]
    close_ch = "}" if open_ch == "{" else "]"
    depth, i = 1, brace_at + 1
    while i < len(text) and depth:
        if text[i] == open_ch:   depth += 1
        elif text[i] == close_ch: depth -= 1
        i += 1
    if depth:
        return None
    return (m.start(), i, text[brace_at + 1:i - 1], brace_at, i - 1)


def find_shim_close(text: str, start: int) -> Optional[int]:
    paren, quote, i = 0, None, start
    while i < len(text):
        c = text[i]
        if quote:
            if c == quote: quote = None
        elif c in "'\"":
            quote = c
        elif c == "(":   paren += 1
        elif c == ")":   paren = max(0, paren - 1)
        elif c == ">" and paren == 0: return i
        i += 1
    return None


# ── transformations ───────────────────────────────────────────────────

def pass_blocklist(text: str, _p: Project) -> tuple:
    sec = find_section(text, "BlockList")
    if not sec:
        return text, []
    _, _, _, open_at, close_at = sec
    if text[open_at] == "[":
        return text, []
    new = text[:open_at] + "[" + text[open_at + 1:close_at] + "]" + text[close_at + 1:]
    return new, ["BlockList: { } → [ ]"]

def pass_lyrics_brackets(text: str, _p: Project) -> tuple:
    sec = find_section(text, "Lyrics")
    if not sec:
        return text, []
    _, _, _, open_at, close_at = sec
    if text[open_at] == "[":
        return text, []
    new = text[:open_at] + "[" + text[open_at + 1:close_at] + "]" + text[close_at + 1:]
    return new, ["Lyrics: { } → [ ]"]

def pass_tempo(text: str, _p: Project) -> tuple:
    sec = find_section(text, "Tempo")
    if not sec:
        return text, []
    start, end, body, _, _ = sec
    m = re.search(r"BeatsPerMinute\s*:\s*([\d.]+)", body)
    if not m:
        return text, ["WARNING: Tempo with no BeatsPerMinute"]
    bpm = m.group(1)
    text = text[:start] + text[end:]
    m2 = re.search(r"\bBlockList\s*(?:\([^)]*\))?\s*\[", text)
    if not m2:
        return text, [f"WARNING: no BlockList; Tempo={bpm} bpm lost"]
    insert_at = m2.end()
    text = text[:insert_at] + f"\n    @<§1/1,#{bpm}, ƒTempo>;" + text[insert_at:]
    return text, [f"Tempo → shim ({bpm} bpm)"]


def pass_instrument_refs(text: str, proj: Project) -> tuple:
    changes = []
    rx = re.compile(r"ƒ'instruments/([^']+?)\.instrument(?:\.txt)?'")
    def repl(m):
        name = m.group(1)
        inst = proj.instruments.get(name)
        if not inst:
            proj.warnings.append(f"unknown instrument: {name}")
            return m.group(0)

        if inst.kind == "sf2":
            # Slicex has envelope/ADSR state with no halotext home yet.
            if inst.slicex:
                proj.warnings.append(
                    f"{name}: Slicex envelope kept as sidecar (not flattened)"
                )
                return m.group(0)
            # Flatten to ƒ'samples/X.sf2'(preset)
            fname = inst.file  # already "samples/X.sf2"
            if not fname:
                return m.group(0)
            ref = (f"'{fname}'({inst.preset})" if inst.bank == 0
                   else f"'{fname}'({inst.bank}, {inst.preset})")
            changes.append(f"{name} → {ref}")
            return "ƒ" + ref

        # VST — sidecar stem (intent name)
        ref = instrument_ref(inst)
        if not ref:
            return m.group(0)
        fb = fallback_for(inst)
        if fb and not any(e.missing == fb.missing for e in proj.fallbacks):
            proj.fallbacks.append(fb)
        changes.append(f"{name} → {ref}")
        return "ƒ" + ref
    return rx.sub(repl, text), changes
    changes = []
    rx = re.compile(r"ƒ'instruments/([^']+?)\.instrument(?:\.txt)?'")
    def repl(m):
        name = m.group(1)
        inst = proj.instruments.get(name)
        if not inst:
            proj.warnings.append(f"unknown instrument: {name}")
            return m.group(0)
        if inst.kind == "sf2":
            return m.group(0)
        ref = instrument_ref(inst)
        if not ref:
            return m.group(0)
        fb = fallback_for(inst)
        if fb and not any(e.missing == fb.missing for e in proj.fallbacks):
            proj.fallbacks.append(fb)
        changes.append(f"{name} → {ref}")
        return "ƒ" + ref
    return rx.sub(repl, text), changes


def pass_sample_paths(text: str, _p: Project) -> tuple:
    changes = set()
    rx = re.compile(r"ƒ'samples/([^/']+\.(?:wav|mp3|ogg))'")
    def repl(m):
        changes.add(f"ƒ'…{m.group(1)}'")
        return f"ƒ'{m.group(1)}'"
    new_text = rx.sub(repl, text)
    return new_text, sorted(changes)


def pass_shim_chains(text: str, _p: Project) -> tuple:
    changes = []
    out = []
    i = 0
    while i < len(text):
        if text[i:i + 2] == "@<":
            start = i
            targets = []
            while i < len(text) and text[i:i + 2] == "@<":
                close = find_shim_close(text, i + 2)
                if close is None:
                    break
                targets.append(text[i + 2:close].strip())
                i = close + 1
            if len(targets) > 1:
                out.append("@<" + ", ".join(targets) + ">")
                changes.append(f"merged {len(targets)}")
            else:
                out.append(text[start:i])
        else:
            out.append(text[i])
            i += 1
    return "".join(out), changes


def pass_add_fallbacks(text: str, proj: Project) -> tuple:
    if not proj.fallbacks:
        return text, []
    lines = ["Fallbacks {"]
    for fb in proj.fallbacks:
        gain = f" Gain: {fb.gain};" if abs(fb.gain - 1.0) > 1e-9 else ""
        lines.append(f"    '{fb.missing}': {{ Use: '{fb.use}';{gain} }};")
    lines.append("}")
    block = "\n".join(lines) + "\n\n"

    sec = find_section(text, "Config")
    if sec:
        _, end, _, _, _ = sec
        text = text[:end] + "\n\n" + block + text[end:].lstrip("\n")
    else:
        text = block + text
    return text, [f"Fallbacks added ({len(proj.fallbacks)} entries)"]


# ── pipeline ──────────────────────────────────────────────────────────

PASSES = [
    ("blocklist",   pass_blocklist),
    ("lyrics",      pass_lyrics_brackets),
    ("tempo",       pass_tempo),
    ("instruments", pass_instrument_refs),
    ("samples",     pass_sample_paths),
    ("shim-chains", pass_shim_chains),
    ("fallbacks",   pass_add_fallbacks),
]


def is_artifact(p: Path) -> bool:
    name = p.name
    return (
        name.endswith(".new")
        or name.endswith(".bak")
        or "-new." in name
        or "-old." in name
        or "-migrated." in name
        or ".pre-migration" in name
    )


def migrate(project_dir: Path) -> Project:
    expected = project_dir / f"{project_dir.name}.halo.txt"
    if expected.exists() and not is_artifact(expected):
        path = expected
    else:
        candidates = [
            p for p in project_dir.glob("*.halo.txt") if not is_artifact(p)
        ]
        if not candidates:
            candidates = [
                p for p in project_dir.glob("*.halotext") if not is_artifact(p)
            ]
        if len(candidates) != 1:
            names = [p.name for p in candidates]
            raise SystemExit(
                f"{project_dir}: expected 1 halotext, found {len(candidates)}: {names}"
            )
        path = candidates[0]

    proj = Project(
        dir=project_dir,
        path=path,
        halotext=path.read_text(encoding="utf-8"),
    )
    load_instruments(proj)
    load_channels(proj)
    for name, fn in PASSES:
        proj.halotext, changes = fn(proj.halotext, proj)
        if changes:
            print(f"  [{name}]")
            for c in changes:
                print(f"    {c}")
    if proj.warnings:
        print("  [warnings]")
        for w in proj.warnings:
            print(f"    {w}")
    return proj


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__,
                                  formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("dirs", nargs="+", type=Path)
    ap.add_argument("--in-place", action="store_true")
    ap.add_argument("--check", action="store_true")
    args = ap.parse_args()

    for d in args.dirs:
        if not d.is_dir():
            print(f"skip (not dir): {d}", file=sys.stderr)
            continue
        print(f"\n{'=' * 60}\n{d.name}")
        proj = migrate(d)
        if args.check:
            continue
        if args.in_place:
            backup = proj.path.with_suffix(proj.path.suffix + ".bak")
            proj.path.rename(backup)
            proj.path.write_text(proj.halotext, encoding="utf-8")
            print(f"  wrote: {proj.path.name}  (backup: {backup.name})")
        else:
            out = proj.path.with_name(proj.path.name + ".new")
            out.write_text(proj.halotext, encoding="utf-8")
            print(f"  wrote: {out.name}")


if __name__ == "__main__":
    main()
