#!/usr/bin/env python3
"""Compare MB_DUMP_DIR dumps of the C++ reference and the Rust port.

Usage: dumpdiff.py REF_DIR PORT_DIR

Dumps are raw arrays named NAME_WxH.EXT (u8/u16/f32). Stages are reported in
pipeline order; the first mismatching stage is where to look.
"""

import re
import sys
from pathlib import Path

import numpy as np

STAGES = ["img", "mask", "shrink", "laplace", "blend", "collapsed", "wrapped", "corrected"]
DTYPES = {"u8": np.uint8, "u16": np.uint16, "f32": np.float32}
NAME = re.compile(r"^(?P<name>.+)_(?P<w>\d+)x(?P<h>\d+)\.(?P<ext>\w+)$")


def load(d):
    out = {}
    for p in Path(d).iterdir():
        m = NAME.match(p.name)
        if m:
            arr = np.fromfile(p, DTYPES[m["ext"]])
            out[m["name"]] = arr.reshape(int(m["h"]), int(m["w"]))
    return out


def stage_of(name):
    for i, s in enumerate(STAGES):
        if name.startswith(s):
            return i
    return len(STAGES)


def main():
    ref, port = load(sys.argv[1]), load(sys.argv[2])
    first_bad = None
    for name in sorted(ref, key=lambda n: (stage_of(n), n)):
        r = ref[name]
        p = port.get(name)
        if p is None:
            print(f"MISSING  {name}")
            first_bad = first_bad or name
            continue
        if r.shape != p.shape:
            print(f"SHAPE    {name}: ref {r.shape} port {p.shape}")
            first_bad = first_bad or name
            continue
        if r.dtype == np.float32:
            same = np.array_equal(r.view(np.uint32), p.view(np.uint32)) or np.array_equal(r, p)
        else:
            same = np.array_equal(r, p)
        if same:
            print(f"ok       {name}")
            continue
        diff = np.abs(r.astype(np.float64) - p.astype(np.float64))
        ys, xs = np.nonzero(diff > 0)
        print(f"DIFF     {name}: {len(ys)} values differ, max {diff.max():.6g}, first at (y={ys[0]}, x={xs[0]}) ref {r[ys[0], xs[0]]!r} port {p[ys[0], xs[0]]!r}")
        first_bad = first_bad or name
    extra = sorted(set(port) - set(ref))
    if extra:
        print("EXTRA in port:", ", ".join(extra))
    print("\nFIRST MISMATCH:", first_bad or "none")
    return 1 if first_bad else 0


if __name__ == "__main__":
    sys.exit(main())
