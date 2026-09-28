# Roadmap: ideas for future work

A backlog of improvements to the Rust port (2.1.0), written while the whole codebase and its
measurements were fresh. Each item has an effort estimate and an **exactness** tag:

- **exact**: output must stay bit-identical. Prove it with `make test-rust`, `rust/tools/sweep.py`
  against `build/multiblend-ref-O2` (expect only the 11 known differences), and
  `rust/tools/dumpdiff.py` for stage-by-stage checks.
- **opt-in**: changes output, so it goes behind a flag with the default unchanged. Add new tests or
  goldens for the flag.
- **new**: new behaviour with no reference to compare against. Needs its own tests.

Effort: **S** is under a day, **M** a few days, **L** a week or more.

## Where the time goes today

Eight overlapping 4000×3000 LZW TIFFs (a 13,700×5,400 output) on a 4-core i7 laptop. Total about
6.8 s on macOS and 6.0 s on Linux (static glibc).

| Stage | Time | Share | Notes |
|---|---|---|---|
| Write (LZW encode + disk) | 2.0–2.6 s | ~35% | strips encoded in parallel; LZW itself is slow |
| Images (decode + trim + inpaint) | ~1.8 s | ~25% | parallel across images; each image decodes on one thread |
| Seaming | ~0.6 s | ~9% | sequential by design |
| Pyramid (copy, shrink, Laplace, blend, collapse, out) | ~0.85 s | ~13% | memory-bandwidth bound; AVX2 build no faster |
| Peak memory | 1.8 GB | | vs 1.3 GB for the C++ (parallel reads) |

So file formats and I/O are the biggest levers, then seaming, then memory. Pyramid arithmetic is
already close to the hardware limit on this machine.

---

## 1. Performance

### 1.1 Build profile: LTO, one codegen unit, `panic = "abort"`. **S, exact**
Add `[profile.release] lto = "fat"`, `codegen-units = 1` and `panic = "abort"` to `Cargo.toml`.
These are free wins, typically 5–15% for code like this, with a smaller binary. Measure with the
benchmark (item 4.1). Also try profile-guided optimisation (`cargo pgo`), trained on the test scenes
plus the 8-image benchmark.

### 1.2 Parallel strip decoding within one image. **M, exact**
Parallel reads help only when there are several images. A single huge input (say one 100-megapixel
TIFF, or two images) still decodes on one thread. TIFF strips are independent: open one decoder per
thread and decode strip ranges with `read_chunk`. PNG can't be split this way (the zlib stream is
serial), but JPEG restart intervals could be.

### 1.3 Faster output compression options. **S–M, new (pixels unchanged)**
LZW encoding is the largest single cost.
- Add `--compression=deflate[:level]` (TIFF tag 8), using `zlib-rs`/`libdeflater`-class speed.
  At a low level it's usually faster *and* smaller than LZW.
- Add the horizontal predictor (tag 317 = 2) for LZW and deflate. It often shrinks files 30–50% at
  little CPU cost.
- Optionally add zstd (tag 50000). libtiff reads it, but many other tools don't, so it's opt-in only.
- Keep LZW as the default for compatibility, but document the faster choices.

### 1.4 Overlap writing with computing. **M, exact**
Output currently runs as compute all three channels → assemble → compress → write. Instead,
pipeline by strip: as soon as a strip's rows are converted for all channels, hand it to the
compressor pool while the next strip is converted. The easiest version converts all channels per
strip, since `out()` is row-local.

### 1.5 Blend all three channels in one pass. **M–L, exact**
Each channel runs its own shrink → Laplace → composite → collapse, and compositing re-decodes the
mask run-lengths 3× per image and level. An RGB-interleaved pyramid (3 or 4 floats per pixel) would
decode masks once, traverse memory once, and give natural 4-lane SIMD. The arithmetic per element
stays identical, so output stays bit-identical. It trades memory: all channels live at once, unless
combined with 3.2.

### 1.6 Faster seaming. **M, exact**
Seaming is sequential because each row's distance transform depends on the previous row, but:
- **Fast path:** rows (or long runs) covered by exactly one image need no DT; they're a copy. Skip
  the per-pixel candidate logic there. Most of a typical panorama is single-coverage.
- **Pipelining:** run the backward and forward passes as a two-stage pipeline over row blocks. The
  C++ did something similar for its compression step.
- **Precomputed runs:** the inner loop over images on every run boundary is O(images). Precompute
  per-row run boundaries for all images once.

### 1.7 Inpaint only what's needed. **M, exact if bounded correctly**
Inpainting fills every transparent pixel inside the trimmed bounding box, but the pyramid only
samples within roughly 2^levels × 3 pixels of real data. Beyond that halo the inpainted values
never reach the output. Bounding the inpaint to that halo would save time on sparse or oddly shaped
inputs. It needs a careful proof, or a sweep over many shapes, to stay exact.

### 1.8 SIMD via runtime dispatch. **M, exact; deprioritised**
AVX2 gave nothing measurable on the 4-core laptop because we're memory-bound. It may still pay on
many-core servers with more memory bandwidth per core. Revisit only with a benchmark from such a
machine. `std::arch` with runtime detection, or the `multiversion` crate.

### 1.9 GPU backend (experiment). **L, opt-in**
Shrink, expand, composite and collapse are embarrassingly parallel. A `wgpu` compute backend could
take the pyramid phase to near zero on big panoramas. GPUs don't guarantee IEEE `f32` operation
order (FMA contraction), so this would be an approximate mode with its own tolerance tests. It's
worth it only if pyramid time dominates, e.g. after 1.3 and 1.2 shrink I/O.

---

## 2. Functionality

### 2.1 Tiled TIFF input. **S–M, new**
Currently rejected with an error. Tiled TIFFs are common from GDAL, big stitchers and some cameras'
tools. The `tiff` crate decodes tiles; mostly this is removing the guard and testing. Tiled *output*
would pair naturally with 3.2.

### 2.2 Soft alpha and feathered masks. **M, opt-in**
Today any alpha below the maximum counts as fully transparent (the C++ rule). Stitchers often
produce anti-aliased or feathered alpha. An option could use alpha as a weight in the level-0 masks,
instead of binary coverage, so soft edges blend rather than get cut.

### 2.3 Exposure and colour matching before blending. **M–L, opt-in**
Multi-band blending hides seams but not exposure differences between overlapping frames. It only
spreads them out; the `exposure_step` test scene shows this. Estimate per-image gain (and optionally
a white balance per channel) from the overlap regions, which the seam pass already identifies, then
apply it before blending: `--exposure-compensation`.

### 2.4 Smarter seams. **L, opt-in**
The seam is placed by distance to image edges and ignores content, so it can cut through a moving
person or a misaligned edge. Options:
- **Cost-aware seams:** a graph cut or dynamic-programming seam through low-difference paths in each
  overlap (enblend's `--optimize`).
- **Ghost avoidance:** keep the seam off regions where overlapping images disagree strongly.

Both could start from the existing seam as an initial guess and refine it within overlaps.
`--save-seams`/`--load-seams` already make a good debugging loop.

### 2.5 High dynamic range and float input/output. **M, new**
The pyramid is already `f32`. Accept 32-bit float TIFF (and possibly OpenEXR) and write float
output, with no clamping or dithering. HDR stitching workflows need this, and it's mostly I/O plus a
float output path in `Pyramid::out`.

### 2.6 Grayscale support. **S, new**
Now rejected. Blend a single channel and write grayscale output, useful for scientific and scanning
workflows. Handle it with channel-count plumbing, not by converting to RGB.

### 2.7 Metadata passthrough. **S–M, new**
Copy the ICC profile, EXIF and XMP from the first input, so colour-managed workflows keep their
profile; today output is untagged. Also write resolution and position consistently for PNG
(`pHYs`).

### 2.8 Output conveniences. **S each, new**
- **BigTIFF on demand:** switch to BigTIFF automatically when output would exceed 4 GB. Today that's
  an error suggesting `--bigtiff`.
- **Auto-crop:** `--crop` to the largest rectangle fully covered by images, with no alpha holes.
- **Background colour:** `--background=RRGGBB` for uncovered areas instead of transparency.
- **More output formats:** WebP, AVIF or JPEG XL, since the port has no C library constraints.
- **Level clamping:** warn when `-l` is clamped to 1–29, or reject it.
- **`--version`:** print the version and exit.

### 2.9 Enblend compatibility mode. **M, new**
Hugin can call a custom blender, so accepting enblend's common options and output conventions
would make multiblend a drop-in replacement there. That means mapping the options, not reproducing
enblend's algorithm. Enblend options are already silently ignored, which is a start.

### 2.10 Seam maps for more than 256 images. **S, new**
Seam and XOR maps are 8-bit palette PNGs, so they stop at 256 images. Use 16-bit grayscale PNG
above that, and accept both on `--load-seams`.

---

## 3. Architecture and scale

### 3.1 Library crate and API. **M, new**
Split `main.rs` into a library (`multiblend::Blender` with inputs, options and progress
callbacks) plus a thin CLI.
- `die!()` exits the process; a library must return errors instead. This is the main refactor:
  convert the `die!` sites into a `thiserror` error type.
- This unlocks 3.3 and 3.4, embedding in other Rust tools, and unit-level tests of the pipeline
  without subprocesses.

### 3.2 Out-of-core / tiled blending. **L, exact**
This restores (and improves on) the dropped `--cache-threshold` disk spill, the one functional
regression from the C++. Two approaches:
- **Quick:** back the large buffers (channel planes and level-0 pyramids) with `memmap2` temp files
  above a size threshold. It keeps everything else unchanged; the OS pages things in and out.
- **Proper:** process the output in horizontal bands, with a halo of about 3 × 2^levels rows. Each
  band needs only nearby input rows, so memory becomes proportional to band height rather than
  image area. Gigapixel panoramas would fit in fixed RAM. This is a substantial rewrite of the
  pyramid driver, but the arithmetic is unchanged, so the sweep and dumps can verify it.

### 3.3 Python bindings. **M, new**
Build a PyO3 module that blends numpy arrays in memory, so users need no temp files. The existing
test suite is already Python and numpy, so bindings could also speed up the tests.

### 3.4 WebAssembly build. **M, new**
The crate has no C dependencies, so it compiles to `wasm32` cleanly. Use it for a browser demo, or
for client-side blending in a web stitcher. `rayon` needs wasm threads (`wasm-bindgen-rayon`), or a
serial fallback.

### 3.5 Progress reporting for GUIs. **S, new**
Add `--progress=json`: one JSON line per stage, with percent-done within the long stages, on stderr.
It makes multiblend easy to drive from GUIs and scripts without scraping text.

---

## 4. Quality, testing and tooling

### 4.1 Benchmark harness in the repo. **S**
The 8-image benchmark and the Linux container comparison were run with throwaway scripts.
- Add `rust/tools/bench.py`. It generates the scene, runs N interleaved warm runs per binary, and
  prints best/median plus the `--timing` breakdown and peak RSS.
- Optionally add Criterion micro-benchmarks for `squeeze`, `expand` and `composite_line`.
- Run it in CI as an informational job: no gating, but the numbers are in the logs for spotting
  regressions.

### 4.2 Fuzzing. **S–M**
The C++ crashed on many malformed inputs. The port exits cleanly on everything tried, but that's a
handful of hand-made files.
- Add `cargo fuzz` targets for the input decoders and for the full pipeline on tiny images, plus a
  CLI-argument fuzzer.
- Seed the corpus from the test scenes.

### 4.3 Property tests for the mask code. **S**
`squish`/`shrink_masks` keep the reference's run-length encoding for exactness, which makes them the
subtlest code in the port.
- Use `proptest` to check them against a dense, obviously-correct implementation on random masks,
  requiring the same float values within a tolerance.
- Add an exact check where values are 0/1 runs.

### 4.4 Remove the C++ from the test loop eventually. **M**
Today, parity is checked against a C++ build compiled in CI. Once the port is the product, freeze
the reference outputs: store `sweep.py` results as compressed goldens, keep the C++ as an archival
tool, and drop the Linux `apt` and C++ build from CI. Decide deliberately, since the live
comparison is currently our strongest correctness guarantee.

### 4.5 Distribution. **S–M each**
- **macOS:** a universal binary (x86_64 + arm64 via `lipo`) in CI, and Developer ID signing plus
  notarization, so downloads aren't quarantined.
- **Windows:** code signing, to avoid SmartScreen warnings.
- **Package managers:** a Homebrew tap, `cargo install multiblend` (publish the crate), winget and
  AUR.
- **Reproducibility:** add `--remap-path-prefix` so identical sources give identical binaries.

### 4.6 Documentation. **S**
- A man page, generated from the help text (`clap_mangen`, if the CLI moves to `clap`).
- A user guide covering what levels, wrap and seams do, with images.
- A `docs/` page explaining the algorithm and the exactness strategy for contributors.

---

## Suggested order

1. **Quick wins (S):** 1.1 build profile, 4.1 benchmark harness (do this first, so everything after
   is measured), the 2.8 conveniences (`--version`, level-clamp warning, BigTIFF on demand), and
   2.1 tiled TIFF input.
2. **Biggest measured gains:** 1.3 deflate plus predictor, 1.2 parallel strip decode, then 1.4
   write pipelining. Together these attack about 60% of runtime.
3. **The memory story:** 3.2 quick version (memmap), then 1.5 single-pass channels if memory
   allows.
4. **Foundation for integrations:** 3.1 library crate, then 3.5 progress output and 3.3 Python
   bindings.
5. **Image quality features:** 2.3 exposure compensation, 2.2 soft alpha, 2.4 smarter seams, all
   opt-in.
6. **Scale:** 3.2 proper banded blending, for gigapixel inputs.

Throughout: 4.2 fuzzing and 4.3 property tests are cheap insurance before the larger refactors
(3.1, 3.2, 1.5).
