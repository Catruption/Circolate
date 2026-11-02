#!/usr/bin/env python3
import os
import json
import hashlib
import datetime
import sys

if len(sys.argv) < 2:
    print("Usage: python scripts/fix_manifest.py <path_to_project_dir>")
    sys.exit(1)

root = sys.argv[1].rstrip("/")
title = os.path.basename(root)

# Automatically find the .halo.txt entry point
entry = None
for f in os.listdir(root):
    if f.endswith(".halo.txt"):
        entry = f
        break

if not entry:
    print(f"Error: No .halo.txt found in {root}")
    sys.exit(1)

manifest = {
    "format": "halotext-archive",
    "version": 1,
    "tool": "circolate 0.2.0",
    "created": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "title": title,
    "entry": entry,
    "sample_format": "flac",
    "sample_rate": 44100,
    "entries": []
}

# Load sample IDs from samples.json so we don't hardcode them
sample_ids = {}
samples_json_path = os.path.join(root, "samples.json")
if os.path.exists(samples_json_path):
    try:
        with open(samples_json_path, "r") as f:
            data = json.load(f)
            for s in data.get("samples", []):
                if "file" in s and "id" in s:
                    sample_ids[s["file"]] = s["id"]
                    sample_ids[s["file"].lstrip("./")] = s["id"]
    except Exception as e:
        print(f"Warning: Could not parse samples.json: {e}")

def get_hash(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(65536), b""):
            h.update(chunk)
    return h.hexdigest()

for dirpath, _, filenames in os.walk(root):
    for f in filenames:
        full_path = os.path.join(dirpath, f)
        rel_path = os.path.relpath(full_path, root)
        
        # Exclude the manifest and any existing zip files
        if rel_path in ("manifest.json", f"{title}.halo.zip") or f.endswith(".zip"):
            continue
            
        kind = "other"
        if f.endswith(".halo.txt"): kind = "halotext"
        elif f == "samples.json": kind = "sample_library"
        elif f.startswith("cover."): kind = "cover"
        elif f.endswith((".wav", ".mp3", ".ogg", ".flac")): kind = "sample"

        entry_obj = {
            "path": rel_path,
            "size": os.path.getsize(full_path),
            "sha256": get_hash(full_path),
            "kind": kind
        }
        
        if kind == "sample":
            if rel_path in sample_ids:
                entry_obj["sample_id"] = sample_ids[rel_path]
            elif f in sample_ids:
                entry_obj["sample_id"] = sample_ids[f]
                
            entry_obj["encoding"] = "flac"
            entry_obj["channels"] = 2
            # Default rate to 44100, adjust for known 48k/96k files if needed
            entry_obj["rate"] = 44100

        manifest["entries"].append(entry_obj)

manifest_path = os.path.join(root, "manifest.json")
with open(manifest_path, "w") as f:
    json.dump(manifest, f, indent=2)

print(f"✅ Rewrote {manifest_path} with {len(manifest['entries'])} entries.")
