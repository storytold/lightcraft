//! Reading a whole file the user or a preset named, with a size cap that holds (issue #2570).
//!
//! `metadata().len()` then `fs::read` is not a cap: a FIFO, a device or a `/proc` file reports
//! 0 bytes, so opening a FIFO hangs the caller and reading `/dev/zero` grows until the process
//! aborts. [`read_bounded`] only opens regular files (a link to one is fine) and reads at most
//! `max + 1` bytes, so a file that grew after the check is refused too.

use std::io::Read;
use std::path::Path;

/// Why [`read_bounded`] refused a file.
#[derive(Debug)]
pub enum ReadError {
    /// The file can't be found, opened or read.
    Io(std::io::Error),
    /// A folder, FIFO, device or socket, not a regular file.
    NotAFile,
    /// Larger than the cap; the size is the length the file reports.
    TooLarge(u64),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Io(e) => write!(f, "{e}"),
            ReadError::NotAFile => f.write_str("not a regular file"),
            ReadError::TooLarge(n) => write!(f, "{n} bytes is too large"),
        }
    }
}

/// The bytes of the regular file at `path`, at most `max` of them.
pub fn read_bounded(path: &Path, max: u64) -> Result<Vec<u8>, ReadError> {
    let meta = std::fs::metadata(path).map_err(ReadError::Io)?;
    if !meta.is_file() {
        return Err(ReadError::NotAFile);
    }
    if meta.len() > max {
        return Err(ReadError::TooLarge(meta.len()));
    }
    let file = std::fs::File::open(path).map_err(ReadError::Io)?;
    let mut out = Vec::new();
    file.take(max.saturating_add(1)).read_to_end(&mut out).map_err(ReadError::Io)?;
    if out.len() as u64 > max {
        return Err(ReadError::TooLarge(out.len() as u64));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("lc-bounded-read-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn reads_a_small_file_and_refuses_a_large_one() {
        let dir = dir("small");
        let path = dir.join("a.bin");
        std::fs::write(&path, [7u8; 10]).unwrap();
        assert_eq!(read_bounded(&path, 10).unwrap(), vec![7u8; 10]);
        assert!(matches!(read_bounded(&path, 9), Err(ReadError::TooLarge(10))));
        assert!(matches!(read_bounded(&dir, 10), Err(ReadError::NotAFile)));
        assert!(matches!(read_bounded(&dir.join("missing"), 10), Err(ReadError::Io(_))));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A FIFO reports 0 bytes and opening it waits for a writer: it must be refused, not opened.
    #[cfg(unix)]
    #[test]
    fn a_fifo_is_refused_without_waiting() {
        let dir = dir("fifo");
        let fifo = dir.join("preset.xmp");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status().unwrap();
        assert!(made.success());
        let (tx, rx) = std::sync::mpsc::channel();
        let path = fifo.clone();
        std::thread::spawn(move || {
            let _ = tx.send(read_bounded(&path, 1024).map(|b| b.len()));
        });
        let got = rx.recv_timeout(std::time::Duration::from_secs(5)).expect("reading a FIFO must not hang");
        assert!(matches!(got, Err(ReadError::NotAFile)), "{got:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A character device reports 0 bytes but never ends: it must be refused, not read to the end.
    #[cfg(unix)]
    #[test]
    fn a_device_is_refused() {
        assert!(matches!(read_bounded(Path::new("/dev/zero"), 1024), Err(ReadError::NotAFile)));
    }
}
