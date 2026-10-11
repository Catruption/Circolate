#!/usr/bin/env python3
"""List the arrangements (playlists) stored in an FLP.

Usage: python3 flp_arrangements.py "SE V2 ACT 2.flp"

FL can hold several arrangements, each with its own playlist event (233).
The Rust parser keeps only the LAST 233 event, so patterns used only in
other arrangements look "not on the playlist" and get dropped.
"""
import struct
import sys

# Pattern ids the converter reported as "not on playlist" for channels 4/5/6/10
MISSING = {1, 2, 3, 4, 24, 25, 27, 29, 30, 31, 32, 33, 34, 35, 37, 38, 39,
           41, 42, 47, 48, 49, 50, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61}


def events(data):
    assert data[:4] == b"FLhd", "not an FLP"
    hdr = struct.unpack_from("<I", data, 4)[0]
    p = 8 + hdr
    assert data[p:p + 4] == b"FLdt", "missing FLdt"
    length = struct.unpack_from("<I", data, p + 4)[0]
    p += 8
    end = min(p + length, len(data))
    while p < end:
        ev = data[p]
        p += 1
        if ev < 64:
            size = 1
        elif ev < 128:
            size = 2
        elif ev < 192:
            size = 4
        else:
            size = 0
            shift = 0
            while True:
                b = data[p]
                p += 1
                size |= (b & 0x7F) << shift
                shift += 7
                if not b & 0x80:
                    break
        yield ev, data[p:p + size]
        p += size


def text(raw):
    if len(raw) % 2:
        return raw.split(b"\0")[0].decode("latin-1")
    end = 0
    while end + 1 < len(raw) and raw[end:end + 2] != b"\0\0":
        end += 2
    return raw[:end].decode("utf-16-le", "replace")


def playlist_items(raw):
    size = 60 if len(raw) % 60 == 0 else 32
    for i in range(0, len(raw) - size + 1, size):
        rec = raw[i:i + size]
        pos, base, idx, length = struct.unpack_from("<IHHI", rec, 0)
        if base != 0x5000:
            continue
        yield pos, length, ("pattern", idx - base) if idx > base else ("channel", idx)


def detect_stride(raw):
    """Item stride = len / (number of 60-byte-aligned records that look like items)."""
    n = sum(1 for i in range(0, len(raw) - 59, 60)
            if struct.unpack_from("<H", raw, i + 4)[0] == 0x5000)
    if n and len(raw) % n == 0:
        return len(raw) // n
    return 60 if len(raw) % 60 == 0 else 32


def dump_items(raw, stride, wanted, limit=3):
    """Print the non-zero 8-byte words of full items for the wanted refs."""
    shown = {w: 0 for w in wanted}
    for i in range(len(raw) // stride):
        rec = raw[i * stride:(i + 1) * stride]
        pos, base, idx, length = struct.unpack_from("<IHHI", rec, 0)
        if base != 0x5000:
            continue
        key = ("p", idx - base) if idx > base else ("c", idx)
        if key not in shown or shown[key] >= limit:
            continue
        shown[key] += 1
        print(f"  item {key[0]}{key[1]} pos={pos} len={length} ({stride} bytes)")
        for off in range(0, stride, 8):
            w = rec[off:off + 8]
            if len(w) < 8 or w == b"\0" * 8:
                continue
            d = struct.unpack("<d", w)[0]
            f1, f2 = struct.unpack("<ff", w)
            u1, u2 = struct.unpack("<II", w)
            print(f"    +{off:3d}: {w.hex()}  u32 {u1},{u2}  f32 {f1:.4g},{f2:.4g}  f64 {d:.6g}")


def main(path, wanted=()):
    data = open(path, "rb").read()
    arr_idx = None
    arr_name = ""
    n = 0
    for ev, raw in events(data):
        if ev == 99 and len(raw) == 2:
            arr_idx = struct.unpack("<H", raw)[0]
            arr_name = ""
            print(f"event 99  NewArrangement index {arr_idx}")
        elif ev == 241:
            arr_name = text(raw)
            print(f"event 241 arrangement name: {arr_name!r}")
        elif ev == 233:
            n += 1
            items = list(playlist_items(raw))
            pats = sorted({r[1] for _, _, r in items if r[0] == "pattern"})
            chans = sorted({r[1] for _, _, r in items if r[0] == "channel"})
            last = max((p + l for p, l, _ in items), default=0)
            print(f"event 233 Playlist #{n}: arrangement={arr_idx} name={arr_name!r} "
                  f"bytes={len(raw)} items={len(items)} ends_at_tick={last}")
            print(f"    patterns used ({len(pats)}): {pats}")
            print(f"    channel clips ({len(chans)}): {chans}")
            print(f"    overlap with the 'missing' patterns: {sorted(MISSING & set(pats))}")
            # Records the Rust decoder throws away (pattern_base != 0x5000).
            size = 60 if len(raw) % 60 == 0 else 32
            total = len(raw) // size
            odd = []
            for i in range(total):
                rec = raw[i * size:(i + 1) * size]
                if struct.unpack_from("<H", rec, 4)[0] != 0x5000:
                    odd.append((i, rec))
            print(f"    record size {size}: {total} records, {total - len(odd)} kept, "
                  f"{len(odd)} skipped (base != 0x5000)")
            import collections
            kinds = collections.Counter(rec.hex() for _, rec in odd)
            print(f"    distinct skipped records: {len(kinds)}")
            for hx, cnt in kinds.most_common(5):
                print(f"      x{cnt}: {hx[:64]}")
            stride = detect_stride(raw)
            print(f"    detected item stride: {stride} bytes "
                  f"({len(raw) // stride} items x {stride})")
            if wanted:
                dump_items(raw, stride, set(wanted))
            firsts = [(i, rec[:16].hex()) for i, rec in odd[:5]]
            print(f"    first skipped (index, first 16 bytes): {firsts}")
    print(f"\n{n} playlist event(s) in total; the Rust parser keeps only the last one.")


if __name__ == "__main__":
    # extra args like c87 p15 dump the full records of those playlist items
    want = [(a[0], int(a[1:])) for a in sys.argv[2:]]
    main(sys.argv[1], want)
