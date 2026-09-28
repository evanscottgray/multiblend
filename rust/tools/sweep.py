#!/usr/bin/env python3
"""Differential sweep: every test scene x option set, C++ reference vs Rust port.

Usage: sweep.py REF_BIN PORT_BIN
Compares output pixels and seam maps exactly. Option sets that exercise known
reference bugs (8->16 depth changes) are compared separately and reported, not failed.
"""
import itertools, pathlib, subprocess, sys, tempfile
import numpy as np
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[2] / "tests"))
import mb, scenes

REF, PORT = map(lambda p: str(pathlib.Path(p).resolve()), sys.argv[1:3])
OPTS = [[], ["--gamma"], ["--no-dither"], ["--wrap=h"], ["--wrap=v"], ["-l", "2"], ["-l", "+2"], ["--wideblend"],
        ["--gamma", "--wrap=h"], ["--no-dither", "--gamma"], ["--bgr"], ["--reverse"]]
DEPTH = [["-d", "16"], ["-d", "8"]]  # reference bug #18: expected to differ when depth changes

def run(binary, args, cwd):
    for _ in range(5):  # reference startup race
        r = subprocess.run([binary, "-q", *args], cwd=cwd, capture_output=True, text=True)
        if r.returncode >= 0:
            return r
    return r

bad, total = [], 0
scene_fns = {**scenes.SCENES, "png_inputs": scenes.png_inputs, "single": lambda d: scenes.two_circles(d)[:1]}
for (name, fn), opts in itertools.product(scene_fns.items(), OPTS + DEPTH):
    d = pathlib.Path(tempfile.mkdtemp())
    args = fn(d)
    outs = []
    for b, tag in ((REF, "ref"), (PORT, "port")):
        r = run(b, [*opts, "--save-seams", f"{tag}.png", "-o", f"{tag}.tif", *args], d)
        if r.returncode:
            outs.append(None); print(f"{tag} failed: {name} {opts}: {r.stdout[-300:]}"); continue
        outs.append((mb.read(d / f"{tag}.tif"), mb.read_palette_png(d / f"{tag}.png")[0]))
    total += 1
    if None in outs:
        bad.append((name, opts, "run failed")); continue
    (ri, rs), (pi, ps) = outs
    same = ri.shape == pi.shape and np.array_equal(ri, pi) and np.array_equal(rs, ps)
    if not same:
        diff = np.abs(ri.astype(int) - pi.astype(int)) if ri.shape == pi.shape else None
        info = f"max {diff.max()} frac {(diff > 0).mean():.4%}" if diff is not None else f"shape {ri.shape} vs {pi.shape}"
        seams = "seams equal" if np.array_equal(rs, ps) else "SEAMS DIFFER"
        tag = "expected (depth fix)" if opts in DEPTH else "UNEXPECTED"
        bad.append((name, opts, f"{tag}: {info}, {seams}"))
print(f"\n{total} runs, {len(bad)} differ")
for b in bad:
    print("  ", *b)
sys.exit(1 if any("UNEXPECTED" in b[2] or "failed" in b[2] for b in bad) else 0)
