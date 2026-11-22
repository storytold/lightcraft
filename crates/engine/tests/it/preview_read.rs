//! The embedded-preview read path reads the header and the preview, not the whole raw: the property that matters on
//! a network share. Always checked on a generated RAF (a header pointing at a small JPEG, then megabytes of sensor
//! data; no committed media); optionally also on a **real file the user has**:
//!
//!   LC_PREVIEW_RAW="/run/user/1000/gvfs/smb-share:.../DSCF0001.RAF" \
//!     cargo test --release -p lcc-engine --test it preview_read:: -- --nocapture
use std::time::Instant;

/// A small JPEG of a gradient.
fn jpeg(w: u32, h: u32) -> Vec<u8> {
    let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i * 7 % 251) as u8).collect();
    let img = lcc_codecs::EncodeImage { width: w, height: h, channels: 3, samples: lcc_codecs::Samples::U8(&rgb) };
    lcc_codecs::encode_jpeg(&img, 85, lcc_codecs::ChromaSubsampling::S420, &lcc_codecs::EncodeMeta::default()).unwrap()
}

/// A RAF whose preview pointer sits in its fixed header (offset at byte 84, length at 88, big-endian) followed by
/// 6 MB of sensor data the preview read must not touch.
fn generated_raw(dir: &lcc_testkit::TempDir) -> (String, Vec<u8>) {
    let preview = jpeg(160, 120);
    let head = 160usize;
    let mut f = vec![0u8; head];
    f[..16].copy_from_slice(b"FUJIFILMCCD-RAW ");
    f[84..88].copy_from_slice(&(head as u32).to_be_bytes());
    f[88..92].copy_from_slice(&(preview.len() as u32).to_be_bytes());
    f.extend_from_slice(&preview);
    f.extend((0..6_000_000u32).map(|i| (i * 131 % 251) as u8));
    let path = dir.path().join("generated.raf");
    std::fs::write(&path, f).unwrap();
    (path.to_string_lossy().into_owned(), preview)
}

/// Reads the preview of `path` and checks it cost a small part of the file; returns the preview bytes.
fn check_preview_read(path: &str) -> Vec<u8> {
    let file_len = std::fs::metadata(path).expect("the file exists").len();
    assert!(file_len > 0, "empty file");

    let started = Instant::now();
    let window =
        lcc_engine::files::preview_window(path, usize::MAX).expect("the file reads").expect("it carries an embedded preview the header can point at");
    let elapsed = started.elapsed();
    let (_, preview, read) = window;

    let frac = read.fraction();
    println!(
        "preview of {path}\n  read {} of {} bytes ({:.1} %) in {:.2} s{}",
        read.read,
        read.total,
        frac * 100.0,
        elapsed.as_secs_f64(),
        if read.planned {
            " [plan]"
        } else if read.header_path {
            " [header path]"
        } else {
            " [whole file]"
        }
    );

    assert!(read.header_path, "the header or the plan must answer for a raw whose preview is not the whole file");
    assert_eq!(read.total, file_len, "the report covers the whole file");
    assert!(frac < 0.5, "a preview must not cost most of the file: read {} of {} bytes", read.read, read.total);
    assert!(preview.starts_with(&[0xff, 0xd8]), "the preview bytes are a JPEG: {:?}", preview.get(..4));
    preview
}

#[test]
fn a_generated_raws_preview_reads_only_the_header_and_the_preview() {
    let dir = lcc_testkit::TempDir::new("preview-read");
    let (path, want) = generated_raw(&dir);
    let got = check_preview_read(&path);
    assert_eq!(got, want, "the preview is the embedded JPEG, byte for byte");
}

#[test]
fn a_real_files_preview_reads_only_the_header_and_the_preview() {
    let Ok(path) = std::env::var("LC_PREVIEW_RAW") else {
        lcc_testkit::skip!(Input, "set LC_PREVIEW_RAW=<path to a raw file> to also check a real raw");
        return;
    };
    check_preview_read(&path);
}
