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
/// All three are pinned (exact size + SHA-256) to the official `facebook/sam3` checkpoint files,
/// checked against a copy of them on 2026-10-13.
pub const SAM3_FILES: &[FileSpec] = &[
    FileSpec {
        name: "vocab.json",
        size: Some(862_328),
        sha256: Some("5047b556ce86ccaf6aa22b3ffccfc52d391ea4accdab9c2f2407da5b742d4363"),
        max: 16 << 20,
    },
    FileSpec {
        name: "merges.txt",
        size: Some(524_619),
        sha256: Some("9fd691f7c8039210e0fced15865466c65820d09b63988b0174bfe25de299051a"),
        max: 16 << 20,
    },
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

/// Download settings for SAM 3: the defaults, plus [`TOKEN_ENV`] for a gated mirror.
pub fn options() -> Options {
    Options { token_env: Some(TOKEN_ENV), ..Options::default() }
}

/// Environment variable with a bearer token for the SAM 3 mirror's own host (a gated Hugging Face repository).
pub const TOKEN_ENV: &str = "LIGHTCRAFT_SAM3_TOKEN";

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
