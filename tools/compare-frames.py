#!/usr/bin/env python3
"""Compare the frames of a recomp run with a Dolphin reference of the same game.

Usage: tools/compare-frames.py --recomp DIR --dolphin AVI|DIR [--first N] [--skip K] [--out DIR]

  --dolphin ...   Dolphin's frame dump: the directory it writes with `DumpFrames = True`
                  (tools/dolphin-run.sh --dump-frames: <user>/Dump/Frames/framedump_N.png), or an AVI
                  PNGs written by `ffmpeg` from an AVI (`framedump_0.avi`) are made next to it.
  --first N       how many recomp frames to compare (default 300), starting after --skip K (default 0).
  (the frames are matched by dynamic time warping, not by a fixed offset)
  --out DIR       write side by side images (recomp | Dolphin | difference x4) of the worst frames.
  --min-psnr DB   exit 1 when the median PSNR is below this (default 25).

The two runs do not keep step (a screen lasts a different number of frames in each), so the recomp frames
are matched to Dolphin's by dynamic time warping on 80x60 pictures; then the two sides are scaled to
320x240 (the game is 4:3; Dolphin's dump and ours differ in size) before the PSNR is taken. A fade or a
scene that differs shows as a low PSNR; filtering differences give 25-35 dB; identical pictures 40+.
Needs numpy and Pillow (`.venv-tools`) and ffmpeg for an AVI.
"""
import argparse
import re
import subprocess
import sys
from pathlib import Path

import numpy as np
from PIL import Image

SIZE = (320, 240)
SMALL = (80, 60)


def recomp_frames(d):
    out = []
    for p in Path(d).iterdir():
        m = re.fullmatch(r"frame_(\d+)\.png", p.name)
        if m:
            out.append((int(m.group(1)), p))
    return [p for _, p in sorted(out)]


def numbered(paths):
    """Paths in the order of the number in their names (framedump_2 before framedump_10)."""
    return sorted(paths, key=lambda p: int(re.search(r"(\d+)", p.stem).group(1)))


def dolphin_frames(src):
    src = Path(src)
    if src.is_dir():
        return numbered(src.glob("*.png"))
    out = src.with_suffix(".frames")
    if not out.is_dir() or not any(out.iterdir()):
        out.mkdir(exist_ok=True)
        subprocess.run(
            ["ffmpeg", "-v", "error", "-i", str(src), "-vsync", "0", str(out / "%06d.png")],
            check=True,
        )
    return numbered(out.glob("*.png"))


def load(path):
    return Image.open(path).convert("RGB").resize(SIZE, Image.BILINEAR)


def small(img):
    return np.asarray(img.convert("L").resize(SMALL, Image.BOX), dtype=np.float32)


def psnr(a, b):
    mse = np.mean((a.astype(np.float32) - b.astype(np.float32)) ** 2)
    return 99.0 if mse == 0 else 10 * np.log10(255.0**2 / mse)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--recomp", required=True)
    ap.add_argument("--dolphin", required=True)
    ap.add_argument("--first", type=int, default=300)
    ap.add_argument("--skip", type=int, default=0)
    # (no alignment options: see below)
    ap.add_argument("--out")
    ap.add_argument("--min-psnr", type=float, default=25.0)
    a = ap.parse_args()

    ours = recomp_frames(a.recomp)
    theirs = dolphin_frames(a.dolphin)
    if not ours or not theirs:
        sys.exit(f"no frames: recomp {len(ours)}, Dolphin {len(theirs)}")
    mine = ours[a.skip : a.skip + a.first]
    print(f"recomp: {len(ours)} frames, comparing {len(mine)}; Dolphin: {len(theirs)} frames")

    s_ours = [small(load(p)) for p in mine]
    s_theirs = [small(load(p)) for p in theirs]

    # The runs do not keep step (a screen lasts a different number of frames in each, fades start at other
    # moments), so a constant offset cannot align them: dynamic time warping finds the monotone matching
    # of the recomp frames onto a stretch of the Dolphin frames that makes the pictures most alike.
    A = np.stack([t.ravel() for t in s_ours])
    B = np.stack([t.ravel() for t in s_theirs])
    cost = np.sqrt(np.maximum((A**2).sum(1)[:, None] + (B**2).sum(1)[None, :] - 2 * A @ B.T, 0)) / A.shape[1] ** 0.5
    n, m = cost.shape
    acc = np.full((n, m), np.inf)
    acc[0] = cost[0]  # the recomp run may start anywhere in Dolphin's
    for i in range(1, n):
        prev = acc[i - 1]
        # from above (the recomp frame advances, Dolphin's stays) or diagonally
        best = np.minimum(prev, np.concatenate(([np.inf], prev[:-1])))
        row = cost[i] + best
        # from the left (Dolphin's frame advances, the recomp's stays): a running minimum along the row
        for j in range(1, m):
            if row[j - 1] + cost[i, j] < row[j]:
                row[j] = row[j - 1] + cost[i, j]
        acc[i] = row
    j = int(np.argmin(acc[-1]))
    path = {}
    i = n - 1
    while True:
        path.setdefault(i, []).append(j)
        if i == 0:
            break
        cands = [(acc[i - 1][j], i - 1, j)]
        if j > 0:
            cands += [(acc[i - 1][j - 1], i - 1, j - 1), (acc[i][j - 1], i, j - 1)]
        _, i, j = min(cands, key=lambda c: c[0])
    match = {i: min(js) + (max(js) - min(js)) // 2 for i, js in path.items()}
    first_j, last_j = min(match.values()), max(match.values())
    print(f"matched onto Dolphin frames {first_j} to {last_j} (mean difference {acc[-1][j] / n:.1f}/255 at 80x60)")

    rows = []
    for i, p in enumerate(mine):
        j = match[i]
        mine_img, theirs_img = load(p), load(theirs[j])
        rows.append((i + a.skip, j, psnr(np.asarray(mine_img), np.asarray(theirs_img)), mine_img, theirs_img))

    ps = np.array([r[2] for r in rows])
    print("recomp frame  Dolphin frame  PSNR dB")
    for r in rows[:: max(1, len(rows) // 25)]:
        print(f"{r[0]:12d}  {r[1]:13d}  {r[2]:7.1f}")
    print(
        f"{len(rows)} frames: median {np.median(ps):.1f} dB, worst decile {np.percentile(ps, 10):.1f} dB, "
        f"min {ps.min():.1f} dB, {int((ps < a.min_psnr).sum())} below {a.min_psnr:.0f} dB"
    )
    if a.out:
        out = Path(a.out)
        out.mkdir(parents=True, exist_ok=True)
        for r in sorted(rows, key=lambda r: r[2])[:12]:
            diff = np.clip(np.abs(np.asarray(r[3], np.int16) - np.asarray(r[4], np.int16)) * 4, 0, 255)
            sheet = Image.new("RGB", (SIZE[0] * 3, SIZE[1]))
            for k, im in enumerate((r[3], r[4], Image.fromarray(diff.astype(np.uint8)))):
                sheet.paste(im, (k * SIZE[0], 0))
            sheet.save(out / f"worst_{r[0]:05d}_vs_{r[1]:05d}_{r[2]:.0f}dB.png")
        print(f"wrote the 12 worst frames to {out}")
    sys.exit(0 if np.median(ps) >= a.min_psnr else 1)


if __name__ == "__main__":
    main()
