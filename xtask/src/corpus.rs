use super::raw_samples::RAW_SAMPLES;
use super::{Command, Path, root, run};

/// A Nikon D750 ColorChecker set released under CC0 by dieli (github.com/dieli/d750-lightcraft-colorchecker),
/// pinned to one commit: a matrix-only dual-illuminant DNG camera profile (Cool White Fluorescent + D55)
/// made with Calibrite PROFILER from the two chart shots, the chart NEFs, and Lightroom Classic 15.6's
/// renders of them with that profile. Downloaded into `corpus/d750-colorchecker/` (the repository's own
/// layout) for the `.dcp` import tests; never committed. Fields: path, URL, SHA-256.
const D750_COLORCHECKER: &[(&str, &str, &str)] = &[
    (
        "raw/chart-daylight_flash.NEF",
        "https://raw.githubusercontent.com/dieli/d750-lightcraft-colorchecker/f37f4054b62fa7fb02f35ab8b7ce508f2196459d/raw/chart-daylight_flash.NEF",
        "ebb6768ca1a82e3dc8b455e03842245a8e0d067045f720fbf2dcf20b90f06624",
    ),
    (
        "raw/chart-indoor.NEF",
        "https://raw.githubusercontent.com/dieli/d750-lightcraft-colorchecker/f37f4054b62fa7fb02f35ab8b7ce508f2196459d/raw/chart-indoor.NEF",
        "d687c3efd25697208bdd1015856f30d00f1c4d768237c936640b61997b6d713a",
    ),
    (
        "profiles/20261016_NikonD750_DualIllumination_ColorCheckerPassport.dcp",
        "https://raw.githubusercontent.com/dieli/d750-lightcraft-colorchecker/f37f4054b62fa7fb02f35ab8b7ce508f2196459d/profiles/20261016_NikonD750_DualIllumination_ColorCheckerPassport.dcp",
        "eb7e9465a42099628ffe61838ae4626a4bb51982492975076808c54ebf8d3d78",
    ),
    (
        "images/daylight_flash-lightroom.jpg",
        "https://raw.githubusercontent.com/dieli/d750-lightcraft-colorchecker/f37f4054b62fa7fb02f35ab8b7ce508f2196459d/images/daylight_flash-lightroom.jpg",
        "725ee289efb6edcbbb792a41df5c74df5c865d9e31e55811453a43ddfc5c7bb0",
    ),
    (
        "images/indoor-lightroom.jpg",
        "https://raw.githubusercontent.com/dieli/d750-lightcraft-colorchecker/f37f4054b62fa7fb02f35ab8b7ce508f2196459d/images/indoor-lightroom.jpg",
        "213580e9b0d8e2a76c29aa2eaa9884b724e9b605df2b723ac51af975ec84c414",
    ),
];

pub(crate) fn cmd_corpus(download: bool) -> Result<(), String> {
    let corpus = root().join("corpus");
    println!(
        "Test corpora live under {} (git-ignored, never committed).
Tests that use a corpus skip cleanly when it is absent.

  corpus/raw/        raw.pixls.us samples (CC0) — lightcraft-raw decodes every file.
  corpus/images/     CC0 / public-domain JPEG/PNG/TIFF/HEIC samples (optional)
  corpus/d750-colorchecker/  Nikon D750 ColorChecker set (CC0, dieli): a .dcp, chart NEFs, Lightroom renders
",
        corpus.display()
    );
    if !download {
        return Ok(());
    }
    let mut bad = Vec::new();
    fetch_pinned(&corpus.join("raw"), RAW_SAMPLES, &mut bad)?;
    fetch_pinned(&corpus.join("d750-colorchecker"), D750_COLORCHECKER, &mut bad)?;
    if bad.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{} corpus file(s) do not match their pinned sha256:
  {}",
        bad.len(),
        bad.join(
            "
  "
        )
    ))
}

/// Download each `(path, url, sha256)` of `files` under `dest` unless present, and check its SHA-256
/// (a mismatch is deleted, so the next run downloads it again, and reported in `bad`).
fn fetch_pinned(dest: &Path, files: &[(&str, &str, &str)], bad: &mut Vec<String>) -> Result<(), String> {
    for (name, url, sha256) in files {
        let out = dest.join(name);
        if let Some(dir) = out.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        }
        if !out.exists() {
            let mut curl = Command::new("curl");
            curl.args(["-fsSL", "-o"]).arg(&out).arg(url);
            run(curl, &format!("curl {url}"))?;
        }
        let actual = file_sha256(&out)?;
        if actual != *sha256 {
            // delete it, so the next run downloads it again instead of trusting it
            std::fs::remove_file(&out).map_err(|e| format!("remove {}: {e}", out.display()))?;
            bad.push(format!("{name}: sha256 is {actual}, expected {sha256} (file deleted; source {url})"));
        }
    }
    Ok(())
}

fn file_sha256(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut hash = Sha256::new();
    std::io::copy(&mut file, &mut hash).map_err(|e| format!("read {}: {e}", path.display()))?;
    Ok(format!("{:x}", hash.finalize()))
}

#[cfg(test)]
mod corpus_tests {
    use super::{D750_COLORCHECKER, RAW_SAMPLES};

    #[test]
    fn every_sample_has_a_unique_name_and_a_sha256() {
        for set in [RAW_SAMPLES, D750_COLORCHECKER] {
            let mut names: Vec<_> = set.iter().map(|s| s.0).collect();
            names.sort_unstable();
            names.dedup();
            assert_eq!(names.len(), set.len(), "duplicate file name");
            for (name, _, sha256) in set {
                assert!(sha256.len() == 64 && sha256.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')), "{name}: not a lowercase sha256");
            }
        }
    }

    #[test]
    fn the_d750_set_is_pinned_to_one_commit() {
        for (name, url, _) in D750_COLORCHECKER {
            assert!(url.contains("/f37f4054b62fa7fb02f35ab8b7ce508f2196459d/") && url.ends_with(name), "{name}: {url}");
        }
    }
}
