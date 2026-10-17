//! Downloading the SAM 3 model, only when the user asked for it. The downloader itself (mirrors,
//! resuming, checks, the pure-Rust HTTPS client) is the `lcc-fetch` crate; this module
//! says what SAM 3 needs and where it may come from.
//!
//! The weights are never part of the app (SAM License, see docs/ai-masks.md); where they are
//! downloaded from is configured by [`DEFAULT_MIRRORS`] and the user's own list.

pub use lcc_fetch::*;

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

/// Where the app downloads the model from, in order: each is a base URL, and file `f` is at
/// `<base>/<f>`. **Empty for now**: the app has no download location of its own yet, so the
/// in-app download needs the user's list (`{ENV_PREFIX}_SAM3_MIRRORS` or the mirrors file, see
/// [`mirrors`] and docs/ai-masks.md) until this is filled in.
///
/// Hugging Face's `facebook/sam3` can't be a default: it is gated (each user must accept the
/// SAM License there and download with their own token).
// TODO(maintainer): add the app's CDN locations (https, primary first), e.g.
// "https://<cdn-host>/models/sam3/<revision>" — the files there must match `SAM3_FILES`.
pub const DEFAULT_MIRRORS: &[&str] = &[];

/// Environment variable (name after the app prefix, read with `lcc_brand::env`) with extra mirrors (base URLs separated by commas, spaces or newlines),
/// tried before the defaults.
pub const MIRRORS_ENV: &str = "SAM3_MIRRORS";

/// The mirrors to try for SAM 3, in order: the environment variable's, then the mirrors file's
/// (one base URL per line, `#` comments), then [`DEFAULT_MIRRORS`].
pub fn mirrors(env: Option<&str>, file: Option<&Path>) -> Vec<String> {
    lcc_fetch::mirrors(env, file, DEFAULT_MIRRORS)
}

/// Download settings for SAM 3: the defaults, plus [`TOKEN_ENV`] for a gated mirror.
pub fn options() -> Options {
    Options { token_env: Some(TOKEN_ENV), ..Options::default() }
}

/// Environment variable (name after the app prefix, read with `lcc_brand::env`) with a bearer token for the SAM 3 mirror's own host (a gated Hugging Face repository).
pub const TOKEN_ENV: &str = "SAM3_TOKEN";

/// What to tell the user when there is no mirror for the SAM 3 model.
pub fn no_mirrors_message() -> String {
    format!(
        "no download location is configured for the SAM 3 model in this build (set {}, or put the files in the model folder yourself; see docs/ai-masks.md)",
        lcc_brand::env_var(MIRRORS_ENV)
    )
}

/// The BiRefNet_lite checkpoint (`ZhengPeng7/BiRefNet_lite`, MIT) the Subject and Background
/// masks use: one file, pinned by size and SHA-256 (Hugging Face LFS hash).
pub const BIREFNET_FILES: &[FileSpec] = &[FileSpec {
    name: crate::birefnet::WEIGHTS_FILE,
    size: Some(BIREFNET_WEIGHTS_SIZE),
    sha256: Some("4417d89795250e698c3cb0ae8df15743810065f646f48a694fdfa7ca052d0815"),
    max: BIREFNET_WEIGHTS_SIZE,
}];

/// Size of BiRefNet_lite's `model.safetensors` (178 MB).
pub const BIREFNET_WEIGHTS_SIZE: u64 = 177_634_392;

/// Where BiRefNet_lite downloads from: the author's MIT repository on Hugging Face (not gated),
/// pinned to a revision so the file can't change under the hash.
pub const BIREFNET_MIRRORS: &[&str] = &["https://huggingface.co/ZhengPeng7/BiRefNet_lite/resolve/aa62cd87eafb9cc43056d08ef3615a14628b831d"];

/// Environment variable (after the app prefix) with extra BiRefNet mirrors, tried first.
pub const BIREFNET_MIRRORS_ENV: &str = "BIREFNET_MIRRORS";

/// The mirrors to try for BiRefNet: the environment variable's, then [`BIREFNET_MIRRORS`].
pub fn birefnet_mirrors(env: Option<&str>) -> Vec<String> {
    lcc_fetch::mirrors(env, None, BIREFNET_MIRRORS)
}

#[cfg(test)]
mod tests {
    #[test]
    fn birefnet_downloads_from_the_pinned_revision() {
        let m = super::birefnet_mirrors(None);
        assert_eq!(m.len(), 1);
        assert!(m[0].starts_with("https://huggingface.co/ZhengPeng7/BiRefNet_lite/resolve/"));
        assert_eq!(super::BIREFNET_FILES[0].size, Some(super::BIREFNET_WEIGHTS_SIZE));
    }

    use super::*;

    #[test]
    fn sam3_mirrors_come_from_the_environment_the_file_and_the_defaults() {
        let m = mirrors(Some("https://a.example/x/"), Some(Path::new("/nonexistent/mirrors.txt")));
        assert_eq!(m.len(), 1 + DEFAULT_MIRRORS.len());
        assert!(no_mirrors_message().contains(MIRRORS_ENV));
    }
}
