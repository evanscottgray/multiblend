//! Input images: open, read, trim to the opaque bounding box, inpaint
//! transparent pixels, and split into planar channels (image.cpp).

use crate::cli::ImageType;
use crate::io::{self, InputInfo, Samples};
use crate::masks::{MaskLevel, RunMask};
use crate::util::{dump, dump_enabled};
use crate::{die, out};

pub enum Plane {
    U8(Vec<u8>),
    U16(Vec<u16>),
}


pub struct Image {
    pub filename: String,
    pub kind: ImageType,
    pub info: InputInfo,
    pub xpos: i32,
    pub ypos: i32,
    pub bpp: u32,
    /// Trimmed size.
    pub width: usize,
    pub height: usize,
    /// Opacity runs of the trimmed image (the C++ `tiff_mask`).
    pub opacity: RunMask,
    pub channels: Vec<Option<Plane>>,
    pub masks: Vec<MaskLevel>,
    pub seam_present: bool,
}

pub fn image_type(filename: &str) -> Option<ImageType> {
    let p = filename.rfind('.')?;
    match filename[p + 1..].to_ascii_lowercase().as_str() {
        "tif" | "tiff" => Some(ImageType::Tiff),
        "jpg" | "jpeg" => Some(ImageType::Jpeg),
        "png" => Some(ImageType::Png),
        _ => Some(ImageType::None),
    }
}

impl Image {
    pub fn open(filename: &str, xpos_add: i32, ypos_add: i32) -> Image {
        let kind = match image_type(filename) {
            None => die!("Could not identify file extension: {filename}"),
            Some(ImageType::None) => die!("Unknown file extension: {filename}"),
            Some(k) => k,
        };
        let info = match kind {
            ImageType::Tiff => io::tiff_info(filename),
            ImageType::Png => io::png_info(filename),
            _ => io::jpeg_info(filename),
        }
        .unwrap_or_else(|e| die!("{e}"));
        Image {
            filename: filename.to_string(),
            kind,
            xpos: info.xpos + xpos_add,
            ypos: info.ypos + ypos_add,
            bpp: info.bpp,
            width: 0,
            height: 0,
            info,
            opacity: RunMask::default(),
            channels: Vec::new(),
            masks: Vec::new(),
            seam_present: false,
        }
    }

    /// Rough peak bytes of transient memory while reading (decoded samples, packed
    /// pixels, inpainting distances, channel planes); used to bound parallel reads.
    pub fn read_cost(&self) -> usize {
        let bytes = self.bpp as usize / 8;
        self.info.width * self.info.height * (self.info.spp as usize * bytes * 2 + 4 + 3 * bytes)
    }

    /// Decode, trim, inpaint and extract channels. `index` names debug dumps.
    /// Safe to run for several images in parallel.
    pub fn read(&mut self, index: usize) {
        let samples = match self.kind {
            ImageType::Tiff => io::tiff_read(&self.filename),
            ImageType::Png => io::png_read(&self.filename),
            _ => io::jpeg_read(&self.filename),
        }
        .unwrap_or_else(|e| die!("\n{e}"));

        let (fw, fh) = (self.info.width, self.info.height);
        if self.info.spp == 4 {
            match samples {
                Samples::U8(s) => {
                    let mut px: Vec<u32> = s.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
                    self.process_rgba(&mut px, fw, fh, |p| p >= 0xff00_0000, |p, c| (p >> (8 * c)) & 0xff, index);
                }
                Samples::U16(s) => {
                    let mut px: Vec<u64> = s
                        .as_chunks::<4>().0.iter()
                        .map(|c| c[0] as u64 | (c[1] as u64) << 16 | (c[2] as u64) << 32 | (c[3] as u64) << 48)
                        .collect();
                    self.process_rgba(&mut px, fw, fh, |p| p >= 0xffff_0000_0000_0000, |p, c| ((p >> (16 * c)) & 0xffff) as u32, index);
                }
            }
        } else {
            self.width = fw;
            self.height = fh;
            let mut opacity = RunMask::default();
            for _ in 0..fh {
                opacity.push(0x8000_0000 | fw as u32);
                opacity.next_row();
            }
            self.opacity = opacity;
            let n = fw * fh;
            self.channels = match samples {
                Samples::U8(s) => (0..3).map(|c| Some(Plane::U8((0..n).map(|i| s[i * 3 + c]).collect()))).collect(),
                Samples::U16(s) => (0..3).map(|c| Some(Plane::U16((0..n).map(|i| s[i * 3 + c]).collect()))).collect(),
            };
        }

        if dump_enabled() {
            for c in 0..3 {
                match self.channels[c].as_ref().unwrap() {
                    Plane::U8(v) => dump(&format!("img{index}_ch{c}"), v, self.width, self.height, "u8"),
                    Plane::U16(v) => {
                        let b: Vec<u8> = v.iter().flat_map(|x| x.to_le_bytes()).collect();
                        dump(&format!("img{index}_ch{c}"), &b, self.width, self.height, "u16")
                    }
                }
            }
        }
        out!(1, "Processing {}...\n", self.filename);
    }

    fn process_rgba<P: Copy>(&mut self, px: &mut [P], fw: usize, fh: usize, opaque: impl Fn(P) -> bool, chan: impl Fn(P, u32) -> u32, _index: usize) {
        // Trim to the bounding box of fully opaque pixels.
        let (mut top, mut bottom, mut left, mut right) = (usize::MAX, 0, usize::MAX, 0);
        for y in 0..fh {
            let row = &px[y * fw..(y + 1) * fw];
            if let Some(l) = row.iter().position(|&p| opaque(p)) {
                let r = row.iter().rposition(|&p| opaque(p)).unwrap();
                top = top.min(y);
                bottom = y;
                left = left.min(l);
                right = right.max(r);
            }
        }
        if top == usize::MAX {
            die!("\nError: {} has no fully opaque pixels", self.filename);
        }
        let (w, h) = (right + 1 - left, bottom + 1 - top);
        self.width = w;
        self.height = h;
        self.xpos += left as i32;
        self.ypos += top as i32;

        self.opacity = inpaint(px, fw, top * fw + left, w, h, &opaque);

        let is16 = self.bpp == 16;
        let px = &*px;
        let plane = |c: u32| {
            let rows = (top..top + h).map(move |y| &px[y * fw + left..y * fw + left + w]);
            if is16 {
                Plane::U16(rows.flat_map(|r| r.iter().map(|&p| chan(p, c) as u16)).collect())
            } else {
                Plane::U8(rows.flat_map(|r| r.iter().map(|&p| chan(p, c) as u8)).collect())
            }
        };
        self.channels = (0..3).map(|c| Some(plane(c))).collect();
    }
}

/// Fill transparent pixels with the nearest opaque pixel (3/4 chamfer distance),
/// forward pass then backward pass, exactly as `Image::Read` does. Operates on the
/// `w`x`h` window of `px` starting at index `org` with row stride `stride`.
/// Returns the opacity runs of the window.
fn inpaint<P: Copy>(px: &mut [P], stride: usize, org: usize, w: usize, h: usize, opaque: &impl Fn(P) -> bool) -> RunMask {
    let mut mask = RunMask::default();
    let mut fwd = vec![0u32; w * h];

    for y in 0..h {
        let row = org + y * stride;
        let (before, rest) = fwd.split_at_mut(y * w);
        let prev: &[u32] = if y > 0 { &before[(y - 1) * w..] } else { &[] };
        let this = &mut rest[..w];
        let (mut a, mut b, mut d) = (0u32, 0u32, 0u32);
        let mut c: u32;
        let mut x = 0;
        while x < w {
            let mut mc = 0u32;
            let mut first = true;
            while x < w && !opaque(px[row + x]) {
                if y == 0 {
                    if x == 0 {
                        this[0] = 0x8000_0000;
                    } else {
                        this[x] = this[x - 1].wrapping_add(3);
                        px[row + x] = px[row + x - 1];
                    }
                } else if x == 0 {
                    b = prev[0].wrapping_add(3);
                    c = if w > 1 { prev[1].wrapping_add(4) } else { u32::MAX };
                    let copy = if b < c {
                        d = b;
                        row - stride
                    } else {
                        d = c;
                        row - stride + 1
                    };
                    this[0] = d;
                    px[row] = px[copy];
                    a = b.wrapping_add(1);
                    b = c.wrapping_sub(1);
                    d = d.wrapping_add(3);
                } else {
                    if first {
                        a = prev[x - 1].wrapping_add(4);
                        b = prev[x].wrapping_add(3);
                        d = this[x - 1].wrapping_add(3);
                        first = false;
                    }
                    c = if x < w - 1 { prev[x + 1].wrapping_add(4) } else { 0xffff_ffff };
                    let mut copy = row + x - 1;
                    if a < d {
                        d = a;
                        copy = row - stride + x - 1;
                    }
                    if b < d {
                        d = b;
                        copy = row - stride + x;
                    }
                    if c < d {
                        d = c;
                        copy = row - stride + x + 1;
                    }
                    this[x] = d;
                    px[row + x] = px[copy];
                    a = b.wrapping_add(1);
                    b = c.wrapping_sub(1);
                    d = d.wrapping_add(3);
                }
                x += 1;
                mc += 1;
            }
            if mc > 0 {
                mask.push(mc);
            }
            mc = 0;
            while x < w && opaque(px[row + x]) {
                this[x] = 0;
                x += 1;
                mc += 1;
            }
            if mc > 0 {
                mask.push(0x8000_0000 | mc);
            }
        }
        mask.next_row();
    }

    // Backward pass. Row h-1 starts from its own forward values.
    let wi = w as isize;
    let mut prev: Vec<u32> = fwd[(h - 1) * w..h * w].to_vec();
    {
        let row = org + (h - 1) * stride;
        let mut x = wi - 1;
        let mut d = 0u32;
        for &run in mask.row(h - 1).iter().rev() {
            let count = (run & 0x7fff_ffff) as isize;
            if run & 0x8000_0000 != 0 {
                x -= count;
                d = 3;
            } else {
                let mut m = count;
                if x == wi - 1 {
                    d = prev[x as usize].wrapping_add(3);
                    m -= 1;
                    x -= 1;
                }
                while m > 0 {
                    let xu = x as usize;
                    let best = prev[xu];
                    if d < best {
                        prev[xu] = d;
                        px[row + xu] = px[row + xu + 1];
                        d = d.wrapping_add(3);
                    } else {
                        d = best.wrapping_add(3);
                    }
                    x -= 1;
                    m -= 1;
                }
            }
        }
    }

    let mut this = vec![0u32; w];
    for y in (0..h.saturating_sub(1)).rev() {
        let row = org + y * stride;
        let below = row + stride;
        let mut x = wi - 1;
        let mut c: u32;
        let mut d: u32 = 0x8000_0000;
        for &run in mask.row(y).iter().rev() {
            let count = (run & 0x7fff_ffff) as isize;
            if run & 0x8000_0000 != 0 {
                x -= count;
                this[(x + 1) as usize..(x + 1 + count) as usize].fill(0);
                d = 3;
            } else {
                let xu = x as usize;
                let mut b = prev[xu].wrapping_add(3);
                c = if xu < w - 1 { prev[xu + 1].wrapping_add(4) } else { 0x8000_0000 };
                let mut m = count;
                while m > 0 {
                    let xu = x as usize;
                    let a = if xu > 0 { prev[xu - 1].wrapping_add(4) } else { 0x8000_0000 };
                    let mut best = fwd[y * w + xu];
                    let mut copy = None;
                    if a < best {
                        best = a;
                        copy = Some(below + xu - 1);
                    }
                    if b < best {
                        best = b;
                        copy = Some(below + xu);
                    }
                    if c < best {
                        best = c;
                        copy = Some(below + xu + 1);
                    }
                    if d < best {
                        best = d;
                        copy = Some(row + xu + 1);
                    }
                    if let Some(cp) = copy {
                        px[row + xu] = px[cp];
                    }
                    this[xu] = best;
                    x -= 1;
                    c = b.wrapping_add(1);
                    d = best.wrapping_add(3);
                    b = a.wrapping_sub(1);
                    m -= 1;
                }
            }
        }
        std::mem::swap(&mut this, &mut prev);
    }
    mask
}
