use super::raw_samples::RAW_SAMPLES;
use super::{Command, Path, root, run};

pub(crate) fn cmd_corpus(download: bool) -> Result<(), String> {
    let corpus = root().join("corpus");
    println!(
        "Test corpora live under {} (git-ignored, never committed).
Tests that use a corpus skip cleanly when it is absent.

  corpus/raw/        raw.pixls.us samples (CC0) — lightcraft-raw decodes every file.
  corpus/images/     CC0 / public-domain JPEG/PNG/TIFF/HEIC samples (optional)
",
        corpus.display()
    );
    if !download {
        return Ok(());
    }
    let dest = corpus.join("raw");
    std::fs::create_dir_all(&dest).map_err(|e| format!("create {}: {e}", dest.display()))?;
    let mut bad = Vec::new();
    for (name, url, sha256) in RAW_SAMPLES {
        let out = dest.join(name);
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

fn file_sha256(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut hash = Sha256::new();
    std::io::copy(&mut file, &mut hash).map_err(|e| format!("read {}: {e}", path.display()))?;
    Ok(format!("{:x}", hash.finalize()))
}

#[cfg(test)]
mod corpus_tests {
    use super::RAW_SAMPLES;

    #[test]
    fn every_sample_has_a_unique_name_and_a_sha256() {
        let mut names: Vec<_> = RAW_SAMPLES.iter().map(|s| s.0).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), RAW_SAMPLES.len(), "duplicate file name");
        for (name, _, sha256) in RAW_SAMPLES {
            assert!(sha256.len() == 64 && sha256.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')), "{name}: not a lowercase sha256");
        }
    }
}
