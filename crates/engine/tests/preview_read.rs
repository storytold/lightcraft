//! Opt-in test of the embedded-preview read path on a **real file the user has** — typically one on a
//! network share, which is where reading only the header instead of the whole raw matters:
//!
//!   LC_PREVIEW_RAW="/run/user/1000/gvfs/smb-share:.../DSCF0001.RAF" \
//!     cargo test --release -p lightcraft-engine --test preview_read -- --ignored --nocapture
//!
//! It reports what the preview cost and fails when the read is not the small part of the file the
//! header path promises. Skipped without `LC_PREVIEW_RAW` (there is no committed raw fixture: media
//! is never committed, see AGENTS.md).
use std::time::Instant;

#[test]
#[ignore = "needs LC_PREVIEW_RAW pointing at a real raw file"]
fn a_preview_reads_only_the_header_and_the_preview() {
    let Ok(path) = std::env::var("LC_PREVIEW_RAW") else {
        eprintln!("skipped: set LC_PREVIEW_RAW=<path to a raw file>");
        return;
    };
    let file_len = std::fs::metadata(&path).expect("the file exists").len();
    assert!(file_len > 0, "empty file");

    let started = Instant::now();
    let window =
        lightcraft_engine::files::preview_window(&path).expect("the file reads").expect("it carries an embedded preview the header can point at");
    let elapsed = started.elapsed();
    let (_, preview, read) = window;

    let frac = read.fraction();
    println!(
        "preview of {path}\n  read {} of {} bytes ({:.1} %) in {:.2} s{}",
        read.read,
        read.total,
        frac * 100.0,
        elapsed.as_secs_f64(),
        if read.header_path { " [header path]" } else { " [whole file]" }
    );

    assert!(read.header_path, "the header path must answer for a raw whose pointer is in its header");
    assert_eq!(read.total, file_len, "the report covers the whole file");
    assert!(frac < 0.5, "a preview must not cost most of the file: read {} of {} bytes", read.read, read.total);
    assert!(preview.starts_with(&[0xff, 0xd8]), "the preview bytes are a JPEG: {:?}", preview.get(..4));
}
