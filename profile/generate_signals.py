#!/usr/bin/env python3
"""Generate test signals for FL effect profiling.

Produces mono 44100 Hz 16-bit WAVs in ./signals/:
  ramp.wav    - linear sweep from -1.0 to +1.0 over 10 s (waveshapers)
  impulse.wav - single sample at 1.0 at 1 s, silence elsewhere (IR capture)
  sweep.wav   - log sine sweep 20 Hz -> 20 kHz over 10 s (filters / exciters)
  step.wav    - step envelope 0 / 0.5 / 1.0 / 0 (compressors)

Each WAV has a 0.5 s lead and 0.5 s tail of silence so any DC blocker
or stateful pre-stage settles before and after the interesting region.
"""
import wave
import struct
import math
from pathlib import Path

SR = 44100
OUT = Path(__file__).parent / "signals"
OUT.mkdir(exist_ok=True)


def write_wav(name: str, samples: list[float]) -> None:
    path = OUT / name
    with wave.open(str(path), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(SR)
        frames = bytearray()
        for s in samples:
            v = max(-1.0, min(1.0, s))
            frames += struct.pack("<h", int(v * 32767))
        w.writeframes(bytes(frames))
    print(f"wrote {path} ({len(samples)} samples, {len(samples)/SR:.2f} s)")


def ramp() -> list[float]:
    n = SR * 10
    lead = [0.0] * (SR // 2)
    tail = [0.0] * (SR // 2)
    body = [-1.0 + 2.0 * i / (n - 1) for i in range(n)]
    return lead + body + tail


def impulse() -> list[float]:
    n = SR * 3
    s = [0.0] * n
    s[SR] = 1.0
    return s


def sweep() -> list[float]:
    n = SR * 10
    f0, f1 = 20.0, 20000.0
    T = n / SR
    k = math.log(f1 / f0) / T
    out = []
    for i in range(n):
        t = i / SR
        phase = (2 * math.pi * f0 / k) * (math.exp(k * t) - 1.0)
        out.append(0.5 * math.sin(phase))
    return out


def step() -> list[float]:
    return (
        [0.0] * SR
        + [0.5] * SR
        + [1.0] * SR
        + [0.0] * SR
    )


if __name__ == "__main__":
    write_wav("ramp.wav", ramp())
    write_wav("impulse.wav", impulse())
    write_wav("sweep.wav", sweep())
    write_wav("step.wav", step())
