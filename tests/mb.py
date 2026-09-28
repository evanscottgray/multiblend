"""Helpers for the multiblend black-box test suite.

The suite drives the multiblend executable as a subprocess, so it can be
pointed at the C++ reference build or at a port via MULTIBLEND_BIN.
"""

import os
import subprocess
from dataclasses import dataclass
from pathlib import Path

import imagecodecs
import numpy as np
import tifffile

ROOT = Path(__file__).resolve().parent.parent
BIN = Path(os.environ.get("MULTIBLEND_BIN", ROOT / "build" / "multiblend")).resolve()

# The C++ build is the reference unless MULTIBLEND_BIN points elsewhere.
IS_REFERENCE = "MULTIBLEND_BIN" not in os.environ or os.environ.get("MULTIBLEND_IS_REFERENCE") == "1"

# The reference has a thread-pool startup race (threadpool.cpp: workers are
# started before their mutex pointers are assigned) that segfaults ~2% of runs.
# Against the reference only, retry runs killed by a signal so that the race
# doesn't make unrelated tests flaky. Deterministic crashes still fail every
# attempt. test_known_bugs.py covers the race itself with retries disabled.
REFERENCE_SIGNAL_RETRIES = 3

# TIFF tag ids
XPOSITION = 286
YPOSITION = 287
GEOPIXELSCALE = 33550
GEOTIEPOINTS = 33922


@dataclass
class Result:
    returncode: int
    stdout: str
    stderr: str

    @property
    def output(self):
        return self.stdout + self.stderr


def run(*args, cwd=None, timeout=120, retry_signals=True):
    """Run multiblend with the given arguments (all converted to str)."""
    attempts = REFERENCE_SIGNAL_RETRIES if (IS_REFERENCE and retry_signals) else 1
    for _ in range(attempts):
        proc = subprocess.run(
            [str(BIN), *map(str, args)],
            cwd=cwd,
            capture_output=True,
            text=True,
            errors="replace",
            timeout=timeout,
        )
        if proc.returncode >= 0:
            break
    return Result(proc.returncode, proc.stdout, proc.stderr)


def run_ok(*args, **kw):
    r = run(*args, **kw)
    assert r.returncode == 0, f"multiblend failed ({r.returncode}):\n{r.output}"
    return r


# ---------------------------------------------------------------------------
# Synthetic image generation
# ---------------------------------------------------------------------------

def gradient(w, h, bpp=8, seed=0, noise=True):
    """Smooth RGB content with some texture; deterministic for a given seed."""
    rng = np.random.default_rng(seed)
    maxv = 255 if bpp == 8 else 65535
    yy, xx = np.mgrid[0:h, 0:w].astype(np.float64)
    phase = rng.uniform(0, 2 * np.pi, size=3)
    chans = []
    for c in range(3):
        v = 0.5 + 0.35 * np.sin(xx / (7 + 5 * c) + phase[c]) * np.cos(yy / (11 + 3 * c) - phase[c])
        v += 0.1 * (xx / max(w - 1, 1)) - 0.05 * (yy / max(h - 1, 1))
        if noise:
            v += rng.normal(0, 0.02, size=(h, w))
        chans.append(np.clip(v, 0, 1))
    rgb = np.stack(chans, axis=-1) * maxv
    return np.rint(rgb).astype(np.uint8 if bpp == 8 else np.uint16)


def flat(w, h, rgb, bpp=8):
    dtype = np.uint8 if bpp == 8 else np.uint16
    return np.broadcast_to(np.array(rgb, dtype=dtype), (h, w, 3)).copy()


def with_alpha(rgb, alpha=None):
    """Append an alpha channel. `alpha` is a bool mask (True = opaque) or None (all opaque)."""
    h, w, _ = rgb.shape
    maxv = np.iinfo(rgb.dtype).max
    if alpha is None:
        a = np.full((h, w), maxv, dtype=rgb.dtype)
    else:
        a = np.where(alpha, maxv, 0).astype(rgb.dtype)
    return np.concatenate([rgb, a[..., None]], axis=-1)


def circle_mask(w, h, cx=None, cy=None, r=None):
    cx = (w - 1) / 2 if cx is None else cx
    cy = (h - 1) / 2 if cy is None else cy
    r = min(w, h) * 0.45 if r is None else r
    yy, xx = np.mgrid[0:h, 0:w]
    return (xx - cx) ** 2 + (yy - cy) ** 2 <= r * r


# ---------------------------------------------------------------------------
# Writers
# ---------------------------------------------------------------------------

def write_tiff(path, arr, pos=None, res=72.0, compression=None, rowsperstrip=None, extratags=()):
    """Write an RGB/RGBA TIFF. `pos` = (x, y) in pixels, stored as X/YPOSITION in resolution units."""
    path = Path(path)
    tags = list(extratags)
    if pos is not None:
        x, y = pos
        tags.append((XPOSITION, "2i", 1, _rational(x / res), False))
        tags.append((YPOSITION, "2i", 1, _rational(y / res), False))
    kw = {}
    if arr.shape[-1] == 4:
        kw["extrasamples"] = ["unassalpha"]
    tifffile.imwrite(
        path,
        arr,
        photometric="rgb",
        planarconfig="contig",
        resolution=(res, res) if res else None,
        compression=compression,
        rowsperstrip=rowsperstrip,
        extratags=tags,
        **kw,
    )
    return path


def _rational(v, den=10000):
    return (int(round(v * den)), den)


def write_png(path, arr):
    Path(path).write_bytes(imagecodecs.png_encode(arr))
    return Path(path)


def write_jpeg(path, arr, quality=95):
    Path(path).write_bytes(imagecodecs.jpeg8_encode(arr, level=quality))
    return Path(path)


# ---------------------------------------------------------------------------
# Readers
# ---------------------------------------------------------------------------

def read(path):
    """Read an output image into an (H, W, C) numpy array with its native dtype."""
    path = Path(path)
    ext = path.suffix.lower()
    if ext in (".tif", ".tiff"):
        return tifffile.imread(path)
    data = path.read_bytes()
    if ext == ".png":
        return imagecodecs.png_decode(data)
    if ext in (".jpg", ".jpeg"):
        return imagecodecs.jpeg8_decode(data)
    raise ValueError(path)


def read_palette_png(path):
    """Read a palettised PNG (seam/XOR maps) as raw indices, plus the palette."""
    from PIL import Image

    with Image.open(path) as im:
        assert im.mode == "P", im.mode
        return np.array(im), im.getpalette()


def tiff_tags(path):
    with tifffile.TiffFile(path) as t:
        return {tag.code: tag.value for tag in t.pages[0].tags.values()}
