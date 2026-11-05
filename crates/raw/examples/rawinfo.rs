//! `cargo run --release -p lightcraft-raw --example rawinfo -- [--tree] FILE...`
//!
//! Prints the TIFF structure (with `--tree`), decode result, timings (MP/s) and basic statistics.
//! Exits unsuccessfully if any input cannot be read, decoded or developed, while inspecting the remaining files.

use lightcraft_raw::{Method, RawData, decode, embedded_preview, probe};
use lightcraft_tiff::{Ifd, Tiff, Value};
use std::process::ExitCode;
use std::time::Instant;

fn short(v: &Value) -> String {
    let s = match v {
        Value::Ascii(s) => format!("{s:?}"),
        Value::Byte(b) | Value::Undefined(b) => format!("{} bytes {:02x?}", b.len(), &b[..b.len().min(12)]),
        other => {
            let f = other.to_f64_vec();
            format!("{:?}{}", &f[..f.len().min(12)], if f.len() > 12 { format!(" …({})", f.len()) } else { String::new() })
        }
    };
    s.chars().take(160).collect()
}

fn dump(ifd: &Ifd, depth: usize, label: &str) {
    let pad = "  ".repeat(depth);
    println!("{pad}{label} @{} ({} entries)", ifd.offset, ifd.entries.len());
    for e in &ifd.entries {
        println!("{pad}  {:5} 0x{:04x} {:?}: {}", e.tag, e.tag, e.field_type(), short(&e.value));
    }
    for (i, s) in ifd.sub_ifds.iter().enumerate() {
        dump(s, depth + 1, &format!("SubIFD[{i}]"));
    }
    for (n, c) in [("Exif", &ifd.exif), ("GPS", &ifd.gps), ("Interop", &ifd.interop)] {
        if let Some(c) = c {
            dump(c, depth + 1, n);
        }
    }
}

fn inspect_files(args: &[String]) -> usize {
    let tree = args.iter().any(|a| a == "--tree");
    let paths: Vec<_> = args.iter().filter(|a| !a.starts_with("--")).collect();
    if paths.is_empty() {
        eprintln!("usage: rawinfo [--tree] FILE...");
        return 1;
    }
    let mut failures = 0;
    for path in paths {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) => {
                eprintln!("{path}: read error: {e}");
                failures += 1;
                continue;
            }
        };
        println!("== {path} ({:.1} MB) format {:?}", bytes.len() as f64 / 1e6, probe(&bytes));
        if tree && let Ok(t) = Tiff::parse(&bytes) {
            for (i, ifd) in t.ifds.iter().enumerate() {
                dump(ifd, 0, &format!("IFD{i}"));
            }
        }
        let t0 = Instant::now();
        match decode(&bytes) {
            Ok(r) => {
                let dt = t0.elapsed().as_secs_f64();
                let mp = (r.width * r.height) as f64 / 1e6;
                let (mn, mx, mean) = match &r.data {
                    RawData::U16(v) => {
                        let mn = *v.iter().min().unwrap_or(&0) as f64;
                        let mx = *v.iter().max().unwrap_or(&0) as f64;
                        (mn, mx, v.iter().map(|&x| x as f64).sum::<f64>() / v.len() as f64)
                    }
                    RawData::F32(v) => (
                        v.iter().cloned().fold(f32::MAX, f32::min) as f64,
                        v.iter().cloned().fold(f32::MIN, f32::max) as f64,
                        v.iter().map(|&x| x as f64).sum::<f64>() / v.len() as f64,
                    ),
                };
                println!(
                    "  {}x{} cpp {} bits {} cfa {:?} black {:.1} white {:?} active {:?} crop {:?} orient {:?}",
                    r.width,
                    r.height,
                    r.cpp,
                    r.bits,
                    r.cfa.as_ref().map(|c| c.name()),
                    r.black.mean(),
                    r.white,
                    r.active_area,
                    r.crop,
                    r.orientation
                );
                println!(
                    "  data min {mn} max {mx} mean {mean:.1}; wb {:?}; opcodes {}/{}/{}",
                    r.wb_multipliers,
                    r.opcodes.list1.len(),
                    r.opcodes.list2.len(),
                    r.opcodes.list3.len()
                );
                println!("  decode {:.0} ms ({:.0} MP/s)", dt * 1e3, mp / dt);
                println!(
                    "  meta: {:?} {:?} lens {:?} iso {:?} f {:?} t {:?} date {:?}",
                    r.metadata.make,
                    r.metadata.model,
                    r.metadata.lens_model,
                    r.metadata.iso,
                    r.metadata.f_number,
                    r.metadata.exposure_display(),
                    r.metadata.capture_time.map(|d| d.to_iso())
                );
                let xy = lightcraft_raw::color::as_shot_white_xy(&r);
                let ct = lightcraft_raw::color::camera_transform(&r, xy);
                println!("  as-shot xy ({:.4}, {:.4}) wb {:?} fallback {}", xy.x, xy.y, ct.wb, ct.matrix_is_fallback);
                for m in [Method::Bilinear, Method::Ppg, Method::Ahd] {
                    let t1 = Instant::now();
                    match r.develop(m) {
                        Ok(img) => {
                            let dt = t1.elapsed().as_secs_f64();
                            let mut s = [0f64; 3];
                            for p in &img.data {
                                for c in 0..3 {
                                    s[c] += p[c] as f64;
                                }
                            }
                            let n = img.data.len() as f64;
                            println!(
                                "  develop {m:?}: {}x{} in {:.0} ms ({:.0} MP/s), mean rgb {:.3} {:.3} {:.3}",
                                img.width,
                                img.height,
                                dt * 1e3,
                                mp / dt,
                                s[0] / n,
                                s[1] / n,
                                s[2] / n
                            );
                        }
                        Err(e) => {
                            println!("  develop {m:?}: {e}");
                            failures += 1;
                        }
                    }
                }
            }
            Err(e) => {
                println!("  decode error: {e}");
                failures += 1;
            }
        }
        match embedded_preview(&bytes) {
            Some(p) => println!("  preview: {} bytes", p.len()),
            None => println!("  preview: none"),
        }
    }
    failures
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if inspect_files(&args) == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

#[cfg(test)]
mod tests {
    use super::inspect_files;
    use lightcraft_raw::{BlackLevel, Cfa, ColorData, DngWriteOptions, OpcodeLists, Orientation, RawData, RawFormat, RawImage, Rect, write_dng};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!("lightcraft-rawinfo-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            std::fs::create_dir(&path).expect("create test directory");
            Self(path)
        }

        fn path(&self, name: &str) -> String {
            self.0.join(name).to_string_lossy().into_owned()
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn unreadable_input_is_a_failure_without_panicking() {
        let dir = TempDir::new();
        assert_eq!(inspect_files(&[dir.path("absent.arw")]), 1);
    }

    #[test]
    fn invalid_inputs_are_counted_without_stopping_the_batch() {
        let dir = TempDir::new();
        let path = dir.path("invalid.arw");
        std::fs::write(&path, b"not a raw file").expect("write invalid fixture");
        assert_eq!(inspect_files(&[path.clone(), path]), 2);
    }

    #[test]
    fn input_paths_are_required() {
        assert_eq!(inspect_files(&[]), 1);
        assert_eq!(inspect_files(&["--tree".into()]), 1);
    }

    #[test]
    fn valid_generated_dng_succeeds_after_an_unreadable_input() {
        let dir = TempDir::new();
        let raw = RawImage {
            format: RawFormat::Dng,
            width: 16,
            height: 16,
            cpp: 1,
            data: RawData::U16(vec![2048; 16 * 16]),
            cfa: Some(Cfa::bayer("RGGB").expect("valid CFA")),
            bits: 14,
            black: BlackLevel::uniform(0.0),
            white: vec![16383.0],
            active_area: Rect::new(0, 0, 16, 16),
            crop: Rect::new(0, 0, 16, 16),
            orientation: Orientation::Normal,
            color: ColorData::default(),
            wb_multipliers: Some([1.0; 3]),
            linearized: false,
            opcodes: OpcodeLists::default(),
            metadata: Default::default(),
            lens_metadata: Default::default(),
            pixel_aspect: 1.0,
        };
        let path = dir.path("valid.dng");
        std::fs::write(&path, write_dng(&raw, &DngWriteOptions::default()).expect("encode generated fixture")).expect("write fixture");
        assert_eq!(inspect_files(std::slice::from_ref(&path)), 0);
        assert_eq!(inspect_files(&[dir.path("absent.arw"), path]), 1);
    }
}
