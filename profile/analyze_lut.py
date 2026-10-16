#!/usr/bin/env python3
"""Convert an FL-rendered ramp response into a waveshaper LUT.

Usage:
    analyze_lut.py captures/fast_dist.wav models/fast_dist.lut.bin

Reads a mono (or stereo, averaged) 44.1 kHz WAV produced by playing
signals/ramp.wav through the effect in FL and recording with Edison.
Finds the ramp region, resamples onto a uniform [-1, 1] input grid,
and writes a binary LUT of f32 LE values.
"""
import argparse
import struct
import sys
import wave
from pathlib import Path

SR = 44100
LEAD = SR // 2        # matches generate_signals.ramp()
RAMP_LEN = SR * 10    # 10 s of sweep
TAIL = SR // 2


def read_wav_mono(path: Path) -> list[float]:
    with wave.open(str(path), "rb") as w:
        n = w.getnframes()
        ch = w.getnchannels()
        frames = w.readframes(n)
    samples = struct.unpack(f"<{n * ch}h", frames)
    if ch == 1:
        return [s / 32767.0 for s in samples]
    out = []
    for i in range(n):
        s = sum(samples[i * ch : i * ch + ch]) / ch
        out.append(s / 32767.0)
    return out


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("capture", help="FL-rendered WAV")
    ap.add_argument("out", help="output LUT bin path")
    ap.add_argument("--n", type=int, default=16384,
                    help="LUT size (default 16384; 4096 is plenty for most shapers)")
    ap.add_argument("--skip", type=int, default=LEAD)
    ap.add_argument("--length", type=int, default=RAMP_LEN)
    args = ap.parse_args()

    y = read_wav_mono(Path(args.capture))

    expected = args.skip + args.length
    if len(y) < expected:
        print(f"warning: capture shorter than expected "
              f"({len(y)} < {expected}); body will be shorter", file=sys.stderr)
        args.length = max(0, len(y) - args.skip)

    body = y[args.skip : args.skip + args.length]
    if len(body) < 16:
        print("error: capture body too short to analyze", file=sys.stderr)
        sys.exit(1)

    n_out = args.n
    lut = []
    for k in range(n_out):
        x = -1.0 + 2.0 * k / (n_out - 1)
        i = int(round((x + 1.0) * 0.5 * (len(body) - 1)))
        i = max(0, min(len(body) - 1, i))
        lut.append(body[i])

    lo = min(lut)
    hi = max(lut)
    print(f"LUT range: [{lo:+.4f}, {hi:+.4f}]")

    out_path = Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    with open(out_path, "wb") as f:
        for v in lut:
            f.write(struct.pack("<f", v))
    print(f"wrote {out_path} ({n_out} entries, {n_out * 4} bytes)")


if __name__ == "__main__":
    main()
