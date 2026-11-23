//! Reads of files named by a user, a sidecar or a library: the OPENED file must be a regular file within a byte
//! cap, and no more than the cap is ever read. (A size taken from the pathname, then a second open and a read to EOF,
//! lets a link, a FIFO, a device or a file that grows in between turn into an unbounded read.)

use std::io::{self, Read};
use std::path::Path;

/// Most bytes of a photo, video or other media file read into memory.
pub(crate) const MAX_MEDIA_BYTES: u64 = 8 << 30;

/// `O_NONBLOCK` of the platforms that have a FIFO to wait on (std has no name for it, and libc is a
/// dev-dependency only); 0 elsewhere, where a FIFO open still waits for a writer.
#[cfg(unix)]
const O_NONBLOCK: i32 = if cfg!(all(
    any(target_os = "linux", target_os = "android"),
    not(any(
        target_arch = "mips",
        target_arch = "mips32r6",
        target_arch = "mips64",
        target_arch = "mips64r6",
        target_arch = "sparc",
        target_arch = "sparc64"
    ))
)) {
    0x800
} else if cfg!(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly"
)) {
    0x4
} else {
    0
};

/// Opens `path` for reading without waiting for a FIFO's writer (a FIFO is refused by [`read_open`], not waited on). A
/// regular file is unaffected by the flag.
pub(crate) fn open_for_read(path: impl AsRef<Path>) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, O_NONBLOCK);
    options.open(path)
}

/// The whole of the regular file at `path`, at most `max` bytes.
pub(crate) fn read_regular(path: impl AsRef<Path>, max: u64) -> io::Result<Vec<u8>> {
    read_open(open_for_read(path)?, max)
}

/// The whole of an already opened regular file, at most `max` bytes.
pub(crate) fn read_open(file: std::fs::File, max: u64) -> io::Result<Vec<u8>> {
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a regular file"));
    }
    if meta.len() > max {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("{} bytes is more than the {max} allowed", meta.len())));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
    file.take(max.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("more than the {max} bytes allowed")));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_regular_file_within_the_cap_is_read_and_others_are_refused() {
        let dir = std::env::temp_dir().join(format!("read-file-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let f = dir.join("a");
        std::fs::write(&f, b"abcd").unwrap();
        assert_eq!(read_regular(&f, 4).unwrap(), b"abcd");
        assert!(read_regular(&f, 3).is_err());
        assert!(read_regular(&dir, 100).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/dev/zero", dir.join("z")).unwrap();
            assert!(read_regular(dir.join("z"), 1 << 20).is_err());
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A FIFO with no writer is refused, not waited on forever in `open`.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_fifo_without_a_writer_is_refused_without_blocking() {
        let dir = std::env::temp_dir().join(format!("read-file-fifo-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let fifo = dir.join("pipe");
        // (no unsafe code in this crate: the coreutils tool makes the FIFO)
        if !std::process::Command::new("mkfifo").arg(&fifo).status().is_ok_and(|s| s.success()) {
            eprintln!("skipped: no mkfifo");
            return;
        }
        let err = read_regular(&fifo, 1 << 20).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
