# Multiblend 2.0rc5: pre-port examination

This is what a line-by-line read of `src/` (about 4,700 lines of C++) and targeted experiments
turned up ahead of the Rust port. It covers the confirmed defects, the behaviour a port has to
reproduce, and the code a port can drop.

**How findings were checked.** Every item marked **repro** was reproduced against
`build/multiblend`, built with the documented `-Ofast -ffast-math` flags. Tools used:

- `build/multiblend-asan` (AddressSanitizer + UBSan)
- a ThreadSanitizer build (the startup race had to be patched out first)
- lldb

Items marked **code** come from reading only. The **Test** column names the test that pins the
behaviour down. Each one is a `reference_bug` test: expected to fail against the C++ build, and
must pass against the port.

## Defects

### Crashes, memory errors and races

| # | Where | Problem | Evidence | Test |
|---|---|---|---|---|
| 1 | `threadpool.cpp:19-21` | Workers are started with `pthread_create` *before* `main_mutex` and the other pointers are assigned. A worker that runs first locks a null mutex. | repro: ~2% of runs segfault, 24% under CPU contention; lldb shows `pthread_mutex_lock(NULL)` | `test_startup_is_race_free` |
| 2 | `threadpool.cpp:92-111` | `Wait()` reads `queue.size()` and the workers' `free` flags without holding `main_mutex`. It can return while a task is still running. The main thread then races background `CompressSeamLine`/`CompressDTLine` (TSan: `multiblend.cpp:910/950/973/1024/1051`, `image.cpp:634/646`). | repro: TSan reports 9 races per run; one full-suite run produced a golden mismatch (max diff 72 over 2% of pixels) that did not recur | `test_output_is_deterministic` |
| 3 | `image.cpp:302-407` | Trimming never initialises `left/top/right/bottom` when an image has no fully opaque pixel, e.g. fully transparent, or alpha 254 everywhere. | repro: segfault | `test_input_without_opaque_pixels_is_an_error` |
| 4 | `image.cpp:581` | For a 1-row image with transparency, `thread_lines[(y - 2) % n_threads]` evaluates with y=1, giving a negative index. | repro: ASan heap-buffer-overflow | `test_one_row_image_with_transparency` |
| 5 | `functions.cpp:17-19` | `Flex` sizes its first line as `max(height,16)*16` bytes, but one mask line can need `width*4` bytes. Wide, short images with busy alpha overflow the heap. | repro: ASan overflow; release build segfaults on 3000×4 noisy alpha | `test_wide_short_image_with_noisy_alpha` |
| 6 | `image.cpp:138, 274` | Tiled TIFFs are read with the strip API. | repro: segfault | `test_tiled_tiff_input_is_an_error` |
| 7 | `geotiff.cpp:47-65` | libtiff has already auto-registered the GeoTIFF tags with 32-bit counts, but they are read into an `unsigned short nCount`. GDAL_NODATA (passcount=false) is also read with a count argument. The stack gets corrupted. | repro: release build segfaults on any GeoTIFF input (clean under ASan); cause from code | `test_geotiff_tags_are_ignored_on_input` (GeoTIFF dropped) |
| 8 | `multiblend.cpp:536`, `pyramid.cpp:17-20` | `-l -100` makes the level count negative. `Pyramid` reads a negative count as "default minus N", so each image pyramid gets 8 + 100 = 108 levels while masks get none. | repro: segfault | `test_excessively_negative_levels` |
| 9 | `multiblend.cpp:359` | With no inputs after the options, `my_argv[i]` is read past the end. | repro: ASan | `test_cli::test_no_inputs_is_an_error` |
| 10 | `multiblend.cpp:227-234` | `-l abc` / `-l +`: `sscanf("%d%n")` matches nothing, and the uninitialised `n` is then used. | repro: SIGBUS or silently accepted, depending on the run | `test_cli::test_bad_levels_value` |
| 11 | `multiblend.cpp:250-265` | `--cache-threshold=K`: the threshold is uninitialised when no digits precede the suffix. | repro: accepted | `test_cli::test_bad_cache_threshold` |
| 12 | `multiblend.cpp:1121` | `png_height != png_height` is always false, so a seam PNG of the wrong height reaches libpng. libpng has no `setjmp` handler anywhere, so any libpng error aborts the process. | repro: `libpng error` + SIGABRT | `test_seam_png_height_mismatch_is_a_clean_error` |
| 13 | `multiblend.cpp:459` | `MapAlloc` throws `char*` for temp-file failures. This call (and others) sits outside any `try`. | repro: `--tempdir nonexistent --cache-threshold=0` → `terminating due to uncaught exception` | (not covered) |
| 14 | `multiblend.cpp:588-592, 737-741` | Seam-line compression writes into a `width`-byte buffer, but each literal takes 9 bytes. The `p > width` check runs *after* the write. `CompressDTLine` has the same shape (5-byte literals into `4*width`). | code: needs a line with many image changes | (not covered) |
| 15 | `mapalloc.cpp:60, 80` | `tmpdir[l - 1]` is read with `l == 0` on an empty string, the operators have the wrong precedence, and `strcpy` into a 256-byte buffer has no bounds check. | code | (not covered) |
| 16 | `multiblend.cpp:1318-1319` | `-l 50`: shift exponent ≥ 32 (UB). | repro: UBSan | (not covered) |
| 16b | `image.cpp:273-276` | A truncated or corrupt TIFF strip makes `TIFFReadEncodedStrip` return -1, which is added to the write pointer unchecked. | repro: TIFF truncated to half its size segfaults (truncated PNGs abort, see #12) | (not covered; the port exits 1 with a decode error) |

### Wrong or corrupt output

| # | Where | Problem | Evidence | Test |
|---|---|---|---|---|
| 17 | `image.cpp:213-251, 290` | 16-bit PNG inputs are read without `png_set_swap`, so every sample is byte-swapped. PNG *output* does swap. | repro: output equals `source.byteswap()` | `test_16bit_png_input_byte_order` |
| 18 | `multiblend.cpp:1448-1454` | When output depth ≠ input depth, the DC offset correction scales the reference mean by 256 while pixels are scaled by 257. It also ignores `--gamma`, where the factor should be 257² = 66049. | repro: 8→16 blends ~130–185 units darker; 16→8 blends ~0.5 LSB brighter (single-image output is unaffected) | `test_8bit_inputs_to_16bit_output_are_not_darkened`, `test_16bit_inputs_to_8bit_output_are_not_brightened` |
| 19 | `multiblend.cpp:1521-1522` | If any image has a negative position, the TIFF writer gets a negative XPOSITION/YPOSITION. libtiff refuses it and drops the whole directory, leaving an unreadable file, yet the exit code is 0. | repro | `test_negative_positions_are_clamped_with_a_warning` |
| 20 | `image.cpp:123-135` | `xpos`/`ypos` stay uninitialised when only one of X/YPOSITION is present, or when the resolution is missing or ≤ 0. | code; UB, varies by run | `test_missing_yposition_means_zero` |
| 21 | `image.cpp:126-127`, `multiblend.cpp:1524-1531`, `geotiff.cpp:16, 85-87` | GeoTIFF handling has four faults: northing is not negated, so north-up images stack upside down; output writes `YGeoRef = -min_ypos*res`, flipping the sign; `GDAL_NODATA` is written from an uninitialised `char[50]`; `GEOASCIIPARAMS` reuses tag 34736 (should be 34737). | repro (under ASan): input northing +5000 → output −4988 | `test_geotiff_tags_are_not_written` (GeoTIFF dropped) |
| 22 | `image.cpp:119` | The sample-count check is commented out. Grayscale (spp=1/2) and other layouts are misread as RGB instead of being rejected. PLANARCONFIG_SEPARATE is not handled either. | repro: grayscale TIFF accepted silently | `test_grayscale_tiff_is_an_error` |
| 23 | `multiblend.cpp:569, 79` | The distance-transform sentinel `DT_MAX = 0x9000000000000000` has image index 0 in its low word. `MASKVAL` then clears the "indeterminate" bit wherever image 0 is present, so the `--reverse` branch never runs. | repro: `--reverse` has no effect; cause from code | `test_reverse_prefers_last_image` |
| 24 | `multiblend.cpp:1135` | The seam-file index check is `> n_images`; it should be `>=`. | repro | `test_seam_png_index_equal_to_image_count_is_rejected` |
| 25 | `image.cpp:48` | `seam_present` is never initialised, so the "fully obscured" warning depends on heap garbage. | repro: UBSan `load of value 190 ... bool` | `test_blend::test_fully_obscured_image_warns` |
| 26 | `image.cpp:164-177` | Strip-skip validation uses `&&` where `&` is meant, reads scanline `tiff_u_height - 1` before `tiff_u_height` is computed, and leaves `trans` uninitialised for other bit depths. | code (compiler warnings); results matched the plain path in every encoding tried | `test_formats::test_tiff_input_encodings_are_equivalent` |
| 27 | `image.cpp:290` | Interlaced (Adam7) PNG inputs are read row by row without `png_set_interlace_handling`. | repro: interlaced RGBA PNG gives different output from the same image non-interlaced (the port matches) | (not covered; the test suite can't write interlaced PNGs) |

### Options that don't match the documentation

| # | Where | Problem | Test |
|---|---|---|---|
| 28 | `multiblend.cpp:242-243` | `--wrap=HORIZONTAL/VERTICAL/NONE` appear in `--help`, but modes are compared case-sensitively, so the documented spellings fail with "Unknown argument". | `test_wrap_modes_as_documented` |
| 29 | `multiblend.cpp:112, 306, 394` | `all_threads` defaults to `true` and maps to `GetInstance(2)`, so the reference **always uses at most 2 threads**, and `--all-threads` does nothing. | (performance; not black-box testable) |
| 30 | `multiblend.cpp:161 vs 344` | Help says TIFF compression defaults to NONE; it is LZW. | `test_formats::test_tiff_compression` pins LZW |
| 31 | `multiblend.cpp:305` | `a \|\| b && c` precedence: `--tempdir` given as the last argument reads past the end. | (not covered) |
| 32 | `multiblend.cpp:190-211` | Splitting on `=` stops only after `-o`. With `--no-output`, input filenames containing `=` get split. | (not covered) |

Two things are **not** bugs, despite appearances:

- **Reproducibility.** Output is bit-identical at 1–8 threads for every scene tested, and between `-O2`
  and `-Ofast` builds for 8-bit output. 16-bit output differs by 1 LSB in ≤0.22% of samples.
- **The false alarm about overwritten inputs.** During testing, `-o a.tif A.tif` overwrote the input on
  macOS's case-insensitive filesystem. That was a test-harness mistake. The reference doesn't guard
  against output == input, and a port might want to.

## Semantics the port must reproduce

These are what the goldens (`test_golden.py`) and invariants (`test_blend.py`) encode.

- **Arithmetic.** The pipeline runs in `f32`. The final conversion is `cvtps_epi32`, i.e. **round half to
  even**. Output loaders compute `min(max(v, 0) [sqrt if gamma] + dither, max)` in that order. Because
  `+0.4999` rounds to `+0.5` in f32 at large 16-bit magnitudes, dithering nudges ~3% of exact 16-bit
  values by 1 (`test_single_16bit_image_dither_moves_values_by_at_most_one`).
- **Ordered dither.** Indexed by `[y & 3][x & 3]`, where x is the output column (pitch is a multiple of
  4). Values in lane order:
  - row 0: `-0.125, 0.375, 0.0, 0.4999`
  - row 1: `0.125, -0.375, 0.25, -0.25`
  - row 2: `-0.0625, 0.4375, -0.1875, 0.3125`
  - row 3: `0.1875, -0.3125, 0.0625, -0.4375`

  `--no-dither` uses 0.
- **Coverage.** A pixel is opaque only if alpha is the maximum value (255 / 65535). Anything less is
  transparent. Each image is trimmed to the bounding box of its opaque pixels. Transparent pixels
  are inpainted by nearest-opaque copy using a 3/4 chamfer distance, forward then backward pass.
  Output alpha is binary (0/max) and is written only if some output pixel is uncovered, and never
  for JPEG.
- **Placement.** Position = TIFF X/YPOSITION × resolution, rounded (JPEG/PNG: 0), plus any `X,Y`
  arguments, plus the trim offset. The output is the bounding box of all trimmed images. Output
  X/YPOSITION = top-left / first image's resolution, and resolution is copied from the first image.
- **Levels.** `floor(log2(blend_wh + 4) - 1)`. `blend_wh` is `max(median width, median height)` of the
  trimmed images (for even counts, the rounded-up mean of the middle two), or `max(out_w, out_h)`
  with `--wideblend` (which also adds 1). `-l N` fixes the count (0 → 1); `-l ±N` adjusts it. A single
  image means no blending.
- **Seams.** A 3/4 chamfer distance transform is seeded from the regions covered by exactly one
  image. Ties and fully indeterminate regions go to the lowest image index. Seam maps are palette
  PNGs whose index is the image number; the XOR map uses 255 for "not exclusive".
- **Pyramid.** Level size is `(w + x_shift + 6) >> 1`. x/y shift parities follow the image's absolute
  position (alignment doubles per level). Shrink uses the 5-tap `[1 4 6 4 1]/16` kernel with the
  edge handling in `ShrinkThread`/`Squeeze`. Expand uses `[1/8 3/4 1/8]` / `[1/2 1/2]`. Masks are
  shrunk with the same kernel (`ShrinkMasks`/`Squish`), and each level composites `Σ mask·laplacian`.
- **Offset correction.** After collapse, a constant is added to level 0 so the mean over exclusive
  pixels matches the inputs' mean there. With a single image and no wrap it has no effect.
- **Wrap.** Swap halves, blend with two half-width (or half-height) pyramids of
  `floor(log2(w/2 + 4) - 1)` levels, then unswap.
- **Output formats.**
  - TIFF: LZW by default, 64 rows/strip, contiguous RGB(A), unassociated-alpha extrasample, `--bigtiff`.
  - JPEG: libjpeg defaults (4:2:0), quality 75.
  - PNG: zlib level 3, 16-bit big-endian.
- **CLI.** `-o FILE` ends option parsing, so everything after it is an input. `--opt=value` and
  `--opt value` are equivalent. Exit codes are 0 on success and 1 on error. The wording of errors,
  warnings, the `W x H, N levels, B bpp` line and the `--timing` labels is part of the contract:
  `test_cli.py` checks them as substrings, because scripts may parse them.

**Not part of the contract:**
- the internal formats (`Flex` run-length masks, seam and distance-transform line compression, `MapAlloc`)
- thread count and banding
- timing values, the banner, and the other progress lines

## Code the port can skip

These are never called from `main`:

- `geotiff.cpp` and `GeoTIFFInfo` (GeoTIFF support is dropped, decision 3)

- `Pyramid::Subsample`/`Subsample_Squeeze`, `Average`, `MultiplyAndAdd`, `MultiplyAddClamp`,
  `MultplyByPyramid`, both `Fuse` overloads/`FuseThread`, `Denoise`, `Blend`, `BlurX`/`BlurXThread`,
  `Png`, `DefaultNumLevels` (effectively)
- the interleaved output path (`OutInterleaved`, `Out` with `step != 0`), which also has its own bugs
- the `level`/`chroma` arguments of `Out`, float (`OutPlanar32`) output, and `Copy` with `step > 1`
  (`CopyInterleaved*`)
- `Image::MaskPng`, the `hist_*` arrays, `Pnger::Quick` (debug only), `MapAlloc::LastFile`, and the
  `share` constructor argument of `Pyramid`

## Port decisions (confirmed 2026-09-28)

These are settled. Each one is encoded in a `reference_bug` test (marked `DECISION` in
`test_known_bugs.py`), and the port must pass all of them. Every assertion was checked by
implementing the decision in a scratch copy of the C++ and confirming the tests pass without
disturbing any golden or invariant test.

1. **8↔16-bit conversion.** ×257 / ÷257 everywhere, including the DC offset correction; ×66049 /
   ÷66049 in the squared domain under `--gamma`.
   Tests: `test_8bit_inputs_to_16bit_output_are_not_darkened`,
   `test_16bit_inputs_to_8bit_output_are_not_brightened` (each with and without `--gamma`).
2. **`--reverse`.** Indeterminate pixels go to the *last* image.
   Test: `test_reverse_prefers_last_image`.
3. **GeoTIFF support is dropped.** Geo tags on input are ignored (placement comes only from
   X/YPOSITION and `X,Y` arguments), and outputs never carry geo tags. The port should not carry over
   `geotiff.cpp` or `Image::geotiff`. This retires defects #7 and #21.
   Tests: `test_geotiff_tags_are_ignored_on_input`, `test_geotiff_tags_are_not_written`.
4. **Inputs with no fully opaque pixel** are a clean error (exit 1) naming the file.
   Test: `test_input_without_opaque_pixels_is_an_error`.
5. **Tiled TIFFs and non-RGB(A) inputs** (grayscale, etc.) are a clean error (exit 1) naming the
   file. Tests: `test_tiled_tiff_input_is_an_error`, `test_grayscale_tiff_is_an_error`.
6. **Threads.** Use all cores by default. Output doesn't depend on thread count (verified bit-exact at
   1–8 threads), so this isn't black-box tested. `--all-threads` stays accepted as a no-op for
   compatibility.
7. **Negative output position.** TIFF X/YPOSITION are clamped to 0, with a warning on stdout (a line
   containing "Warning" and "position", e.g. `Warning: output has a negative position; TIFF position
   clamped to 0`). No warning for PNG/JPEG output, which stores no position.
   Test: `test_negative_positions_are_clamped_with_a_warning`.
8. **JPEG decoding.** The port may use any decoder. JPEG input is checked by mean luma error
   (< 1.0 levels) against libjpeg-turbo, not per pixel.
   Test: `test_formats::test_jpeg_input_decodes_close_to_libjpeg`.

## Rust port status

The port is in `rust/` (see `rust/README.md`). It passes the whole suite and is bit-identical to an
`-O2` build of the reference, apart from the decisions above. Differences worth knowing:

- **No disk-backed memory.** `MapAlloc` (spilling allocations to temp files past
  `--cache-threshold`) is not ported. `--cache-threshold` and `--tempdir` are still validated and
  accepted, but do nothing, so very large blends need enough RAM. Peak memory on a 19-megapixel
  four-image blend is the same as the reference's (about 380 MB). Follow-up if needed: memory-map
  the per-image channel planes and the pyramid level buffers.
- **Intermediate storage is uncompressed.** The reference packs the seam distance transform and the
  inpainting distance transform into custom byte codes. The port keeps them as plain arrays: the
  seam transform's values for overlap pixels only (8 bytes each), and the inpainting transform at
  4 bytes per pixel for each image while it is being read.
- **Level count is clamped to 1–29.** This covers `-l`, `-l ±N` and wrap levels. The reference
  crashed below 1 and hit undefined shifts above 31.
- **Degenerate sizes are guarded.** Where the reference read outside its buffers (1-pixel-wide rows,
  pyramid levels only 1–2 rows tall), the port clamps the reads. For those cases only, output may
  differ from the reference, whose behaviour there was undefined.
- **Checked with the C libraries.** libtiff (`tiffinfo -D`, `tiffcp`, `tiffcmp` against the reference), libjpeg (`djpeg`) and libpng (via the reference binary) read the port's TIFF (none/LZW/PackBits, classic and BigTIFF, 8/16-bit, RGB/RGBA), JPEG and PNG output. Seam files cross-load between the port and the reference in both directions. Corrupt, empty and truncated inputs exit 1 with a message; the debug build, which panics on integer overflow, passes the suite and the sweep.
- **Inputs:** interlaced PNGs work (#27). Tiled TIFFs, non-RGB(A) images and planar-separate TIFFs
  are rejected with a clean error naming the file.
