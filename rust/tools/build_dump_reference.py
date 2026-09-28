#!/usr/bin/env python3
"""Build build/multiblend-ref-O2: the C++ reference at -O2 with debug dump hooks.

The Rust port is bit-exact against this build (not the -Ofast one the goldens
come from). With MB_DUMP_DIR=dir set, both binaries write the same intermediate
arrays, and rust/tools/dumpdiff.py reports the first stage that differs.

The copy lives in build/src-dump; src/ is never modified. The copy also fixes
the thread-pool startup race so the dumping reference doesn't crash randomly.

Usage: .venv/bin/python rust/tools/build_dump_reference.py
"""

import shutil
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SRC = ROOT / "src"
DST = ROOT / "build" / "src-dump"
OUT = ROOT / "build" / "multiblend-ref-O2"


def patch(name, old, new):
    p = DST / name
    s = p.read_text()
    assert s.count(old) == 1, f"{name}: expected one match for {old[:60]!r}"
    p.write_text(s.replace(old, new))


def main():
    shutil.rmtree(DST, ignore_errors=True)
    shutil.copytree(SRC, DST)

    patch("multiblend.cpp", '#include "image.cpp"', r'''
#include <cstdlib>
static const char* dump_dir() { return getenv("MB_DUMP_DIR"); }
static void dump_raw(const char* name, const void* p, size_t bytes, int w, int h, const char* ext) {
	if (!dump_dir()) return;
	char path[1024];
	snprintf(path, sizeof path, "%s/%s_%dx%d.%s", dump_dir(), name, w, h, ext);
	FILE* f = fopen(path, "wb"); fwrite(p, 1, bytes, f); fclose(f);
}
static void dump_flex_mask(const char* name, Flex* m) {
	if (!dump_dir()) return;
	std::vector<float> dense((size_t)m->width * m->height);
	for (int y = 0; y < m->height; ++y) {
		uint32_t* data = (uint32_t*)(m->data + m->rows[y]);
		int x = 0;
		while (x < m->width) {
			uint32_t cur = *data++; float val; int count;
			if (cur & 0x80000000) { count = cur & 0x00ffffff; if (cur & 0x20000000) val = *(float*)data++; else val = (float)((cur >> 30) & 1); }
			else { val = *((float*)&cur); count = 1; }
			for (int t = x + count; x < t && x < m->width; ++x) dense[(size_t)y * m->width + x] = val;
		}
	}
	dump_raw(name, dense.data(), dense.size() * 4, m->width, m->height, "f32");
}
static void dump_level(const char* name, Pyramid* py, int l) {
	auto& lev = py->GetLevel(l);
	dump_raw(name, lev.data, (size_t)lev.pitch * lev.height * 4, lev.pitch, lev.height, "f32");
}
static int dump_image_counter = 0;
#include "image.cpp"''')

    patch("image.cpp", '''	Output(1, "\\n");
}''', '''	{
		char n[64];
		for (int c = 0; c < 3; ++c) {
			snprintf(n, sizeof n, "img%d_ch%d", dump_image_counter, c);
			dump_raw(n, channels[c]->data, channel_bytes, width, height, bpp == 8 ? "u8" : "u16");
		}
		dump_image_counter++;
	}
	Output(1, "\\n");
}''')

    patch("multiblend.cpp", '''		shrink_mask_time = timer.Read();''', '''		shrink_mask_time = timer.Read();
		for (i = 0; i < n_images; ++i) {
			for (int l = 0; l < (int)images[i]->masks.size(); ++l) {
				char n[64]; snprintf(n, sizeof n, "mask%d_l%d", i, l);
				dump_flex_mask(n, images[i]->masks[l]);
			}
		}''')

    patch("multiblend.cpp", '''					images[i]->pyramid->Shrink();
					shrink_time += timer.Read();

					timer.Start();
					images[i]->pyramid->Laplace();''', '''					images[i]->pyramid->Shrink();
					shrink_time += timer.Read();
					for (int l = 0; l < blend_levels; ++l) { char n[64]; snprintf(n, sizeof n, "shrink%d_c%d_l%d", i, c, l); dump_level(n, images[i]->pyramid, l); }

					timer.Start();
					images[i]->pyramid->Laplace();
					for (int l = 0; l < blend_levels; ++l) { char n[64]; snprintf(n, sizeof n, "laplace%d_c%d_l%d", i, c, l); dump_level(n, images[i]->pyramid, l); }''')

    patch("multiblend.cpp", '''				timer.Start();
				output_pyramid->Collapse(blend_levels);''', '''				for (int l = 0; l < blend_levels; ++l) { char n[64]; snprintf(n, sizeof n, "blend_c%d_l%d", c, l); dump_level(n, output_pyramid, l); }
				timer.Start();
				output_pyramid->Collapse(blend_levels);''')

    patch("multiblend.cpp", '''			if (wrap) {
				timer.Start();

				int p = 0;''', '''			{ char n[64]; snprintf(n, sizeof n, "collapsed_c%d", c); dump_level(n, output_pyramid, 0); }
			if (wrap) {
				timer.Start();

				int p = 0;''')
    patch("multiblend.cpp", '''			if (total_pixels) {
				double channel_total = 0; // must be a double''', '''			{ char n[64]; snprintf(n, sizeof n, "wrapped_c%d", c); dump_level(n, output_pyramid, 0); }
			if (total_pixels) {
				double channel_total = 0; // must be a double''')
    patch("multiblend.cpp", '''			timer.Start();

			try {
				output_channels[c] = MapAlloc::Alloc(((size_t)width * height) << (output_bpp >> 4));''', '''			{ char n[64]; snprintf(n, sizeof n, "corrected_c%d", c); dump_level(n, output_pyramid, 0); }
			timer.Start();

			try {
				output_channels[c] = MapAlloc::Alloc(((size_t)width * height) << (output_bpp >> 4));''')

    # Start worker threads only after their fields are set (FINDINGS #1).
    p = DST / "threadpool.cpp"
    s = p.read_text()
    blk = "#ifdef _WIN32\n\t\tthreads[i].handle = CreateThread(NULL, 1, (LPTHREAD_START_ROUTINE)Thread, &threads[i], 0, NULL);\n#else\n\t\tpthread_create(&threads[i].handle, NULL, TP_Thread, &threads[i]);\n#endif\n"
    assert blk in s
    s = s.replace(blk, "").replace("\t\tthreads[i].i = i;\n", "\t\tthreads[i].i = i;\n\t\tpthread_create(&threads[i].handle, NULL, TP_Thread, &threads[i]);\n")
    p.write_text(s)

    try:
        prefix = subprocess.run(["brew", "--prefix"], capture_output=True, text=True).stdout.strip() or "/usr"
    except FileNotFoundError:  # no Homebrew (e.g. Linux CI): system libraries
        prefix = "/usr"
    inc = [f"-I{prefix}/opt/{lib}/include" for lib in ("jpeg-turbo", "libpng", "libtiff")] + [f"-I{prefix}/include"]
    lib = [f"-L{prefix}/opt/{lib}/lib" for lib in ("jpeg-turbo", "libpng", "libtiff")] + [f"-L{prefix}/lib"]
    subprocess.run(["c++", "-std=c++14", "-msse4.1", "-pthread", "-w", "-O2", *inc, *lib, "-o", str(OUT),
                    str(DST / "multiblend.cpp"), "-lpng", "-ltiff", "-ljpeg"], check=True)
    print(f"built {OUT}")


if __name__ == "__main__":
    main()
