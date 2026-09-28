//! Gaussian/Laplacian pyramids (pyramid.cpp / pyramid.h).
//!
//! The reference uses SSE; this is scalar f32 with every expression in the
//! reference's operand order (including `hadd` pairings), so results are
//! bit-identical to an `-O2` build of the reference. Output never depends on
//! how rows are split across threads.

use rayon::prelude::*;

#[derive(Clone, Debug)]
pub struct Level {
    pub width: usize,
    pub height: usize,
    pub pitch: usize,
    /// Rows allocated (height rounded up; shrink may touch them for tiny levels).
    pub rows: usize,
    pub x: i32,
    pub y: i32,
    pub x_shift: bool,
    pub y_shift: bool,
}

pub struct Pyramid {
    pub levels: Vec<Level>,
    pub data: Vec<Vec<f32>>,
}

impl Pyramid {
    pub fn geometry(mut width: usize, mut height: usize, n_levels: usize, mut x: i32, mut y: i32) -> Vec<Level> {
        let mut levels: Vec<Level> = Vec::with_capacity(n_levels);
        let mut b: i32 = 0;
        let mut req: i32 = 2;
        for n in 0..n_levels {
            let x_shift = (x.wrapping_sub(b) & (req - 1)) != 0;
            let y_shift = (y.wrapping_sub(b) & (req - 1)) != 0;
            let pitch = (width + x_shift as usize + 7) & !7;
            let rows = (height + y_shift as usize + 3) & !3;
            levels.push(Level { width, height, pitch, rows, x, y, x_shift, y_shift });
            x = x.wrapping_sub((x_shift as i32) << n).wrapping_sub(req);
            y = y.wrapping_sub((y_shift as i32) << n).wrapping_sub(req);
            b -= req;
            req = req.wrapping_shl(1);
            width = (width + x_shift as usize + 6) >> 1;
            height = (height + y_shift as usize + 6) >> 1;
        }
        levels
    }

    pub fn new(width: usize, height: usize, n_levels: usize, x: i32, y: i32) -> Pyramid {
        Self::with_buffers(width, height, n_levels, x, y, Vec::new())
    }

    /// Build a pyramid reusing previously allocated level buffers (see `into_buffers`).
    /// Buffer contents are stale until filled; every row a level uses is written
    /// before it is read, and rows past a level's height are cleared.
    pub fn with_buffers(width: usize, height: usize, n_levels: usize, x: i32, y: i32, mut pool: Vec<Vec<f32>>) -> Pyramid {
        let levels = Self::geometry(width, height, n_levels, x, y);
        pool.resize_with(levels.len().max(pool.len()), Vec::new);
        let mut data: Vec<Vec<f32>> = pool.drain(..levels.len()).collect();
        for (buf, l) in data.iter_mut().zip(&levels) {
            buf.resize(buf.len().max(l.pitch * l.rows), 0.0);
            buf[l.pitch * l.height..l.pitch * l.rows].fill(0.0);
        }
        Pyramid { levels, data }
    }

    pub fn into_buffers(self) -> Vec<Vec<f32>> {
        self.data
    }

    pub fn n_levels(&self) -> usize {
        self.levels.len()
    }

    /// Fill level 0 from planar integer samples (row stride `src_pitch`).
    /// Columns beyond the width repeat the last sample.
    pub fn copy_from<T: Copy + Into<u32> + Sync>(&mut self, src: &[T], src_pitch: usize, gamma: bool) {
        let l = &self.levels[0];
        let (w, pitch) = (l.width, l.pitch);
        self.data[0][..pitch * l.height].par_chunks_mut(pitch).enumerate().for_each(|(y, row)| {
            let s = &src[y * src_pitch..y * src_pitch + w];
            for x in 0..w {
                let f = s[x].into() as f32;
                row[x] = if gamma { f * f } else { f };
            }
            let last = row[w - 1];
            row[w..].fill(last);
        });
    }

    /// Fill level 0 from floats (wrapping), gamma off.
    pub fn copy_from_f32(&mut self, src: &[f32], src_pitch: usize) {
        let l = &self.levels[0];
        let (w, pitch) = (l.width, l.pitch);
        self.data[0][..pitch * l.height].par_chunks_mut(pitch).enumerate().for_each(|(y, row)| {
            row[..w].copy_from_slice(&src[y * src_pitch..y * src_pitch + w]);
            let last = row[w - 1];
            row[w..].fill(last);
        });
    }

    pub fn multiply(&mut self, level: usize, mul: f32) {
        if mul == 1.0 {
            return;
        }
        let l = &self.levels[level];
        let d = &mut self.data[level][..l.pitch * l.height];
        if mul == 0.0 {
            d.fill(0.0);
            return;
        }
        d.par_iter_mut().for_each(|v| *v *= mul);
    }

    /// Add a constant to levels below `n` (the reference only ever adds to level 0,
    /// and not at all when the pyramid has a single level).
    pub fn add(&mut self, add: f32, n: usize) {
        let lim = n.min(self.levels.len() - 1);
        for l in 0..lim {
            let lev = &self.levels[l];
            self.data[l][..lev.pitch * lev.height].par_iter_mut().for_each(|v| *v += add);
        }
    }

    pub fn shrink(&mut self) {
        for l in 0..self.levels.len() - 1 {
            let (hi_data, lo_data) = self.data.split_at_mut(l + 1);
            let hi = &hi_data[l];
            let hl = &self.levels[l];
            let ll = &self.levels[l + 1];
            let lo_pitch = ll.pitch;
            lo_data[0][..lo_pitch * ll.height].par_chunks_mut(lo_pitch).enumerate().for_each_init(
                || vec![0f32; hl.pitch],
                |line, (y, lo)| {
                    let mul = vertical_line(hi, hl, ll.height, y, line);
                    squeeze(line, lo, mul, hl.x_shift);
                },
            );
        }
    }

    /// Convert levels [0, n-1) between Gaussian and (negated) Laplacian form.
    /// The operation is its own inverse: `upper = expand(lower) - upper`.
    fn laplace_collapse(&mut self, n_levels: usize, collapse: bool) {
        for j in 0..n_levels.saturating_sub(1) {
            let l = if collapse { n_levels - 2 - j } else { j };
            let (up_data, lo_data) = self.data.split_at_mut(l + 1);
            let up = &mut up_data[l];
            let lo = &lo_data[0];
            let ul = &self.levels[l];
            let ll = &self.levels[l + 1];
            let pitch = ul.pitch;
            up[..pitch * ul.height].par_chunks_mut(pitch).enumerate().for_each_init(
                || (vec![0f32; pitch], vec![0f32; pitch], vec![0f32; pitch]),
                |(t1, t2, t3), (y, hi)| {
                    let s = y + ul.y_shift as usize;
                    let lo_row = |k: usize| {
                        let k = k.min(ll.rows - 1);
                        &lo[k * ll.pitch..(k + 1) * ll.pitch]
                    };
                    if s & 1 == 0 {
                        let k = s / 2 + 1;
                        expand(lo_row(k - 1), t1, ul.x_shift);
                        expand(lo_row(k), t2, ul.x_shift);
                        expand(lo_row(k + 1), t3, ul.x_shift);
                        for x in 0..pitch {
                            hi[x] = ((t1[x] + t3[x]) * 0.125f32 + t2[x] * 0.75f32) - hi[x];
                        }
                    } else {
                        let k = s.div_ceil(2);
                        expand(lo_row(k), t1, ul.x_shift);
                        expand(lo_row(k + 1), t2, ul.x_shift);
                        for x in 0..pitch {
                            hi[x] = (t1[x] + t2[x]) * 0.5f32 - hi[x];
                        }
                    }
                },
            );
        }
    }

    pub fn laplace(&mut self) {
        self.laplace_collapse(self.levels.len(), false);
    }

    pub fn collapse(&mut self, n_levels: usize) {
        self.laplace_collapse(n_levels, true);
    }

    /// Level 0 to integer samples: clamp, optional sqrt (gamma) and ordered
    /// dither, then round half to even (`cvtps_epi32`).
    pub fn out<T: Copy + Send>(&self, dst: &mut [T], gamma: bool, dither: bool, max: f32, conv: impl Fn(i32) -> T + Sync) {
        const DITHER: [[f32; 4]; 4] = [
            [-0.125, 0.375, 0.0, 0.4999],
            [0.125, -0.375, 0.25, -0.25],
            [-0.0625, 0.4375, -0.1875, 0.3125],
            [0.1875, -0.3125, 0.0625, -0.4375],
        ];
        let l = &self.levels[0];
        let (w, pitch) = (l.width, l.pitch);
        let data = &self.data[0];
        dst.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            let src = &data[y * pitch..y * pitch + w];
            let dith = &DITHER[y & 3];
            for x in 0..w {
                let mut v = maxps(src[x], 0.0);
                if gamma {
                    v = v.sqrt();
                }
                if dither {
                    v += dith[x & 3];
                }
                v = minps(v, max);
                row[x] = conv(cvtps(v));
            }
        });
    }
}

/// `_mm_max_ps(a, b)`: returns b unless a > b (so NaN -> b).
#[inline]
fn maxps(a: f32, b: f32) -> f32 {
    if a > b { a } else { b }
}
#[inline]
fn minps(a: f32, b: f32) -> f32 {
    if a < b { a } else { b }
}
/// `_mm_cvtps_epi32`: round half to even; out of range or NaN -> i32::MIN.
#[inline]
fn cvtps(v: f32) -> i32 {
    let r = v.round_ties_even();
    if (-2147483648.0..2147483648.0).contains(&r) { r as i32 } else { i32::MIN }
}

/// Vertical 5-tap filter for low-level row `y` into `line` (ShrinkThread).
/// Returns the normalising multiplier for `squeeze`.
fn vertical_line(hi: &[f32], hl: &Level, lo_height: usize, y: usize, line: &mut [f32]) -> f32 {
    let p = hl.pitch;
    let h = hl.height as i64;
    let ys = hl.y_shift as i64;
    let height_odd = (h & 1) ^ ys;
    let fbl = lo_height as i64 - (3 - height_odd);
    let sy = if ys != 0 { 3 } else { 2 };
    let row = |k: i64| {
        let k = k.clamp(0, hl.rows as i64 - 1) as usize;
        &hi[k * p..(k + 1) * p]
    };
    let yi = y as i64;
    let c = 2 * yi - 2 - ys;
    const S16: f32 = 1.0 / 16.0;
    const S256: f32 = 1.0 / 256.0;

    if yi == 0 {
        line.copy_from_slice(row(0));
        return S16;
    }
    if yi == 1 && ys == 0 {
        let (r0, r1, r2) = (row(0), row(1), row(2));
        for x in 0..p {
            line[x] = (r0[x] * 11.0 + r1[x] * 4.0) + r2[x];
        }
        return S256;
    }
    if yi == 1 {
        let (r0, r1) = (row(0), row(1));
        for x in 0..p {
            line[x] = r0[x] * 15.0 + r1[x];
        }
        return S256;
    }
    if yi == 2 && ys != 0 {
        let (r0, r1, r2, r3) = (row(0), row(1), row(2), row(3));
        for x in 0..p {
            line[x] = ((r0[x] + r3[x]) + (r0[x] + r2[x]) * 4.0) + r1[x] * 6.0;
        }
        return S256;
    }
    if yi >= sy && yi < fbl {
        let (a, b, cc, d, e) = (row(c - 2), row(c - 1), row(c), row(c + 1), row(c + 2));
        for x in 0..p {
            line[x] = ((a[x] + e[x]) + (b[x] + d[x]) * 4.0) + cc[x] * 6.0;
        }
        return S256;
    }
    if yi == fbl {
        if height_odd == 0 {
            let (a, b, cc, d) = (row(c - 2), row(c - 1), row(c), row(c + 1));
            for x in 0..p {
                line[x] = ((a[x] + d[x]) + (b[x] + d[x]) * 4.0) + cc[x] * 6.0;
            }
        } else {
            let (a, b, cc) = (row(c - 2), row(c - 1), row(c));
            for x in 0..p {
                line[x] = (cc[x] * 11.0 + b[x] * 4.0) + a[x];
            }
        }
        return S256;
    }
    if yi == fbl + 1 && height_odd == 0 {
        let (a, b) = (row(c - 2), row(c - 1));
        for x in 0..p {
            line[x] = a[x] + b[x] * 15.0;
        }
        return S256;
    }
    // last line
    line.copy_from_slice(row(h - 1));
    S16
}

/// Horizontal 5-tap filter and 2:1 decimation (`Squeeze`).
fn squeeze(line: &mut [f32], lo: &mut [f32], mul: f32, x_shift: bool) {
    let n = line.len();
    if x_shift {
        line.copy_within(0..n - 1, 1);
    }
    let e = |j: i64| line[j.clamp(0, n as i64 - 1) as usize];
    for (k, out) in lo.iter_mut().enumerate() {
        let c = 2 * k as i64 - 2;
        let outer = e(c - 2) + e(c + 2);
        let inner = 4.0f32 * (e(c - 1) + e(c + 1));
        let center = 6.0f32 * e(c);
        *out = (outer + (inner + center)) * mul;
    }
}

/// Expand one lower-level row into the upper level's pitch
/// (`LaplaceExpand` / `LaplaceExpandShifted`).
fn expand(lo: &[f32], hi: &mut [f32], shifted: bool) {
    let n = lo.len();
    let e = |i: usize| lo[i.min(n - 1)];
    for m in 0..hi.len() / 4 {
        let (t0, t1, t2, t3) = (e(2 * m), e(2 * m + 1), e(2 * m + 2), e(2 * m + 3));
        let o = &mut hi[4 * m..4 * m + 4];
        if !shifted {
            o[0] = (t0 * 0.125 + t1 * 0.75) + (t2 * 0.125 + t3 * 0.0);
            o[1] = (t0 * 0.0 + t1 * 0.5) + (t2 * 0.5 + t3 * 0.0);
            o[2] = (t0 * 0.0 + t1 * 0.125) + (t2 * 0.75 + t3 * 0.125);
            o[3] = (t0 * 0.0 + t1 * 0.0) + (t2 * 0.5 + t3 * 0.5);
        } else {
            let (p0, p1, p2, p3) = (t2, t3, e(2 * m + 4), e(2 * m + 5));
            o[0] = (t0 * 0.0 + t1 * 0.5) + (t2 * 0.5 + t3 * 0.0);
            o[1] = (t0 * 0.0 + t1 * 0.125) + (t2 * 0.75 + t3 * 0.125);
            o[2] = (t0 * 0.0 + t1 * 0.0) + (t2 * 0.5 + t3 * 0.5);
            o[3] = (p0 * 0.125 + p1 * 0.75) + (p2 * 0.125 + p3 * 0.0);
        }
    }
}

/// Composite one input row into an output row using a mask row (`CompositeLine`).
/// Image 0 assigns, later images accumulate; mask value 1 always assigns.
#[allow(clippy::too_many_arguments)]
pub fn composite_line(input: &[f32], output: &mut [f32], first: bool, x_offset: i64, in_w: i64, out_w: i64, mask: &[u32]) {
    let mut x: i64 = 0;
    let mut p = 0usize;
    while x < out_w {
        let cur = mask[p];
        p += 1;
        let mut last_int = -1;
        let mut val = 0f32;
        let count: i64;
        if cur & 0x8000_0000 != 0 {
            count = (cur & 0x0fff_ffff) as i64;
            if cur & 0x2000_0000 != 0 {
                val = f32::from_bits(mask[p]);
                p += 1;
            } else {
                last_int = ((cur >> 30) & 1) as i32;
            }
        } else {
            count = 1;
            val = f32::from_bits(cur);
        }
        let lim = x + count;

        if last_int == 0 {
            if first {
                output[x as usize..lim as usize].fill(0.0);
            }
            x = lim;
        } else if last_int == 1 {
            if x < x_offset {
                let f = input[0];
                while x < lim && x < x_offset {
                    output[x as usize] = f;
                    x += 1;
                }
            }
            while x < lim && x < x_offset + in_w {
                output[x as usize] = input[(x - x_offset) as usize];
                x += 1;
            }
            if x < lim {
                let f = input[(in_w - 1) as usize];
                while x < lim {
                    output[x as usize] = f;
                    x += 1;
                }
            }
        } else {
            let mut put = |x: i64, f: f32| {
                if first {
                    output[x as usize] = f;
                } else {
                    output[x as usize] += f;
                }
            };
            if x < x_offset {
                let f = input[0] * val;
                while x < lim && x < x_offset {
                    put(x, f);
                    x += 1;
                }
            }
            while x < lim && x < x_offset + in_w {
                put(x, input[(x - x_offset) as usize] * val);
                x += 1;
            }
            if x < lim {
                let f = input[(in_w - 1) as usize] * val;
                while x < lim {
                    put(x, f);
                    x += 1;
                }
            }
        }
    }
    let pitch = output.len() as i64;
    if x < pitch {
        let f = output[(x - 1) as usize];
        output[x as usize..].fill(f);
    }
}

/// Rotate level 0 rows left by ceil(w/2) columns (or back).
pub fn swap_h(py: &mut Pyramid, unswap: bool) {
    let l = &py.levels[0];
    let (w, pitch) = (l.width, l.pitch);
    let major = w.div_ceil(2);
    py.data[0][..pitch * l.height].par_chunks_mut(pitch).for_each(|row| {
        if unswap {
            row[..w].rotate_right(major);
        } else {
            row[..w].rotate_left(major);
        }
    });
}

/// Rotate level 0 rows up by floor(h/2) (or back).
pub fn swap_v(py: &mut Pyramid, unswap: bool) {
    let l = &py.levels[0];
    let (h, pitch) = (l.height, l.pitch);
    let rows = &mut py.data[0][..pitch * h];
    if unswap {
        rows.rotate_right((h / 2) * pitch);
    } else {
        rows.rotate_left((h / 2) * pitch);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cvtps_rounds_half_to_even() {
        assert_eq!([0.5f32, 1.5, 2.5, -0.5, 254.5, 255.4999].map(cvtps), [0, 2, 2, 0, 254, 255]);
        assert_eq!(cvtps(f32::NAN), i32::MIN);
    }

    #[test]
    fn geometry_matches_reference_dimensions() {
        // From the reference dump of an image at (0,0) sized 62x62 with 5 levels.
        let l = Pyramid::geometry(62, 62, 5, 0, 0);
        let dims: Vec<(usize, usize)> = l.iter().map(|l| (l.pitch, l.height)).collect();
        assert_eq!(dims, [(64, 62), (40, 34), (24, 20), (16, 13), (16, 9)]);
    }

    #[test]
    fn laplace_then_collapse_is_identity_for_smooth_data() {
        let mut py = Pyramid::new(37, 23, 4, 3, 5);
        let src: Vec<u16> = (0..37 * 23).map(|i| (i % 37 * 7 + i / 37 * 3) as u16).collect();
        py.copy_from(&src, 37, false);
        let before = py.data[0].clone();
        py.shrink();
        py.laplace();
        py.collapse(4);
        let l0 = &py.levels[0];
        for y in 0..l0.height {
            for x in 0..l0.width {
                let (a, b) = (before[y * l0.pitch + x], py.data[0][y * l0.pitch + x]);
                assert!((a - b).abs() < 1e-3, "({x},{y}) {a} vs {b}");
            }
        }
    }
}
