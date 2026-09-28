"""Deterministic input scenes shared by the invariant and golden tests.

Each scene function writes its input files into `d` and returns the list of
positional arguments (input files and optional X,Y offsets) to pass to
multiblend after the options.
"""

import numpy as np

from mb import circle_mask, gradient, with_alpha, write_png, write_tiff


def two_circles(d, bpp=8, w=100, h=70, offset=(50, 10)):
    m = circle_mask(w, h)
    write_tiff(d / "A.tif", with_alpha(gradient(w, h, bpp, seed=10), m), pos=(0, 0))
    write_tiff(d / "B.tif", with_alpha(gradient(w, h, bpp, seed=11), m), pos=offset)
    return ["A.tif", "B.tif"]


def three_circles(d, bpp=8):
    m = circle_mask(100, 70)
    write_tiff(d / "A.tif", with_alpha(gradient(100, 70, bpp, seed=10), m), pos=(0, 0))
    write_tiff(d / "B.tif", with_alpha(gradient(100, 70, bpp, seed=11), m), pos=(50, 10))
    write_tiff(d / "C.tif", with_alpha(gradient(100, 70, bpp, seed=12), m), pos=(25, 40))
    return ["A.tif", "B.tif", "C.tif"]


def odd_sizes(d):
    """Odd dimensions and odd offsets exercise the pyramid's x/y shift paths."""
    specs = [((37, 23), (0, 0)), ((41, 29), (19, 7)), ((33, 31), (7, 15))]
    args = []
    for i, ((w, h), pos) in enumerate(specs):
        alpha = circle_mask(w, h, r=max(w, h) * 0.6)
        write_tiff(d / f"o{i}.tif", with_alpha(gradient(w, h, seed=30 + i), alpha), pos=pos)
        args.append(f"o{i}.tif")
    return args


def rgb_grid(d):
    """2x2 grid of opaque RGB tiles (no alpha channel) placed with [X,Y] arguments."""
    args = []
    for i, (x, y) in enumerate([(0, 0), (60, 0), (0, 45), (60, 45)]):
        write_tiff(d / f"t{i}.tif", gradient(80, 60, seed=40 + i))
        args += [f"t{i}.tif", f"{x},{y}"]
    return args


def exposure_step(d):
    """Same content, different brightness: blending should produce a smooth transition."""
    base = gradient(120, 80, seed=50, noise=False).astype(np.int32)
    write_tiff(d / "dark.tif", np.clip(base - 40, 0, 255).astype(np.uint8)[:, :80], pos=(0, 0))
    write_tiff(d / "bright.tif", np.clip(base + 40, 0, 255).astype(np.uint8)[:, 40:], pos=(40, 0))
    return ["dark.tif", "bright.tif"]


def panorama_strip(d):
    """Images spanning 360 degrees horizontally, for --wrap=horizontal."""
    args = []
    for i in range(4):
        m = np.ones((60, 70), bool)
        m[:, :3] = False  # ragged edges so trimming/inpainting is exercised
        write_tiff(d / f"p{i}.tif", with_alpha(gradient(70, 60, seed=60 + i), m), pos=(i * 50, (i % 2) * 3))
    return [f"p{i}.tif" for i in range(4)]


def vertical_strip(d):
    for i in range(3):
        write_tiff(d / f"v{i}.tif", gradient(50, 60, seed=70 + i), pos=(0, i * 45))
    return [f"v{i}.tif" for i in range(3)]


def many_tiles(d):
    """Nine overlapping tiles with circular alpha: a many-image stress case."""
    args = []
    for i in range(9):
        x, y = (i % 3) * 30, (i // 3) * 25
        m = circle_mask(50, 40, r=24)
        write_tiff(d / f"m{i}.tif", with_alpha(gradient(50, 40, seed=80 + i), m), pos=(x, y))
        args.append(f"m{i}.tif")
    return args


def odd_canvas(d):
    """Canvas with odd width and height (151x97), for wrapping's odd-size swaps."""
    specs = [((81, 61), (0, 0)), ((81, 61), (70, 5)), ((77, 55), (33, 44))]
    for i, ((w, h), pos) in enumerate(specs):
        m = np.ones((h, w), bool)
        m[:2, :] = False  # transparent top rows: exercises trimming too
        write_tiff(d / f"c{i}.tif", with_alpha(gradient(w, h, seed=90 + i), m), pos=pos)
    return [f"c{i}.tif" for i in range(3)]


def png_inputs(d, bpp=8):
    m = circle_mask(100, 70)
    write_png(d / "A.png", with_alpha(gradient(100, 70, bpp, seed=10), m))
    write_png(d / "B.png", with_alpha(gradient(100, 70, bpp, seed=11), m))
    return ["A.png", "B.png", "50,10"]


SCENES = {
    "two_circles": two_circles,
    "three_circles": three_circles,
    "two_circles_16": lambda d: two_circles(d, bpp=16),
    "odd_sizes": odd_sizes,
    "rgb_grid": rgb_grid,
    "exposure_step": exposure_step,
    "panorama_strip": panorama_strip,
    "vertical_strip": vertical_strip,
    "many_tiles": many_tiles,
    "odd_canvas": odd_canvas,
}
