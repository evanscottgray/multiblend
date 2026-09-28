# multiblend (Rust port)

A Rust port of Multiblend 2.0 (`../src`, C++). The command-line interface is the same.

```sh
make rust        # or: cd rust && cargo build --release
make test-rust   # the black-box suite in ../tests against rust/target/release/multiblend
cargo test       # a few unit tests for helpers the black-box suite only reaches indirectly
```

## Status

- **Test suite:** every test in `../tests` passes (200), including the golden outputs and the
  `reference_bug` tests, which encode the port decisions in `../tests/FINDINGS.md`.
- **Exactness:** output is **bit-identical** to an `-O2` build of the C++ reference, in both output
  pixels and seam maps, across `tools/sweep.py` (168 scene/option combinations) and a 5200×3700
  four-image blend. The only differences are the deliberate fixes, such as the ×257 scaling when the
  output depth differs from the input.
- **Speed:** on the 5200×3700 blend it runs in about 1.3–1.6 s, against 1.7 s for the C++ build. It
  uses all cores; the reference used 2 threads.

## Layout

| File | Contents | C++ origin |
|---|---|---|
| `cli.rs` | argument parsing and help | top of `main()` |
| `io.rs` | TIFF/PNG/JPEG input; baseline TIFF/BigTIFF writer; PNG and JPEG output | `image.cpp` Open/Read, `pnger.cpp` |
| `image.rs` | trim to the opaque bounding box, nearest-pixel inpainting, channel planes | `image.cpp` |
| `seam.rs` | backward/forward chamfer distance transform, seam and XOR maps | "Seaming" in `main()` |
| `masks.rs` | run-length blend masks and their reduction per level | `ShrinkMasks`, `Squish` |
| `pyramid.rs` | shrink, expand, Laplacian collapse, compositing, output | `pyramid.cpp`, `CompositeLine` |
| `main.rs` | the pipeline | `main()` |

## How exactness is kept

- The reference's SSE arithmetic is written as scalar `f32`, in the same operand order: every
  `hadd` pairing, every `a + b*4` versus `(a + b)*4`, and the per-case edge formulas in
  `ShrinkThread`. Rust never reassociates floats or fuses multiply-adds.
- Rounding matches `cvtps_epi32` (round half to even), and clamping matches `max_ps`/`min_ps`,
  including their NaN behaviour.
- The mask word format is kept, because whether a value is stored as a run or as a single float
  changes the rounding in the next level.
- Output never depends on how rows are split across threads; the reference behaves the same way
  (verified at 1–8 threads).

## Debugging a mismatch

```sh
.venv/bin/python rust/tools/build_dump_reference.py    # C++ -O2 build with dump hooks
MB_DUMP_DIR=ref build/multiblend-ref-O2 -o r.tif a.tif b.tif
MB_DUMP_DIR=port rust/target/release/multiblend -o p.tif a.tif b.tif
.venv/bin/python rust/tools/dumpdiff.py ref port       # first stage that differs
.venv/bin/python rust/tools/sweep.py build/multiblend-ref-O2 rust/target/release/multiblend
```

The dumps cover the inpainted channels, the masks per level, the pyramids after shrink and after
Laplace, the composited output levels, and the output after collapse, wrap and offset correction.
