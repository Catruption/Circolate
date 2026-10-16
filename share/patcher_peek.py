#!/usr/bin/env python3
"""Peek at Patcher state blobs."""
import sys, json, re
from pathlib import Path

def analyze(path: Path):
    data = path.read_bytes()
    print(f"\n=== {path.name} ({len(data)} bytes) ===")
    print(f"first 16 bytes: {data[:16].hex(' ')}")

    # Try JSON at the top level.
    for offset in range(0, min(64, len(data))):
        for enc in ('utf-8', 'utf-16-le'):
            try:
                text = data[offset:].decode(enc)
            except UnicodeDecodeError:
                continue
            if text.lstrip().startswith(('{', '[')):
                try:
                    obj = json.loads(text)
                    print(f"  -> JSON at offset {offset} ({enc})")
                    print(f"  keys: {list(obj.keys()) if isinstance(obj, dict) else type(obj).__name__}")
                    return obj
                except Exception:
                    pass

    # Otherwise, extract printable strings.
    strings = re.findall(rb'[\x20-\x7e]{4,}', data)
    print(f"  {len(strings)} printable strings:")
    for s in strings[:60]:
        print(f"    {s.decode('ascii', 'replace')}")

    # Hex dump the first 512 bytes.
    print("  --- hexdump ---")
    for off in range(0, min(512, len(data)), 16):
        row = data[off:off+16]
        hx = ' '.join(f'{b:02x}' for b in row)
        asc = ''.join(chr(b) if 32 <= b < 127 else '.' for b in row)
        print(f"  {off:04x}  {hx:<48}  {asc}")

if __name__ == '__main__':
    for arg in sys.argv[1:] or ['trackscripts/AquaTemp/inserts/insert03_slot05_Patcher.state']:
        analyze(Path(arg))
