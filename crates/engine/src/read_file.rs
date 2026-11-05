//! Reads of files named by a user, a sidecar or a library: the OPENED file must be a regular file within a byte
//! cap, and no more than the cap is ever read. (A size taken from the pathname, then a second open and a read to EOF,
//! lets a link, a FIFO, a device or a file that grows in between turn into an unbounded read.)

use std::io::{self, Read};
use std::path::Path;

/// Most bytes of a photo, video or other media file read into memory.
pub(crate) const MAX_MEDIA_BYTES: u64 = 8 << 30;

/// The whole of the regular file at `path`, at most `max` bytes.
pub(crate) fn read_regular(path: impl AsRef<Path>, max: u64) -> io::Result<Vec<u8>> {
    read_open(std::fs::File::open(path)?, max)
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
}
