#!/usr/bin/env python3
"""Frame regression test: play scripted scenes headless and check that known screens still look right.

Usage:
  tools/regress.py record --data DIR [--card RAW] [--bin PATH] [--ref DIR]   store the reference pictures
  tools/regress.py check  --data DIR [--card RAW] [--bin PATH] [--ref DIR] [--runs N]
  tools/regress.py shots  --data DIR [--card RAW] [--bin PATH] --out DIR     only dump frames (to choose scenes)

  --data DIR   a prepared data folder (`fepor-recomp prepare`): its `disc` is used, its settings are not.
  --card RAW   a memory card image holding the player's chapter 1 save: copied for each run, never written
               back. Without it the scenes that need a save are skipped.
  --bin PATH   the game binary (default target/release/fepor-game).
  --ref DIR    where the references live (default .cache/regress, ignored by git: they are pictures of the
               game, which this repository must not contain).
  --runs N     how many times to play each script (default 2; the guest follows the wall clock, so a bug
               that depends on timing shows in some runs only: a scene must be found in every run).

Each scene names a script (the button presses that lead to it), a rectangle of the 608x448 picture and the
lowest PSNR that counts as "the same". A scene passes when some frame of the run, anywhere in it, matches
the reference in that rectangle: the runs do not keep step, so there is no fixed frame number to compare.
The rectangle leaves out what moves (typewriter text, characters). Exit status 1 when a scene is missing.

Needs numpy and Pillow (`.venv-tools`).
"""
import argparse
import concurrent.futures as cf
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

import numpy as np
from PIL import Image

REPO = Path(__file__).resolve().parent.parent
FULL = (0, 0, 608, 448)

# The two scripts. Button presses are FEPOR_PAD entries (BUTTON@frame+hold).
NEW_GAME = "A@400+8,START@1000+8,START@1400+8,A@1800+6,LEFT@2000+6,A@2100+6,A@2500+6,START@3000+6,START@3600+6" + "".join(
    f",A@{f}+6" for f in range(4200, 9000, 200)
)
SAVE = (
    "A@400+8,START@1000+8,START@1400+8,A@1800+6,LEFT@2000+6,A@2100+6,A@2500+6,START@3000+6,START@3600+6"
    + "".join(f",A@{f}+6" for f in range(4200, 8700, 250))
    + ",B@9200+6,B@9400+6,B@9600+6,START@10000+6,START@10600+6"
)
SCRIPTS = {
    # no memory card: through the warning, the logos, the title and the "no card" question into the prologue
    "new": dict(pad=NEW_GAME, card=False, seconds=110),
    # the player's save: loads chapter 1, plays the introduction, opens the map menu and the victory terms
    "save": dict(pad=SAVE, card=True, seconds=230),
}

# name -> script, frame to record from, rectangle (x, y, w, h), lowest PSNR in dB
SCENES = {
    "warning": dict(script="new", at=100, rect=FULL, psnr=30),
    "title": dict(script="new", at=1000, rect=(60, 20, 490, 230), psnr=22),
    "difficulty": dict(script="new", at=2200, rect=FULL, psnr=25),
    "prologue-field": dict(script="new", at=4300, rect=(150, 150, 310, 190), psnr=18),
    "chapter-list": dict(script="save", at=1900, rect=FULL, psnr=25),
    # the tactical map in the victory-terms menu is an EFB copy made once: it was transparent, then wrong
    "victory-map": dict(script="save", at=12000, rect=(228, 110, 357, 270), psnr=20),
}


def psnr(a, b):
    mse = np.mean((a.astype(np.float32) - b.astype(np.float32)) ** 2)
    return 99.0 if mse == 0 else 10 * np.log10(255.0**2 / mse)


def load(path, rect):
    x, y, w, h = rect
    im = Image.open(path).convert("RGB").resize((608, 448)).crop((x, y, x + w, y + h))
    return np.asarray(im.resize((max(w // 4, 1), max(h // 4, 1)), Image.BILINEAR))


def play(name, args, workdir):
    """One run of a script: the frame files it dumped, in order."""
    s = SCRIPTS[name]
    data = workdir / f"data-{name}"
    data.mkdir(parents=True)
    (data / "disc").symlink_to((args.data / "disc").resolve())
    (data / "settings.toml").write_text('[video]\nframe_multiplier = 1\nresolution = 1\nstats = "off"\n')
    frames = workdir / f"frames-{name}"
    env = dict(
        os.environ,
        FEPOR_HEADLESS="1",
        FEPOR_NO_AUDIO="1",
        FEPOR_FPS_MULT="1",
        FEPOR_RES="1",
        FEPOR_PAD=s["pad"],
        FEPOR_DUMP_FRAMES=str(frames),
        FEPOR_DUMP_EVERY=str(args.every),
    )
    if s["card"]:
        card = workdir / f"card-{name}.raw"
        shutil.copy(args.card, card)
        env["FEPOR_MEMCARD_A"] = str(card)
    else:
        env["FEPOR_MEMCARD_A"] = "none"
    try:
        subprocess.run(
            [str(args.bin), "--data", str(data)],
            env=env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=s["seconds"],
        )
    except subprocess.TimeoutExpired:
        pass  # the game never quits by itself: the time is the run
    out = []
    for p in frames.glob("frame_*.png"):
        m = re.fullmatch(r"frame_(\d+)\.png", p.name)
        if m:
            out.append((int(m.group(1)), p))
    return sorted(out)


def play_all(args, scripts, workdir):
    """Every (script, run) in parallel: {script: [frames of run 1, frames of run 2, ...]}"""
    jobs = {}
    with cf.ThreadPoolExecutor() as pool:
        for name in scripts:
            for r in range(args.runs):
                jobs[(name, r)] = pool.submit(play, name, args, workdir / f"run{r}")
    res = {name: [] for name in scripts}
    for (name, r), fut in sorted(jobs.items()):
        res[name].append(fut.result())
    return res


def wanted(args):
    scripts = sorted({s["script"] for s in SCENES.values() if not SCRIPTS[s["script"]]["card"] or args.card})
    for name, s in SCENES.items():
        if SCRIPTS[s["script"]]["card"] and not args.card:
            print(f"skip {name}: needs --card")
    return scripts


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("mode", choices=["record", "check", "shots"])
    ap.add_argument("--data", type=Path, required=True)
    ap.add_argument("--card", type=Path)
    ap.add_argument("--bin", type=Path, default=REPO / "target/release/fepor-game")
    ap.add_argument("--ref", type=Path, default=REPO / ".cache/regress")
    ap.add_argument("--runs", type=int, default=2)
    ap.add_argument("--every", type=int, default=50)
    ap.add_argument("--out", type=Path)
    args = ap.parse_args()
    if not args.bin.exists():
        sys.exit(f"{args.bin} not found: cargo build --release -p fepor-game")
    if args.mode != "check":
        args.runs = 1
    if args.mode == "shots":
        args.every = 100
        if not args.out:
            sys.exit("shots needs --out")
        scripts = sorted(SCRIPTS) if args.card else ["new"]
    else:
        scripts = wanted(args)
    with tempfile.TemporaryDirectory(prefix="regress-") as tmp:
        runs = play_all(args, scripts, Path(tmp))
        if args.mode == "shots":
            args.out.mkdir(parents=True, exist_ok=True)
            for name, rr in runs.items():
                for n, p in rr[0]:
                    shutil.copy(p, args.out / f"{name}_{n:05d}.png")
            print(f"{sum(len(r[0]) for r in runs.values())} frames in {args.out}")
            return
        args.ref.mkdir(parents=True, exist_ok=True)
        bad = 0
        for name, s in SCENES.items():
            if s["script"] not in runs:
                continue
            ref = args.ref / f"{name}.png"
            if args.mode == "record":
                frames = runs[s["script"]][0]
                n, p = min(frames, key=lambda f: abs(f[0] - s["at"]))
                shutil.copy(p, ref)
                print(f"recorded {name} from frame {n}")
                continue
            if not ref.exists():
                print(f"MISSING reference for {name}: run `record` first")
                bad += 1
                continue
            want = load(ref, s["rect"])
            for r, frames in enumerate(runs[s["script"]]):
                best = max((psnr(load(p, s["rect"]), want), n) for n, p in frames)
                ok = best[0] >= s["psnr"]
                bad += not ok
                print(f"{'ok  ' if ok else 'FAIL'} {name} run {r + 1}: best {best[0]:.1f} dB at frame {best[1]} (needs {s['psnr']})")
        sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
