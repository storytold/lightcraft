//! Downloading the SAM 3 model, only when the user asked for it. The downloader itself (mirrors,
//! resuming, checks, the pure-Rust HTTPS client) is the `dac-fetch` crate; this module
//! says what SAM 3 needs and where it may come from.
//!
//! The weights are never part of the app (SAM License, see docs/ai-masks.md); where they are
//! downloaded from is configured by [`DEFAULT_MIRRORS`] and the user's own list.

pub use dac_fetch::*;

use std::path::Path;

/// The `facebook/sam3` checkpoint files [`crate::Sam3::load`] needs.
///
/// `model.safetensors` is pinned to the official checkpoint (Hugging Face LFS SHA-256). The two
/// tokenizer files are small text files without a pinned hash yet: they are only parsed by the
/// tokenizer (never executed) and capped in size.
// TODO(maintainer): pin `vocab.json` and `merges.txt` (size + SHA-256) when the files are
// uploaded to the app's CDN.
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

/// Environment variable (name after the app prefix, read with `dac_brand::env`) with extra mirrors (base URLs separated by commas, spaces or newlines),
/// tried before the defaults.
pub const MIRRORS_ENV: &str = "SAM3_MIRRORS";

/// The mirrors to try for SAM 3, in order: the environment variable's, then the mirrors file's
/// (one base URL per line, `#` comments), then [`DEFAULT_MIRRORS`].
pub fn mirrors(env: Option<&str>, file: Option<&Path>) -> Vec<String> {
    dac_fetch::mirrors(env, file, DEFAULT_MIRRORS)
}

/// Download settings for SAM 3: the defaults, plus [`TOKEN_ENV`] for a gated mirror.
pub fn options() -> Options {
    Options { token_env: Some(TOKEN_ENV), ..Options::default() }
}

/// Environment variable (name after the app prefix, read with `dac_brand::env`) with a bearer token for the SAM 3 mirror's own host (a gated Hugging Face repository).
pub const TOKEN_ENV: &str = "SAM3_TOKEN";

/// What to tell the user when there is no mirror for the SAM 3 model.
pub fn no_mirrors_message() -> String {
    format!(
        "no download location is configured for the SAM 3 model in this build (set {}, or put the files in the model folder yourself; see docs/ai-masks.md)",
        dac_brand::env_var(MIRRORS_ENV)
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
