# Plan: GitHub Actions for Linux and Windows builds

**Status: implemented** in `.github/workflows/rust.yml`; see "Implementation notes" at the end
for where it deviates from the plan and what was verified before the first push. The goal is a CI workflow that builds dependency-free
multiblend binaries for Linux and Windows, proves they work on each platform with the existing test
suite, checks that they're bit-identical to the C++ reference, and publishes them on tagged
releases.

## Outputs

| Artifact | Target | Built on | Linking |
|---|---|---|---|
| `multiblend-linux-x86_64` | `x86_64-unknown-linux-gnu` + `crt-static` | `ubuntu-24.04` | fully static (glibc) |
| `multiblend-linux-arm64` | `aarch64-unknown-linux-gnu` + `crt-static` | `ubuntu-24.04-arm` | fully static (glibc) |
| `multiblend-windows-x86_64.exe` | `x86_64-pc-windows-msvc` | `windows-2022` | static C runtime; only system DLLs (KERNEL32 etc.) |

- **Optional, later:** `aarch64-pc-windows-msvc`, and a universal macOS binary (`macos-14`, `lipo` of
  the x86_64 and aarch64 builds). The macOS build works locally already.
- **Why static glibc, not musl:** a normal glibc build runs only on distros with a glibc at least
  as new as the build machine's. The plan first chose musl for a fully static binary, but musl
  measured 65% slower (see Implementation notes). glibc linked with `+crt-static` is just as
  portable at full speed.
## Prep changes (before the workflow)

1. **Static Windows runtime.** Add `rust/.cargo/config.toml`:
   ```toml
   [target.x86_64-pc-windows-msvc]
   rustflags = ["-C", "target-feature=+crt-static"]
   [target.aarch64-pc-windows-msvc]
   rustflags = ["-C", "target-feature=+crt-static"]
   ```
2. **Remove the one platform-dependent float call.** `level_count` in `main.rs` uses `f32::log2`, which
   comes from each platform's libm. Every other operation is IEEE-exact, `sqrt` included.
   - For the integer sizes it's called with, `floor(log2(n + 4) - 1)` is exactly
     `31 - (n + 4).leading_zeros() - 1`.
   - Switch to that. Keep a unit test that compares it to the `f32` formula for `n` in `0..1<<20`,
     so the change is provably identical on the dev machine.
3. **Run `cargo fmt` once.** The tree currently has 108 formatting diffs, and CI will enforce
   `cargo fmt --check`. Do this as its own commit, since it's a large no-op diff.
4. **Make `rust/tools/build_dump_reference.py` work without Homebrew.** It calls `brew --prefix`
   unconditionally, which raises on Linux. Fall back to `/usr` when `brew` is missing.
5. **Check musl allocator speed.** musl's `malloc` is slow under multithreaded allocation.
   - Time the 8-image benchmark from `rust/README.md` with the musl build against a glibc build.
   - If musl is more than ~10% slower, add `mimalloc` as the global allocator for musl targets only
     (`#[cfg(target_env = "musl")]`).
   - Record the numbers either way.

## Workflow: `.github/workflows/rust.yml`

**Triggers:** push to `master` and `rust`, pull requests, and tags `v*` (tags also publish a
release). Cache with `Swatinem/rust-cache` (keyed per target) and pip's cache.

### Jobs

1. **`lint`** (`ubuntu-24.04`)
   - `cargo fmt --check`
   - `cargo clippy --release --all-targets --locked -- -D warnings`
   - `cargo test --release --locked`

2. **`build`** (matrix over the three targets above)
   - `rustup target add <target>`
   - Linux runners also get `sudo apt-get install -y musl-tools`. This is only a fallback linker;
     the crates are pure Rust, so the bundled self-contained musl linking should be enough on its own.
   - `cargo build --release --locked --target <target>`
   - **Prove it's dependency-free:**
     - Linux: `file` must say `statically linked`, and `ldd` must say `not a dynamic executable`.
     - Windows: `dumpbin /dependents` must list only system DLLs. It must not list `VCRUNTIME*.dll`
       or `api-ms-win-crt-*`.
   - Upload the binary as an artifact.

3. **`test`** (matrix, needs `build`, runs on the same OS/arch as each target)
   - `actions/setup-python` 3.13, then `pip install -r tests/requirements.txt`. Wheels exist for all
     pinned packages on Linux x86_64/arm64 and Windows x86_64. Confirm `imagecodecs` for arm64 on the
     first run.
   - Download the artifact, then run
     `MULTIBLEND_BIN=<path> python -m pytest` (Windows: `multiblend.exe`).
   - With `MULTIBLEND_BIN` set, the suite runs in port mode: no retries, and the `reference_bug`
     tests are ordinary tests. So this is the full contract: 200 tests.
   - **Linux only, no-dependency smoke test:** run a two-image blend inside `alpine:3` and
     `busybox`-based containers with the binary mounted in, and check the output with the runner's
     Python afterwards. This proves the binary needs nothing from the host.

4. **`parity`** (`ubuntu-24.04` x86_64, needs `build`)
   - `apt-get install -y libpng-dev libtiff-dev libjpeg-turbo8-dev g++`
   - `python rust/tools/build_dump_reference.py`, which builds the C++ `-O2` reference.
   - `python rust/tools/sweep.py build/multiblend-ref-O2 <linux-x86_64 binary>`
   - Expect `168 runs, 11 differ`, all of them the deliberate depth fix. Fail on any `UNEXPECTED`.
   - This is the bit-exactness guarantee, run on a different OS and libm than it was developed on.
   - Optionally run the sweep against the arm64 binary as well, with a second job on
     `ubuntu-24.04-arm` (the C++ reference needs SSE4.1, so it must still be built on x86).
     Rust never fuses multiply-adds, so ARM results should be identical; this checks that claim.

5. **`release`** (only on `v*` tags, needs `test` and `parity`)
   - Package each binary with `LICENSE`, `README.md` and a short `SOURCE.txt` linking the tagged
     source. Multiblend is GPLv3, so the source must be offered with the binary.
     - Linux: `.tar.gz`
     - Windows: `.zip`
   - Generate `SHA256SUMS`, then `gh release create $TAG` with the archives and checksums.

### Skeleton

```yaml
name: rust
on:
  push: { branches: [master, rust], tags: ["v*"] }
  pull_request:
jobs:
  lint: { runs-on: ubuntu-24.04, steps: [checkout, rust toolchain + cache, fmt, clippy, cargo test] }
  build:
    strategy:
      matrix:
        include:
          - { target: x86_64-unknown-linux-gnu,  os: ubuntu-24.04 }      # + crt-static
          - { target: aarch64-unknown-linux-gnu, os: ubuntu-24.04-arm }  # + crt-static
          - { target: x86_64-pc-windows-msvc,     os: windows-2022 }
    runs-on: ${{ matrix.os }}
    steps: [checkout, rustup target add, cargo build --target, dependency check, upload-artifact]
  test:
    needs: build
    strategy: { matrix: <same include list> }
    runs-on: ${{ matrix.os }}
    steps: [checkout, setup-python, pip install, download-artifact, pytest, (linux) alpine smoke test]
  parity:
    needs: build
    runs-on: ubuntu-24.04
    steps: [checkout, apt deps, build_dump_reference.py, sweep.py]
  release:
    if: startsWith(github.ref, 'refs/tags/v')
    needs: [test, parity]
    runs-on: ubuntu-24.04
    permissions: { contents: write }
    steps: [download all artifacts, package + LICENSE + SOURCE.txt, sha256sum, gh release create]
```

## Risks and open questions

- **Windows test harness:** it has only been run on macOS. Likely friction points:
  - path handling in `tests/mb.py` (`MULTIBLEND_BIN` needs the `.exe`);
  - spawn cost in `test_startup_is_race_free` (150 runs of a tiny blend should still take a few
    seconds);
  - the filesystem is case-insensitive, which the suite already allows for.

  Budget one iteration to fix whatever the first Windows run turns up.
- **Golden input digests:** these hash images generated by numpy and tifffile. They're deterministic
  across platforms with pinned versions, but if a digest fails on Windows or ARM, the likely cause is
  a wheel difference (e.g. `imagecodecs` built against a different library). Fix the digest scope
  rather than regenerating the goldens.
- **The C++ reference on Linux:** only built on macOS so far. `build.txt` gives Linux flags, and
  `mapalloc.cpp` includes `<malloc.h>` off Apple, which is fine.
- **Code signing:** Windows SmartScreen will warn about an unsigned `.exe`. Signing is out of scope
  for this plan; note it in the release notes.
- **Runner cost:** `ubuntu-24.04-arm` runners are free for public repositories. For a private repo,
  check the plan's minutes, or drop arm64 to tags only.

## Done when

- A push to `rust` gives green `lint`, `build`, `test` (3 targets × 200 tests) and `parity`
  (sweep equals the 11 expected differences).
- Linux binaries report as statically linked and run inside a bare Alpine container.
- The Windows binary's only DLL dependencies are system DLLs.
- Pushing a `v*` tag produces a GitHub Release with three archives, `SHA256SUMS`, the license and a
  source link.

## Implementation notes (2026-09-28)

### Deviation: static glibc instead of musl on Linux

The plan chose musl for fully static Linux binaries, with `mimalloc` as a fallback if musl's
allocator proved slow. Measured on the 8-image benchmark (8 × 4000×3000 LZW TIFFs), in
`ubuntu:24.04` containers with 4 CPUs. Inputs were copied into the container, there was one
warm-up run, and runs were interleaved; the table shows best of 4:

| Build | Best | Notes |
|---|---|---|
| musl | 12.2 s | image reading 3× slower than glibc |
| musl + mimalloc | 10.2 s | allocator helps a little; reading still slow |
| glibc, dynamic | 6.1 s | |
| **glibc, static (`+crt-static`)** | **6.0 s** | chosen |

The gap is in image reading. LZW decompression makes very many small memory copies, and musl's
`memcpy` is far slower than glibc's for those, so it isn't an allocator problem. A fully static
glibc binary keeps glibc's speed and still has no runtime dependencies.

- It needs Linux 3.2 or newer.
- Static glibc can't use NSS (user and host lookups), `dlopen` or `iconv`. multiblend uses none of
  them, and the build produces no static-glibc linker warnings.
- `mimalloc` was removed again.
- `+crt-static` is set per target in the CI build step, not in `.cargo/config.toml`. For
  `*-linux-gnu` it must not reach host build scripts or proc macros (the tree includes
  `zerocopy-derive`), which a global setting would for builds without `--target`.

### Other changes from the plan

- `level_count` is now integer-exact. It matches the reference's f32 formula for every size below
  2^21 (unit test over 0..2^20). Above that the reference rounds up for a few sizes just below a
  power of two.
- `cargo fmt` was applied to the whole crate.
- `build_dump_reference.py` falls back to `/usr` when Homebrew is absent.
- **`ldd` isn't used for the static check.** On static-PIE binaries it isn't reliable, and `file`
  reports "static-pie linked" rather than "statically linked". The check instead requires no
  `NEEDED` entries and no program interpreter.
- actionlint found that a bare `! cmd` doesn't fail a `bash -e` step. The check now uses explicit
  `if … exit 1`.

### Verified locally before the first push

- **Formatting and lints:** `cargo fmt --check` passes, and so does
  `cargo clippy --release --all-targets --locked -- -D warnings`. `cargo test` passes 7 tests.
  actionlint reports no issues.
- **Linux x86_64 build step, replayed exactly** in `ubuntu:24.04`:
  - The static check passes on the static build.
  - It fails on a dynamic build (negative control).
- **`test` job, replayed:** 200/200 tests pass against the static Linux binary. The binary also runs
  in bare `alpine:3` and `busybox:musl` containers, with output byte-identical to the C++ reference.
- **`parity` job, replayed:** the C++ reference, built by GCC on Ubuntu 24.04 with the fixed build
  script, gives `168 runs, 11 differ`. All 11 are the expected depth-fix cases.
- **`release` packaging, dry run:** three archives, each with the binary, `LICENSE`, `README.md` and
  `SOURCE.txt`, plus `SHA256SUMS`.

### Only verifiable on GitHub

- Windows: the build, the `dumpbin` dependency check, and the test suite on Windows (see Risks).
- linux-arm64 on `ubuntu-24.04-arm`.
- The action versions, artifact names, and `gh release create` permissions.
