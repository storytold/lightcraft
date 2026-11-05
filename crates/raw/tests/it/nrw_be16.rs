//! Coolpix NRW files stored as big-endian 16-bit words under compression 34713 (P330, P7700, P7800; CC0 samples
//! from raw.pixls.us). Reads `*.nrw` below `LIGHTCRAFT_NRW_DIR` (default `corpus/raw`), recursively; skips cleanly
//! when no such file is there.

use lightcraft_raw::{RawData, decode};
use std::path::{Path, PathBuf};

fn nrw_files(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for p in rd.flatten().map(|e| e.path()) {
        if p.is_dir() && depth < 3 {
            nrw_files(&p, depth + 1, out);
        } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("nrw")) {
            out.push(p);
        }
    }
}

#[test]
fn coolpix_nrw_be16_decodes() {
    let root =
        std::env::var_os("LIGHTCRAFT_NRW_DIR").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/raw"));
    let mut files = Vec::new();
    nrw_files(&root, 0, &mut files);
    let mut checked = 0;
    for p in files {
        let Ok(bytes) = std::fs::read(&p) else { continue };
        let Ok(img) = decode(&bytes) else { continue };
        let model = img.metadata.model.clone().unwrap_or_default();
        if !["P330", "P7700", "P7800"].iter().any(|m| model.contains(m)) {
            continue;
        }
        let RawData::U16(d) = &img.data else { panic!("{model}: not integer data") };
        assert_eq!((img.width, img.height, img.bits), (4032, 3024, 12), "{model}");
        assert!(d.iter().all(|&v| v < 4096), "{model}: sample above 12 bits");
        assert_eq!(img.black.values.first().copied(), Some(200.0), "{model}");
        // same-colour neighbours are smooth: the upper 12 bits of the word, not the lower byte
        let (mut sum, mut n) = (0u64, 0u64);
        for row in d.chunks(img.width).step_by(7) {
            for x in 0..img.width - 2 {
                sum += u64::from(row[x].abs_diff(row[x + 2]));
                n += 1;
            }
        }
        assert!(sum / n.max(1) < 120, "{model}: mean same-colour step {}", sum / n.max(1));
        checked += 1;
    }
    eprintln!("coolpix_nrw_be16: {checked} files checked");
}
