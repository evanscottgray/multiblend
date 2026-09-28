"""Image input/output: formats, bit depths, TIFF metadata."""

import numpy as np
import pytest
import tifffile

import scenes
from mb import (XPOSITION, YPOSITION, circle_mask, gradient, read, run_ok, tiff_tags, with_alpha,
                write_jpeg, write_png, write_tiff)

COMPRESSION, ROWSPERSTRIP, XRESOLUTION, YRESOLUTION, EXTRASAMPLES = 259, 278, 282, 283, 338


@pytest.fixture
def two(work):
    return scenes.two_circles(work)


@pytest.fixture
def reference(two):
    run_ok("-o", "ref.tif", *two)
    return read("ref.tif")


# ---------------------------------------------------------------------------
# Output formats
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("name", ["out.tif", "out.tiff", "out.TIF", "out.png", "out.PNG"])
def test_lossless_outputs_agree(two, reference, name):
    run_ok("-o", name, *two)
    out = tifffile.imread(name) if name.lower().endswith(("tif", "tiff")) else read(name)
    assert np.array_equal(out, reference)


@pytest.mark.parametrize("name", ["out.jpg", "out.jpeg", "out.JPG"])
def test_jpeg_output(two, reference, name):
    run_ok("--compression=95", "-o", name, *two)
    out = read(name)
    assert out.shape == reference.shape[:2] + (3,)  # JPEG never carries alpha
    covered = reference[..., 3] == 255
    # compare luma: libjpeg's default 4:2:0 chroma subsampling blurs colour noise
    luma = lambda im: im[..., :3].astype(float) @ [0.299, 0.587, 0.114]
    err = np.abs(luma(out) - luma(reference))[covered]
    assert err.mean() < 1.5


def test_jpeg_default_quality_is_75(work, two):
    run_ok("-o", "default.jpg", *two)
    run_ok("--compression=75", "-o", "q75.jpg", *two)
    run_ok("--compression=76", "-o", "q76.jpg", *two)
    assert (work / "default.jpg").read_bytes() == (work / "q75.jpg").read_bytes()
    assert (work / "default.jpg").read_bytes() != (work / "q76.jpg").read_bytes()


def test_jpeg_uncovered_pixels_are_black(two, reference):
    run_ok("--compression=100", "-o", "out.jpg", *two)
    out = read("out.jpg").astype(int)
    uncovered = reference[..., 3] == 0
    far = uncovered & np.roll(uncovered, 4, 0) & np.roll(uncovered, -4, 0) & np.roll(uncovered, 4, 1) & np.roll(uncovered, -4, 1)
    assert out[far].max() < 8


def test_jpeg_quality_controls_size(work, two):
    run_ok("--compression=10", "-o", "q10.jpg", *two)
    run_ok("--compression=95", "-o", "q95.jpg", *two)
    assert (work / "q10.jpg").stat().st_size < (work / "q95.jpg").stat().st_size


def test_png_compression_level_does_not_change_pixels(work, two):
    run_ok("--compression=0", "-o", "c0.png", *two)
    run_ok("--compression=9", "-o", "c9.png", *two)
    assert np.array_equal(read("c0.png"), read("c9.png"))
    assert (work / "c9.png").stat().st_size < (work / "c0.png").stat().st_size


def test_16bit_png_output(work):
    args = scenes.two_circles(work, bpp=16)
    run_ok("-o", "out.tif", *args)
    run_ok("-o", "out.png", *args)
    png = read("out.png")
    assert png.dtype == np.uint16
    assert np.array_equal(png, read("out.tif"))


@pytest.mark.parametrize("option,code", [([], 5), (["--compression=lzw"], 5), (["--compression=packbits"], 32773), (["--compression=none"], 1), (["--compression=NONE"], 1)])
def test_tiff_compression(two, reference, option, code):
    run_ok(*option, "-o", "out.tif", *two)
    assert tiff_tags("out.tif")[COMPRESSION] == code
    assert np.array_equal(read("out.tif"), reference)


def test_tiff_layout(two):
    run_ok("-o", "out.tif", *two)
    tags = tiff_tags("out.tif")
    assert tags[ROWSPERSTRIP] == 64
    assert tags[EXTRASAMPLES] == (2,) or tags[EXTRASAMPLES] == 2  # unassociated alpha
    with tifffile.TiffFile("out.tif") as t:
        assert not t.is_bigtiff
        page = t.pages[0]
        assert page.planarconfig == 1  # contiguous
        assert page.photometric == 2  # RGB


def test_bigtiff(two, reference):
    run_ok("--bigtiff", "-o", "out.tif", *two)
    with tifffile.TiffFile("out.tif") as t:
        assert t.is_bigtiff
    assert np.array_equal(read("out.tif"), reference)


def test_no_alpha_channel_when_fully_covered(work):
    write_tiff("a.tif", gradient(60, 40, seed=1))
    write_tiff("b.tif", gradient(60, 40, seed=2), pos=(30, 0))
    run_ok("-o", "out.tif", "a.tif", "b.tif")
    run_ok("-o", "out.png", "a.tif", "b.tif")
    assert read("out.tif").shape == (40, 90, 3)
    assert read("out.png").shape == (40, 90, 3)
    assert EXTRASAMPLES not in tiff_tags("out.tif")


def test_bgr_swaps_red_and_blue(two, reference):
    run_ok("--bgr", "-o", "out.tif", *two)
    assert np.array_equal(read("out.tif"), reference[..., [2, 1, 0, 3]])


def test_resolution_is_copied_from_first_input(work):
    write_tiff("a.tif", gradient(40, 30, seed=1), res=300)
    write_tiff("b.tif", gradient(40, 30, seed=2), res=300, pos=(20, 0))
    run_ok("-o", "out.tif", "a.tif", "b.tif")
    tags = tiff_tags("out.tif")
    assert tags[XRESOLUTION][0] / tags[XRESOLUTION][1] == pytest.approx(300)
    assert tags[YRESOLUTION][0] / tags[YRESOLUTION][1] == pytest.approx(300)


def test_resolution_mismatch_warns(work):
    write_tiff("a.tif", gradient(40, 30, seed=1), res=300)
    write_tiff("b.tif", gradient(40, 30, seed=2), res=72, pos=(20, 0))
    r = run_ok("-o", "out.tif", "a.tif", "b.tif")
    assert "TIFF resolution mismatch" in r.output


def test_output_position_is_top_left_of_blend(work):
    # Output X/YPOSITION = (minimum trimmed position) / resolution.
    m = np.zeros((30, 40), bool)
    m[5:, 3:] = True
    write_tiff("a.tif", with_alpha(gradient(40, 30, seed=1), m), pos=(10, 20), res=100)
    write_tiff("b.tif", gradient(40, 30, seed=2), pos=(30, 22), res=100)
    run_ok("-o", "out.tif", "a.tif", "b.tif")
    tags = tiff_tags("out.tif")
    assert tags[XPOSITION][0] / tags[XPOSITION][1] == pytest.approx(13 / 100, abs=1e-4)
    assert tags[YPOSITION][0] / tags[YPOSITION][1] == pytest.approx(22 / 100, abs=1e-4)


# ---------------------------------------------------------------------------
# Input formats
# ---------------------------------------------------------------------------

def test_png_rgba_inputs_match_tiff_inputs(work, reference):
    args = scenes.png_inputs(work)
    run_ok("-o", "out.tif", *args)
    assert np.array_equal(read("out.tif"), reference)


def test_png_rgb_input(work):
    g = gradient(50, 40, seed=5)
    write_png("in.png", g)
    run_ok("-o", "out.tif", "in.png")
    assert np.array_equal(read("out.tif"), g)


def test_jpeg_input_decodes_close_to_libjpeg(work):
    # Decoders legitimately differ in IDCT and chroma upsampling, so compare luma on
    # average rather than pixels exactly (port decision 8 in FINDINGS.md).
    g = gradient(64, 48, seed=6)
    write_jpeg("in.jpg", g, quality=90)
    expected = read("in.jpg")
    run_ok("-o", "out.tif", "in.jpg")
    out = read("out.tif")
    assert out.shape == expected.shape
    luma = lambda im: im[..., :3].astype(float) @ [0.299, 0.587, 0.114]
    assert np.abs(luma(out) - luma(expected)).mean() < 1.0


def test_jpeg_inputs_blend(work):
    write_jpeg("a.jpg", gradient(80, 60, seed=1))
    write_jpeg("b.jpg", gradient(80, 60, seed=2))
    run_ok("-o", "out.tif", "a.jpg", "b.jpg", "40,10")
    assert read("out.tif").shape == (70, 120, 4)


@pytest.mark.parametrize("compression", [None, "lzw", "zlib", "packbits"])
@pytest.mark.parametrize("rowsperstrip", [None, 1, 7, 64])
def test_tiff_input_encodings_are_equivalent(work, compression, rowsperstrip):
    # Compressed RGBA TIFFs with many identical (transparent) strips take the
    # reference's strip-skipping fast path; results must not change.
    g = gradient(90, 80, seed=3)
    m = np.zeros((80, 90), bool)
    m[30:55, 10:80] = True
    write_tiff("plain.tif", with_alpha(g, m))
    write_tiff("enc.tif", with_alpha(g, m), compression=compression, rowsperstrip=rowsperstrip)
    write_tiff("other.tif", gradient(60, 30, seed=4), pos=(40, 45))
    run_ok("-o", "a.tif", "plain.tif", "other.tif")
    run_ok("-o", "b.tif", "enc.tif", "other.tif")
    assert np.array_equal(read("a.tif"), read("b.tif"))


def test_big_endian_tiff_input(work):
    for bpp in (8, 16):
        g = gradient(50, 40, bpp, seed=7)
        tifffile.imwrite(f"be{bpp}.tif", g, byteorder=">", photometric="rgb")
        run_ok("--no-dither", "-o", f"out{bpp}.tif", f"be{bpp}.tif")
        assert np.array_equal(read(f"out{bpp}.tif"), g)


def test_16bit_tiff_input(work):
    g = gradient(50, 40, 16, seed=8)
    m = circle_mask(50, 40)
    write_tiff("in.tif", with_alpha(g, m))
    run_ok("--no-dither", "-o", "out.tif", "in.tif")
    out = read("out.tif")
    assert out.dtype == np.uint16
    ys, xs = np.nonzero(m)
    crop = (slice(ys.min(), ys.max() + 1), slice(xs.min(), xs.max() + 1))
    assert np.array_equal(out[..., :3][m[crop]], g[crop][m[crop]])


def test_xy_arguments_equal_tiff_position_tags(work):
    m = circle_mask(100, 70)
    a, b = with_alpha(gradient(100, 70, seed=10), m), with_alpha(gradient(100, 70, seed=11), m)
    write_tiff("a_tag.tif", a, pos=(0, 0))
    write_tiff("b_tag.tif", b, pos=(50, 10))
    write_tiff("a_arg.tif", a)
    write_tiff("b_arg.tif", b)
    run_ok("-o", "tags.png", "a_tag.tif", "b_tag.tif")
    run_ok("-o", "args.png", "a_arg.tif", "b_arg.tif", "50,10")
    run_ok("-o", "neg.png", "a_arg.tif", "-50,-10", "b_arg.tif")
    assert np.array_equal(read("tags.png"), read("args.png"))
    assert np.array_equal(read("tags.png"), read("neg.png"))


def test_xy_arguments_add_to_tiff_position(work):
    m = circle_mask(100, 70)
    write_tiff("a.tif", with_alpha(gradient(100, 70, seed=10), m), pos=(0, 0))
    write_tiff("b.tif", with_alpha(gradient(100, 70, seed=11), m), pos=(30, 4))
    write_tiff("b2.tif", with_alpha(gradient(100, 70, seed=11), m), pos=(50, 10))
    run_ok("-o", "sum.tif", "a.tif", "b.tif", "20,6")
    run_ok("-o", "abs.tif", "a.tif", "b2.tif")
    assert np.array_equal(read("sum.tif"), read("abs.tif"))
