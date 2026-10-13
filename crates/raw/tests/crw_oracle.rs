//! Full-frame exact-match verification of the CRW decoder against reference TIFFs.
//!
//! Points `LIGHTCRAFT_CRW_ORACLE` at a directory of `<name>.crw` files with `<name>.crw.tiff`
//! references (16-bit grey, every sensor sample, as written by LibRaw's `unprocessed_raw -T`;
//! see `clean-room/crw/07-verification.md`). Skips cleanly when the variable is unset — the
//! reference files are not distributable and live outside the repository.

use lightcraft_raw::{RawData, RawError, decode};
use lightcraft_tiff::Tiff;
use std::path::PathBuf;

fn oracle_samples(bytes: &[u8]) -> (usize, usize, Vec<u16>) {
    let t = Tiff::parse(bytes).expect("oracle TIFF parses");
    let ifd = t.ifds.first().expect("oracle TIFF has an image");
    let info = ifd.image().expect("oracle TIFF image info");
    assert_eq!(info.bits(), 16, "oracle TIFF is 16-bit");
    let (w, h) = (info.width as usize, info.height as usize);
    let mut data = Vec::with_capacity(w * h);
    for c in info.chunks(bytes.len() as u64) {
        let src = lightcraft_tiff::image::chunk_bytes(bytes, &c).expect("oracle strip in file");
        data.extend(src.as_chunks::<2>().0.iter().map(|p| u16::from_le_bytes(*p)));
    }
    data.truncate(w * h);
    (w, h, data)
}

#[test]
fn crw_decodes_match_the_oracle() {
    let Some(dir) = std::env::var_os("LIGHTCRAFT_CRW_ORACLE").map(PathBuf::from) else {
        eprintln!("LIGHTCRAFT_CRW_ORACLE not set; skipping");
        return;
    };
    let mut checked = 0;
    let mut entries: Vec<_> = std::fs::read_dir(&dir).expect("oracle directory").filter_map(|e| e.ok()).map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        if path.extension().and_then(|e| e.to_str()).is_none_or(|e| !e.eq_ignore_ascii_case("crw")) {
            continue;
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
        let bytes = std::fs::read(&path).expect("read crw");
        let tiff_path = path.with_extension("crw.tiff");
        let img = match decode(&bytes) {
            Ok(img) => img,
            Err(RawError::Unsupported(w)) if w.contains("PowerShot G5") => {
                eprintln!("{name}: G5 refused as documented");
                continue;
            }
            Err(e) => panic!("{name}: {e}"),
        };
        let reference = std::fs::read(&tiff_path).expect("read oracle tiff");
        let (w, h, oracle) = oracle_samples(&reference);
        assert_eq!((img.width, img.height), (w, h), "{name}: dimensions");
        let RawData::U16(samples) = &img.data else { panic!("{name}: not U16") };
        let mismatches = samples.iter().zip(oracle.iter()).filter(|(a, b)| a != b).count();
        assert_eq!(mismatches, 0, "{name}: {mismatches} mismatching samples");
        eprintln!("{name}: EXACT ({w}x{h})");
        checked += 1;
    }
    assert!(checked > 0, "no .crw files found in {}", dir.display());
}
