//! Command-line parsing, ported from the top of `main()` in multiblend.cpp.
//!
//! Behavioural differences from the C++ (see tests/FINDINGS.md):
//! * wrap modes are case-insensitive, as documented;
//! * malformed `--levels` / `--cache-threshold` values are errors instead of UB;
//! * reading past the end of the argument list is an error instead of UB.

use crate::util::{adjust_verbosity, atoi, output, scan_int};
use crate::{die, out};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ImageType {
    None,
    Tiff,
    Jpeg,
    Png,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TiffCompression {
    None,
    Lzw,
    PackBits,
}

pub struct Input {
    pub filename: String,
    pub xpos_add: i32,
    pub ypos_add: i32,
}

pub struct Options {
    pub fixed_levels: i32,
    pub add_levels: i32,
    pub no_mask: bool,
    pub big_tiff: bool,
    pub bgr: bool,
    pub wideblend: bool,
    pub reverse: bool,
    pub timing: bool,
    pub dither: bool,
    pub gamma: bool,
    pub wrap: i32,
    pub output_type: ImageType,
    pub jpeg_quality: i32,
    pub compression: Option<TiffCompression>,
    pub seamsave: Option<String>,
    pub seamload: Option<String>,
    pub xor: Option<String>,
    pub output: Option<String>,
    pub output_bpp: i32,
    pub inputs: Vec<Input>,
}

pub const BANNER: &str =
    "Multiblend v2.0.0 (c) 2021 David Horman        http://horman.net/multiblend/\n";
pub const RULE: &str =
    "----------------------------------------------------------------------------\n";

fn print_help() -> ! {
    output(1, "\n");
    output(1, BANNER);
    output(1, RULE);
    let help = "\
Usage: multiblend [options] [-o OUTPUT] INPUT [X,Y] [INPUT] [X,Y] [INPUT]...

Options:
  --levels X / -l X      X: set number of blending levels to X
                        -X: decrease number of blending levels by X
                        +X: increase number of blending levels by X
  --depth D / -d D       Override automatic output image depth (8 or 16)
  --bgr                  Swap RGB order
  --wideblend            Calculate number of levels based on output image size,
                         rather than input image size
  -w, --wrap=[mode]      Blend around images boundaries (NONE (default),
                         HORIZONTAL, VERTICAL). When specified without a mode,
                         defaults to HORIZONTAL.
  --compression=X        Output file compression. For TIFF output, X may be:
                         NONE, PACKBITS, or LZW (default)
                         For JPEG output, X is JPEG quality (0-100, default 75)
                         For PNG output, X is PNG filter (0-9, default 3)
  --cache-threshold=     Allocate memory beyond X bytes/[K]ilobytes/
      X[K/M/G]           [M]egabytes/[G]igabytes to disk
  --no-dither            Disable dithering
  --tempdir <dir>        Specify temporary directory (default: system temp)
  --save-seams <file>    Save seams to PNG file for external editing
  --load-seams <file>    Load seams from PNG file
  --no-output            Do not blend (for use with --save-seams)
                         Must be specified as last option before input images
  --bigtiff              BigTIFF output
  --reverse              Reverse image priority (last=highest) for resolving
                         indeterminate pixels
  --quiet                Suppress output (except warnings)
  --all-threads          Use all available CPU threads (the default)
  [X,Y]                  Optional position adjustment for previous input image
";
    print!("{help}");
    std::process::exit(0);
}

/// Split `--opt=value` into two arguments, up to (and including) `-o`/`--output`.
fn split_args(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut skip = false;
    for a in args {
        if skip {
            out.push(a.clone());
            continue;
        }
        let head = match a.find('=') {
            Some(p) => {
                out.push(a[..p].to_string());
                if p + 1 < a.len() {
                    out.push(a[p + 1..].to_string());
                }
                &a[..p]
            }
            None => {
                out.push(a.clone());
                a.as_str()
            }
        };
        if head == "-o" || head == "--output" {
            skip = true;
        }
    }
    out
}

fn extension(name: &str) -> Option<String> {
    name.rfind('.').map(|p| name[p + 1..].to_ascii_lowercase())
}

pub fn parse(argv: Vec<String>) -> Options {
    let args = &argv[1..];
    if args.is_empty() || args[0] == "-h" || args[0] == "--help" || args[0] == "/?" {
        print_help();
    }

    let a = split_args(args);
    if a.len() < 3 {
        die!("Error: Not enough arguments (try -h for help)");
    }

    let mut o = Options {
        fixed_levels: 0,
        add_levels: 0,
        no_mask: false,
        big_tiff: false,
        bgr: false,
        wideblend: false,
        reverse: false,
        timing: false,
        dither: true,
        gamma: false,
        wrap: 0,
        output_type: ImageType::None,
        jpeg_quality: -1,
        compression: None,
        seamsave: None,
        seamload: None,
        xor: None,
        output: None,
        output_bpp: 0,
        inputs: Vec::new(),
    };

    let n = a.len();
    let has_next = |i: usize| i + 1 < n;
    let mut i = 0;
    while i < n {
        let arg = a[i].as_str();
        match arg {
            "-d" | "--d" | "--depth" | "--bpp" => {
                if !has_next(i) {
                    die!("Error: Missing parameter value");
                }
                i += 1;
                o.output_bpp = atoi(&a[i]) as i32;
                if o.output_bpp != 8 && o.output_bpp != 16 {
                    die!("Error: Invalid output depth specified");
                }
            }
            "-l" | "--levels" => {
                if !has_next(i) {
                    die!("Error: Missing parameter value");
                }
                i += 1;
                let v = &a[i];
                let (val, used) = match scan_int(v) {
                    Some(r) => r,
                    None => die!("Error: Bad --levels parameter"),
                };
                if used != v.len() {
                    die!("Error: Bad --levels parameter");
                }
                if v.starts_with('+') || v.starts_with('-') {
                    o.add_levels = val as i32;
                } else {
                    o.fixed_levels = if val == 0 { 1 } else { val as i32 };
                }
            }
            "--wrap" | "-w" => {
                if !has_next(i) {
                    die!("Error: Missing parameters");
                }
                match a[i + 1].to_ascii_lowercase().as_str() {
                    "none" | "open" => i += 1,
                    "horizontal" | "h" => {
                        o.wrap = 1;
                        i += 1
                    }
                    "vertical" | "v" => {
                        o.wrap = 2;
                        i += 1
                    }
                    "both" | "hv" => {
                        o.wrap = 3;
                        i += 1
                    }
                    _ => o.wrap = 1,
                }
            }
            "--cache-threshold" => {
                if !has_next(i) {
                    die!("Error: Missing parameters");
                }
                i += 1;
                let v = &a[i];
                let digits = v.bytes().take_while(|c| c.is_ascii_digit()).count();
                if digits == 0 {
                    die!("Error: Bad --cache-threshold parameter");
                }
                if digits != v.len() {
                    let ok = digits == v.len() - 1
                        && matches!(
                            v.as_bytes()[digits],
                            b'k' | b'K' | b'm' | b'M' | b'g' | b'G'
                        );
                    if !ok {
                        die!("Error: Bad --cache-threshold parameter");
                    }
                }
                // Accepted for compatibility; the port keeps everything in memory.
            }
            "--nomask" | "--no-mask" => o.no_mask = true,
            "--timing" | "--timings" => o.timing = true,
            "--bigtiff" => o.big_tiff = true,
            "--bgr" => o.bgr = true,
            "--wideblend" => o.wideblend = true,
            "--reverse" => o.reverse = true,
            "--gamma" => o.gamma = true,
            "--no-dither" | "--nodither" => o.dither = false,
            _ if arg.starts_with("-f") => out!(0, "ignoring Enblend option -f\n"),
            "-a" => out!(0, "ignoring Enblend option -a\n"),
            "--no-ciecam" => out!(0, "ignoring Enblend option --no-ciecam\n"),
            "--primary-seam-generator" => {
                out!(0, "ignoring Enblend option --primary-seam-generator\n");
                i += 1;
            }
            "--compression" => {
                if !has_next(i) {
                    die!("Error: Missing parameter value");
                }
                i += 1;
                let v = a[i].as_str();
                let lower = v.to_ascii_lowercase();
                if v == "0" {
                    o.jpeg_quality = 0;
                } else if atoi(v) > 0 {
                    o.jpeg_quality = atoi(v) as i32;
                } else if lower == "lzw" {
                    o.compression = Some(TiffCompression::Lzw);
                } else if lower == "packbits" {
                    o.compression = Some(TiffCompression::PackBits);
                } else if lower == "none" {
                    o.compression = Some(TiffCompression::None);
                } else {
                    die!("Error: Unknown compression codec {v}");
                }
            }
            "-v" | "--verbose" => adjust_verbosity(1),
            "-q" | "--quiet" => adjust_verbosity(-1),
            "--saveseams" | "--save-seams" if has_next(i) => {
                i += 1;
                o.seamsave = Some(a[i].clone());
            }
            "--loadseams" | "--load-seams" if has_next(i) => {
                i += 1;
                o.seamload = Some(a[i].clone());
            }
            "--savexor" | "--save-xor" if has_next(i) => {
                i += 1;
                o.xor = Some(a[i].clone());
            }
            "--tempdir" | "--tmpdir" if has_next(i) => i += 1, // accepted; no disk cache
            "--all-threads" => {}
            "-o" | "--output" => {
                if has_next(i) {
                    i += 1;
                    let name = a[i].clone();
                    let ext = match extension(&name) {
                        Some(e) => e,
                        None => die!("Error: Unknown output filetype"),
                    };
                    o.output_type = match ext.as_str() {
                        "jpg" | "jpeg" => {
                            if o.jpeg_quality == -1 {
                                o.jpeg_quality = 75;
                            }
                            ImageType::Jpeg
                        }
                        "tif" | "tiff" => ImageType::Tiff,
                        "png" => ImageType::Png,
                        _ => die!("Error: Unknown file extension"),
                    };
                    o.output = Some(name);
                    i += 1;
                    break;
                }
                i += 1;
                break;
            }
            "--no-output" => {
                i += 1;
                break;
            }
            _ => die!("Error: Unknown argument \"{arg}\""),
        }
        i += 1;
    }

    if o.compression.is_some() {
        if o.output_type != ImageType::Tiff {
            out!(
                0,
                "Warning: non-TIFF output; ignoring TIFF compression setting\n"
            );
        }
    } else if o.output_type == ImageType::Tiff {
        o.compression = Some(TiffCompression::Lzw);
    }

    if o.jpeg_quality != -1 && o.output_type != ImageType::Jpeg && o.output_type != ImageType::Png {
        out!(
            0,
            "Warning: non-JPEG/PNG output; ignoring compression quality setting\n"
        );
    }

    if (o.jpeg_quality < -1 || o.jpeg_quality > 9) && o.output_type == ImageType::Png {
        die!("Error: Bad PNG compression quality setting\n");
    }

    if o.output_type == ImageType::None && o.seamsave.is_none() {
        die!("Error: No output file specified");
    }
    if o.seamload.is_some() && o.seamsave.is_some() {
        die!("Error: Cannot load and save seams at the same time");
    }
    if o.wrap == 3 {
        die!("Error: Wrapping in both directions is not currently supported");
    }

    if i < n && a[i] == "--" {
        i += 1;
    }

    while i < n {
        if let Some(last) = o.inputs.last_mut()
            && let Some((x, y)) = parse_xy(&a[i])
        {
            last.xpos_add = x;
            last.ypos_add = y;
            i += 1;
            continue;
        }
        o.inputs.push(Input {
            filename: a[i].clone(),
            xpos_add: 0,
            ypos_add: 0,
        });
        i += 1;
    }

    if o.inputs.is_empty() {
        die!("Error: No input files specified");
    }
    let n_images = o.inputs.len();
    if o.seamsave.is_some() && n_images > 256 {
        o.seamsave = None;
        out!(
            0,
            "Warning: seam saving not possible with more than 256 images\n"
        );
    }
    if o.seamload.is_some() && n_images > 256 {
        o.seamload = None;
        out!(
            0,
            "Warning: seam loading not possible with more than 256 images\n"
        );
    }
    if o.xor.is_some() && n_images > 255 {
        o.xor = None;
        out!(
            0,
            "Warning: XOR map saving not possible with more than 255 images\n"
        );
    }

    o
}

/// `sscanf("%d,%d%n")` consuming the whole string.
fn parse_xy(s: &str) -> Option<(i32, i32)> {
    let (x, n1) = scan_int(s)?;
    let rest = &s[n1..];
    let rest = rest.strip_prefix(',')?;
    let (y, n2) = scan_int(rest)?;
    if n2 != rest.len() {
        return None;
    }
    Some((x as i32, y as i32))
}
