# multiblend test suite

This is a black-box suite: it runs the `multiblend` executable on generated images and checks
the files it writes. It is the behavioural contract for the Rust port. The same tests run against
the C++ reference and against any other binary.

```sh
make test                      # builds build/multiblend (+ ASan build) and .venv, then runs pytest
.venv/bin/python -m pytest -k golden          # run a subset
MULTIBLEND_BIN=path/to/port .venv/bin/python -m pytest   # run against another implementation
make test-rust                 # build the Rust port (rust/) and run the suite against it
```

The build needs libpng, libtiff and libjpeg(-turbo). The Makefile defaults to Homebrew paths;
override them with `PREFIX=...`.

## Layout

| File | What it checks |
|---|---|
| `test_cli.py` | Argument parsing, validation errors, exit codes, option aliases |
| `test_formats.py` | TIFF/PNG/JPEG input and output, 8/16-bit, compression, strips, byte order, metadata tags |
| `test_blend.py` | Invariants any correct blender satisfies: identity cases, disjoint images, coverage/alpha, seams, wrap, bit depth, dither |
| `test_golden.py` | Recorded reference outputs for 18 scenes (`golden/*.npz`) |
| `test_known_bugs.py` | Correct behaviour for confirmed reference defects and the confirmed port decisions |
| `FINDINGS.md` | Bug table, the semantics a port must keep, unused code, port decisions |
| `mb.py`, `scenes.py` | Helpers: run the binary, generate images, read outputs |

## Reference vs port

The binary under test counts as the **reference** when `MULTIBLEND_BIN` is unset, or when
`MULTIBLEND_IS_REFERENCE=1`. The two modes behave differently:

| | reference (C++) | any other binary |
|---|---|---|
| `@reference_bug` tests | `xfail` (strict unless the failure is nondeterministic) | ordinary tests, so the port must fix every one |
| process killed by a signal | retried up to 3 times (startup race) | fails |
| other failing tests | rerun up to 2 times (thread-pool races) | fails |

The retries exist only because the C++ thread pool has two races (`FINDINGS.md` #1 and #2): about 2%
of runs crash at startup, and rarely a run produces corrupted output. A port gets no retries.

## Golden outputs

The comparison is strict where the output is discrete and tolerant only where floating-point
arithmetic can legitimately differ:

- Alpha channels and seam maps must match exactly.
- Colour samples may be off by 1 in at most 0.1% (8-bit) or 0.5% (16-bit) of samples.

For scale: the reference's own `-O2` build differs from the `-Ofast` goldens by at most 0.004% /
0.22%. Changing one blend coefficient by 0.8% fails 17 of 18 goldens. Set
`MULTIBLEND_GOLDEN_EXACT=1` to require bit-exact output. Only the `-Ofast` reference build passes that mode: `-O2` builds, including the Rust port, differ by 1 in a few 16-bit and gamma cases. For exact checks of the port, compare against `build/multiblend-ref-O2` with `rust/tools/sweep.py`.

Each golden also stores a digest of its generated inputs. If a numpy, tifffile or imagecodecs
upgrade changes the synthetic images, the test says so instead of reporting a blend regression.
Regenerate the goldens with the reference build only:

```sh
.venv/bin/python -m pytest tests/test_golden.py --update-golden
```

Regeneration records the majority result of three runs.

JPEG output is compared by luminance error rather than by golden, since encoders differ.
Scenarios that hit known reference bugs are deliberately kept out of the goldens.
