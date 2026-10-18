//! Query a borrowed Windows file handle without opening, reading or owning it.
#![deny(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

/// The volume serial number and complete 128-bit file ID of an open handle.
/// Unsupported providers and OS failures retain their original `io::Error`; there is no legacy-ID fallback.
/// The borrowed file remains open and its cursor is unchanged.
pub fn identity(file: &std::fs::File) -> std::io::Result<(u64, [u8; 16])> {
    #[cfg(windows)]
    {
        windows::identity(file)
    }
    #[cfg(not(windows))]
    {
        let _ = file;
        Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "Windows FileIdInfo is unavailable on this platform"))
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod windows {
    use std::fs::File;
    use std::io;
    use std::mem::{MaybeUninit, size_of};
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{FILE_ID_INFO, FileIdInfo, GetFileInformationByHandleEx};

    pub(super) fn identity(file: &File) -> io::Result<(u64, [u8; 16])> {
        let mut info = MaybeUninit::<FILE_ID_INFO>::uninit();
        let size = u32::try_from(size_of::<FILE_ID_INFO>()).map_err(|_| io::Error::other("FILE_ID_INFO exceeds the OS buffer size"))?;
        // SAFETY: the borrowed File keeps its HANDLE live for this synchronous documented call.
        // The binding's aligned FILE_ID_INFO storage and exact size match FileIdInfo. Windows
        // initializes that entire output on nonzero success; failure returns before it is read.
        // No handle ownership, cursor, file bytes or caller memory beyond this output is changed.
        let info = unsafe {
            if GetFileInformationByHandleEx(file.as_raw_handle(), FileIdInfo, info.as_mut_ptr().cast(), size) == 0 {
                return Err(io::Error::last_os_error());
            }
            info.assume_init()
        };
        Ok((info.VolumeSerialNumber, info.FileId.Identifier))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Seek, SeekFrom, Write};
    #[test]
    fn borrowed_query_preserves_file_and_cursor() {
        let dir = std::env::temp_dir().join(format!("lc-file-id-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("source");
        let mut file = std::fs::OpenOptions::new().read(true).write(true).create_new(true).open(&path).unwrap();
        file.write_all(b"identity").unwrap();
        file.seek(SeekFrom::Start(2)).unwrap();
        let result = identity(&file);
        #[cfg(windows)]
        assert!(result.is_ok(), "{result:?}");
        #[cfg(not(windows))]
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::Unsupported);
        assert_eq!(file.stream_position().unwrap(), 2);
        let mut rest = String::new();
        file.read_to_string(&mut rest).unwrap();
        assert_eq!(rest, "entity");
        drop(file);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn full_identity_distinguishes_files_and_matches_hard_links() {
        let dir = std::env::temp_dir().join(format!("lc-full-file-id-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b, link) = (dir.join("a"), dir.join("b"), dir.join("link"));
        std::fs::write(&a, b"same").unwrap();
        std::fs::write(&b, b"same").unwrap();
        std::fs::hard_link(&a, &link).unwrap();
        let (a, b, link) = (std::fs::File::open(a).unwrap(), std::fs::File::open(b).unwrap(), std::fs::File::open(link).unwrap());
        assert_eq!(identity(&a).unwrap(), identity(&link).unwrap());
        assert_ne!(identity(&a).unwrap(), identity(&b).unwrap());
        assert!(identity(&std::fs::File::open("NUL").unwrap()).is_err());
        drop((a, b, link));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
