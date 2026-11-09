//! The header probe reads a bounded part of a raw and describes it as the full probe does, minus the content hash.

use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use super::{Arc, FileSource, Opener, full_probe, header_probe, os_opener};

/// The file system's opener, counting the bytes read through the file layer into `count`.
pub(crate) fn counting_opener(count: Arc<AtomicU64>) -> Opener {
    let inner = os_opener();
    Arc::new(move |path: &str| {
        let src = inner(path)?;
        Ok(Box::new(Counting { src, count: count.clone() }) as Box<dyn FileSource>)
    })
}

struct Counting {
    src: Box<dyn FileSource>,
    count: Arc<AtomicU64>,
}

impl Read for Counting {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.src.read(buf)?;
        self.count.fetch_add(n as u64, Relaxed);
        Ok(n)
    }
}

impl Seek for Counting {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        self.src.seek(to)
    }
}

impl FileSource for Counting {
    fn size(&self) -> u64 {
        self.src.size()
    }

    fn modified(&self) -> Option<String> {
        self.src.modified()
    }
}

/// An uncompressed Bayer DNG with about `pad` bytes of sensor data (rows of 2,048 bytes), the samples and the capture
/// time derived from `seed`: two seeds are two files of one size with headers that differ in the time of the shot.
pub(crate) fn padded_dng(seed: u8, pad: usize) -> Vec<u8> {
    let (w, h) = (1024usize, (pad / 2048).max(4));
    let mut x = u32::from(seed).wrapping_mul(2_654_435_761).wrapping_add(1);
    let data: Vec<u16> = (0..w * h)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            256 + (x >> 8) as u16 % 15000
        })
        .collect();
    let capture = lightcraft_meta::DateTime { year: 2026, month: 1, day: 1, second: seed % 60, minute: seed / 60, ..Default::default() };
    let metadata = lightcraft_meta::Metadata { capture_time: Some(capture), ..Default::default() };
    let tiny = crate::tests_xmp::synthetic_dng_of(w, h, "RGGB", data, None, metadata);
    let raw = lightcraft_raw::decode(&tiny).unwrap();
    lightcraft_raw::write_dng(
        &raw,
        &lightcraft_raw::DngWriteOptions { compression: lightcraft_raw::DngCompression::Uncompressed, ..Default::default() },
    )
    .unwrap()
}

fn write(name: &str, bytes: &[u8]) -> String {
    let dir = std::env::temp_dir().join(format!("lc-header-probe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path.to_string_lossy().to_string()
}

#[test]
fn a_raw_is_described_from_a_bounded_part_of_it_without_a_hash() {
    let bytes = padded_dng(1, 8 << 20);
    let path = write("big.dng", &bytes);
    let (headers, whole) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
    let by_headers = header_probe(counting_opener(headers.clone()))(&path).unwrap();
    let in_full = full_probe(counting_opener(whole.clone()))(&path).unwrap();
    assert_eq!(whole.load(Relaxed), bytes.len() as u64, "the full probe reads the file");
    assert!(headers.load(Relaxed) < bytes.len() as u64 / 10, "read {} bytes of a {}-byte file", headers.load(Relaxed), bytes.len());
    assert_eq!(by_headers.content_hash, None);
    assert_eq!(in_full.content_hash, Some(lightcraft_preview::hash_bytes(&bytes).to_string()));
    for (a, b) in [(by_headers.width, in_full.width), (by_headers.height, in_full.height), (by_headers.file_size as u32, in_full.file_size as u32)] {
        assert_eq!(a, b);
    }
    assert_eq!((&by_headers.format, &by_headers.captured, &by_headers.file_modified), (&in_full.format, &in_full.captured, &in_full.file_modified));
    assert_eq!(by_headers.file_size, bytes.len() as u64);
}

#[test]
fn a_file_no_longer_than_the_prefix_is_read_whole_and_hashed() {
    let bytes = padded_dng(2, 1000);
    let path = write("small.dng", &bytes);
    let count = Arc::new(AtomicU64::new(0));
    let info = header_probe(counting_opener(count.clone()))(&path).unwrap();
    assert_eq!(count.load(Relaxed), bytes.len() as u64);
    assert_eq!(info.content_hash, Some(lightcraft_preview::hash_bytes(&bytes).to_string()));
}

#[test]
fn a_file_that_is_not_a_raw_is_read_whole_and_hashed() {
    let img = lightcraft_raster::Rgba8 { width: 4, height: 4, data: vec![[9, 8, 7, 255]; 16] };
    let mut bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    bytes.resize(1 << 20, 0); // (trailing bytes a PNG reader ignores: longer than the prefix)
    let path = write("pad.png", &bytes);
    let count = Arc::new(AtomicU64::new(0));
    let info = header_probe(counting_opener(count.clone()))(&path).unwrap();
    assert_eq!(count.load(Relaxed), bytes.len() as u64);
    assert_eq!(info.content_hash, Some(lightcraft_preview::hash_bytes(&bytes).to_string()));
}

#[test]
fn hostile_files_fail_cleanly() {
    let probe = header_probe(os_opener());
    // zero length
    let empty = write("empty.dng", &[]);
    assert!(probe(&empty).is_err());
    // a raw's header cut off after the prefix (a file that shrank): described or refused, never a panic
    let bytes = padded_dng(3, 4 << 20);
    let cut = write("cut.dng", bytes.get(..bytes.len() / 2).unwrap());
    let _ = probe(&cut);
    // a file gone
    assert!(probe("/nonexistent/lc-header-probe.dng").is_err());
    // a file whose listed size is longer than what is there: reads fail short
    struct Liar(Box<dyn FileSource>);
    impl Read for Liar {
        fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
            self.0.read(b)
        }
    }
    impl Seek for Liar {
        fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
            self.0.seek(to)
        }
    }
    impl FileSource for Liar {
        fn size(&self) -> u64 {
            self.0.size() * 2
        }
        fn modified(&self) -> Option<String> {
            None
        }
    }
    let inner = os_opener();
    let liar: Opener = Arc::new(move |p: &str| Ok(Box::new(Liar(inner(p)?)) as Box<dyn FileSource>));
    let _ = header_probe(liar.clone())(&write("liar.dng", &bytes));
    let _ = full_probe(liar)(&write("liar2.dng", &bytes));
}
