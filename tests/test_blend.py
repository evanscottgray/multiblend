"""Behavioural invariants of the blend.

These hold for any correct implementation, independent of the exact
pyramid arithmetic, so they are the primary contract for a port.
"""

import numpy as np
import pytest

import scenes
from mb import (circle_mask, flat, gradient, read, read_palette_png, run_ok, with_alpha,
                write_tiff)


def maxdiff(a, b, mask=None):
    d = np.abs(a.astype(np.int64) - b.astype(np.int64))
    if d.ndim == 3:
        d = d.max(axis=-1)
    if mask is not None:
        d = d[mask]
    return int(d.max()) if d.size else 0


def bbox(mask):
    ys, xs = np.nonzero(mask)
    return slice(ys.min(), ys.max() + 1), slice(xs.min(), xs.max() + 1)


# ---------------------------------------------------------------------------
# Single image (no blending; exercises load, trim, inpaint, output)
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("bpp", [8, 16])
def test_single_opaque_image_passes_through(work, bpp):
    g = gradient(90, 60, bpp, seed=3)
    write_tiff("in.tif", g)
    run_ok("--no-dither", "-o", "out.tif", "in.tif")
    out = read("out.tif")
    assert out.shape == g.shape  # no alpha channel when everything is covered
    assert out.dtype == g.dtype
    assert maxdiff(out, g) == 0


def test_single_8bit_image_is_exact_even_with_dither(work):
    g = gradient(90, 60, 8, seed=3)
    write_tiff("in.tif", g)
    run_ok("-o", "out.tif", "in.tif")
    assert maxdiff(read("out.tif"), g) == 0


def test_single_16bit_image_dither_moves_values_by_at_most_one(work):
    # The ordered dither offsets (up to 0.4999) are added in float32, where
    # large 16-bit values round them to 0.5; round-half-even then moves odd
    # values up by one. A port that computes in float32 reproduces this.
    g = gradient(90, 60, 16, seed=3)
    write_tiff("in.tif", g)
    run_ok("-o", "out.tif", "in.tif")
    assert maxdiff(read("out.tif"), g) <= 1


@pytest.mark.parametrize("bpp", [8, 16])
def test_single_image_is_trimmed_to_opaque_bbox(work, bpp):
    g = gradient(90, 60, bpp, seed=3)
    m = circle_mask(90, 60, cx=50, cy=25, r=20)
    write_tiff("in.tif", with_alpha(g, m))
    run_ok("--no-dither", "-o", "out.tif", "in.tif")
    out = read("out.tif")
    crop = bbox(m)
    assert out.shape[:2] == m[crop].shape
    maxv = np.iinfo(out.dtype).max
    # alpha is exactly the input's opaque region, binary
    assert np.array_equal(out[..., 3] == maxv, m[crop])
    assert set(np.unique(out[..., 3])) <= {0, maxv}
    # colour is preserved where opaque, and zeroed where transparent
    assert maxdiff(out[..., :3], g[crop], m[crop]) == 0
    assert out[..., :3][~m[crop]].max() == 0


def test_partially_transparent_pixels_are_treated_as_transparent(work):
    g = gradient(40, 30, seed=4)
    a = np.full((30, 40), 255, np.uint8)
    a[:, 20:] = 254  # anything below full opacity is excluded
    write_tiff("in.tif", np.concatenate([g, a[..., None]], axis=-1))
    run_ok("-o", "out.tif", "in.tif")
    out = read("out.tif")
    assert out.shape == (30, 20, 3)
    assert maxdiff(out, g[:, :20]) == 0


def test_output_position_tags_are_shared_by_all_inputs_offset(work):
    # Moving every input by the same amount only changes the output's position.
    args = scenes.two_circles(work)
    run_ok("-o", "a_out.tif", *args)
    write_tiff("A2.tif", read("A.tif"), pos=(30, 20))
    write_tiff("B2.tif", read("B.tif"), pos=(80, 30))
    run_ok("-o", "b_out.tif", "A2.tif", "B2.tif")
    assert np.array_equal(read("a_out.tif"), read("b_out.tif"))


# ---------------------------------------------------------------------------
# Multiple images
# ---------------------------------------------------------------------------

def test_disjoint_images_are_reproduced_exactly(work):
    a, b = gradient(90, 60, seed=1), gradient(90, 60, seed=2)
    write_tiff("a.tif", a)
    write_tiff("b.tif", b, pos=(200, 30))
    run_ok("-o", "out.tif", "a.tif", "b.tif")
    out = read("out.tif")
    assert out.shape == (90, 290, 4)
    assert maxdiff(out[:60, :90, :3], a) == 0
    assert maxdiff(out[30:, 200:, :3], b) == 0
    cover = np.zeros((90, 290), bool)
    cover[:60, :90] = True
    cover[30:, 200:] = True
    assert np.array_equal(out[..., 3] == 255, cover)
    assert out[~cover].max() == 0


@pytest.mark.parametrize("n", [2, 3, 5])
def test_identical_images_blend_to_themselves(work, n):
    g = gradient(90, 60, seed=5)
    for i in range(n):
        write_tiff(f"i{i}.tif", g)
    run_ok("-o", "out.tif", *[f"i{i}.tif" for i in range(n)])
    assert maxdiff(read("out.tif"), g) == 0


def test_same_content_at_consistent_offsets_is_seamless(work):
    # Two crops of one larger image placed at their true offsets: blending must
    # not introduce visible change anywhere.
    big = gradient(160, 90, seed=6)
    write_tiff("l.tif", big[:, :100], pos=(0, 0))
    write_tiff("r.tif", big[:, 60:], pos=(60, 0))
    run_ok("-o", "out.tif", "l.tif", "r.tif")
    assert maxdiff(read("out.tif"), big) <= 1


def test_alpha_is_union_of_input_coverage(work):
    args = scenes.three_circles(work)
    run_ok("-o", "out.tif", *args)
    out = read("out.tif")
    cover = np.zeros(out.shape[:2], bool)
    for f in args:
        src = read(f)
        m = src[..., 3] == 255
        crop = bbox(m)
        # inputs are placed relative to the tightest trimmed bbox
        cover_i = np.zeros_like(cover)
        cover_i[: m[crop].shape[0], : m[crop].shape[1]] = m[crop]
        cover |= np.roll(cover_i, _offset(f, args, work), axis=(0, 1))
    assert np.array_equal(out[..., 3] == 255, cover)


def _offset(f, args, work):
    # Offsets of the trimmed images relative to the output origin.
    pos = {"A.tif": (0, 0), "B.tif": (50, 10), "C.tif": (25, 40)}
    trims = {}
    for g in args:
        m = read(work / g)[..., 3] == 255
        ys, xs = bbox(m)
        trims[g] = (pos[g][1] + ys.start, pos[g][0] + xs.start)
    oy = min(t[0] for t in trims.values())
    ox = min(t[1] for t in trims.values())
    return trims[f][0] - oy, trims[f][1] - ox


def test_blend_transition_is_smooth(work):
    # Across an exposure step, multi-band blending should spread the transition
    # rather than leaving a hard edge.
    args = scenes.exposure_step(work)
    run_ok("-o", "out.tif", *args)
    out = read("out.tif").astype(np.int64)
    row = out[40, :, 1]
    steps = np.abs(np.diff(row))
    base = gradient(120, 80, seed=50, noise=False).astype(np.int64)[40, :, 1]
    content_steps = np.abs(np.diff(base))
    # the 80-level exposure jump must not appear as a single-pixel step
    assert (steps - content_steps).max() < 20
    # left edge is the dark image, right edge the bright one
    assert abs((out[40, 5, 1] - base[5]) + 40) <= 2
    assert abs((out[40, 115, 1] - base[115]) - 40) <= 2


def test_blend_is_deterministic(work):
    args = scenes.many_tiles(work)
    run_ok("-o", "r1.tif", *args)
    run_ok("-o", "r2.tif", *args)
    run_ok("-o", "r3.tif", *args)
    assert np.array_equal(read("r1.tif"), read("r2.tif"))
    assert np.array_equal(read("r1.tif"), read("r3.tif"))


def test_input_order_only_matters_through_seams(work):
    # Disjoint images: order cannot matter.
    write_tiff("a.tif", gradient(50, 40, seed=1))
    write_tiff("b.tif", gradient(50, 40, seed=2), pos=(100, 0))
    run_ok("-o", "ab.tif", "a.tif", "b.tif")
    run_ok("-o", "ba.tif", "b.tif", "a.tif")
    assert np.array_equal(read("ab.tif"), read("ba.tif"))


def test_same_footprint_prefers_first_image(work):
    a, b = gradient(90, 60, seed=1), gradient(90, 60, seed=2)
    write_tiff("a.tif", a)
    write_tiff("b.tif", b)
    run_ok("-o", "out.tif", "a.tif", "b.tif")
    assert maxdiff(read("out.tif"), a) == 0


def test_flat_colours_blend_to_flat_average_transition(work):
    write_tiff("a.tif", flat(80, 40, (200, 100, 50)))
    write_tiff("b.tif", flat(80, 40, (100, 100, 150)), pos=(40, 0))
    run_ok("-o", "out.tif", "a.tif", "b.tif")
    out = read("out.tif").astype(int)
    assert out.shape == (40, 120, 3)
    # rows are identical (the problem is horizontally symmetric)
    assert maxdiff(out, np.broadcast_to(out[:1], out.shape)) <= 1
    # green is constant; red/blue move monotonically between the two colours
    assert maxdiff(out[..., 1], np.full(out.shape[:2], 100)) <= 1
    r = out[20, :, 0]
    assert abs(r[0] - 200) <= 1 and abs(r[-1] - 100) <= 1
    assert (np.diff(r) <= 1).all()


def test_gamma_changes_blend_but_not_disjoint_regions(work):
    a, b = gradient(90, 60, seed=1), gradient(90, 60, seed=2)
    write_tiff("a.tif", a)
    write_tiff("b.tif", b, pos=(200, 0))
    run_ok("--gamma", "-o", "out.tif", "a.tif", "b.tif")
    out = read("out.tif")
    assert maxdiff(out[:, :90, :3], a) <= 1
    assert maxdiff(out[:, 200:, :3], b) <= 1


def test_gamma_blend_differs_from_linear(work):
    args = scenes.exposure_step(work)
    run_ok("-o", "lin.tif", *args)
    run_ok("--gamma", "-o", "gam.tif", *args)
    assert maxdiff(read("lin.tif"), read("gam.tif")) > 0


# ---------------------------------------------------------------------------
# Seams
# ---------------------------------------------------------------------------

def test_seam_map_assigns_every_pixel_to_a_covering_image(work):
    args = scenes.three_circles(work)
    run_ok("--save-seams", "seams.png", "-o", "out.tif", *args)
    seams, palette = read_palette_png("seams.png")
    out = read("out.tif")
    assert seams.shape == out.shape[:2]
    assert set(np.unique(seams)) == {0, 1, 2}
    assert len(palette) == 256 * 3


def test_seam_map_respects_exclusive_regions(work):
    args = scenes.three_circles(work)
    run_ok("--save-seams", "seams.png", "--save-xor", "xor.png", "--no-output", *args)
    seams, _ = read_palette_png("seams.png")
    xor, _ = read_palette_png("xor.png")
    exclusive = xor != 255
    # wherever only one image covers a pixel, the seam map must choose it
    assert np.array_equal(seams[exclusive], xor[exclusive])
    assert set(np.unique(xor)) == {0, 1, 2, 255}


def test_no_output_only_writes_seams(work):
    args = scenes.two_circles(work)
    r = run_ok("--save-seams", "seams.png", "--no-output", *args)
    assert (work / "seams.png").exists()
    assert "Blending" not in r.output
    assert sorted(p.name for p in work.iterdir()) == ["A.tif", "B.tif", "seams.png"]


def test_seam_save_then_load_reproduces_output(work):
    args = scenes.three_circles(work)
    run_ok("--save-seams", "seams.png", "-o", "saved.tif", *args)
    run_ok("--load-seams", "seams.png", "-o", "loaded.tif", *args)
    assert np.array_equal(read("saved.tif"), read("loaded.tif"))


def test_edited_seams_change_output(work):
    from PIL import Image

    args = scenes.two_circles(work)
    run_ok("--save-seams", "seams.png", "-o", "orig.tif", *args)
    with Image.open("seams.png") as im:
        im = im.copy()
    px = np.array(im)
    px[:] = 0  # give everything to image 0
    edited = Image.fromarray(px, "P")
    edited.putpalette(im.getpalette())
    edited.save("edited.png")
    run_ok("--load-seams", "edited.png", "-o", "edited.tif", *args)
    orig, new = read("orig.tif"), read("edited.tif")
    assert np.array_equal(orig[..., 3], new[..., 3])
    assert maxdiff(orig, new) > 10


@pytest.mark.reference_bug(reason="Image::seam_present is never initialised (image.cpp:48); the warning depends on heap garbage", strict=False)
def test_fully_obscured_image_warns(work):
    write_tiff("big.tif", gradient(100, 70, seed=4))
    write_tiff("small.tif", gradient(10, 10, seed=3), pos=(40, 30))
    r = run_ok("-o", "out.tif", "big.tif", "small.tif")
    assert "small.tif is fully obscured" in r.output


# ---------------------------------------------------------------------------
# Wrapping
# ---------------------------------------------------------------------------

def _edge_jump(img, axis):
    img = img.astype(np.int64)[..., :3]
    if axis == 1:
        return np.abs(img[:, 0] - img[:, -1]).mean()
    return np.abs(img[0] - img[-1]).mean()


@pytest.mark.parametrize("mode", ["-w", "--wrap", "--wrap=h", "--wrap=horizontal"])
def test_horizontal_wrap_joins_left_and_right_edges(work, mode):
    args = scenes.panorama_strip(work)
    run_ok("-o", "plain.tif", *args)
    run_ok(mode, "-o", "wrapped.tif", *args)
    plain, wrapped = read("plain.tif"), read("wrapped.tif")
    assert plain.shape == wrapped.shape
    assert np.array_equal(plain[..., 3], wrapped[..., 3])
    assert _edge_jump(wrapped, 1) < _edge_jump(plain, 1) / 3


@pytest.mark.parametrize("mode", ["--wrap=v", "--wrap=vertical"])
def test_vertical_wrap_joins_top_and_bottom_edges(work, mode):
    args = scenes.vertical_strip(work)
    run_ok("-o", "plain.tif", *args)
    run_ok(mode, "-o", "wrapped.tif", *args)
    plain, wrapped = read("plain.tif"), read("wrapped.tif")
    assert _edge_jump(wrapped, 0) < _edge_jump(plain, 0) / 3


@pytest.mark.parametrize("mode", ["--wrap=none", "--wrap=open"])
def test_wrap_none_is_default(work, mode):
    args = scenes.panorama_strip(work)
    run_ok("-o", "plain.tif", *args)
    run_ok(mode, "-o", "none.tif", *args)
    assert np.array_equal(read("plain.tif"), read("none.tif"))


def test_single_image_wrap(work):
    g = gradient(80, 40, seed=9)
    write_tiff("in.tif", g)
    r = run_ok("--wrap=h", "-o", "out.tif", "in.tif")
    assert "Wrapping" in r.output
    out = read("out.tif")
    assert _edge_jump(out, 1) < _edge_jump(g, 1) / 3


# ---------------------------------------------------------------------------
# Bit depth
# ---------------------------------------------------------------------------

def test_16bit_inputs_produce_16bit_output(work):
    args = scenes.two_circles(work, bpp=16)
    run_ok("-o", "out.tif", *args)
    assert read("out.tif").dtype == np.uint16


def test_16bit_input_forced_to_8bit(work):
    g = gradient(60, 40, 16, seed=2)
    write_tiff("in.tif", g)
    run_ok("--no-dither", "-d", "8", "-o", "out.tif", "in.tif")
    out = read("out.tif")
    assert out.dtype == np.uint8
    assert maxdiff(out, np.rint(g / 257.0)) <= 1


def test_8bit_input_promoted_to_16bit_single(work):
    g = gradient(60, 40, 8, seed=2)
    write_tiff("in.tif", g)
    run_ok("--no-dither", "-d", "16", "-o", "out.tif", "in.tif")
    out = read("out.tif")
    assert out.dtype == np.uint16
    assert maxdiff(out, g.astype(np.uint32) * 257) == 0


def test_16bit_output_forced_to_8bit_for_jpeg(work):
    g = gradient(60, 40, 16, seed=2)
    write_tiff("in.tif", g)
    r = run_ok("-o", "out.jpg", "in.tif")
    assert "8bpp output forced by JPEG" in r.output
    assert read("out.jpg").dtype == np.uint8


# ---------------------------------------------------------------------------
# Dither
# ---------------------------------------------------------------------------

def test_no_dither_changes_only_rounding(work):
    args = scenes.three_circles(work)
    run_ok("-o", "d.tif", *args)
    run_ok("--no-dither", "-o", "nd.tif", *args)
    d, nd = read("d.tif"), read("nd.tif")
    assert maxdiff(d, nd) == 1
    assert np.array_equal(d[..., 3], nd[..., 3])
