use std::io::{BufWriter, Stdout, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

/// Process-global buffered stdout (64 KiB): every `outln!`/`bytes` call
/// shares one writer, so a line-per-record hot path costs one write
/// syscall per flush instead of one per line.
static STDOUT: OnceLock<Mutex<BufWriter<Stdout>>> = OnceLock::new();

fn stdout_writer() -> &'static Mutex<BufWriter<Stdout>> {
    STDOUT.get_or_init(|| Mutex::new(BufWriter::with_capacity(64 * 1024, std::io::stdout())))
}

/// A downstream reader that closed the pipe early (`| head`, `| grep -m1`,
/// quitting `less`) is a clean exit, not an error. Any other write
/// failure exits 2 — the "findings" code 1 is reserved for results.
fn exit_on_write_error(e: std::io::Error) -> ! {
    if e.kind() == std::io::ErrorKind::BrokenPipe {
        std::process::exit(0);
    }
    eprintln!("error: failed to write to stdout: {e}");
    std::process::exit(2);
}

/// Write one formatted line to stdout (buffered; see [`flush`]).
pub fn line(args: std::fmt::Arguments<'_>) {
    let mut out = stdout_writer().lock().unwrap_or_else(|p| p.into_inner());
    if let Err(e) = out.write_fmt(args).and_then(|()| out.write_all(b"\n")) {
        exit_on_write_error(e);
    }
}

/// Write a whole buffer to stdout with the same semantics as [`line`].
pub fn bytes(buf: &[u8]) {
    let mut out = stdout_writer().lock().unwrap_or_else(|p| p.into_inner());
    if let Err(e) = out.write_all(buf) {
        exit_on_write_error(e);
    }
}

/// Flush the buffered stdout. Call before process exit, and before
/// writing a final error line to stderr so stdout output is not lost.
pub fn flush() {
    let mut out = stdout_writer().lock().unwrap_or_else(|p| p.into_inner());
    if let Err(e) = out.flush() {
        exit_on_write_error(e);
    }
}

macro_rules! outln {
    () => {
        $crate::output::line(format_args!(""))
    };
    ($($arg:tt)*) => {
        $crate::output::line(format_args!($($arg)*))
    };
}

pub(crate) use outln;

/// `-q/--quiet`: suppresses non-error stderr diagnostics (`diagln!`)
/// only — real errors still print, and stdout is never touched.
static QUIET: AtomicBool = AtomicBool::new(false);

pub fn set_quiet(q: bool) {
    QUIET.store(q, Ordering::Relaxed);
}

pub fn quiet() -> bool {
    QUIET.load(Ordering::Relaxed)
}

/// Diagnostic stderr line — same as `eprintln!` but a no-op under
/// `-q/--quiet`. For advisory output (scan warnings, filter notes,
/// impact hints), never for errors.
macro_rules! diagln {
    ($($arg:tt)*) => {
        if !$crate::output::quiet() {
            eprintln!($($arg)*);
        }
    };
}

pub(crate) use diagln;
