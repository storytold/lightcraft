//! [`lightcraft_raw::sparse::header_plan`] over `corpus/raw/**` (git-ignored CC0 samples from
//! raw.pixls.us, fetched with `cargo xtask corpus --download`; `LIGHTCRAFT_CORPUS` overrides the
//! corpus root). Skips cleanly when absent.
//!
//! For every file the plan reaches `Ready` on, probing the partly loaded buffer must give exactly
//! what probing the whole file gives — with the skipped bytes zero and again with them 0xFF, so a
//! reader that looked at skipped data would be caught. CR3 and DNG files must be described from a
//! small part of the file in a few reads (`cargo test -p lightcraft-raw --release --test sparse_corpus -- --nocapture`
//! prints what each format reads).

use lightcraft_raw::sparse::{HeaderPlan, READ_GAP, READ_MIN, Ranges, header_plan, preview_plan, reads};
use lightcraft_raw::{embedded_preview, probe, probe_info};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn corpus_root() -> PathBuf {
    std::env::var_os("LIGHTCRAFT_CORPUS").map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus"))
}

/// What reading a file in part took: rounds of the plan and reads issued.
#[derive(Clone, Copy, Default)]
struct Cost {
    rounds: u64,
    reads: u64,
}

/// Run the plan (with the embedded preview when `preview`), loading from `file` what it asks for
/// in the reads the engine makes of it ([`reads`]) into a buffer whose other bytes are `fill`.
fn run(file: &[u8], fill: u8, preview: bool) -> (HeaderPlan, Vec<u8>, Ranges, Cost) {
    let mut buf = vec![fill; file.len()];
    let mut loaded = Ranges::new();
    let prefix = file.len().min(512 * 1024);
    buf[..prefix].copy_from_slice(&file[..prefix]);
    loaded.insert(0, prefix as u64);
    let mut cost = Cost { rounds: 0, reads: 1 };
    for _ in 0..32 {
        match if preview { preview_plan(&buf, &loaded) } else { header_plan(&buf, &loaded) } {
            HeaderPlan::Need(r) => {
                cost.rounds += 1;
                for &(a, b) in r.spans() {
                    assert!(!loaded.overlaps(a, b), "asked again for {a}..{b}");
                }
                for &(a, b) in reads(&r, &loaded, file.len() as u64, READ_MIN, READ_GAP).spans() {
                    buf[a as usize..b as usize].copy_from_slice(&file[a as usize..b as usize]);
                    loaded.insert(a, b);
                    cost.reads += 1;
                }
            }
            done => return (done, buf, loaded, cost),
        }
    }
    panic!("no answer after 32 rounds");
}

/// What an import learns from a file: the raw description and the metadata.
fn describe(bytes: &[u8]) -> String {
    let e = lightcraft_meta::embedded(bytes);
    format!("{:?}\n{:?}\n{:?}", probe_info(bytes), lightcraft_meta::extract(bytes), e.xmp)
}

#[test]
fn partial_reads_describe_raws_as_whole_files_do() {
    let dir = corpus_root().join("raw");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(corpus_root().join("leica"))
        .into_iter()
        .chain(std::fs::read_dir(&dir))
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && !p.extension().is_some_and(|e| e == "part" || e == "txt" || e == "md"))
        .collect();
    if paths.is_empty() {
        lightcraft_testkit::skip!(Corpus, "{} absent", dir.display());
        return;
    }
    paths.sort();
    // format → (files ready, bytes read, bytes total, files read whole, rounds, reads)
    let mut stats: BTreeMap<String, (usize, u64, u64, usize, u64, u64)> = BTreeMap::new();
    let mut preview_stats: BTreeMap<String, (usize, u64, u64, usize, u64, u64)> = BTreeMap::new();
    for p in paths {
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        let file = std::fs::read(&p).unwrap();
        let format = probe(&file).map_or("?".to_string(), |f| format!("{f:?}"));
        let whole = describe(&file);
        let preview = embedded_preview(&file);
        let entry = stats.entry(format.clone()).or_default();
        let header_ready = match run(&file, 0, false) {
            (HeaderPlan::Ready, zeros, loaded, cost) => {
                let (plan_ff, ones, loaded_ff, _) = run(&file, 0xff, false);
                assert_eq!((plan_ff, &loaded_ff), (HeaderPlan::Ready, &loaded), "{name}: the plan read skipped bytes");
                assert_eq!(describe(&zeros), whole, "{name}: differs from the whole file (skipped bytes 0)");
                assert_eq!(describe(&ones), whole, "{name}: differs from the whole file (skipped bytes 0xFF)");
                entry.0 += 1;
                entry.1 += loaded.total();
                entry.2 += file.len() as u64;
                entry.4 += cost.rounds;
                entry.5 += cost.reads;
                if loaded.total() * 4 > file.len() as u64 {
                    eprintln!("{name}: read {:.0} %", loaded.total() as f64 * 100.0 / file.len() as f64);
                }
                if matches!(format.as_str(), "Cr3" | "Dng") && file.len() > 8 << 20 {
                    assert!(loaded.total() * 4 < file.len() as u64, "{name}: read {} of {} bytes", loaded.total(), file.len());
                }
                // each read is a round trip on a network share: a tiled DNG once took 240 (#832)
                assert!(cost.reads <= 8, "{name}: {} reads", cost.reads);
                true
            }
            (HeaderPlan::Whole(why), ..) => {
                entry.3 += 1;
                eprintln!("{name}: whole ({why})");
                false
            }
            (HeaderPlan::Need(_), ..) => unreachable!(),
        };
        // The preview plan (what the display path reads) is asked for **every** file, because a
        // header probe that reads samples (a NEF, an ORF, a PEF) makes the plan above `Whole` and
        // would otherwise hide its preview from this check. Ready means the preview handed on is the
        // whole file's, byte for byte, with the skipped bytes zeroed and again 0xFF; `Whole` is a
        // legitimate answer here (that format keeps reading whole for a preview too) and is counted.
        let pentry = preview_stats.entry(format.clone()).or_default();
        match run(&file, 0, true) {
            (HeaderPlan::Ready, buf, loaded_p, cost) => {
                if header_ready {
                    assert_eq!(describe(&buf), whole, "{name}: preview plan differs from the whole file");
                }
                assert!(embedded_preview(&buf) == preview, "{name}: a different embedded preview");
                let (plan_ff, buf_ff, loaded_ff, _) = run(&file, 0xff, true);
                assert_eq!((plan_ff, loaded_ff.total()), (HeaderPlan::Ready, loaded_p.total()), "{name}: the preview plan read skipped bytes");
                assert!(embedded_preview(&buf_ff) == preview, "{name}: a different embedded preview (skipped bytes 0xFF)");
                pentry.0 += 1;
                pentry.1 += loaded_p.total();
                pentry.2 += file.len() as u64;
                pentry.4 += cost.rounds;
                pentry.5 += cost.reads;
            }
            (HeaderPlan::Whole(why), ..) => {
                pentry.3 += 1;
                eprintln!("{name}: preview whole ({why})");
            }
            (HeaderPlan::Need(_), ..) => unreachable!(),
        }
    }
    let per = |n: u64, files: usize| n as f64 / files.max(1) as f64;
    for (format, (ready, read, total, whole, rounds, reads)) in stats {
        eprintln!(
            "{format:>10}: {ready} described from {:.1} % of their bytes in {:.1} reads over {:.1} rounds each, {whole} read whole",
            read as f64 * 100.0 / total.max(1) as f64,
            per(reads, ready),
            per(rounds, ready)
        );
    }
    for (format, (ready, read, total, whole, rounds, reads)) in preview_stats {
        eprintln!(
            "{format:>10} preview: {ready} previews read from {:.1} % of their bytes in {:.1} reads over {:.1} rounds each, {whole} read whole",
            read as f64 * 100.0 / total.max(1) as f64,
            per(reads, ready),
            per(rounds, ready)
        );
    }
}
