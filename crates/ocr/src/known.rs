//! The text-reading model sets LightCraft can download: PaddleOCR's PP-OCRv6 tiers (Apache-2.0), each a text detector,
//! a recogniser and its alphabet, plus the text-line orientation classifier (PP-LCNet x0.25, the same file in every set).
//!
//! Every file is pinned here: the Hugging Face repository, the commit, the path in it, the exact size and the SHA-256.
//! What arrives is checked against the size and hash before anything uses it. The weights are never part of LightCraft:
//! they are an opt-in download, and [`download`] refuses to start until the caller says the user has seen the licence
//! ([`ModelSet::licence`]) and accepted it.

use std::path::Path;

/// The detector, recogniser, orientation classifier and alphabet in a model's folder.
pub const DETECTOR: &str = "det.onnx";
pub const RECOGNISER: &str = "rec.onnx";
pub const ORIENTATION: &str = "textline_ori.onnx";
pub const ALPHABET: &str = "alphabet.json";

/// One file of a model set: where it comes from (a repository on Hugging Face, at a pinned commit), the name it is kept
/// under, its size and SHA-256.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelFile {
    /// The name in the model's folder (one of [`DETECTOR`], [`RECOGNISER`], [`ORIENTATION`], [`ALPHABET`]).
    pub name: &'static str,
    /// `owner/name` on huggingface.co.
    pub repo: &'static str,
    /// The full 40-character commit the file is pinned to (never a branch).
    pub revision: &'static str,
    /// The file's path in the repository.
    pub path: &'static str,
    pub size: u64,
    /// Lower-case hex.
    pub sha256: &'static str,
}

/// The terms a model set comes under: what the user is shown before the first download.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Licence {
    pub name: &'static str,
    /// The licence text.
    pub url: &'static str,
    /// Whether the terms allow commercial use.
    pub commercial: bool,
    /// What is and is not known about the weights, as the user reads it.
    pub notice: &'static str,
}

/// The licence of every PP-OCRv6 file: each Hugging Face model card we download from says `license: apache-2.0`, and
/// PaddleOCR's repository is Apache-2.0.
pub const APACHE_2_0: Licence = Licence {
    name: "Apache-2.0",
    url: "https://github.com/PaddlePaddle/PaddleOCR/blob/main/LICENSE",
    commercial: true,
    notice: "Apache-2.0 weights from PaddlePaddle (Baidu), commercial use allowed. The training data (public and in-house text images in Chinese, English, Japanese and other languages) is not itemised by the publisher.",
};

/// A text-reading model set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelSet {
    /// The id commands and the catalog use; also the folder name.
    pub id: &'static str,
    pub name: &'static str,
    pub files: &'static [ModelFile],
}

/// Every set, smallest first.
static SETS: [ModelSet; 3] = [
    ModelSet {
        id: "pp-ocrv6-tiny",
        name: "PP-OCRv6 tiny",
        files: &[
            ModelFile {
                name: "det.onnx",
                repo: "PaddlePaddle/PP-OCRv6_tiny_det_onnx",
                revision: "2ba1506c0380b8f0b03dd142459aac66d4421f6c",
                path: "inference.onnx",
                size: 1_780_590,
                sha256: "193bab7a04fca699a6c82e6abb5b81bdb28177f0abd4062552b04908dafb19f8",
            },
            ModelFile {
                name: "rec.onnx",
                repo: "PaddlePaddle/PP-OCRv6_tiny_rec_onnx",
                revision: "2612ab37152ae0a677521bae4e1e3d4fb4cf7c30",
                path: "inference.onnx",
                size: 4_462_639,
                sha256: "9ef676d6ed3c88256a2d92c640c44f25b0c40947e111b14b8be8f594091563e6",
            },
            ModelFile {
                name: "textline_ori.onnx",
                repo: "PaddlePaddle/PP-LCNet_x0_25_textline_ori_onnx",
                revision: "aea1f18d97338aaf84b463f90192f2820524a00a",
                path: "inference.onnx",
                size: 1_019_454,
                sha256: "94a6a0a0425f2b5f08b5df72086f2d72abe40f1d22f6d12d2cd83674f11f2ff3",
            },
            ModelFile {
                name: "alphabet.json",
                repo: "PaddlePaddle/PP-OCRv6_tiny_rec_safetensors",
                revision: "6f2d2d51b4b4226d7a2329a02f416f4994106f3a",
                path: "preprocessor_config.json",
                size: 75_758,
                sha256: "196f42619e2e9e0e93a6dc1a622fa1c664628e1c63ce116b376275b3e192125f",
            },
        ],
    },
    ModelSet {
        id: "pp-ocrv6-small",
        name: "PP-OCRv6 small",
        files: &[
            ModelFile {
                name: "det.onnx",
                repo: "PaddlePaddle/PP-OCRv6_small_det_onnx",
                revision: "28fe5895c24fd108c19eb3e8479f4ab385fbfc62",
                path: "inference.onnx",
                size: 9_880_512,
                sha256: "d73e0058b7a8086bbd57f3d10b8bcd4ff95363f67e06e2762b5e814fe9c9410e",
            },
            ModelFile {
                name: "rec.onnx",
                repo: "PaddlePaddle/PP-OCRv6_small_rec_onnx",
                revision: "b8f84f0b80c529de40b4fbb3544b84fa7233a513",
                path: "inference.onnx",
                size: 21_159_378,
                sha256: "5435fd747c9e0efe15a96d0b378d5bd157e9492ed8fd80edf08f30d02fa24634",
            },
            ModelFile {
                name: "textline_ori.onnx",
                repo: "PaddlePaddle/PP-LCNet_x0_25_textline_ori_onnx",
                revision: "aea1f18d97338aaf84b463f90192f2820524a00a",
                path: "inference.onnx",
                size: 1_019_454,
                sha256: "94a6a0a0425f2b5f08b5df72086f2d72abe40f1d22f6d12d2cd83674f11f2ff3",
            },
            ModelFile {
                name: "alphabet.json",
                repo: "PaddlePaddle/PP-OCRv6_small_rec_safetensors",
                revision: "fe049fb103f57443fe8840c54ed06b702f3c1de5",
                path: "preprocessor_config.json",
                size: 206_177,
                sha256: "2b24fa36f548893f26a931cf44f1a1a6b2b14ed7e2f8f8e28e6848801a8278db",
            },
        ],
    },
    ModelSet {
        id: "pp-ocrv6-medium",
        name: "PP-OCRv6 medium",
        files: &[
            ModelFile {
                name: "det.onnx",
                repo: "PaddlePaddle/PP-OCRv6_medium_det_onnx",
                revision: "61323801669c338b7891481ec7bac61ce31b576a",
                path: "inference.onnx",
                size: 62_032_837,
                sha256: "eb13b44b25bb36f89528b68720af8a61d9cf381176107f465db1757b65d086e1",
            },
            ModelFile {
                name: "rec.onnx",
                repo: "PaddlePaddle/PP-OCRv6_medium_rec_onnx",
                revision: "50c7eacafc52fa7bcf4194e8cd08e46f8558504b",
                path: "inference.onnx",
                size: 76_554_979,
                sha256: "9c09abf0957f7968c7586464b7397b84ad2387a0497a351af40e9acc71b673ba",
            },
            ModelFile {
                name: "textline_ori.onnx",
                repo: "PaddlePaddle/PP-LCNet_x0_25_textline_ori_onnx",
                revision: "aea1f18d97338aaf84b463f90192f2820524a00a",
                path: "inference.onnx",
                size: 1_019_454,
                sha256: "94a6a0a0425f2b5f08b5df72086f2d72abe40f1d22f6d12d2cd83674f11f2ff3",
            },
            ModelFile {
                name: "alphabet.json",
                repo: "PaddlePaddle/PP-OCRv6_medium_rec_safetensors",
                revision: "024cad6a831de75c2c3c26e711ba8c4a82ccd24b",
                path: "preprocessor_config.json",
                size: 206_177,
                sha256: "2b24fa36f548893f26a931cf44f1a1a6b2b14ed7e2f8f8e28e6848801a8278db",
            },
        ],
    },
];

/// The id of the set LightCraft recommends (see the measurements in this crate's README).
pub const RECOMMENDED: &str = "pp-ocrv6-small";

/// Every set, smallest first.
pub fn all() -> &'static [ModelSet] {
    &SETS
}

/// The set with this id.
pub fn find(id: &str) -> Option<&'static ModelSet> {
    SETS.iter().find(|m| m.id == id)
}

/// The set LightCraft recommends.
pub fn recommended() -> Option<&'static ModelSet> {
    find(RECOMMENDED)
}

impl ModelSet {
    /// The download size.
    pub fn bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    /// The terms of this set's weights.
    pub fn licence(&self) -> &'static Licence {
        &APACHE_2_0
    }

    /// Every file is in `dir` at its pinned size (the hash is checked when the file is downloaded).
    pub fn installed_in(&self, dir: &Path) -> bool {
        self.files.iter().all(|f| std::fs::metadata(dir.join(f.name)).is_ok_and(|m| m.is_file() && m.len() == f.size))
    }

    /// The pinned address of one of the set's files.
    pub fn url_of(&self, file: &ModelFile) -> String {
        format!("https://huggingface.co/{}/resolve/{}/{}", file.repo, file.revision, file.path)
    }
}

/// Download `set` into `dir` (created if missing) from its pinned addresses. Files already there and verified are
/// kept; the others resume where they stopped and are checked against their size and SHA-256 (`lightcraft-fetch`).
///
/// `acknowledged` must be true: the caller has shown the user [`ModelSet::licence`] and its notice and the user accepted
/// them. Without it nothing is fetched. `progress` is called as bytes arrive (often: throttle in it); setting `cancel`
/// stops the download.
#[cfg(not(target_arch = "wasm32"))]
pub fn download(
    set: &ModelSet,
    dir: &Path,
    acknowledged: bool,
    cancel: &std::sync::atomic::AtomicBool,
    progress: &mut dyn FnMut(&lightcraft_fetch::Progress),
) -> crate::Result<()> {
    if !acknowledged {
        return Err(crate::Error::NotAcknowledged(set.licence().name.into()));
    }
    for f in set.files {
        let spec = lightcraft_fetch::FileSpec { name: f.name, size: Some(f.size), sha256: Some(f.sha256), max: f.size };
        lightcraft_fetch::download_url(&spec, &set.url_of(f), dir, &lightcraft_fetch::Options::default(), cancel, progress)
            .map_err(|e| crate::Error::Download(e.to_string()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_file_is_pinned_and_named_once() {
        assert_eq!(all().iter().map(|m| m.id).collect::<Vec<_>>(), ["pp-ocrv6-tiny", "pp-ocrv6-small", "pp-ocrv6-medium"]);
        assert_eq!(recommended().map(|m| m.id), Some(RECOMMENDED));
        for m in all() {
            let mut names: Vec<&str> = m.files.iter().map(|f| f.name).collect();
            names.sort_unstable();
            assert_eq!(names, vec![ALPHABET, DETECTOR, RECOGNISER, ORIENTATION], "{}", m.id);
            for f in m.files {
                assert_eq!(f.sha256.len(), 64, "{}", f.name);
                assert!(f.sha256.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
                assert_eq!(f.revision.len(), 40);
                assert!(f.revision.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()), "pinned to a commit, not a branch");
                assert!(f.size > 0 && f.repo.starts_with("PaddlePaddle/") && !f.path.starts_with('/'));
                let url = m.url_of(f);
                assert!(url.starts_with("https://huggingface.co/PaddlePaddle/") && url.contains(f.revision) && url.ends_with(f.path));
            }
            assert_eq!(m.bytes(), m.files.iter().map(|f| f.size).sum::<u64>());
        }
        assert_eq!(find("pp-ocrv6-tiny").map(|m| m.id), Some("pp-ocrv6-tiny"));
        assert!(find("nope").is_none() && find("").is_none() && find("../pp-ocrv6-small").is_none());
        assert!(all().iter().all(|m| m.licence().commercial && m.licence().url.starts_with("https://")));
    }

    #[test]
    fn a_set_is_installed_only_with_every_file_at_its_pinned_size() {
        let dir = std::env::temp_dir().join(format!("lc-ocr-known-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let Some(set) = find("pp-ocrv6-tiny") else { return };
        assert!(!set.installed_in(&dir), "no folder");
        assert!(std::fs::create_dir_all(&dir).is_ok());
        for f in set.files {
            // right name, wrong size
            assert!(std::fs::write(dir.join(f.name), b"x").is_ok());
        }
        assert!(!set.installed_in(&dir), "files of the wrong size");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn nothing_is_downloaded_before_the_licence_is_acknowledged() {
        let dir = std::env::temp_dir().join(format!("lc-ocr-consent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let mut calls = 0;
        let Some(set) = recommended() else { return };
        let r = download(set, &dir, false, &cancel, &mut |_| calls += 1);
        assert!(matches!(r, Err(crate::Error::NotAcknowledged(_))), "{r:?}");
        assert_eq!(calls, 0);
        assert!(!dir.exists(), "not even the folder is made");
    }
}
