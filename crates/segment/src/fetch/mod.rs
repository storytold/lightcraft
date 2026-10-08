//! Downloading the SAM 3 model, only when the user asked for it. The downloader itself (mirrors,
//! resuming, checks, the pure-Rust HTTPS client) is the `lightcraft-fetch` crate; this module
//! says what SAM 3 needs and where it may come from.
//!
//! The weights are never part of LightCraft (SAM License, see docs/ai-masks.md); where they are
//! downloaded from is configured by [`DEFAULT_MIRRORS`] and the user's own list.

pub use lightcraft_fetch::*;

use std::path::Path;

/// The `facebook/sam3` checkpoint files [`crate::Sam3::load`] needs.
///
/// `model.safetensors` is pinned to the official checkpoint (Hugging Face LFS SHA-256). The two
/// tokenizer files are small text files without a pinned hash yet: they are only parsed by the
/// tokenizer (never executed) and capped in size.
// TODO(maintainer): pin `vocab.json` and `merges.txt` (size + SHA-256) when the files are
// uploaded to LightCraft's CDN.
pub const SAM3_FILES: &[FileSpec] = &[
    FileSpec { name: "vocab.json", size: None, sha256: None, max: 16 << 20 },
    FileSpec { name: "merges.txt", size: None, sha256: None, max: 16 << 20 },
    FileSpec {
        name: crate::WEIGHTS_FILE,
        size: Some(SAM3_WEIGHTS_SIZE),
        sha256: Some("6d06f0a5f84e435071fe6603e61d0b4cc7b40e0d39d487cfd4d67d8cc11cc14a"),
        max: SAM3_WEIGHTS_SIZE,
    },
];

/// Size of the official `model.safetensors` (3.44 GB).
pub const SAM3_WEIGHTS_SIZE: u64 = 3_439_938_512;

/// Where LightCraft downloads the model from, in order: each is a base URL, and file `f` is at
/// `<base>/<f>`. **Empty for now**: LightCraft has no download location of its own yet, so the
/// in-app download needs the user's list (`LIGHTCRAFT_SAM3_MIRRORS` or the mirrors file, see
/// [`mirrors`] and docs/ai-masks.md) until this is filled in.
///
/// Hugging Face's `facebook/sam3` can't be a default: it is gated (each user must accept the
/// SAM License there and download with their own token).
// TODO(maintainer): add LightCraft's CDN locations (https, primary first), e.g.
// "https://<cdn-host>/models/sam3/<revision>" — the files there must match `SAM3_FILES`.
pub const DEFAULT_MIRRORS: &[&str] = &[];

/// Environment variable with extra mirrors (base URLs separated by commas, spaces or newlines),
/// tried before the defaults.
pub const MIRRORS_ENV: &str = "LIGHTCRAFT_SAM3_MIRRORS";

/// The mirrors to try for SAM 3, in order: the environment variable's, then the mirrors file's
/// (one base URL per line, `#` comments), then [`DEFAULT_MIRRORS`].
pub fn mirrors(env: Option<&str>, file: Option<&Path>) -> Vec<String> {
    lightcraft_fetch::mirrors(env, file, DEFAULT_MIRRORS)
}

/// What to tell the user when there is no mirror for the SAM 3 model.
pub fn no_mirrors_message() -> String {
    format!(
        "no download location is configured for the SAM 3 model in this build (set {MIRRORS_ENV}, or put the files in the model folder yourself; see docs/ai-masks.md)"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sam3_mirrors_come_from_the_environment_the_file_and_the_defaults() {
        let m = mirrors(Some("https://a.example/x/"), Some(Path::new("/nonexistent/mirrors.txt")));
        assert_eq!(m.len(), 1 + DEFAULT_MIRRORS.len());
        assert!(no_mirrors_message().contains(MIRRORS_ENV));
    }
}
