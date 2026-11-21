#!/usr/bin/env python3
"""Look for clicks and crackle in a WAV written with FEPOR_DUMP_AUDIO (or Dolphin's dspdump.wav).

Usage: tools/audio-check.py file.wav [--from SECONDS] [--to SECONDS] [--bands]

Default: per second, the peak level, clipped samples and "clicks" (second differences far above the
local level: discontinuities heard as crackle). `--bands`: share of the energy per frequency band over
the loud parts, to compare two dumps of the same scene (recomp vs Dolphin). Needs numpy (`.venv-tools`).
"""
import argparse
import wave

import numpy as np


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("wav")
    ap.add_argument("--from", dest="t0", type=float, default=0.0)
    ap.add_argument("--to", dest="t1", type=float, default=1e9)
    ap.add_argument("--bands", action="store_true", help="print energy per frequency band instead")
    a = ap.parse_args()
    with wave.open(a.wav) as w:
        rate, ch = w.getframerate(), w.getnchannels()
        raw = w.readframes(w.getnframes())
    x = np.frombuffer(raw[: len(raw) // 2 * 2], dtype="<i2").astype(np.float32)
    x = x.reshape(-1, ch)[:, 0]
    x = x[int(a.t0 * rate) : int(min(a.t1, 1e9) * rate)] if a.t1 < 1e9 or a.t0 > 0 else x
    print(f"{len(x) / rate:.1f} s at {rate} Hz, {ch} channels")
    if a.bands:
        n = 4096
        frames = x[: len(x) // n * n].reshape(-1, n) * np.hanning(n)
        spec = np.abs(np.fft.rfft(frames, axis=1)) ** 2
        loud = spec.sum(axis=1) > np.percentile(spec.sum(axis=1), 50)
        spec = spec[loud].sum(axis=0)
        f = np.fft.rfftfreq(n, 1 / rate)
        edges = [0, 500, 2000, 5000, 8000, 12000, rate / 2 + 1]
        for lo, hi in zip(edges, edges[1:]):
            share = spec[(f >= lo) & (f < hi)].sum() / spec.sum()
            print(f"{int(lo):6d}-{int(hi):6d} Hz: {share * 100:6.2f} %")
        return
    d2 = np.abs(np.diff(x, 2))
    # local level: moving RMS of the first difference over ~5 ms
    win = max(1, rate // 200)
    d1 = np.diff(x) ** 2
    level = np.sqrt(np.convolve(d1, np.ones(win) / win, mode="same")) + 50.0
    clicks = d2 > 8.0 * level[:-1]
    for s in range(int(len(x) / rate)):
        seg = slice(s * rate, (s + 1) * rate)
        seg_c = clicks[seg.start : seg.stop - 2]
        print(
            f"{s:4d}s peak {int(np.abs(x[seg]).max()):6d} clipped {int((np.abs(x[seg]) >= 32767).sum()):4d} "
            f"clicks {int(seg_c.sum()):5d}"
        )


if __name__ == "__main__":
    main()
