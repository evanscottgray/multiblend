"""Command-line parsing and validation."""

import pytest

import scenes
from mb import gradient, read, run, run_ok, tiff_tags, write_tiff

PRESENTATION_LINES = ("Processing", "Seaming", "Shrinking", "Blending", "Writing")


@pytest.fixture
def two(work):
    return scenes.two_circles(work)


def test_help_with_no_arguments():
    r = run()
    assert r.returncode == 0
    assert "Usage: multiblend" in r.output


@pytest.mark.parametrize("flag", ["-h", "--help", "/?"])
def test_help_flags(flag):
    r = run(flag)
    assert r.returncode == 0
    assert "Usage: multiblend" in r.output
    assert "--wideblend" in r.output


def test_not_enough_arguments():
    r = run("-o", "x.tif")
    assert r.returncode == 1
    assert "Not enough arguments" in r.output


def test_unknown_argument(two):
    r = run("--frobnicate", "-o", "out.tif", *two)
    assert r.returncode == 1
    assert 'Unknown argument "--frobnicate"' in r.output


@pytest.mark.parametrize("name,msg", [("out.bmp", "Unknown file extension"), ("out", "Unknown output filetype")])
def test_bad_output_extension(two, name, msg):
    r = run("-o", name, *two)
    assert r.returncode == 1
    assert msg in r.output


def test_no_output_and_no_seam_save_is_an_error(two):
    r = run("--no-output", *two)
    assert r.returncode == 1
    assert "No output file specified" in r.output


@pytest.mark.reference_bug(reason="multiblend.cpp:359 reads my_argv[i] past the end when no inputs follow", strict=False)
def test_no_inputs_is_an_error():
    r = run("--save-seams", "s.png", "--no-output")
    assert r.returncode == 1


def test_missing_input_file(work, two):
    r = run("-o", "out.tif", two[0], "missing.tif")
    assert r.returncode == 1
    assert "Could not open missing.tif" in r.output


def test_unknown_input_extension(work, two):
    (work / "x.bmp").write_bytes(b"BM")
    r = run("-o", "out.tif", two[0], "x.bmp")
    assert r.returncode == 1
    assert "Unknown file extension: x.bmp" in r.output


@pytest.mark.parametrize("depth", ["12", "0", "32", "x"])
def test_invalid_depth(two, depth):
    r = run("-d", depth, "-o", "out.tif", *two)
    assert r.returncode == 1
    assert "Invalid output depth" in r.output


@pytest.mark.parametrize("flag", ["-d", "--depth", "--bpp", "--d"])
def test_depth_aliases(two, flag):
    run_ok(flag, "16", "-o", "out.tif", *two)
    assert read("out.tif").dtype.itemsize == 2


def test_depth_16_with_jpeg_output_is_an_error(two):
    r = run("-d", "16", "-o", "out.jpg", *two)
    assert r.returncode == 1
    assert "incompatible with JPEG" in r.output


def test_load_and_save_seams_together_is_an_error(two):
    r = run("--save-seams", "a.png", "--load-seams", "b.png", "-o", "out.tif", *two)
    assert r.returncode == 1
    assert "Cannot load and save seams" in r.output


def test_wrap_both_is_unsupported(two):
    r = run("--wrap=both", "-o", "out.tif", *two)
    assert r.returncode == 1
    assert "not currently supported" in r.output


_UNINIT_N = pytest.mark.reference_bug(reason="multiblend.cpp:229-234 uses %n result uninitialised when sscanf matches nothing", strict=False)


@pytest.mark.parametrize("value", [pytest.param("abc", marks=_UNINIT_N), "3x", pytest.param("+", marks=_UNINIT_N)])
def test_bad_levels_value(two, value):
    r = run("-l", value, "-o", "out.tif", *two)
    assert r.returncode == 1
    assert "Bad --levels parameter" in r.output


@pytest.mark.parametrize(
    "args,expected",
    [
        ([], 5),  # automatic: floor(log2(median_size + 4) - 1) for the ~63px trimmed circles
        (["-l", "3"], 3),
        (["--levels", "7"], 7),
        (["-l", "0"], 1),  # zero is promoted to one
        (["-l", "+2"], 7),
        (["-l", "-2"], 3),
        (["--levels=4"], 4),
        (["--wideblend"], 6),  # floor(log2(112 + 4) - 1) from the 112x72 output size, plus one
    ],
)
def test_level_count(two, args, expected):
    r = run_ok(*args, "-o", "out.tif", *two)
    assert f"{expected} levels" in r.output


def test_single_image_reports_no_levels(work):
    args = scenes.two_circles(work)[:1]
    r = run_ok("-o", "out.tif", *args)
    assert "levels" not in r.output
    assert "8 bpp" in r.output


@pytest.mark.parametrize(
    "value",
    ["12Q", pytest.param("K", marks=pytest.mark.reference_bug(reason="multiblend.cpp:253-265 shifts an uninitialised threshold when no digits precede the suffix", strict=False)), "1.5M", "12KB"],
)
def test_bad_cache_threshold(two, value):
    r = run(f"--cache-threshold={value}", "-o", "out.tif", *two)
    assert r.returncode == 1
    assert "Bad --cache-threshold parameter" in r.output


@pytest.mark.parametrize("value", ["0", "1024", "1K", "2k", "1M", "1G"])
def test_cache_threshold_does_not_change_output(two, value):
    run_ok("-o", "ref.tif", *two)
    run_ok(f"--cache-threshold={value}", "-o", "out.tif", *two)
    assert (read("out.tif") == read("ref.tif")).all()


def test_tempdir_is_used_and_cleaned_up(work, two):
    (work / "tmp").mkdir()
    run_ok("--cache-threshold=0", "--tempdir", "tmp/", "-o", "out.tif", *two)
    assert list((work / "tmp").iterdir()) == []


def test_equals_form_and_space_form_are_equivalent(two):
    # (output names must not collide with inputs on case-insensitive filesystems)
    run_ok("--compression=packbits", "--levels=3", "-o", "eq1.tif", *two)
    run_ok("--compression", "packbits", "--levels", "3", "-o", "eq2.tif", *two)
    assert (read("eq1.tif") == read("eq2.tif")).all()


def test_output_long_form(two):
    run_ok("--output=out.tif", *two)
    assert read("out.tif").shape == (72, 112, 4)


def test_double_dash_separates_inputs(two):
    run_ok("-o", "out.tif", "--", *two)
    assert read("out.tif").shape == (72, 112, 4)


@pytest.mark.parametrize(
    "args",
    [["-f", "-o", "out.tif"], ["-fsomething", "-o", "out.tif"], ["-a", "-o", "out.tif"],
     ["--no-ciecam", "-o", "out.tif"], ["--primary-seam-generator", "nft", "-o", "out.tif"]],
)
def test_enblend_options_are_ignored(two, args):
    r = run_ok(*args, *two)
    assert "ignoring Enblend option" in r.output


def test_quiet_suppresses_progress(two):
    r = run_ok("--quiet", "-o", "out.tif", *two)
    assert not any(line.startswith(PRESENTATION_LINES) for line in r.stdout.splitlines())
    assert "Multiblend v2" not in r.stdout


def test_timing_prints_summary(two):
    # Timing labels are part of the CLI contract (see FINDINGS.md); the values are not.
    r = run_ok("--timing", "-o", "out.tif", *two)
    assert "Seaming:" in r.output
    assert "Blend complete. Total execution time" in r.output


def test_compression_warning_for_non_tiff(two):
    r = run_ok("--compression=lzw", "-o", "out.png", *two)
    assert "ignoring TIFF compression setting" in r.output


def test_quality_warning_for_tiff(two):
    r = run_ok("--compression=50", "-o", "out.tif", *two)
    assert "ignoring compression quality setting" in r.output
    assert tiff_tags("out.tif")[259] == 5  # still LZW


def test_png_compression_level_out_of_range(two):
    r = run("--compression=12", "-o", "out.png", *two)
    assert r.returncode == 1
    assert "Bad PNG compression quality setting" in r.output


def test_unknown_compression(two):
    r = run("--compression=zip", "-o", "out.tif", *two)
    assert r.returncode == 1
    assert "Unknown compression codec zip" in r.output


def test_options_after_output_are_treated_as_inputs(two):
    # -o terminates option parsing: everything after its value is an input.
    r = run("-o", "out.tif", "--bgr", *two)
    assert r.returncode == 1
    assert "--bgr" in r.output


def test_mixed_bit_depths_are_rejected(work):
    scenes.two_circles(work)
    write_tiff(work / "C16.tif", gradient(40, 40, 16))
    r = run("-o", "out.tif", "A.tif", "C16.tif")
    assert r.returncode == 1
    assert "mixture of 8bpp and 16bpp" in r.output


def test_seam_load_index_out_of_range(work, two):
    run_ok("--save-seams", "s.png", "--no-output", *two)
    from PIL import Image

    with Image.open("s.png") as im:
        im = im.copy()
    im.load()[0, 0] = 5
    im.save("bad.png")
    r = run("--load-seams", "bad.png", "-o", "out.tif", *two)
    assert r.returncode == 1
    assert "Bad pixel found in seam file: 0,0" in r.output


def test_seam_load_width_mismatch(work, two):
    run_ok("--save-seams", "s.png", "--no-output", *two)
    from PIL import Image

    with Image.open("s.png") as im:
        im.crop((0, 0, im.width - 10, im.height)).save("narrow.png")
    r = run("--load-seams", "narrow.png", "-o", "out.tif", *two)
    assert r.returncode == 1
    assert "dimensions don't match" in r.output


def test_seam_load_missing_file(two):
    r = run("--load-seams", "nope.png", "-o", "out.tif", *two)
    assert r.returncode == 1
    assert "Couldn't open seam file" in r.output
