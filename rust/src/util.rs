//! Console output, fatal errors and timing, mirroring the C++ `Output`, `die`
//! and `Timer` helpers. All messages go to stdout, as `printf` did.

use std::io::Write;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Instant;

static VERBOSITY: AtomicI32 = AtomicI32::new(1);

pub fn verbosity() -> i32 {
    VERBOSITY.load(Ordering::Relaxed)
}

pub fn adjust_verbosity(delta: i32) {
    VERBOSITY.fetch_add(delta, Ordering::Relaxed);
}

/// Print if `level <= verbosity`; always flush (so progress interleaves correctly).
pub fn output(level: i32, msg: &str) {
    let mut out = std::io::stdout().lock();
    if level <= verbosity() {
        let _ = out.write_all(msg.as_bytes());
    }
    let _ = out.flush();
}

#[macro_export]
macro_rules! out {
    ($level:expr, $($arg:tt)*) => { $crate::util::output($level, &format!($($arg)*)) };
}

/// Print an error and exit with status 1.
pub fn die_msg(msg: &str) -> ! {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{msg}");
    let _ = out.flush();
    std::process::exit(1);
}

#[macro_export]
macro_rules! die {
    ($($arg:tt)*) => { $crate::util::die_msg(&format!($($arg)*)) };
}

pub struct Timer(Instant);

impl Timer {
    pub fn start() -> Self {
        Timer(Instant::now())
    }
    pub fn restart(&mut self) {
        self.0 = Instant::now();
    }
    pub fn read(&self) -> f64 {
        self.0.elapsed().as_secs_f64()
    }
}

/// `sscanf("%d%n")`: optional whitespace and sign, then at least one digit.
/// Returns the value and the number of bytes consumed.
pub fn scan_int(s: &str) -> Option<(i64, usize)> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    let mut neg = false;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        neg = b[i] == b'-';
        i += 1;
    }
    let start = i;
    let mut v: i64 = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        v = v.saturating_mul(10).saturating_add((b[i] - b'0') as i64);
        i += 1;
    }
    if i == start {
        return None;
    }
    Some((if neg { -v } else { v }, i))
}

/// C `atoi`: leading integer or 0.
pub fn atoi(s: &str) -> i64 {
    scan_int(s).map(|(v, _)| v).unwrap_or(0)
}

/// Optional debug dumps of intermediate arrays (see rust/tools/dumpdiff.py).
pub fn dump(name: &str, bytes: &[u8], w: usize, h: usize, ext: &str) {
    if let Some(dir) = std::env::var_os("MB_DUMP_DIR") {
        let path = std::path::Path::new(&dir).join(format!("{name}_{w}x{h}.{ext}"));
        let _ = std::fs::write(path, bytes);
    }
}

pub fn dump_enabled() -> bool {
    std::env::var_os("MB_DUMP_DIR").is_some()
}

pub fn f32_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}
