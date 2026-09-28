//! Seam placement (the "Seaming" section of multiblend.cpp's main()).
//!
//! A 3/4 chamfer distance transform runs backwards then forwards over the output
//! canvas. Each DT value packs `distance << 32 | image index`; bit 63 marks a
//! value that refers to an image absent at the current pixel. Pixels covered by
//! exactly one image seed the transform with that image. The forward pass
//! assigns every pixel to an image, which becomes the level-0 blend masks.

use crate::image::{Image, Plane};
use crate::io;
use crate::masks::{MaskLevel, owner_runs};
use crate::{die, out};

const FLAG: u64 = 0x8000_0000_0000_0000;
const ABSENT: u64 = FLAG;

pub struct SeamResult {
    /// Per-row coverage runs: (length, number of covering images != 0, exactly one).
    pub coverage: Vec<Vec<(u32, bool, bool)>>,
    /// Some output pixel is covered by no image.
    pub alpha: bool,
    pub channel_totals: [u64; 3],
    pub total_pixels: u64,
}

struct State {
    mask_state: u64,
    count: i64,
    limit: i64,
    /// Cursor into the image's opacity words.
    cursor: usize,
}

pub struct SeamOptions<'a> {
    pub reverse: bool,
    pub gamma: bool,
    pub seamload: Option<&'a str>,
    pub seamsave: Option<&'a str>,
    pub xor: Option<&'a str>,
}

pub fn seam(images: &mut [Image], width: usize, height: usize, opts: &SeamOptions) -> SeamResult {
    let n = images.len();
    let w = width as i64;
    // The C++ used index 0 here, which made --reverse a no-op (FINDINGS #23).
    let dt_max: u64 = 0x9000_0000_0000_0000 | if opts.reverse { (n - 1) as u64 } else { 0 };

    let mut st: Vec<State> = images
        .iter()
        .map(|_| State {
            mask_state: ABSENT,
            count: 0,
            limit: 0,
            cursor: 0,
        })
        .collect();
    let maskval = |st: &[State], v: u64| -> u64 {
        let idx = (v & 0xffff_ffff) as usize;
        let s = if idx < st.len() {
            st[idx].mask_state
        } else {
            ABSENT
        };
        (v & !FLAG) | s
    };

    // Backward DT: row 0 kept whole, other rows keep the values the forward pass
    // will read (those with a non-zero distance word), in left-to-right order.
    let mut row0: Vec<u64> = Vec::new();
    let mut backward: Vec<Vec<u64>> = vec![Vec::new(); height];

    if opts.seamload.is_none() {
        for (s, img) in st.iter_mut().zip(images.iter()) {
            s.cursor = img.opacity.words.len();
        }
        let mut this = vec![0u64; width];
        let mut prev = vec![0u64; width];
        let mut last_pixel = false;

        for y in (0..height).rev() {
            let yi = y as i64;
            for (s, img) in st.iter_mut().zip(images.iter()) {
                s.mask_state = ABSENT;
                if yi >= img.ypos as i64 && yi < img.ypos as i64 + img.height as i64 {
                    s.count = w - (img.xpos as i64 + img.width as i64);
                    s.limit = img.xpos as i64;
                } else {
                    s.count = w;
                    s.limit = w;
                }
            }

            let prev_at = |prev: &[u64], x: i64| {
                if x >= 0 && x < w {
                    prev[x as usize]
                } else {
                    dt_max
                }
            };
            let mut x: i64 = w - 1;
            while x >= 0 {
                let mut min_count = x + 1;
                let mut xor_count = 0;
                let mut xor_image = 0usize;
                for (i, (s, img)) in st.iter_mut().zip(images.iter()).enumerate() {
                    if s.count == 0 {
                        if x >= s.limit {
                            s.cursor -= 1;
                            let u = img.opacity.words[s.cursor] as u64;
                            s.mask_state = ((!u) << 32) & FLAG;
                            s.count = (u & 0x7fff_ffff) as i64;
                        } else {
                            s.mask_state = ABSENT;
                            s.count = min_count;
                        }
                    }
                    if s.count < min_count {
                        min_count = s.count;
                    }
                    if s.mask_state == 0 {
                        xor_count += 1;
                        xor_image = i;
                    }
                }

                let mut stop = x - min_count;

                if xor_count == 1 {
                    images[xor_image].seam_present = true;
                    while x > stop {
                        this[x as usize] = xor_image as u64;
                        x -= 1;
                    }
                } else if y == height - 1 {
                    if x == w - 1 {
                        while x > stop {
                            this[x as usize] = dt_max;
                            x -= 1;
                        }
                    } else {
                        let mut ut = maskval(&st, this[(x + 1) as usize]);
                        while x > stop {
                            ut = ut.wrapping_add(0x3_0000_0000);
                            this[x as usize] = ut;
                            x -= 1;
                        }
                    }
                } else {
                    let (mut a, mut b, mut c, mut d);
                    if x == w - 1 {
                        a = maskval(&st, prev_at(&prev, x - 1).wrapping_add(0x4_0000_0000));
                        b = maskval(&st, prev[x as usize].wrapping_add(0x3_0000_0000));
                        d = if a < b { a } else { b };
                        this[x as usize] = d;
                        x -= 1;
                        if x == stop {
                            for s in st.iter_mut() {
                                s.count -= min_count;
                            }
                            continue;
                        }
                        c = b.wrapping_add(0x1_0000_0000);
                        b = a.wrapping_sub(0x1_0000_0000);
                        d = d.wrapping_add(0x3_0000_0000);
                    } else {
                        b = maskval(&st, prev[x as usize].wrapping_add(0x3_0000_0000));
                        c = maskval(&st, prev[(x + 1) as usize].wrapping_add(0x4_0000_0000));
                        d = maskval(&st, this[(x + 1) as usize].wrapping_add(0x3_0000_0000));
                    }

                    if stop == -1 {
                        stop = 0;
                        last_pixel = true;
                    }

                    while x > stop {
                        a = maskval(&st, prev[(x - 1) as usize].wrapping_add(0x4_0000_0000));
                        if a < d {
                            d = a;
                        }
                        if b < d {
                            d = b;
                        }
                        if c < d {
                            d = c;
                        }
                        this[x as usize] = d;
                        x -= 1;
                        c = b.wrapping_add(0x1_0000_0000);
                        b = a.wrapping_sub(0x1_0000_0000);
                        d = d.wrapping_add(0x3_0000_0000);
                    }

                    if last_pixel {
                        if b < d {
                            d = b;
                        }
                        if c < d {
                            d = c;
                        }
                        this[x as usize] = d;
                        x -= 1;
                        last_pixel = false;
                    }
                }

                for s in st.iter_mut() {
                    s.count -= min_count;
                }
            }

            if y > 0 {
                backward[y] = this
                    .iter()
                    .copied()
                    .filter(|v| v & 0xffff_ffff_0000_0000 != 0)
                    .collect();
            } else {
                row0 = this.clone();
            }
            std::mem::swap(&mut this, &mut prev);
        }

        for img in images.iter() {
            if !img.seam_present {
                out!(
                    1,
                    "Warning: {} is fully obscured by other images\n",
                    img.filename
                );
            }
        }
    }

    for img in images.iter_mut() {
        img.masks = vec![MaskLevel::new(width, height)];
    }

    let mut seam_map = opts.seamsave.map(|_| vec![0u8; width * height]);
    let mut xor_map = opts.xor.map(|_| vec![0u8; width * height]);

    // Forward DT.
    for s in st.iter_mut() {
        s.cursor = 0;
    }
    let mut result = SeamResult {
        coverage: Vec::with_capacity(height),
        alpha: false,
        channel_totals: [0; 3],
        total_pixels: 0,
    };
    let mut this = row0;
    if this.is_empty() {
        this = vec![0u64; width];
    }
    let mut prev = vec![0u64; width];
    let mut assign = vec![0u32; width];
    let seamload = opts.seamload.is_some();

    for y in 0..height {
        let yi = y as i64;
        for (s, img) in st.iter_mut().zip(images.iter()) {
            s.mask_state = ABSENT;
            if yi >= img.ypos as i64 && yi < img.ypos as i64 + img.height as i64 {
                s.count = img.xpos as i64;
                s.limit = img.xpos as i64 + img.width as i64;
            } else {
                s.count = w;
                s.limit = w;
            }
        }
        let mut bw = backward[y].iter().copied();
        let mut seam_dt = || bw.next().unwrap_or(dt_max);
        let prev_at = |prev: &[u64], x: i64| {
            if x >= 0 && x < w {
                prev[x as usize]
            } else {
                dt_max
            }
        };

        let mut coverage: Vec<(u32, bool, bool)> = Vec::new();
        let mut x: i64 = 0;
        let mut best: u64 = 0;
        let mut last_pixel = false;

        while x < w {
            let mut min_count = w - x;
            let mut xor_count = 0;
            let mut xor_image = 0usize;
            for (i, (s, img)) in st.iter_mut().zip(images.iter()).enumerate() {
                if s.count == 0 {
                    if x < s.limit {
                        let u = img.opacity.words[s.cursor] as u64;
                        s.cursor += 1;
                        s.mask_state = ((!u) << 32) & FLAG;
                        s.count = (u & 0x7fff_ffff) as i64;
                    } else {
                        s.mask_state = ABSENT;
                        s.count = min_count;
                    }
                }
                if s.count < min_count {
                    min_count = s.count;
                }
                if s.mask_state == 0 {
                    xor_count += 1;
                    xor_image = i;
                }
            }

            let mut stop = x + min_count;
            if xor_count == 0 {
                result.alpha = true;
            }
            match coverage.last_mut() {
                Some(last) if last.1 == (xor_count != 0) && last.2 == (xor_count == 1) => {
                    last.0 += min_count as u32
                }
                _ => coverage.push((min_count as u32, xor_count != 0, xor_count == 1)),
            }

            // arbitrary choice for pixels no seed reaches
            let arbitrary = |st: &[State], best: &mut u64, xor_count: i32| {
                if *best & FLAG != 0 && xor_count != 0 {
                    for (i, s) in st.iter().enumerate() {
                        if s.mask_state == 0 {
                            *best = FLAG | i as u64;
                            if !opts.reverse {
                                break;
                            }
                        }
                    }
                }
            };

            if xor_count == 1 {
                if let Some(m) = xor_map.as_mut() {
                    m[y * width + x as usize..y * width + stop as usize].fill(xor_image as u8);
                }
                let img = &images[xor_image];
                let base = (y as i64 - img.ypos as i64) as usize * img.width
                    + (x - img.xpos as i64) as usize;
                result.total_pixels += min_count as u64;
                for c in 0..3 {
                    let range = base..base + min_count as usize;
                    result.channel_totals[c] += match img.channels[c].as_ref().unwrap() {
                        Plane::U8(v) => sum_samples(&v[range], opts.gamma),
                        Plane::U16(v) => sum_samples(&v[range], opts.gamma),
                    };
                }
                if !seamload {
                    while x < stop {
                        assign[x as usize] = xor_image as u32;
                        this[x as usize] = xor_image as u64;
                        x += 1;
                    }
                } else {
                    x = stop;
                }
                best = xor_image as u64;
            } else {
                if let Some(m) = xor_map.as_mut() {
                    m[y * width + x as usize..y * width + stop as usize].fill(0xff);
                }
                if !seamload {
                    if y == 0 {
                        while x < stop {
                            best = this[x as usize];
                            if x > 0 {
                                let d = maskval(
                                    &st,
                                    this[(x - 1) as usize].wrapping_add(0x3_0000_0000),
                                );
                                if d < best {
                                    best = d;
                                }
                            }
                            arbitrary(&st, &mut best, xor_count);
                            assign[x as usize] = (best & 0xffff_ffff) as u32;
                            this[x as usize] = best;
                            x += 1;
                        }
                    } else {
                        let (mut a, mut b, mut c, mut d);
                        if x == 0 {
                            best = seam_dt();
                            b = maskval(&st, prev[0].wrapping_add(0x3_0000_0000));
                            if b < best {
                                best = b;
                            }
                            c = maskval(&st, prev_at(&prev, 1).wrapping_add(0x4_0000_0000));
                            if c < best {
                                best = c;
                            }
                            arbitrary(&st, &mut best, xor_count);
                            assign[0] = (best & 0xffff_ffff) as u32;
                            this[0] = best;
                            x += 1;
                            if x == stop {
                                for s in st.iter_mut() {
                                    s.count -= min_count;
                                }
                                continue;
                            }
                            a = b.wrapping_add(0x1_0000_0000);
                            b = c.wrapping_sub(0x1_0000_0000);
                        } else {
                            a = maskval(&st, prev[(x - 1) as usize].wrapping_add(0x4_0000_0000));
                            b = maskval(&st, prev[x as usize].wrapping_add(0x3_0000_0000));
                        }
                        d = maskval(&st, best.wrapping_add(0x3_0000_0000));

                        if stop == w {
                            stop -= 1;
                            last_pixel = true;
                        }

                        while x < stop {
                            c = maskval(&st, prev[(x + 1) as usize].wrapping_add(0x4_0000_0000));
                            best = seam_dt();
                            if a < best {
                                best = a;
                            }
                            if b < best {
                                best = b;
                            }
                            if c < best {
                                best = c;
                            }
                            if d < best {
                                best = d;
                            }
                            arbitrary(&st, &mut best, xor_count);
                            assign[x as usize] = (best & 0xffff_ffff) as u32;
                            this[x as usize] = best;
                            x += 1;
                            a = b.wrapping_add(0x1_0000_0000);
                            b = c.wrapping_sub(0x1_0000_0000);
                            d = best.wrapping_add(0x3_0000_0000);
                        }

                        if last_pixel {
                            best = seam_dt();
                            if a < best {
                                best = a;
                            }
                            if b < best {
                                best = b;
                            }
                            if d < best {
                                best = d;
                            }
                            arbitrary(&st, &mut best, xor_count);
                            assign[x as usize] = (best & 0xffff_ffff) as u32;
                            this[x as usize] = best;
                            x += 1;
                            last_pixel = false;
                        }
                    }
                } else {
                    x = stop;
                }
            }

            for s in st.iter_mut() {
                s.count -= min_count;
            }
        }

        result.coverage.push(coverage);
        if !seamload {
            let runs = owner_runs(assign.iter().copied());
            for (i, img) in images.iter_mut().enumerate() {
                let m = &mut img.masks[0];
                m.push_owner_row(&runs, i as u32);
                if y + 1 < height {
                    m.next_row();
                }
            }
            if let Some(sm) = seam_map.as_mut() {
                for (x, &a) in assign.iter().enumerate() {
                    sm[y * width + x] = a as u8;
                }
            }
        }
        std::mem::swap(&mut this, &mut prev);
    }

    if let (Some(name), Some(m)) = (opts.xor, xor_map.as_ref())
        && io::png_write_palette(name, width, height, m).is_err()
    {
        out!(0, "WARNING: Could not save XOR map\n");
    }
    if let (Some(name), Some(m)) = (opts.seamsave, seam_map.as_ref())
        && io::png_write_palette(name, width, height, m).is_err()
    {
        out!(0, "WARNING: Could not save Seam map\n");
    }

    if let Some(name) = opts.seamload {
        let (pw, ph, idx) = io::png_read_indices(name).unwrap_or_else(|e| die!("{e}"));
        if pw != width || ph != height {
            die!("Error: Seam PNG dimensions don't match workspace");
        }
        for y in 0..height {
            for x in 0..width {
                if idx[y * width + x] as usize >= n {
                    die!("Error: Bad pixel found in seam file: {x},{y}");
                }
            }
            let runs = owner_runs(idx[y * width..(y + 1) * width].iter().map(|&v| v as u32));
            for (i, img) in images.iter_mut().enumerate() {
                let m = &mut img.masks[0];
                m.push_owner_row(&runs, i as u32);
                if y + 1 < height {
                    m.next_row();
                }
            }
        }
    }

    result
}

fn sum_samples<T: Copy + Into<u64>>(v: &[T], gamma: bool) -> u64 {
    if gamma {
        v.iter().map(|&x| x.into() * x.into()).sum()
    } else {
        v.iter().map(|&x| x.into()).sum()
    }
}
