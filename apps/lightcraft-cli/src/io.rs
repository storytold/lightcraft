//! The two streams a command writes to, so `run` can be called with any writers and a test reads
//! what a command printed.

use std::io::Write;

/// Standard output and standard error of one run.
pub struct Io<'a> {
    pub out: &'a mut dyn Write,
    pub err: &'a mut dyn Write,
}

impl Io<'_> {
    /// A line on stdout. A closed pipe (`| head`) is not an error worth failing over.
    pub fn line(&mut self, s: &str) {
        let _ = self.out.write_all(s.as_bytes()).and_then(|()| self.out.write_all(b"\n")).and_then(|()| self.out.flush());
    }
}

/// `println!` to the run's stdout.
macro_rules! outln {
    ($io:expr) => {{ $io.line("") }};
    ($io:expr, $($arg:tt)*) => {{ $io.line(&format!($($arg)*)) }};
}

/// `eprintln!` to the run's stderr.
macro_rules! errln {
    ($io:expr, $($arg:tt)*) => {{ let _ = writeln!($io.err, $($arg)*); }};
}
