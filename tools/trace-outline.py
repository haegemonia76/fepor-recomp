#!/usr/bin/env python3
"""Trace the outline of a controller from a photo, to draw it in the overlay (ADR 0020).

    tools/trace-outline.py PHOTO --mode alpha|white|dark|border|saturated --name XBOX [--threshold N] [--points 160] [--grid out.png]

Only the silhouette is kept: the photo's pixels, colours and artwork are not. The outline is simplified to
`--points` points, normalised to a bounding box that is 1.0 wide, and printed as a Rust constant for
`crates/overlay/src/outlines.rs`. `--grid` also writes the photo cropped to the outline with a labelled grid
(coordinates in the same normalised units times 1000) to read the positions of the buttons from.

Needs numpy and Pillow (`.venv-tools`) and `potrace` on the PATH.
"""
import argparse
import re
import subprocess
import sys
import tempfile

import numpy as np
from PIL import Image, ImageDraw, ImageFilter


def mask_of(img, mode, threshold, close=0, close_top=0, opening=25):
    rgb = np.asarray(img.convert("RGB")).astype(np.int32)
    if mode == "alpha":
        m = np.asarray(img.convert("RGBA"))[..., 3] > 128
    elif mode == "white":
        m = (255 - rgb.min(axis=2)) > (threshold or 40)
    elif mode == "dark":
        m = rgb.mean(axis=2) < (threshold or 60)
    elif mode == "border":
        # the photo's background is whatever colour runs along its border
        edge = np.concatenate([rgb[0], rgb[-1], rgb[:, 0], rgb[:, -1]])
        bg = np.median(edge, axis=0)
        m = np.abs(rgb - bg).sum(axis=2) > (threshold or 90)
    elif mode == "saturated":
        # a coloured controller on a pale background
        m = np.asarray(img.convert("RGB").convert("HSV"))[..., 1].astype(np.int32) > (threshold or 120)
    else:
        sys.exit("unknown mode")
    pil = Image.fromarray((m * 255).astype(np.uint8))
    if close_top:
        big = pil.filter(ImageFilter.MaxFilter(close_top)).filter(ImageFilter.MinFilter(close_top))
        top = int(pil.height * 0.45)
        pil.paste(big.crop((0, 0, pil.width, top)), (0, 0))
    if close:
        pil = pil.filter(ImageFilter.MaxFilter(close)).filter(ImageFilter.MinFilter(close))
    # close small gaps, open specks, keep the largest blob, fill holes
    pil = pil.filter(ImageFilter.MaxFilter(7)).filter(ImageFilter.MinFilter(7))
    pil = pil.filter(ImageFilter.MinFilter(opening)).filter(ImageFilter.MaxFilter(opening))
    a = np.asarray(pil) > 128
    # largest connected component by flood fill on a coarse grid
    h, w = a.shape
    seen = np.zeros_like(a, dtype=bool)
    best, best_n = None, 0
    for y0 in range(0, h, 4):
        for x0 in range(0, w, 4):
            if a[y0, x0] and not seen[y0, x0]:
                stack, comp = [(y0, x0)], []
                seen[y0, x0] = True
                while stack:
                    y, x = stack.pop()
                    comp.append((y, x))
                    for dy, dx in ((4, 0), (-4, 0), (0, 4), (0, -4)):
                        ny, nx = y + dy, x + dx
                        if 0 <= ny < h and 0 <= nx < w and a[ny, nx] and not seen[ny, nx]:
                            seen[ny, nx] = True
                            stack.append((ny, nx))
                if len(comp) > best_n:
                    best, best_n = comp, len(comp)
    out = np.zeros_like(a)
    if best:
        ys, xs = zip(*best)
        out[np.array(ys), np.array(xs)] = True
        pil = Image.fromarray((out * 255).astype(np.uint8)).filter(ImageFilter.MaxFilter(9))
        # fill holes: flood from the border on the inverse
        inv = np.asarray(pil) < 128
        fill = np.zeros_like(inv)
        stack = [(0, 0)]
        fill[0, 0] = True
        while stack:
            y, x = stack.pop()
            for dy, dx in ((1, 0), (-1, 0), (0, 1), (0, -1)):
                ny, nx = y + dy, x + dx
                if 0 <= ny < h and 0 <= nx < w and inv[ny, nx] and not fill[ny, nx]:
                    fill[ny, nx] = True
                    stack.append((ny, nx))
        out = ~fill
    return out


def potrace_points(mask):
    with tempfile.NamedTemporaryFile(suffix=".pbm") as f:
        Image.fromarray(((~mask) * 255).astype(np.uint8)).convert("1").save(f.name)
        svg = subprocess.run(
            ["potrace", f.name, "-s", "--flat", "-o", "-", "-t", "50", "-a", "1.2", "-O", "0.6"],
            capture_output=True,
            check=True,
        ).stdout.decode()
    d = re.search(r' d="([^"]+)"', svg).group(1)
    tok = re.findall(r"[MmCcLlZz]|-?\d+\.?\d*", d)
    pts, i, cur, cmd = [], 0, (0.0, 0.0), "M"
    while i < len(tok):
        t = tok[i]
        if re.match(r"[A-Za-z]", t):
            cmd = t
            i += 1
            if cmd in "Zz":
                break
            continue
        if cmd in "Mm" or cmd in "Ll":
            x, y = float(tok[i]), float(tok[i + 1])
            cur = (x, y) if cmd in "ML" else (cur[0] + x, cur[1] + y)
            pts.append(cur)
            i += 2
            if cmd == "M":
                cmd = "L"
            elif cmd == "m":
                cmd = "l"
        elif cmd in "Cc":
            v = [float(x) for x in tok[i : i + 6]]
            p0 = cur
            if cmd == "c":
                p1, p2, p3 = (p0[0] + v[0], p0[1] + v[1]), (p0[0] + v[2], p0[1] + v[3]), (p0[0] + v[4], p0[1] + v[5])
            else:
                p1, p2, p3 = (v[0], v[1]), (v[2], v[3]), (v[4], v[5])
            for k in range(1, 9):
                t_ = k / 8
                a, b, c, e = (1 - t_) ** 3, 3 * (1 - t_) ** 2 * t_, 3 * (1 - t_) * t_**2, t_**3
                pts.append((a * p0[0] + b * p1[0] + c * p2[0] + e * p3[0], a * p0[1] + b * p1[1] + c * p2[1] + e * p3[1]))
            cur = p3
            i += 6
    # potrace's svg has a flipped y axis in the transform: the path itself is in a y-up frame
    return pts


def resample(pts, n):
    p = np.array(pts)
    seg = np.hypot(*(np.diff(np.vstack([p, p[:1]]), axis=0).T))
    cum = np.concatenate([[0], np.cumsum(seg)])
    t = np.linspace(0, cum[-1], n, endpoint=False)
    full = np.vstack([p, p[:1]])
    return np.stack([np.interp(t, cum, full[:, 0]), np.interp(t, cum, full[:, 1])], axis=1)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("photo")
    ap.add_argument("--mode", required=True)
    ap.add_argument("--name", required=True)
    ap.add_argument("--threshold", type=int, default=0)
    ap.add_argument("--points", type=int, default=160)
    ap.add_argument("--close", type=int, default=0, help="extra closing radius (odd) to fill dark buttons and logos")
    ap.add_argument("--close-top", type=int, default=0, help="a larger closing used only on the upper 45%% of the photo")
    ap.add_argument("--opening", type=int, default=25, help="opening radius (odd): removes cables and other thin parts")
    ap.add_argument("--grid")
    a = ap.parse_args()
    img = Image.open(a.photo)
    mask = mask_of(img, a.mode, a.threshold, a.close, a.close_top, a.opening)
    h, w = mask.shape
    # potrace's svg is in tenths of a pixel with y pointing up: back to pixels, y down
    pts = np.array(potrace_points(mask)) / 10.0
    pts[:, 1] = h - pts[:, 1]
    pts = resample(pts, a.points)
    x0, y0 = pts.min(axis=0)
    x1, y1 = pts.max(axis=0)
    scale = 1.0 / (x1 - x0)
    norm = [((x - x0) * scale, (y - y0) * scale) for x, y in pts]
    height = (y1 - y0) * scale
    print(f"/// Traced from a photo (tools/trace-outline.py): {len(norm)} points, 1.0 wide, {height:.4f} high.")
    print(f"pub const {a.name}_HEIGHT: f32 = {height:.4f};")
    print(f"pub const {a.name}: &[(f32, f32)] = &[")
    for i in range(0, len(norm), 4):
        print("    " + " ".join(f"({x:.4f}, {y:.4f})," for x, y in norm[i : i + 4]))
    print("];")
    if a.grid:
        crop = img.convert("RGB").crop((int(x0), int(y0), int(x1), int(y1)))
        size = 1000
        crop = crop.resize((size, int(size * height)))
        d = ImageDraw.Draw(crop)
        for k in range(0, size + 1, 50):
            d.line([(k, 0), (k, crop.height)], fill=(255, 0, 255) if k % 100 == 0 else (120, 120, 255), width=1)
            if k % 100 == 0:
                d.text((k + 2, 2), str(k), fill=(255, 255, 0))
        for k in range(0, crop.height + 1, 50):
            d.line([(0, k), (size, k)], fill=(255, 0, 255) if k % 100 == 0 else (120, 120, 255), width=1)
            if k % 100 == 0:
                d.text((2, k + 2), str(k), fill=(255, 255, 0))
        # the traced outline in green
        d.line([(x * size, y * size) for x, y in norm + [norm[0]]], fill=(0, 255, 0), width=2)
        crop.save(a.grid)


if __name__ == "__main__":
    main()
