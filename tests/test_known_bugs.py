"""Confirmed defects in the C++ reference, written as tests of the correct behaviour.

Every test here carries @reference_bug: it is an expected failure against the
C++ build and an ordinary test against any other implementation. A port must
therefore fix all of them. Where the "correct" behaviour is a judgement call,
the assertion encodes a decision confirmed on 2026-09-28 and is flagged
"DECISION" (see "Port decisions" in tests/FINDINGS.md).

Memory-safety bugs that don't reliably crash an optimised build are run
through build/multiblend-asan when testing the reference, so that the expected
failure is deterministic; those tests skip if the ASan build is missing.
"""

import numpy as np
import pytest
import tifffile

import mb
import scenes
from mb import (GEOPIXELSCALE, GEOTIEPOINTS, XPOSITION, YPOSITION, circle_mask, gradient, read, run,
                run_ok, tiff_tags, with_alpha, write_png, write_tiff)

ASAN_BIN = mb.ROOT / "build" / "multiblend-asan"


def run_robust(*args):
    """Run the binary under test; for the reference, run the ASan build instead.

    Some of these memory errors don't crash an optimised build, so without ASan
    the reference could pass by accident and trip the strict xfail.
    """
    if mb.IS_REFERENCE:
        if not ASAN_BIN.exists():
            pytest.skip("needs build/multiblend-asan (make asan)")
        saved = mb.BIN
        mb.BIN = ASAN_BIN
        try:
            r = run(*args)
        finally:
            mb.BIN = saved
        assert "ERROR: AddressSanitizer" not in r.output, r.output[-3000:]
        return r
    return run(*args)


def assert_clean_exit(r):
    """No crash: the process exited normally (0 or a clean error)."""
    assert r.returncode in (0, 1), f"process died with {r.returncode}:\n{r.output[-2000:]}"


# ---------------------------------------------------------------------------
# Crashes and memory errors
# ---------------------------------------------------------------------------

@pytest.mark.reference_bug(reason="image.cpp:302-407 trim leaves left/top/right/bottom uninitialised with no fully-opaque pixel; segfault")
@pytest.mark.parametrize("alpha", [0, 254], ids=["fully_transparent", "alpha_254"])
def test_input_without_opaque_pixels_is_an_error(work, alpha):
    # DECISION: an input with no fully opaque pixel is a clean error naming the file.
    write_tiff("a.tif", gradient(60, 40, seed=1))
    g = with_alpha(gradient(40, 40, seed=2))
    g[..., 3] = alpha
    write_tiff("empty.tif", g)
    r = run("-o", "out.tif", "a.tif", "empty.tif")
    assert r.returncode == 1, f"expected a clean error, got {r.returncode}:\n{r.output[-2000:]}"
    assert "empty.tif" in r.output


@pytest.mark.reference_bug(reason="image.cpp:581 indexes thread_lines[(y - 2) % n] with y=1 (negative index) for 1-row images")
def test_one_row_image_with_transparency(work):
    m = np.ones((1, 50), bool)
    m[0, :5] = False
    g = gradient(50, 1, seed=3)
    write_tiff("row.tif", with_alpha(g, m))
    r = run_robust("-o", "out.tif", "row.tif")
    assert r.returncode == 0
    assert np.array_equal(read("out.tif"), g[:, 5:])


@pytest.mark.reference_bug(reason="functions.cpp:17-19 Flex sizes its first line as max(height,16)*16 bytes, less than the width*4 a line can need")
def test_wide_short_image_with_noisy_alpha(work):
    rng = np.random.default_rng(0)
    m = rng.random((4, 3000)) > 0.5
    m[:, 0] = m[:, -1] = True
    g = gradient(3000, 4, seed=4)
    write_tiff("wide.tif", with_alpha(g, m))
    r = run_robust("-o", "out.tif", "wide.tif")
    assert r.returncode == 0
    out = read("out.tif")
    assert np.array_equal(out[..., 3] == 255, m)
    assert np.array_equal(out[..., :3][m], g[m])


@pytest.mark.reference_bug(reason="tiled TIFFs are read with strip APIs (image.cpp:138, 274); segfault")
def test_tiled_tiff_input_is_an_error(work):
    # DECISION: tiled TIFFs are unsupported and rejected with a clean error naming the file.
    g = with_alpha(gradient(96, 64, seed=5), circle_mask(96, 64))
    tifffile.imwrite("tiled.tif", g, photometric="rgb", tile=(32, 32), extrasamples=["unassalpha"], resolution=(72, 72))
    r = run("-o", "out.tif", "tiled.tif")
    assert r.returncode == 1, f"expected a clean error, got {r.returncode}:\n{r.output[-2000:]}"
    assert "tiled.tif" in r.output


@pytest.mark.reference_bug(reason="-l with a large negative value yields negative blend levels; Pyramid then allocates 8-levels garbage and crashes", strict=False)
def test_excessively_negative_levels(work):
    args = scenes.two_circles(work)
    r = run("-l", "-100", "-o", "out.tif", *args)
    assert_clean_exit(r)


@pytest.mark.reference_bug(reason="threadpool.cpp:19-21 starts workers before assigning their mutex pointers; ~2% of runs segfault", strict=False)
def test_startup_is_race_free(work):
    write_tiff("a.tif", gradient(16, 16, seed=1))
    write_tiff("b.tif", gradient(16, 16, seed=2), pos=(8, 0))
    for _ in range(150):
        r = run("-q", "-o", "out.tif", "a.tif", "b.tif", retry_signals=False)
        assert r.returncode == 0, f"crashed with {r.returncode}"


@pytest.mark.reference_bug(reason="threadpool.cpp:92-111 Wait() reads the queue and busy flags without the lock, so it can return while seam/inpaint compression is still running (TSan: multiblend.cpp:950/973, image.cpp:634/646); rarely corrupts output", strict=False)
def test_output_is_deterministic(work):
    args = scenes.two_circles(work)
    run_ok("-q", "-l", "2", "-o", "first.tif", *args)
    first = read("first.tif")
    for i in range(60):
        run_ok("-q", "-l", "2", "-o", "again.tif", *args)
        assert np.array_equal(read("again.tif"), first), f"run {i + 2} differed from run 1"


# ---------------------------------------------------------------------------
# Wrong or corrupt output
# ---------------------------------------------------------------------------

@pytest.mark.reference_bug(reason="image.cpp:286-293 reads 16-bit PNG rows without png_set_swap, so samples are byte-swapped")
def test_16bit_png_input_byte_order(work):
    g = gradient(64, 48, 16, seed=3)
    write_png("in.png", g)
    run_ok("--no-dither", "-o", "out.tif", "in.png")
    assert np.array_equal(read("out.tif"), g)


@pytest.mark.reference_bug(reason="multiblend.cpp:1449-1453 offset correction scales 8->16 by 256 while pixels are scaled by 257, darkening the blend")
@pytest.mark.parametrize("gamma", [[], ["--gamma"]], ids=["linear", "gamma"])
def test_8bit_inputs_to_16bit_output_are_not_darkened(work, gamma):
    # DECISION: 8->16 promotion means x * 257 everywhere, including the blend's DC offset
    # correction (x * 66049 in the squared domain under --gamma).
    args = scenes.two_circles(work)
    run_ok(*gamma, "--no-dither", "-o", "out8.tif", *args)
    run_ok(*gamma, "--no-dither", "-d", "16", "-o", "out16.tif", *args)
    o8, o16 = read("out8.tif").astype(float), read("out16.tif").astype(float)
    covered = o8[..., 3] == 255
    bias = (o16[..., :3] - o8[..., :3] * 257)[covered].mean()
    assert abs(bias) < 64, f"8->16 output biased by {bias:.1f} (16-bit units)"


@pytest.mark.reference_bug(reason="multiblend.cpp:1449-1453 offset correction scales 16->8 by 1/256 while pixels are scaled by 1/257, brightening the blend by ~0.5 LSB")
@pytest.mark.parametrize("gamma", [[], ["--gamma"]], ids=["linear", "gamma"])
def test_16bit_inputs_to_8bit_output_are_not_brightened(work, gamma):
    # DECISION: 16->8 means / 257 everywhere (/ 66049 in the squared domain under --gamma).
    args = scenes.two_circles(work, bpp=16)
    run_ok(*gamma, "--no-dither", "-o", "out16.tif", *args)
    run_ok(*gamma, "--no-dither", "-d", "8", "-o", "out8.tif", *args)
    o16, o8 = read("out16.tif").astype(float), read("out8.tif").astype(float)
    covered = o16[..., 3] == 65535
    bias = (o8[..., :3] - o16[..., :3] / 257)[covered].mean()
    assert abs(bias) < 0.1, f"16->8 output biased by {bias:.3f} (8-bit units)"


@pytest.mark.reference_bug(reason="multiblend.cpp:1521-1522 writes a negative XPOSITION when an image has a negative position; libtiff then drops the whole directory (exit code 0)")
def test_negative_positions_are_clamped_with_a_warning(work):
    # DECISION: TIFF can't store a negative position, so X/YPOSITION are clamped to 0
    # and the user is warned. PNG/JPEG outputs carry no position, so no warning there.
    m = circle_mask(100, 70)
    write_tiff("a.tif", with_alpha(gradient(100, 70, seed=10), m))
    write_tiff("b.tif", with_alpha(gradient(100, 70, seed=11), m))
    png = run_ok("-o", "expected.png", "a.tif", "-50,-10", "b.tif")
    r = run_ok("-o", "out.tif", "a.tif", "-50,-10", "b.tif")
    assert "Negative value is illegal" not in r.output
    assert np.array_equal(read("out.tif"), read("expected.png"))
    tags = tiff_tags("out.tif")
    assert tags[XPOSITION][0] == 0 and tags[YPOSITION][0] == 0
    warnings = [l for l in r.output.splitlines() if "warning" in l.lower() and "position" in l.lower()]
    assert warnings, "expected a warning that the output position was clamped"
    assert not [l for l in png.output.splitlines() if "warning" in l.lower() and "position" in l.lower()]


@pytest.mark.reference_bug(reason="image.cpp:133-134 leaves ypos uninitialised when only XPOSITION is present", strict=False)
def test_missing_yposition_means_zero(work):
    m = circle_mask(100, 70)
    a, b = with_alpha(gradient(100, 70, seed=10), m), with_alpha(gradient(100, 70, seed=11), m)
    write_tiff("a.tif", a, pos=(0, 0))
    write_tiff("b_xy.tif", b, pos=(50, 0))
    write_tiff("b_x.tif", b, extratags=[(XPOSITION, "2i", 1, (50 * 10000 // 72, 10000), False)])
    run_ok("-o", "expected.png", "a.tif", "b_xy.tif")
    run_ok("-o", "out.png", "a.tif", "b_x.tif")
    assert np.array_equal(read("out.png"), read("expected.png"))


@pytest.mark.reference_bug(reason="grayscale/unsupported sample layouts are not validated (image.cpp:119 check commented out) and are misread as RGB")
def test_grayscale_tiff_is_an_error(work):
    # DECISION: only RGB/RGBA inputs are supported; others are a clean error naming the file.
    g = gradient(60, 40, seed=6)[..., 0]
    tifffile.imwrite("gray.tif", g)
    r = run("-o", "out.tif", "gray.tif")
    assert r.returncode == 1, f"expected a clean error, got {r.returncode}:\n{r.output[-2000:]}"
    assert "gray.tif" in r.output


# ---------------------------------------------------------------------------
# Options that don't behave as documented
# ---------------------------------------------------------------------------

@pytest.mark.reference_bug(reason="--wrap modes are compared case-sensitively (multiblend.cpp:242-243) but documented in upper case")
@pytest.mark.parametrize("mode,equiv", [("HORIZONTAL", "h"), ("VERTICAL", "v"), ("NONE", "none")])
def test_wrap_modes_as_documented(work, mode, equiv):
    args = scenes.panorama_strip(work)
    run_ok(f"--wrap={equiv}", "-o", "expected.tif", *args)
    run_ok(f"--wrap={mode}", "-o", "out.tif", *args)
    assert np.array_equal(read("out.tif"), read("expected.tif"))


@pytest.mark.reference_bug(reason="DT_MAX (multiblend.cpp:569) carries image index 0, so MASKVAL clears its 'indeterminate' bit and the --reverse branch never runs")
def test_reverse_prefers_last_image(work):
    # DECISION: with identical footprints every pixel is indeterminate; --reverse should pick the last image.
    a, b = gradient(90, 60, seed=1), gradient(90, 60, seed=2)
    write_tiff("a.tif", a)
    write_tiff("b.tif", b)
    run_ok("--reverse", "-o", "out.tif", "a.tif", "b.tif")
    assert np.array_equal(read("out.tif"), b)


# ---------------------------------------------------------------------------
# Seam files
# ---------------------------------------------------------------------------

def _seams(work, args):
    run_ok("--save-seams", "s.png", "--no-output", *args)
    from PIL import Image

    with Image.open("s.png") as im:
        return im.copy()


@pytest.mark.reference_bug(reason="multiblend.cpp:1121 compares png_height with itself, so a short seam PNG reaches libpng and aborts")
def test_seam_png_height_mismatch_is_a_clean_error(work):
    args = scenes.two_circles(work)
    im = _seams(work, args)
    im.crop((0, 0, im.width, im.height - 10)).save("short.png")
    r = run("--load-seams", "short.png", "-o", "out.tif", *args)
    assert r.returncode == 1
    assert "dimensions don't match" in r.output


@pytest.mark.reference_bug(reason="multiblend.cpp:1135 checks > n_images instead of >= n_images")
def test_seam_png_index_equal_to_image_count_is_rejected(work):
    args = scenes.two_circles(work)
    im = _seams(work, args)
    im.load()[0, 0] = 2
    im.save("bad.png")
    r = run("--load-seams", "bad.png", "-o", "out.tif", *args)
    assert r.returncode == 1
    assert "Bad pixel found in seam file" in r.output


# ---------------------------------------------------------------------------
# GeoTIFF (support dropped)
# ---------------------------------------------------------------------------

def _geotiff(path, arr, x, y, scale=2.0):
    tags = [
        (GEOPIXELSCALE, "d", 3, (scale, scale, 0.0), False),
        (GEOTIEPOINTS, "d", 6, (0, 0, 0, x, y, 0), False),
    ]
    write_tiff(path, arr, res=None, extratags=tags)


@pytest.mark.reference_bug(reason="geotiff.cpp:47-65 reads auto-registered GeoTIFF tags with a 16-bit count and wrong varargs; release build segfaults", strict=False)
def test_geotiff_tags_are_ignored_on_input(work):
    # DECISION: GeoTIFF support is dropped. Geo tags are ignored, so georeferenced
    # inputs are placed exactly like the same pixels without them.
    a, b = gradient(60, 40, seed=1), gradient(60, 40, seed=2)
    _geotiff("ga.tif", a, 1000.0, 5000.0)
    _geotiff("gb.tif", b, 1100.0, 4940.0)
    write_tiff("pa.tif", a, res=None)
    write_tiff("pb.tif", b, res=None)
    run_ok("-o", "expected.png", "pa.tif", "pb.tif")
    r = run("-o", "out.png", "ga.tif", "gb.tif")
    assert r.returncode == 0, f"process died with {r.returncode}"
    assert np.array_equal(read("out.png"), read("expected.png"))


@pytest.mark.reference_bug(reason="multiblend.cpp:1524-1531 writes GeoTIFF tags (with a flipped Y tiepoint and an uninitialised GDAL_NODATA string)", strict=False)
def test_geotiff_tags_are_not_written(work):
    # DECISION: GeoTIFF support is dropped, so outputs never carry geo tags.
    _geotiff("g.tif", gradient(60, 40, seed=1), 1000.0, 5000.0)
    r = run("-o", "out.tif", "g.tif")
    assert r.returncode == 0, f"process died with {r.returncode}"
    tags = tiff_tags("out.tif")
    assert not {GEOPIXELSCALE, GEOTIEPOINTS, 34264, 34735, 34736, 34737, 42113} & set(tags)
