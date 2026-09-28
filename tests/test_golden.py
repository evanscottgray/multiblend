"""Golden-output tests against outputs recorded from the C++ reference.

Regenerate with (reference build only):

    .venv/bin/python -m pytest tests/test_golden.py --update-golden

Comparison policy (see tests/README.md):
  * alpha channel and seam maps must match exactly;
  * colour samples may differ by at most 1, in at most 0.1% (8-bit) or
    0.5% (16-bit) of samples. The reference's own -O2 build drifts from the
    -Ofast goldens by at most 0.004% / 0.22%, while changing a single blend
    coefficient by 0.8% moves over 1% of samples.
  * MULTIBLEND_GOLDEN_EXACT=1 demands bit-exact output instead.

Scenarios that exercise known reference bugs are deliberately absent; those
live in test_known_bugs.py.
"""

import hashlib
import os
from pathlib import Path

import numpy as np
import pytest

import scenes
from mb import read, read_palette_png, run_ok, tiff_tags

GOLDEN = Path(__file__).parent / "golden"

EXACT = os.environ.get("MULTIBLEND_GOLDEN_EXACT") == "1"
MAX_DIFF = 0 if EXACT else 1
MAX_DIFF_FRACTION = {1: 0.001, 2: 0.005}  # by bytes per sample


def split_seams(d):
    """two_circles plus a hand-made seam file: left half image 0, right half image 1."""
    from PIL import Image

    args = scenes.two_circles(d)
    run_ok("--save-seams", "auto.png", "--no-output", *args, cwd=d)
    with Image.open(d / "auto.png") as im:
        palette = im.getpalette()
        w, h = im.size
    px = np.zeros((h, w), np.uint8)
    px[:, w // 2:] = 1
    out = Image.fromarray(px, "P")
    out.putpalette(palette)
    out.save(d / "split.png")
    (d / "auto.png").unlink()
    return args


# name: (scene, options, output file, save seams?)
CASES = {
    "two_circles": ("two_circles", [], "out.tif", True),
    "three_circles": ("three_circles", [], "out.tif", True),
    "three_circles_nodither": ("three_circles", ["--no-dither"], "out.tif", False),
    "three_circles_wideblend": ("three_circles", ["--wideblend"], "out.tif", False),
    "two_circles_levels2": ("two_circles", ["-l", "2"], "out.tif", False),
    "two_circles_16": ("two_circles_16", [], "out.tif", True),
    "two_circles_16_gamma": ("two_circles_16", ["--gamma"], "out.tif", False),
    "two_circles_16_png": ("two_circles_16", [], "out.png", False),
    "odd_sizes": ("odd_sizes", [], "out.tif", True),
    "rgb_grid": ("rgb_grid", [], "out.tif", True),
    "exposure_step": ("exposure_step", [], "out.tif", True),
    "exposure_step_gamma": ("exposure_step", ["--gamma"], "out.tif", False),
    "panorama_wrap_h": ("panorama_strip", ["--wrap=h"], "out.tif", True),
    "panorama_png": ("panorama_strip", [], "out.png", False),
    "vertical_wrap_v": ("vertical_strip", ["--wrap=v"], "out.tif", True),
    "many_tiles": ("many_tiles", [], "out.tif", True),
    "odd_canvas_wrap_h": ("odd_canvas", ["--wrap=h"], "out.tif", True),
    "odd_canvas_wrap_v": ("odd_canvas", ["--wrap=v"], "out.tif", True),
    "many_tiles_levels_plus2": ("many_tiles", ["-l", "+2"], "out.tif", False),
    "loaded_split_seams": ("split_seams", ["--load-seams", "split.png"], "out.tif", False),
}

SCENE_FUNCS = {**scenes.SCENES, "split_seams": split_seams}


def input_digest(d, args):
    h = hashlib.sha256()
    for a in args:
        h.update(a.encode())
        p = d / a
        if p.is_file():
            arr = read(p)
            h.update(f"{arr.dtype}{arr.shape}".encode())
            h.update(np.ascontiguousarray(arr).tobytes())
            if p.suffix == ".tif":
                tags = tiff_tags(p)
                h.update(repr((tags.get(286), tags.get(287), tags.get(282), tags.get(283))).encode())
    return h.hexdigest()


def compare_colour(name, out, ref):
    assert out.shape == ref.shape, f"{name}: shape {out.shape} != golden {ref.shape}"
    assert out.dtype == ref.dtype, f"{name}: dtype {out.dtype} != golden {ref.dtype}"
    has_alpha = out.shape[-1] == 4
    if has_alpha:
        assert np.array_equal(out[..., 3], ref[..., 3]), f"{name}: alpha channel differs from golden"
    d = np.abs(out[..., :3].astype(np.int64) - ref[..., :3].astype(np.int64))
    frac = float((d > 0).mean())
    allowed = MAX_DIFF_FRACTION[out.dtype.itemsize]
    assert d.max() <= MAX_DIFF and frac <= allowed, (
        f"{name}: colour differs from golden: max {d.max()}, {frac:.3%} of samples differ "
        f"(allowed max {MAX_DIFF}, {allowed:.1%})"
    )


def produce(out_name, opts, args, save_seams):
    seam_opts = ["--save-seams", "seams.png"] if save_seams else []
    run_ok(*seam_opts, *opts, "-o", out_name, *args)
    return read(out_name), (read_palette_png("seams.png")[0] if save_seams else None)


def check(name, produced, golden):
    out, seams = produced
    compare_colour(name, out, golden["image"])
    if seams is not None:
        assert np.array_equal(seams, golden["seams"]), f"{name}: seam map differs from golden"


@pytest.mark.parametrize("name", sorted(CASES))
def test_golden(work, name, update_golden):
    scene, opts, out_name, save_seams = CASES[name]
    args = SCENE_FUNCS[scene](work)
    digest = input_digest(work, args)
    path = GOLDEN / f"{name}.npz"

    if update_golden:
        # Record the majority of three runs: the reference's thread pool can
        # (rarely) corrupt output, see FINDINGS.md.
        runs = [produce(out_name, opts, args, save_seams) for _ in range(3)]
        key = lambda r: (r[0].tobytes(), None if r[1] is None else r[1].tobytes())
        winner = max(runs, key=lambda r: sum(key(r) == key(o) for o in runs))
        assert sum(key(winner) == key(o) for o in runs) >= 2, f"{name}: no two reference runs agreed"
        out, seams = winner
        GOLDEN.mkdir(exist_ok=True)
        extra = {"seams": seams} if seams is not None else {}
        np.savez_compressed(path, image=out, inputs_digest=np.array(digest), **extra)
        pytest.skip("golden updated")

    assert path.exists(), f"missing golden {path.name}; run with --update-golden against the reference build"
    golden = np.load(path)
    assert str(golden["inputs_digest"]) == digest, (
        f"{name}: generated inputs no longer match the golden's inputs (a numpy/tifffile/imagecodecs "
        "change?). Regenerate goldens from the reference build; this is not a blend regression."
    )
    check(name, produce(out_name, opts, args, save_seams), golden)


def test_no_orphan_goldens():
    orphans = {p.stem for p in GOLDEN.glob("*.npz")} - set(CASES)
    assert not orphans, f"golden files without a test case: {sorted(orphans)}"
