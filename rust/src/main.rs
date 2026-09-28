//! Multiblend: multi-level (Laplacian pyramid) image blending.
//! Rust port of Multiblend 2.0 by David Horman (GPLv3).

mod cli;
mod image;
mod io;
mod masks;
mod pyramid;
mod seam;
mod util;

use std::fs::File;

use rayon::prelude::*;

use cli::{BANNER, ImageType, RULE, TiffCompression};
use image::{Image, Plane};
use masks::{MaskLevel, shrink_masks};
use pyramid::{Pyramid, composite_line, swap_h, swap_v};
use util::{Timer, dump, dump_enabled, f32_bytes};

/// Levels are capped so level positions and `>> level` shifts stay in range.
const MAX_LEVELS: i32 = 29;

struct WrapPyramid {
    py: Pyramid,
    masks: Vec<MaskLevel>,
}

fn level_count(size: f32) -> i32 {
    ((size + 4.0f32).log2() - 1.0).floor() as i32
}

fn dump_level(name: &str, py: &Pyramid, l: usize) {
    if dump_enabled() {
        let lev = &py.levels[l];
        dump(name, &f32_bytes(&py.data[l][..lev.pitch * lev.height]), lev.pitch, lev.height, "f32");
    }
}

#[allow(clippy::needless_range_loop)]
/// Composite pyramid `input` into `output` for levels [0, n) with `masks`.
fn composite(output: &mut Pyramid, input: &Pyramid, masks: &[MaskLevel], n: usize, first_at: impl Fn(usize) -> bool + Sync) {
    for l in 0..n {
        let il = &input.levels[l];
        let ol = output.levels[l].clone();
        let x_offset = ((il.x - ol.x) >> l) as i64;
        let y_offset = ((il.y - ol.y) >> l) as i64;
        let in_data = &input.data[l];
        let mask = &masks[l];
        let first = first_at(l);
        output.data[l][..ol.pitch * ol.height].par_chunks_mut(ol.pitch).enumerate().for_each(|(y, out_row)| {
            let in_line = (y as i64 - y_offset).clamp(0, il.height as i64 - 1) as usize;
            let in_row = &in_data[in_line * il.pitch..(in_line + 1) * il.pitch];
            composite_line(in_row, out_row, first, x_offset, il.width as i64, ol.width as i64, mask.row(y));
        });
    }
}

#[allow(clippy::needless_range_loop)]
fn main() {
    let timer_all = Timer::start();
    let opts = cli::parse(std::env::args().collect());

    out!(1, "\n");
    out!(1, "{BANNER}");
    out!(1, "{RULE}");

    let mut output_bpp = opts.output_bpp;
    let output_file: Option<File> = match opts.output_type {
        ImageType::None => None,
        t => {
            if t == ImageType::Jpeg && output_bpp == 16 {
                die!("Error: 16bpp output is incompatible with JPEG output");
            }
            Some(File::create(opts.output.as_ref().unwrap()).unwrap_or_else(|_| die!("Error: Could not open output file")))
        }
    };

    let mut timer = Timer::start();

    // Open images for preliminary info.
    let mut images: Vec<Image> = opts.inputs.iter().map(|i| Image::open(&i.filename, i.xpos_add, i.ypos_add)).collect();
    let n_images = images.len();

    for img in &images[1..] {
        let (a, b) = (&images[0].info, &img.info);
        if a.xres != b.xres || a.yres != b.yres {
            out!(0, "Warning: TIFF resolution mismatch ({:.6} {:.6}/{:.6} {:.6})\n", a.xres, a.yres, b.xres, b.yres);
        }
    }
    for img in &images {
        if output_bpp == 0 && img.bpp == 16 {
            output_bpp = 16;
        }
        if img.bpp != images[0].bpp {
            die!("Error: mixture of 8bpp and 16bpp images detected (not currently handled)\n");
        }
    }
    if output_bpp == 0 {
        output_bpp = 8;
    } else if output_bpp == 16 && opts.output_type == ImageType::Jpeg {
        out!(0, "Warning: 8bpp output forced by JPEG output\n");
        output_bpp = 8;
    }

    for (i, img) in images.iter_mut().enumerate() {
        img.read(i);
    }

    // Tighten
    let min_xpos = images.iter().map(|i| i.xpos).min().unwrap();
    let min_ypos = images.iter().map(|i| i.ypos).min().unwrap();
    let mut width = 0usize;
    let mut height = 0usize;
    for img in images.iter_mut() {
        img.xpos -= min_xpos;
        img.ypos -= min_ypos;
        width = width.max(img.xpos as usize + img.width);
        height = height.max(img.ypos as usize + img.height);
    }
    let images_time = timer.read();

    // Number of levels
    let mut blend_levels = if opts.fixed_levels == 0 {
        let blend_wh = if !opts.wideblend {
            let mut ws: Vec<usize> = images.iter().map(|i| i.width).collect();
            let mut hs: Vec<usize> = images.iter().map(|i| i.height).collect();
            ws.sort_unstable();
            hs.sort_unstable();
            let half = (ws.len() - 1) >> 1;
            let med = |v: &[usize]| if v.len() & 1 == 1 { v[half] } else { (v[half] + v[half + 1] + 1) >> 1 };
            med(&ws).max(med(&hs))
        } else {
            width.max(height)
        };
        level_count(blend_wh as f32) + opts.wideblend as i32
    } else {
        opts.fixed_levels
    };
    blend_levels += opts.add_levels;

    if n_images == 1 {
        blend_levels = 0;
        out!(1, "\n{width} x {height}, {output_bpp} bpp\n\n");
    } else {
        blend_levels = blend_levels.clamp(1, MAX_LEVELS);
        out!(1, "\n{width} x {height}, {blend_levels} levels, {output_bpp} bpp\n\n");
    }
    let blend_levels = blend_levels as usize;

    // Seaming
    timer.restart();
    out!(1, "Seaming");
    match (opts.seamsave.is_some(), opts.xor.is_some()) {
        (false, true) => out!(1, " (saving XOR map)"),
        (true, false) => out!(1, " (saving seam map)"),
        (true, true) => out!(1, " (saving XOR and seam maps)"),
        _ => {}
    }
    out!(1, "...\n");
    let seams = seam::seam(
        &mut images,
        width,
        height,
        &seam::SeamOptions {
            reverse: opts.reverse,
            gamma: opts.gamma,
            seamload: opts.seamload.as_deref(),
            seamsave: opts.seamsave.as_deref(),
            xor: opts.xor.as_deref(),
        },
    );
    let no_mask = opts.no_mask || !seams.alpha || opts.output_type == ImageType::Jpeg;
    let seam_time = timer.read();

    let (mut shrink_mask_time, mut copy_time, mut shrink_time, mut laplace_time) = (0.0, 0.0, 0.0, 0.0);
    let (mut blend_time, mut collapse_time, mut wrap_time, mut out_time, mut write_time) = (0.0, 0.0, 0.0, 0.0, 0.0);

    if let Some(output_file) = output_file {
        out!(1, "Shrinking masks...\n");
        timer.restart();
        images.par_iter_mut().for_each(|img| shrink_masks(&mut img.masks, blend_levels));
        shrink_mask_time = timer.read();
        if dump_enabled() {
            for (i, img) in images.iter().enumerate() {
                for (l, m) in img.masks.iter().enumerate() {
                    dump(&format!("mask{i}_l{l}"), &f32_bytes(&m.dense()), m.width, m.height, "f32");
                }
            }
        }

        // Wrapping pyramids and their masks
        let wrap_levels_h = if opts.wrap & 1 != 0 { level_count((width >> 1) as f32) } else { 0 }.clamp(0, MAX_LEVELS) as usize;
        let wrap_levels_v = if opts.wrap & 2 != 0 { level_count((height >> 1) as f32) } else { 0 }.clamp(0, MAX_LEVELS) as usize;
        let mut wrap_pyramids: Vec<WrapPyramid> = Vec::new();
        let mut add_wrap = |w: usize, h: usize, levels: usize, x: usize, y: usize| {
            let mut m = MaskLevel::new(width, height);
            for row in 0..height {
                if row < y || row >= y + h {
                    m.words.push(0x8000_0000 | width as u32);
                } else if x != 0 {
                    m.words.push(0x8000_0000 | x as u32);
                    m.words.push(0xc000_0000 | w as u32);
                } else {
                    m.words.push(0xc000_0000 | w as u32);
                    if w != width {
                        m.words.push(0x8000_0000 | (width - w) as u32);
                    }
                }
                if row + 1 < height {
                    m.next_row();
                }
            }
            wrap_pyramids.push(WrapPyramid { py: Pyramid::new(w, h, levels, x as i32, y as i32), masks: vec![m] });
        };
        if opts.wrap & 1 != 0 {
            add_wrap(width >> 1, height, wrap_levels_h, 0, 0);
            add_wrap(width.div_ceil(2), height, wrap_levels_h, width >> 1, 0);
        }
        if opts.wrap & 2 != 0 {
            add_wrap(width, height >> 1, wrap_levels_v, 0, 0);
            add_wrap(width, height.div_ceil(2), wrap_levels_v, 0, height >> 1);
        }
        wrap_pyramids.par_iter_mut().for_each(|wp| {
            let levels = wp.py.n_levels();
            shrink_masks(&mut wp.masks, levels);
        });

        let total_levels = blend_levels.max(wrap_levels_h).max(wrap_levels_v).max(1);
        let mut output = Pyramid::new(width, height, total_levels, 0, 0);

        let wrap = opts.wrap != 0;
        out!(
            1,
            "{}",
            match (n_images == 1, wrap) {
                (true, true) => "Wrapping...\n",
                (true, false) => "Processing...\n",
                (false, true) => "Blending/wrapping...\n",
                (false, false) => "Blending...\n",
            }
        );

        let depth_mul: f32 = if opts.gamma {
            if output_bpp == 8 { 1.0f32 / 66049.0 } else { 66049.0 }
        } else if output_bpp == 8 {
            1.0f32 / 257.0
        } else {
            257.0
        };

        let mut pool: Vec<Vec<f32>> = Vec::new();
        let mut out_planes: Vec<Plane> = Vec::with_capacity(3);
        for c in 0..3 {
            if n_images > 1 {
                for i in 0..n_images {
                    timer.restart();
                    let img = &mut images[i];
                    let mut py = Pyramid::with_buffers(img.width, img.height, blend_levels, img.xpos, img.ypos, std::mem::take(&mut pool));
                    match img.channels[c].take().unwrap() {
                        Plane::U8(v) => py.copy_from(&v, img.width, opts.gamma),
                        Plane::U16(v) => py.copy_from(&v, img.width, opts.gamma),
                    }
                    if output_bpp as u32 != img.bpp {
                        py.multiply(0, depth_mul);
                    }
                    copy_time += timer.read();

                    timer.restart();
                    py.shrink();
                    shrink_time += timer.read();
                    for l in 0..blend_levels {
                        dump_level(&format!("shrink{i}_c{c}_l{l}"), &py, l);
                    }

                    timer.restart();
                    py.laplace();
                    laplace_time += timer.read();
                    for l in 0..blend_levels {
                        dump_level(&format!("laplace{i}_c{c}_l{l}"), &py, l);
                    }

                    timer.restart();
                    composite(&mut output, &py, &images[i].masks, blend_levels, |_| i == 0);
                    blend_time += timer.read();
                    pool = py.into_buffers();
                }
                for l in 0..blend_levels {
                    dump_level(&format!("blend_c{c}_l{l}"), &output, l);
                }
                timer.restart();
                output.collapse(blend_levels);
                collapse_time += timer.read();
            } else {
                timer.restart();
                match images[0].channels[c].take().unwrap() {
                    Plane::U8(v) => output.copy_from(&v, images[0].width, opts.gamma),
                    Plane::U16(v) => output.copy_from(&v, images[0].width, opts.gamma),
                }
                if output_bpp as u32 != images[0].bpp {
                    output.multiply(0, depth_mul);
                }
                copy_time += timer.read();
            }
            dump_level(&format!("collapsed_c{c}"), &output, 0);

            // Wrapping
            if wrap {
                timer.restart();
                let mut p = 0;
                for dir in [1, 2] {
                    if opts.wrap & dir == 0 {
                        continue;
                    }
                    if dir == 1 {
                        swap_h(&mut output, false);
                    } else {
                        swap_v(&mut output, false);
                    }
                    let levels = if dir == 1 { wrap_levels_h } else { wrap_levels_v };
                    for wp in 0..2 {
                        let wpy = &mut wrap_pyramids[p];
                        let (x0, y0) = (wpy.py.levels[0].x as usize, wpy.py.levels[0].y as usize);
                        let opitch = output.levels[0].pitch;
                        wpy.py.copy_from_f32(&output.data[0][x0 + y0 * opitch..], opitch);
                        wpy.py.shrink();
                        wpy.py.laplace();
                        let wpy = &wrap_pyramids[p];
                        composite(&mut output, &wpy.py, &wpy.masks, levels, |l| wp == 0 && l != 0);
                        p += 1;
                    }
                    output.collapse(levels);
                    if dir == 1 {
                        swap_h(&mut output, true);
                    } else {
                        swap_v(&mut output, true);
                    }
                }
                wrap_time += timer.read();
            }
            dump_level(&format!("wrapped_c{c}"), &output, 0);

            // Offset correction
            if seams.total_pixels > 0 {
                let mut channel_total = 0f64;
                let pitch = output.levels[0].pitch;
                let data = &output.data[0];
                for (y, runs) in seams.coverage.iter().enumerate() {
                    let mut x = 0usize;
                    for &(len, _, xor) in runs {
                        if xor {
                            for v in &data[y * pitch + x..y * pitch + x + len as usize] {
                                channel_total += *v as f64;
                            }
                        }
                        x += len as usize;
                    }
                }
                let total = seams.total_pixels as f32;
                let mut avg = seams.channel_totals[c] as f32 / total;
                if output_bpp as u32 != images[0].bpp {
                    let factor: f32 = if opts.gamma { 66049.0 } else { 257.0 };
                    if output_bpp == 8 {
                        avg /= factor;
                    } else {
                        avg *= factor;
                    }
                }
                let output_avg = channel_total as f32 / total;
                output.add(avg - output_avg, 1);
            }
            dump_level(&format!("corrected_c{c}"), &output, 0);

            timer.restart();
            let n = width * height;
            out_planes.push(if output_bpp == 8 {
                let mut v = vec![0u8; n];
                output.out(&mut v, opts.gamma, opts.dither, 255.0, |i| i as u8);
                Plane::U8(v)
            } else {
                let mut v = vec![0u16; n];
                output.out(&mut v, opts.gamma, opts.dither, 65535.0, |i| i as u16);
                Plane::U16(v)
            });
            out_time += timer.read();
        }
        drop(output);
        drop(wrap_pyramids);

        // Write
        let filename = opts.output.as_deref().unwrap();
        out!(1, "Writing {filename}...\n");
        timer.restart();
        if opts.bgr {
            out_planes.swap(0, 2);
        }
        let spp = if no_mask { 3 } else { 4 };
        let bytes = output_bpp as usize / 8;
        let big_endian = opts.output_type == ImageType::Png;
        let row_bytes = width * spp * bytes;
        let fill_row = |y: usize, row: &mut [u8]| {
            let mut x = 0usize;
            let mut p = 0usize;
            let put = |row: &mut [u8], p: &mut usize, v: u32| {
                if bytes == 1 {
                    row[*p] = v as u8;
                } else {
                    let b = if big_endian { (v as u16).to_be_bytes() } else { (v as u16).to_le_bytes() };
                    row[*p..*p + 2].copy_from_slice(&b);
                }
                *p += bytes;
            };
            for &(len, covered, _) in &seams.coverage[y] {
                for _ in 0..len {
                    if covered {
                        for plane in &out_planes {
                            put(row, &mut p, plane.get(y * width + x));
                        }
                        if !no_mask {
                            put(row, &mut p, if bytes == 1 { 0xff } else { 0xffff });
                        }
                    } else {
                        row[p..p + spp * bytes].fill(0);
                        p += spp * bytes;
                    }
                    x += 1;
                }
            }
        };

        let result = match opts.output_type {
            ImageType::Tiff => {
                let img0 = &images[0].info;
                let xres = (img0.xres != -1.0).then_some(img0.xres);
                let yres = (img0.yres != -1.0).then_some(img0.yres);
                if (xres.is_some() && min_xpos < 0) || (yres.is_some() && min_ypos < 0) {
                    out!(0, "Warning: output has a negative position; TIFF position clamped to 0\n");
                }
                let meta = io::TiffMeta {
                    xres,
                    yres,
                    xpos: xres.map(|r| min_xpos.max(0) as f32 / r),
                    ypos: yres.map(|r| min_ypos.max(0) as f32 / r),
                };
                let compression = opts.compression.unwrap_or(TiffCompression::Lzw);
                io::TiffWriter::new(output_file, opts.big_tiff, width, height, spp, output_bpp as usize, compression, meta).and_then(|mut w| {
                    // Fill and compress a batch of strips in parallel, then write them in order.
                    let strips: Vec<usize> = (0..height).step_by(io::ROWS_PER_STRIP).collect();
                    let batch = rayon::current_num_threads() * 2;
                    for chunk in strips.chunks(batch) {
                        let encoded: Vec<io::IoResult<Vec<u8>>> = chunk
                            .par_iter()
                            .map(|&y0| {
                                let rows = io::ROWS_PER_STRIP.min(height - y0);
                                let mut strip = vec![0u8; rows * row_bytes];
                                for (r, row) in strip.chunks_mut(row_bytes).enumerate() {
                                    fill_row(y0 + r, row);
                                }
                                w.encode_strip(&strip)
                            })
                            .collect();
                        for e in encoded {
                            w.write_encoded(&e?)?;
                        }
                    }
                    w.finish()
                })
            }
            _ => {
                let mut buf = vec![0u8; height * row_bytes];
                buf.par_chunks_mut(row_bytes).enumerate().for_each(|(y, row)| fill_row(y, row));
                if opts.output_type == ImageType::Png {
                    io::png_write(output_file, width, height, spp, output_bpp as usize, opts.jpeg_quality, &buf)
                } else {
                    io::jpeg_write(output_file, width, height, opts.jpeg_quality, &buf)
                }
            }
        };
        if let Err(e) = result {
            die!("Error: could not write {filename}: {e}");
        }
        write_time = timer.read();
    }

    if opts.timing {
        println!();
        println!("Images:   {images_time:.3}s");
        println!("Seaming:  {seam_time:.3}s");
        if opts.output_type != ImageType::None {
            println!("Masks:    {shrink_mask_time:.3}s");
            println!("Copy:     {copy_time:.3}s");
            println!("Shrink:   {shrink_time:.3}s");
            println!("Laplace:  {laplace_time:.3}s");
            println!("Blend:    {blend_time:.3}s");
            println!("Collapse: {collapse_time:.3}s");
            if opts.wrap != 0 {
                println!("Wrapping: {wrap_time:.3}s");
            }
            println!("Output:   {out_time:.3}s");
            println!("Write:    {write_time:.3}s");
        }
        let what = if opts.output_type == ImageType::None { "Execution" } else { "Blend" };
        println!("\n{what} complete. Total execution time: {:.3}s", timer_all.read());
    }
}
