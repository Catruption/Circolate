#!/usr/bin/env python3
import struct, sys

AUDIO_CLIP_IIDS = [3, 37, 49, 66, 83]


def read_events(data):
    assert data[:4] == b"FLhd", "not an FLP"
    hdr = struct.unpack_from("<I", data, 4)[0]
    ppq = struct.unpack_from("<H", data, 12)[0]
    p = 8 + hdr
    assert data[p:p + 4] == b"FLdt"
    length = struct.unpack_from("<I", data, p + 4)[0]
    p += 8
    end = min(p + length, len(data))
    evs = []
    while p < end:
        ev = data[p]; p += 1
        if ev < 64: size = 1
        elif ev < 128: size = 2
        elif ev < 192: size = 4
        else:
            size = 0; shift = 0
            while True:
                b = data[p]; p += 1
                size |= (b & 0x7F) << shift; shift += 7
                if not b & 0x80: break
        evs.append((ev, data[p:p + size])); p += size
    return ppq, evs


def hexrows(b, width=20, limit=240):
    for i in range(0, min(len(b), limit), width):
        print("   %4d: %s" % (i, " ".join("%02x" % x for x in b[i:i + width])))


def main(path):
    data = open(path, "rb").read()
    ppq, evs = read_events(data)
    ver = next((e[1].split(b"\0")[0].decode("latin1") for e in evs if e[0] == 199), "?")
    print("version %s  ppq %d  events %d" % (ver, ppq, len(evs)))

    pls = [raw for ev, raw in evs if ev == 233]
    print("\n== playlist events (id 233): %d" % len(pls))
    for n, raw in enumerate(pls):
        L = len(raw)
        print("event #%d length %d   %%32=%d  %%60=%d  %%24=%d" % (n, L, L % 32, L % 60, L % 24))
        print("  first 240 bytes (20 per row):")
        hexrows(raw)
        for stride in (32, 60):
            recs = L // stride
            ok = 0
            for i in range(recs):
                pos, base, idx, ln = struct.unpack_from("<IHHI", raw, i * stride)
                if pos < 500000 and ln < 500000:
                    ok += 1
            print("  stride %d: %d records, %d with sane pos/len at offsets 0/8" % (stride, recs, ok))
        print("  where the audio-clip channel ids appear (u16, even offsets):")
        for iid in AUDIO_CLIP_IIDS:
            hits = [p for p in range(0, L - 1, 2) if struct.unpack_from("<H", raw, p)[0] == iid]
            print("   chan %d: %d hits; offsets mod 60 of first 12: %s" %
                  (iid, len(hits), [p % 60 for p in hits[:12]]))
            for p in hits[:2]:
                s = max(0, p - 8)
                print("       @%d: %s" % (p, " ".join("%02x" % x for x in raw[s:s + 28])))

    print("\n== pattern note events (id 224)")
    pat = None; bad_total = 0
    for ev, raw in evs:
        if ev == 65:
            pat = struct.unpack_from("<H", raw, 0)[0]
        elif ev == 224:
            n = len(raw) // 24
            bad = [i for i in range(n) if struct.unpack_from("<I", raw, i * 24)[0] > 500000]
            if raw and (len(raw) % 24 or bad):
                bad_total += len(bad)
                print(" pattern %s: len %d (%%24=%d) notes %d bad %d" % (pat, len(raw), len(raw) % 24, n, len(bad)))
                if bad:
                    i = bad[0]
                    print("    first bad @record %d: %s" % (i, " ".join("%02x" % x for x in raw[i * 24:i * 24 + 24])))
    print(" total bad notes: %d" % bad_total)


if __name__ == "__main__":
    main(sys.argv[1])
