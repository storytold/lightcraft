//! A file as a random-access byte source for the demuxers (no whole-file read: clips are large).

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{Mutex, PoisonError};

pub struct FileSource {
    file: Mutex<File>,
    len: u64,
}

impl FileSource {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        Ok(Self { file: Mutex::new(file), len })
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    /// Fill `buf` from `offset`; reading past the end is an `UnexpectedEof` error.
    pub fn read(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let end = offset.checked_add(buf.len() as u64).ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
        if end > self.len {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        let mut f = self.file.lock().unwrap_or_else(PoisonError::into_inner);
        f.seek(SeekFrom::Start(offset))?;
        f.read_exact(buf)
    }
}

/// Every FilmCraft demuxer has its own (identical) byte-source trait.
macro_rules! byte_source {
    ($($t:path),*) => {$(
        impl $t for FileSource {
            fn len(&self) -> u64 {
                self.len
            }
            fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
                self.read(offset, buf)
            }
        }
    )*};
}

byte_source!(filmcraft_isobmff::ByteSource, filmcraft_matroska::ByteSource, filmcraft_mpegts::ByteSource, filmcraft_mxf::ByteSource);
