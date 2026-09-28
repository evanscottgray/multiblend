//! Run-length masks.
//!
//! `RunMask`: per-row opacity runs of an input image (count, high bit = opaque).
//!
//! `MaskLevel`: blend weights for one pyramid level, in the reference's word
//! format, which is kept because the pyramid arithmetic depends on it
//! (`count` is the low 24 bits):
//!
//! * high bit clear: a single f32 value (its bits);
//! * high bit set, bit 29 set: a run of `count` copies of the f32 in the next word;
//! * high bit set, bit 29 clear: a run of `count` copies of the integer in bit 30.

#[derive(Default)]
pub struct RunMask {
    pub words: Vec<u32>,
    pub rows: Vec<usize>,
}

impl RunMask {
    pub fn push(&mut self, w: u32) {
        if self.rows.is_empty() {
            self.rows.push(0);
        }
        self.words.push(w);
    }
    pub fn next_row(&mut self) {
        if self.rows.is_empty() {
            self.rows.push(0);
        }
        self.rows.push(self.words.len());
    }
    pub fn row(&self, y: usize) -> &[u32] {
        &self.words[self.rows[y]..self.rows[y + 1]]
    }
}

/// Run-length encode a row of owners.
pub fn owner_runs(row: impl IntoIterator<Item = u32>) -> Vec<(u32, u32)> {
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for o in row {
        match runs.last_mut() {
            Some(last) if last.0 == o => last.1 += 1,
            _ => runs.push((o, 1)),
        }
    }
    runs
}

pub struct MaskLevel {
    pub width: usize,
    pub height: usize,
    pub words: Vec<u32>,
    /// Word offset of each row's start.
    pub rows: Vec<usize>,
}

impl MaskLevel {
    pub fn new(width: usize, height: usize) -> Self {
        MaskLevel { width, height, words: Vec::new(), rows: vec![0] }
    }
    pub fn next_row(&mut self) {
        self.rows.push(self.words.len());
    }
    pub fn row(&self, y: usize) -> &[u32] {
        &self.words[self.rows[y]..]
    }

    /// Append one row of 0/1 integer runs: 1 where the row's owner is `image`.
    /// `runs` is the row as (owner, length) runs.
    pub fn push_owner_row(&mut self, runs: &[(u32, u32)], image: u32) {
        let mut cur: Option<(bool, u32)> = None;
        for &(owner, len) in runs {
            let v = owner == image;
            cur = match cur {
                Some((cv, n)) if cv == v => Some((cv, n + len)),
                Some((cv, n)) => {
                    self.words.push(if cv { 0xc000_0000 } else { 0x8000_0000 } | n);
                    Some((v, len))
                }
                None => Some((v, len)),
            };
        }
        if let Some((cv, n)) = cur {
            self.words.push(if cv { 0xc000_0000 } else { 0x8000_0000 } | n);
        }
    }

    /// Dense f32 values (debug dumps).
    pub fn dense(&self) -> Vec<f32> {
        let mut out = vec![0f32; self.width * self.height];
        for y in 0..self.height {
            let words = self.row(y);
            let (mut x, mut p) = (0, 0);
            while x < self.width {
                let cur = words[p];
                p += 1;
                let (count, val) = if cur & 0x8000_0000 != 0 {
                    let count = (cur & 0x00ff_ffff) as usize;
                    if cur & 0x2000_0000 != 0 {
                        p += 1;
                        (count, f32::from_bits(words[p - 1]))
                    } else {
                        (count, ((cur >> 30) & 1) as f32)
                    }
                } else {
                    (1, f32::from_bits(cur))
                };
                for _ in 0..count {
                    if x < self.width {
                        out[y * self.width + x] = val;
                    }
                    x += 1;
                }
            }
        }
        out
    }
}

/// Parse one mask item at `inp[*p]`; returns (count, value, last_int) and advances `p`.
#[inline]
fn item(inp: &[u32], p: &mut usize) -> (i32, f32, i32) {
    let cur = inp[*p];
    *p += 1;
    if cur & 0x8000_0000 != 0 {
        let count = (cur & 0x00ff_ffff) as i32;
        if cur & 0x2000_0000 != 0 {
            let v = f32::from_bits(inp[*p]);
            *p += 1;
            (count, v, -1)
        } else {
            let li = ((cur >> 30) & 1) as i32;
            (count, li as f32, li)
        }
    } else {
        (1, f32::from_bits(cur), -1)
    }
}

/// Horizontal 5-tap reduction of one mask row (`Squish` in functions.cpp).
fn squish(inp: &[u32], out: &mut Vec<u32>, in_width: i32, out_width: i32) {
    out.clear();
    let mut in_p = 0usize;
    let mut last_int: i32 = -1;
    let mut current_val: f32 = 0.0;
    let mut in_count: i32;

    let first = inp[0];
    {
        let (cnt, v, li) = item(inp, &mut in_p);
        in_count = cnt;
        if li >= 0 {
            last_int = li;
        } else {
            current_val = v;
        }
    }

    if in_count == in_width {
        if last_int >= 0 {
            out.push((first & 0xff00_0000) | out_width as u32);
        } else {
            out.push((first & 0xa000_0000) | out_width as u32);
            out.push(current_val.to_bits());
        }
        return;
    }

    let mut out_count = (in_count + 1) >> 1;
    if last_int >= 0 {
        out.push((first & 0xff00_0000) | out_count as u32);
        current_val = last_int as f32;
    } else {
        if out_count > 1 {
            out.push((first & 0xa000_0000) | out_count as u32);
        }
        out.push(current_val.to_bits());
    }
    let mut wrote = out_count;

    let (mut a, mut b, mut c) = (current_val, current_val, current_val);
    let mut read = in_count;
    in_count -= (in_count - 1) | 1;

    let mut c_read = in_p;
    let mut e_read;
    let (mut d, mut e);

    let next = |in_p: &mut usize, in_count: &mut i32, read: &mut i32, current_val: &mut f32, last_int: &mut i32| {
        let (cnt, v, li) = item(inp, in_p);
        *in_count = cnt;
        *read += cnt;
        *current_val = v;
        *last_int = li;
    };

    while wrote < out_width {
        if in_count == 0 && read < in_width {
            next(&mut in_p, &mut in_count, &mut read, &mut current_val, &mut last_int);
        }
        if read == in_width {
            in_count = 0x7fff_ffff;
        }
        if in_count >= 2 {
            d = current_val;
            e = current_val;
            e_read = in_p;
            in_count -= 2;
        } else {
            d = current_val;
            next(&mut in_p, &mut in_count, &mut read, &mut current_val, &mut last_int);
            e = current_val;
            e_read = in_p;
            in_count -= 1;
        }

        let val = (b + d) * 0.25f32 + (a + e + 6.0f32 * c) * 0.0625f32;
        out.push(val.to_bits());
        wrote += 1;

        if c_read == in_p && in_count >= 4 {
            out_count = in_count >> 1;
            if out_count > out_width - wrote {
                out_count = out_width - wrote;
            }
            if last_int >= 0 {
                out.push(((0x2 | last_int as u32) << 30) | out_count as u32);
            } else {
                out.push(0xa000_0000 | out_count as u32);
                out.push(e.to_bits());
            }
            wrote += out_count;
            in_count -= out_count << 1;
        }
        a = c;
        b = d;
        c = e;
        c_read = e_read;
    }
}

/// Build levels 1..n_levels from level 0 (`ShrinkMasks` in functions.cpp).
pub fn shrink_masks(masks: &mut Vec<MaskLevel>, n_levels: usize) {
    for l in 1..n_levels {
        let inp = &masks[l - 1];
        let in_width = inp.width as i32;
        let in_height = inp.height;
        let out_width = (in_width + 6) >> 1;
        let out_height = (in_height + 6) >> 1;
        let mut outm = MaskLevel::new(out_width as usize, out_height);

        let mut real: [Vec<u32>; 5] = Default::default();
        let mut next_row = 0usize;
        let mut squish_next = |buf: &mut Vec<u32>| {
            let r = next_row.min(in_height - 1);
            next_row += 1;
            squish(&inp.words[inp.rows[r]..], buf, in_width, out_width);
        };

        // first line: horizontal only
        squish_next(&mut real[0]);
        {
            let (mut wrote, mut p) = (0i32, 0usize);
            while wrote < out_width {
                let cur = real[0][p];
                p += 1;
                outm.words.push(cur);
                if cur & 0x8000_0000 == 0 {
                    wrote += 1;
                } else {
                    if cur & 0x2000_0000 != 0 {
                        outm.words.push(real[0][p]);
                        p += 1;
                    }
                    wrote += (cur & 0x00ff_ffff) as i32;
                }
            }
        }

        let mut lines = [0usize, 0, 0, 3, 4];
        squish_next(&mut real[3]);
        squish_next(&mut real[4]);
        let mut lines_read = 3usize;

        for y in 1..out_height {
            outm.next_row();
            let mut wrote = 0i32;
            let mut pointer = [0usize; 5];
            let mut count = [0i32; 5];
            let mut vals = [0f32; 5];
            while wrote < out_width {
                let mut min_count = 0x7fff_ffff;
                for i in 0..5 {
                    if count[i] == 0 {
                        let (cnt, v, _) = item(&real[lines[i]], &mut pointer[i]);
                        count[i] = cnt;
                        vals[i] = v;
                    }
                    if count[i] < min_count {
                        min_count = count[i];
                    }
                }
                let val = (vals[0] + vals[4]) * 0.0625f32 + (vals[1] + vals[3]) * 0.25f32 + vals[2] * 0.375f32;
                if val == 0.0 || val == 1.0 {
                    if min_count == 1 {
                        outm.words.push(val.to_bits());
                    } else {
                        outm.words.push(0x8000_0000 | ((val as u32) << 30) | min_count as u32);
                    }
                } else {
                    if min_count > 1 {
                        outm.words.push(0xa000_0000 | min_count as u32);
                    }
                    outm.words.push(val.to_bits());
                }
                wrote += min_count;
                for c in count.iter_mut() {
                    *c -= min_count;
                }
            }

            if y == 1 {
                lines[0] = 1;
                lines[1] = 2;
            }
            let temp = lines[0];
            lines[0] = lines[2];
            lines[2] = lines[4];
            lines[4] = lines[1];
            lines[1] = lines[3];
            lines[3] = temp;

            if lines_read < in_height {
                squish_next(&mut real[lines[3]]);
                lines_read += 1;
            } else {
                lines[3] = lines[2];
            }
            if lines_read < in_height {
                squish_next(&mut real[lines[4]]);
                lines_read += 1;
            } else {
                lines[4] = lines[3];
            }
        }
        masks.push(outm);
    }
}
