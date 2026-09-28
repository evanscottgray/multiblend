# Changelog

This covers the Rust port of multiblend (`rust/`). Versions continue Multiblend's own numbering; the
C++ original in `src/` is Multiblend 2.0.0 (rc5).

## [2.1.0] - 2026-09-28

The first release of the Rust port. It is a drop-in replacement for Multiblend 2.0 with the same
command line. Blended output is **bit-identical** to Multiblend 2.0, except where a bug fix changes
it (listed below). The 168-case comparison in `rust/tools/sweep.py` checks this against the C++
source.

### Distribution

- Self-contained binaries, with the TIFF, PNG and JPEG code compiled in:
  - Linux x86_64 and arm64: fully static; any distro with Linux 3.2+, including Alpine.
  - Windows x86_64: no Visual C++ runtime needed. The binary is not code-signed, so SmartScreen may
    warn.

### Performance

- All CPU cores are used by default. Multiblend 2.0 always used at most 2 threads, even with
  `--all-threads`.
- Input images are decoded in parallel, capped at 1 GiB of temporary buffers.
- On a 4-core machine, eight 4000×3000 LZW TIFFs blend in about 6.8 s against 8.4 s for Multiblend
  2.0. Peak memory is about 1.8 GB against 1.3 GB.

### Fixed: crashes and hangs

- A random crash at startup (~2% of runs, far more under load): a thread-pool race.
- Rare silently corrupted output: a thread-pool race.
- Crashes on:
  - an input with no fully opaque pixel;
  - a 1-row image with transparency;
  - wide, short images with busy alpha;
  - tiled TIFFs;
  - any GeoTIFF;
  - truncated TIFF or PNG files;
  - out-of-range `-l` values;
  - no input files.
- A seam PNG of the wrong height aborted inside libpng. It is now a clear error.

### Fixed: wrong output

- 16-bit PNG inputs were read byte-swapped.
- Changing bit depth (`-d 8` / `-d 16`) biased the blend: 8→16 came out ~130–185 levels too dark,
  and 16→8 half a level too bright. The scale factor is now ×257 (×66049 with `--gamma`)
  throughout.
- If any image had a negative position, the output TIFF was unreadable but the exit code was still
  0. The TIFF position is now clamped to 0, with a warning.
- A TIFF with only an X position (no Y position) was placed at a random height. A missing position
  now means 0.
- `--reverse` had no effect. Undecided pixels now go to the last image.
- Interlaced PNG inputs were misread.
- A seam file index equal to the number of images was accepted. It is now rejected.

### Fixed: command line

- `--wrap=HORIZONTAL`, `VERTICAL` and `NONE`, as documented, were rejected. Wrap modes are now
  case-insensitive.
- Malformed `--levels` and `--cache-threshold` values could be silently accepted; they are now
  errors.
- `--help` wrongly said TIFF compression defaults to NONE. It is LZW.

### Changed

- **GeoTIFF support is removed.** It crashed and placed images upside down. Geo tags on input are
  ignored and no longer written to output.
- Unsupported inputs are now clear errors naming the file, rather than being misread:
  - grayscale or other non-RGB(A) images;
  - tiled or planar-separate TIFFs;
  - images with no fully opaque pixel.
- The level count is limited to 1–29.

### Known limitations

- Everything is held in memory. `--cache-threshold` and `--tempdir` are still accepted but do
  nothing (Multiblend 2.0 could spill to temporary files), so very large blends need enough RAM.
