//! Image file input and output.
//!
//! Inputs are decoded to interleaved RGB or RGBA samples in native byte order.
//! Outputs: a small baseline TIFF/BigTIFF writer (so tags match the libtiff
//! output of the reference exactly), PNG via `png`, JPEG via `jpeg-encoder`.

use std::fs::File;
use std::io::{BufReader, BufWriter, Seek, SeekFrom, Write};

use crate::cli::TiffCompression;

pub enum Samples {
    U8(Vec<u8>),
    U16(Vec<u16>),
}

/// Header information gathered when an input is opened.
pub struct InputInfo {
    pub width: usize,
    pub height: usize,
    pub bpp: u32,
    pub spp: u32,
    /// Position in pixels (TIFF X/YPOSITION * resolution), 0 when absent.
    pub xpos: i32,
    pub ypos: i32,
    /// Resolution, or -1 when the file has none.
    pub xres: f32,
    pub yres: f32,
}

pub type IoResult<T> = Result<T, String>;

// ---------------------------------------------------------------------------
// TIFF input
// ---------------------------------------------------------------------------

use tiff::decoder::{ChunkType, Decoder, Limits, ifd::Value};
use tiff::tags::Tag;

fn open_tiff(filename: &str) -> IoResult<Decoder<BufReader<File>>> {
    let f = File::open(filename).map_err(|_| format!("Could not open {filename}"))?;
    let dec = Decoder::new(BufReader::new(f)).map_err(|_| format!("Could not open {filename}"))?;
    Ok(dec.with_limits(Limits::unlimited()))
}

fn value_f32(v: &Value) -> Option<f32> {
    match v {
        Value::Rational(n, d) => Some((*n as f64 / *d as f64) as f32),
        Value::SRational(n, d) => Some((*n as f64 / *d as f64) as f32),
        Value::Float(f) => Some(*f),
        Value::Double(f) => Some(*f as f32),
        Value::Short(s) => Some(*s as f32),
        Value::Unsigned(u) => Some(*u as f32),
        Value::List(l) if !l.is_empty() => value_f32(&l[0]),
        _ => None,
    }
}

pub fn tiff_info(filename: &str) -> IoResult<InputInfo> {
    let mut dec = open_tiff(filename)?;
    let (w, h) = dec.dimensions().map_err(|e| format!("Could not read {filename}: {e}"))?;
    if dec.get_chunk_type() == ChunkType::Tile {
        return Err(format!("Error: {filename} is a tiled TIFF, which is not supported"));
    }
    let tag_f32 = |dec: &mut Decoder<_>, tag: Tag| dec.find_tag(tag).ok().flatten().as_ref().and_then(value_f32);

    let bpp = dec
        .find_tag_unsigned_vec::<u16>(Tag::BitsPerSample)
        .ok()
        .flatten()
        .and_then(|v| v.first().copied())
        .unwrap_or(1) as u32;
    let spp = dec.find_tag_unsigned::<u16>(Tag::SamplesPerPixel).ok().flatten().unwrap_or(1) as u32;
    let planar = dec.find_tag_unsigned::<u16>(Tag::PlanarConfiguration).ok().flatten().unwrap_or(1);
    let photometric = dec.find_tag_unsigned::<u16>(Tag::PhotometricInterpretation).ok().flatten().unwrap_or(2);

    if bpp != 8 && bpp != 16 {
        return Err(format!("Invalid bpp {bpp} ({filename})"));
    }
    if (spp != 3 && spp != 4) || photometric != 2 || planar != 1 {
        return Err(format!("Error: {filename}: only RGB and RGBA images with contiguous samples are supported"));
    }

    let tiff_xpos = tag_f32(&mut dec, Tag::Unknown(286));
    let tiff_ypos = tag_f32(&mut dec, Tag::Unknown(287));
    let xres = tag_f32(&mut dec, Tag::XResolution).unwrap_or(-1.0);
    let yres = tag_f32(&mut dec, Tag::YResolution).unwrap_or(-1.0);

    // A missing position (or resolution) means 0, rather than uninitialised as in the C++.
    let place = |pos: Option<f32>, res: f32| match pos {
        Some(p) if res > 0.0 => ((p * res) as f64 + 0.5) as i32,
        _ => 0,
    };
    Ok(InputInfo {
        width: w as usize,
        height: h as usize,
        bpp,
        spp,
        xpos: place(tiff_xpos, xres),
        ypos: place(tiff_ypos, yres),
        xres,
        yres,
    })
}

pub fn tiff_read(filename: &str) -> IoResult<Samples> {
    let mut dec = open_tiff(filename)?;
    match dec.read_image() {
        Ok(tiff::decoder::DecodingResult::U8(v)) => Ok(Samples::U8(v)),
        Ok(tiff::decoder::DecodingResult::U16(v)) => Ok(Samples::U16(v)),
        Ok(_) => Err(format!("Error: {filename}: unsupported sample format")),
        Err(e) => Err(format!("Error: could not decode {filename}: {e}")),
    }
}

// ---------------------------------------------------------------------------
// PNG input
// ---------------------------------------------------------------------------

fn png_reader(filename: &str) -> IoResult<png::Reader<BufReader<File>>> {
    let mut f = File::open(filename).map_err(|_| format!("Could not open {filename}"))?;
    let mut sig = [0u8; 8];
    use std::io::Read;
    if f.read_exact(&mut sig).is_err() || sig != [137, 80, 78, 71, 13, 10, 26, 10] {
        return Err(format!("Bad PNG signature ({filename})"));
    }
    f.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut dec = png::Decoder::new(BufReader::new(f));
    dec.set_transformations(png::Transformations::IDENTITY);
    dec.read_info().map_err(|e| format!("Error: could not read {filename}: {e}"))
}

pub fn png_info(filename: &str) -> IoResult<InputInfo> {
    let reader = png_reader(filename)?;
    let info = reader.info();
    let spp = match info.color_type {
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        _ => return Err(format!("Bad PNG colour type ({filename})")),
    };
    let bpp = match info.bit_depth {
        png::BitDepth::Eight => 8,
        png::BitDepth::Sixteen => 16,
        _ => return Err(format!("Bad bit depth ({filename})")),
    };
    Ok(InputInfo { width: info.width as usize, height: info.height as usize, bpp, spp, xpos: 0, ypos: 0, xres: 90.0, yres: 90.0 })
}

pub fn png_read(filename: &str) -> IoResult<Samples> {
    let mut reader = png_reader(filename)?;
    let size = reader.output_buffer_size().ok_or_else(|| format!("Error: {filename} is too large"))?;
    let mut buf = vec![0u8; size];
    let frame = reader.next_frame(&mut buf).map_err(|e| format!("Error: could not decode {filename}: {e}"))?;
    buf.truncate(frame.buffer_size());
    match frame.bit_depth {
        png::BitDepth::Sixteen => Ok(Samples::U16(buf.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes([c[0], c[1]])).collect())),
        _ => Ok(Samples::U8(buf)),
    }
}

/// Read a palettised 8-bit PNG (seam file) as raw indices.
pub fn png_read_indices(filename: &str) -> IoResult<(usize, usize, Vec<u8>)> {
    let f = File::open(filename).map_err(|_| "Error: Couldn't open seam file".to_string())?;
    let mut f = BufReader::new(f);
    let mut sig = [0u8; 8];
    use std::io::Read;
    if f.read_exact(&mut sig).is_err() || sig != [137, 80, 78, 71, 13, 10, 26, 10] {
        return Err("Error: Bad PNG signature".to_string());
    }
    f.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut dec = png::Decoder::new(f);
    dec.set_transformations(png::Transformations::IDENTITY);
    let mut reader = dec.read_info().map_err(|_| "Error: Seam PNG problem".to_string())?;
    let (w, h, ct, bd) = {
        let i = reader.info();
        (i.width as usize, i.height as usize, i.color_type, i.bit_depth)
    };
    if ct != png::ColorType::Indexed || bd != png::BitDepth::Eight {
        return Err("Error: Incorrect seam PNG format".to_string());
    }
    let mut buf = vec![0u8; reader.output_buffer_size().unwrap_or(0)];
    reader.next_frame(&mut buf).map_err(|_| "Error: Seam PNG problem".to_string())?;
    Ok((w, h, buf))
}

// ---------------------------------------------------------------------------
// JPEG input
// ---------------------------------------------------------------------------

fn jpeg_decoder(filename: &str) -> IoResult<zune_jpeg::JpegDecoder<zune_jpeg::zune_core::bytestream::ZCursor<Vec<u8>>>> {
    let data = std::fs::read(filename).map_err(|_| format!("Could not open {filename}"))?;
    let options = zune_jpeg::zune_core::options::DecoderOptions::default()
        .jpeg_set_out_colorspace(zune_jpeg::zune_core::colorspace::ColorSpace::RGB);
    let mut dec = zune_jpeg::JpegDecoder::new_with_options(zune_jpeg::zune_core::bytestream::ZCursor::new(data), options);
    dec.decode_headers().map_err(|_| format!("Unknown JPEG format ({filename})"))?;
    Ok(dec)
}

pub fn jpeg_info(filename: &str) -> IoResult<InputInfo> {
    let dec = jpeg_decoder(filename)?;
    let info = dec.info().ok_or_else(|| format!("Unknown JPEG format ({filename})"))?;
    if info.width == 0 || info.height == 0 || info.components != 3 {
        return Err(format!("Unknown JPEG format ({filename})"));
    }
    Ok(InputInfo { width: info.width as usize, height: info.height as usize, bpp: 8, spp: 3, xpos: 0, ypos: 0, xres: 90.0, yres: 90.0 })
}

pub fn jpeg_read(filename: &str) -> IoResult<Samples> {
    let mut dec = jpeg_decoder(filename)?;
    dec.decode().map(Samples::U8).map_err(|e| format!("Error: could not decode {filename}: {e:?}"))
}

// ---------------------------------------------------------------------------
// TIFF output
// ---------------------------------------------------------------------------

pub const ROWS_PER_STRIP: usize = 64;

#[derive(Default)]
pub struct TiffMeta {
    pub xres: Option<f32>,
    pub yres: Option<f32>,
    pub xpos: Option<f32>,
    pub ypos: Option<f32>,
}

enum TagData {
    Short(Vec<u16>),
    Long(Vec<u32>),
    Long8(Vec<u64>),
    Rational(Vec<(u32, u32)>),
}

impl TagData {
    fn type_and_count(&self) -> (u16, usize) {
        match self {
            TagData::Short(v) => (3, v.len()),
            TagData::Long(v) => (4, v.len()),
            TagData::Long8(v) => (16, v.len()),
            TagData::Rational(v) => (5, v.len()),
        }
    }
    fn bytes(&self) -> Vec<u8> {
        match self {
            TagData::Short(v) => v.iter().flat_map(|x| x.to_le_bytes()).collect(),
            TagData::Long(v) => v.iter().flat_map(|x| x.to_le_bytes()).collect(),
            TagData::Long8(v) => v.iter().flat_map(|x| x.to_le_bytes()).collect(),
            TagData::Rational(v) => v.iter().flat_map(|(n, d)| n.to_le_bytes().into_iter().chain(d.to_le_bytes())).collect(),
        }
    }
}

/// Best rational approximation of a non-negative value within u32 range.
pub fn to_rational(v: f64) -> (u32, u32) {
    if v.is_nan() || v <= 0.0 {
        return (0, 1);
    }
    let (mut h0, mut h1, mut k0, mut k1) = (0u64, 1u64, 1u64, 0u64);
    let mut x = v;
    for _ in 0..64 {
        let a = x.floor();
        if a > u32::MAX as f64 {
            break;
        }
        let a = a as u64;
        let h2 = a * h1 + h0;
        let k2 = a * k1 + k0;
        if h2 > u32::MAX as u64 || k2 > u32::MAX as u64 {
            break;
        }
        (h0, h1, k0, k1) = (h1, h2, k1, k2);
        let frac = x - a as f64;
        if frac.abs() < 1e-12 || (h1 as f64 / k1 as f64 - v).abs() <= v * 1e-9 {
            break;
        }
        x = 1.0 / frac;
    }
    if k1 == 0 { (u32::MAX, 1) } else { (h1 as u32, k1 as u32) }
}

fn packbits_row(row: &[u8], out: &mut Vec<u8>) {
    let mut i = 0;
    let n = row.len();
    while i < n {
        // run of identical bytes?
        let mut run = 1;
        while i + run < n && run < 128 && row[i + run] == row[i] {
            run += 1;
        }
        if run >= 2 {
            out.push((1i32 - run as i32) as i8 as u8);
            out.push(row[i]);
            i += run;
        } else {
            let start = i;
            let mut len = 0;
            while i < n && len < 128 {
                if i + 1 < n && row[i + 1] == row[i] {
                    break;
                }
                i += 1;
                len += 1;
            }
            if len == 0 {
                i += 1;
                len = 1;
            }
            out.push((len - 1) as u8);
            out.extend_from_slice(&row[start..start + len]);
        }
    }
}

pub struct TiffWriter {
    file: BufWriter<File>,
    big: bool,
    width: usize,
    height: usize,
    spp: usize,
    bits: usize,
    compression: TiffCompression,
    offsets: Vec<u64>,
    counts: Vec<u64>,
    meta: TiffMeta,
}

impl TiffWriter {
    #[allow(clippy::too_many_arguments)]
    pub fn new(file: File, big: bool, width: usize, height: usize, spp: usize, bits: usize, compression: TiffCompression, meta: TiffMeta) -> IoResult<Self> {
        let mut file = BufWriter::new(file);
        let header: Vec<u8> = if big {
            let mut h = vec![b'I', b'I', 43, 0, 8, 0, 0, 0];
            h.extend_from_slice(&0u64.to_le_bytes());
            h
        } else {
            vec![b'I', b'I', 42, 0, 0, 0, 0, 0]
        };
        file.write_all(&header).map_err(|e| e.to_string())?;
        Ok(TiffWriter { file, big, width, height, spp, bits, compression, offsets: vec![], counts: vec![], meta })
    }

    fn pos(&mut self) -> IoResult<u64> {
        self.file.stream_position().map_err(|e| e.to_string())
    }

    /// Compress one strip of little-endian interleaved sample bytes (whole rows).
    /// Independent of writer state, so strips can be encoded in parallel.
    pub fn encode_strip(&self, data: &[u8]) -> IoResult<Vec<u8>> {
        let row_bytes = self.width * self.spp * self.bits / 8;
        Ok(match self.compression {
            TiffCompression::None => data.to_vec(),
            TiffCompression::Lzw => weezl::encode::Encoder::with_tiff_size_switch(weezl::BitOrder::Msb, 8)
                .encode(data)
                .map_err(|e| e.to_string())?,
            TiffCompression::PackBits => {
                let mut out = Vec::with_capacity(data.len() + data.len() / 64 + 8);
                for row in data.chunks(row_bytes) {
                    packbits_row(row, &mut out);
                }
                out
            }
        })
    }

    /// Append an encoded strip (strips must be written in order).
    pub fn write_encoded(&mut self, encoded: &[u8]) -> IoResult<()> {
        let offset = self.pos()?;
        self.file.write_all(encoded).map_err(|e| e.to_string())?;
        self.offsets.push(offset);
        self.counts.push(encoded.len() as u64);
        Ok(())
    }

    pub fn finish(mut self) -> IoResult<()> {
        let mut tags: Vec<(u16, TagData)> = vec![
            (256, TagData::Long(vec![self.width as u32])),
            (257, TagData::Long(vec![self.height as u32])),
            (258, TagData::Short(vec![self.bits as u16; self.spp])),
            (
                259,
                TagData::Short(vec![match self.compression {
                    TiffCompression::None => 1,
                    TiffCompression::Lzw => 5,
                    TiffCompression::PackBits => 32773,
                }]),
            ),
            (262, TagData::Short(vec![2])),
            (273, if self.big { TagData::Long8(self.offsets.clone()) } else { TagData::Long(self.offsets.iter().map(|&o| o as u32).collect()) }),
            (277, TagData::Short(vec![self.spp as u16])),
            (278, TagData::Long(vec![ROWS_PER_STRIP as u32])),
            (279, if self.big { TagData::Long8(self.counts.clone()) } else { TagData::Long(self.counts.iter().map(|&o| o as u32).collect()) }),
        ];
        let rational = |code: u16, v: Option<f32>| v.filter(|v| v.is_finite()).map(|v| (code, TagData::Rational(vec![to_rational(v as f64)])));
        tags.extend(rational(282, self.meta.xres));
        tags.extend(rational(283, self.meta.yres));
        tags.push((284, TagData::Short(vec![1])));
        tags.extend(rational(286, self.meta.xpos));
        tags.extend(rational(287, self.meta.ypos));
        if self.spp == 4 {
            tags.push((338, TagData::Short(vec![2])));
        }
        tags.sort_by_key(|t| t.0);

        // Values that don't fit in the entry go before the directory.
        let inline = if self.big { 8 } else { 4 };
        let mut value_offsets = Vec::new();
        for (_, data) in &tags {
            let bytes = data.bytes();
            if bytes.len() > inline {
                let mut pos = self.pos()?;
                if pos % 2 == 1 {
                    self.file.write_all(&[0]).map_err(|e| e.to_string())?;
                    pos += 1;
                }
                self.file.write_all(&bytes).map_err(|e| e.to_string())?;
                value_offsets.push(Some(pos));
            } else {
                value_offsets.push(None);
            }
        }
        let mut ifd_pos = self.pos()?;
        if ifd_pos % 2 == 1 {
            self.file.write_all(&[0]).map_err(|e| e.to_string())?;
            ifd_pos += 1;
        }
        let mut ifd = Vec::new();
        if self.big {
            ifd.extend_from_slice(&(tags.len() as u64).to_le_bytes());
        } else {
            ifd.extend_from_slice(&(tags.len() as u16).to_le_bytes());
        }
        for ((code, data), off) in tags.iter().zip(&value_offsets) {
            let (ty, count) = data.type_and_count();
            ifd.extend_from_slice(&code.to_le_bytes());
            ifd.extend_from_slice(&ty.to_le_bytes());
            let mut field = match off {
                Some(o) => {
                    if self.big {
                        o.to_le_bytes().to_vec()
                    } else {
                        (*o as u32).to_le_bytes().to_vec()
                    }
                }
                None => data.bytes(),
            };
            field.resize(inline, 0);
            if self.big {
                ifd.extend_from_slice(&(count as u64).to_le_bytes());
            } else {
                ifd.extend_from_slice(&(count as u32).to_le_bytes());
            }
            ifd.extend_from_slice(&field);
        }
        if self.big {
            ifd.extend_from_slice(&0u64.to_le_bytes());
        } else {
            ifd.extend_from_slice(&0u32.to_le_bytes());
        }
        self.file.write_all(&ifd).map_err(|e| e.to_string())?;
        if self.big {
            self.file.seek(SeekFrom::Start(8)).map_err(|e| e.to_string())?;
            self.file.write_all(&ifd_pos.to_le_bytes()).map_err(|e| e.to_string())?;
        } else {
            if ifd_pos > u32::MAX as u64 {
                return Err("Error: output exceeds 4GB; use --bigtiff".to_string());
            }
            self.file.seek(SeekFrom::Start(4)).map_err(|e| e.to_string())?;
            self.file.write_all(&(ifd_pos as u32).to_le_bytes()).map_err(|e| e.to_string())?;
        }
        self.file.flush().map_err(|e| e.to_string())
    }

}

// ---------------------------------------------------------------------------
// PNG output
// ---------------------------------------------------------------------------

/// The seam/XOR map palette from the reference's `Pnger`.
pub fn seam_palette() -> Vec<u8> {
    let mut pal = vec![0u8; 768];
    let mut base = 2.0f64;
    for i in 0..255 {
        let mut rad = base;
        let chan = |rad: f64| (0.0f64).max((1.0f64).min(rad.min(4.0 - rad)));
        let r = chan(rad);
        rad += 2.0;
        if rad >= 6.0 {
            rad -= 6.0;
        }
        let g = chan(rad);
        rad += 2.0;
        if rad >= 6.0 {
            rad -= 6.0;
        }
        let b = chan(rad);
        base += 6.0 * 0.618033988749895;
        if base >= 6.0 {
            base -= 6.0;
        }
        pal[i * 3] = (r.sqrt() * 255.0 + 0.5) as u8;
        pal[i * 3 + 1] = (g.sqrt() * 255.0 + 0.5) as u8;
        pal[i * 3 + 2] = (b.sqrt() * 255.0 + 0.5) as u8;
    }
    pal
}

pub fn png_write_palette(filename: &str, width: usize, height: usize, indices: &[u8]) -> IoResult<()> {
    let f = File::create(filename).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(BufWriter::new(f), width as u32, height as u32);
    enc.set_color(png::ColorType::Indexed);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_palette(seam_palette());
    enc.set_deflate_compression(png::DeflateCompression::Level(3));
    let mut w = enc.write_header().map_err(|e| e.to_string())?;
    w.write_image_data(indices).map_err(|e| e.to_string())?;
    w.finish().map_err(|e| e.to_string())
}

/// Write RGB/RGBA PNG. `data` holds interleaved samples as big-endian bytes for 16-bit.
pub fn png_write(file: File, width: usize, height: usize, spp: usize, bits: usize, level: i32, data: &[u8]) -> IoResult<()> {
    let mut enc = png::Encoder::new(BufWriter::new(file), width as u32, height as u32);
    enc.set_color(if spp == 4 { png::ColorType::Rgba } else { png::ColorType::Rgb });
    enc.set_depth(if bits == 16 { png::BitDepth::Sixteen } else { png::BitDepth::Eight });
    let level = if level < 0 { 3 } else { level };
    enc.set_deflate_compression(if level == 0 { png::DeflateCompression::NoCompression } else { png::DeflateCompression::Level(level as u8) });
    let mut w = enc.write_header().map_err(|e| e.to_string())?;
    w.write_image_data(data).map_err(|e| e.to_string())?;
    w.finish().map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// JPEG output
// ---------------------------------------------------------------------------

pub fn jpeg_write(file: File, width: usize, height: usize, quality: i32, rgb: &[u8]) -> IoResult<()> {
    if width > u16::MAX as usize || height > u16::MAX as usize {
        return Err("Error: image too large for JPEG output".to_string());
    }
    let q = quality.clamp(1, 100) as u8;
    let mut enc = jpeg_encoder::Encoder::new(BufWriter::new(file), q);
    enc.set_sampling_factor(jpeg_encoder::SamplingFactor::F_2_2);
    enc.encode(rgb, width as u16, height as u16, jpeg_encoder::ColorType::Rgb).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unpackbits(mut d: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        while let Some((&n, rest)) = d.split_first() {
            let n = n as i8;
            if n >= 0 {
                out.extend_from_slice(&rest[..n as usize + 1]);
                d = &rest[n as usize + 1..];
            } else if n != -128 {
                out.extend(std::iter::repeat_n(rest[0], (1 - n as i32) as usize));
                d = &rest[1..];
            } else {
                d = rest;
            }
        }
        out
    }

    #[test]
    fn packbits_round_trips() {
        let cases: Vec<Vec<u8>> = vec![
            vec![],
            vec![7],
            vec![1, 2],
            vec![5; 300],
            (0..=255).cycle().take(700).collect(),
            [vec![1, 2, 3], vec![9; 129], vec![4, 4, 5], vec![6; 2]].concat(),
        ];
        for row in cases {
            let mut enc = Vec::new();
            packbits_row(&row, &mut enc);
            assert_eq!(unpackbits(&enc), row);
        }
    }

    #[test]
    fn rationals_approximate_closely() {
        for v in [72.0, 300.0, 0.69444, 13.0 / 100.0, 1.0 / 3.0, 12345.678, 1e-6] {
            let (n, d) = to_rational(v);
            assert!(((n as f64 / d as f64) - v).abs() <= v * 1e-6, "{v} -> {n}/{d}");
        }
        assert_eq!(to_rational(0.0), (0, 1));
        assert_eq!(to_rational(72.0), (72, 1));
    }
}
