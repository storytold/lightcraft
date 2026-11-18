//! The release's signed checksum list: `SHA256SUMS.txt` and `SHA256SUMS.txt.sig` beside it.
//!
//! The release pipeline signs the exact bytes of `SHA256SUMS.txt` with an ed25519 key
//! (`cargo xtask update-key sign`) and publishes the signature, base64, as `SHA256SUMS.txt.sig`.
//! The public half is pinned in `brand.toml` (`[updates] public_keys`, [`lcc_brand::UPDATES_PUBLIC_KEYS`]:
//! one key, or two while the key is being rotated), so whoever can change a release's files and its checksums still cannot make this build accept
//! them. The feed's own `sha256` and GitHub's `digest` come from the same place as the file and
//! are never trusted on their own: the hash a download must match comes from the verified list
//! only ([`signed_sha256`]).
//!
//! Fails closed: no key, a list or signature that cannot be fetched, a signature that does not
//! verify, or a file the list does not name are all errors, and nothing is installed.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signature, VerifyingKey};

use crate::check::Update;
use crate::error::UpdateError;
use crate::target::Os;

/// The checksum list's file name on a release.
pub const SUMS_FILE: &str = "SHA256SUMS.txt";
/// Its signature's file name.
pub const SIG_FILE: &str = "SHA256SUMS.txt.sig";

/// The key a release's checksum list is verified against: one public key, or (while the key is
/// being rotated) a few, any of which may have signed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateKey(Vec<VerifyingKey>);

impl UpdateKey {
    /// Read one public key: 32 bytes, standard base64 (what `cargo xtask update-key new` prints).
    pub fn parse(text: &str) -> Result<UpdateKey, String> {
        Ok(UpdateKey(vec![parse_one(text)?]))
    }

    /// Read every key of a list (`brand.toml`'s `public_keys`). An empty list, or one key that does
    /// not parse, is an error: a half-usable list is not quietly trusted.
    pub fn parse_list(texts: &[&str]) -> Result<UpdateKey, String> {
        if texts.is_empty() {
            return Err("no key".into());
        }
        let keys = texts.iter().enumerate().map(|(i, t)| parse_one(t).map_err(|e| format!("key {}: {e}", i + 1))).collect::<Result<Vec<_>, _>>()?;
        Ok(UpdateKey(keys))
    }

    /// The keys this build was made with (`brand.toml`), or `None` when it has none, or one that
    /// does not parse: either way this build cannot verify an update, so it installs none.
    pub fn pinned() -> Option<UpdateKey> {
        match UpdateKey::parse_list(lcc_brand::UPDATES_PUBLIC_KEYS) {
            Ok(key) => Some(key),
            Err(why) => {
                if !lcc_brand::UPDATES_PUBLIC_KEYS.is_empty() {
                    log::warn!("update: the update key in brand.toml is unusable ({why}); updates will not be installed");
                }
                None
            }
        }
    }

    /// The keys as `brand.toml` spells them.
    pub fn to_text(&self) -> Vec<String> {
        self.0.iter().map(|k| STANDARD.encode(k.as_bytes())).collect()
    }

    /// Whether `signature` (the text of `SHA256SUMS.txt.sig`) is one of these keys' signature of
    /// `list`.
    pub fn verify(&self, list: &[u8], signature: &str) -> Result<(), UpdateError> {
        let raw = STANDARD.decode(signature.trim()).map_err(|_| UpdateError::Signature(format!("{SIG_FILE} is not a signature (not base64)")))?;
        let raw: [u8; 64] =
            raw.as_slice().try_into().map_err(|_| UpdateError::Signature(format!("{SIG_FILE} is not a signature ({} bytes, not 64)", raw.len())))?;
        let signature = Signature::from_bytes(&raw);
        // strict: no malleable or small-order signatures
        if self.0.iter().any(|key| key.verify_strict(list, &signature).is_ok()) {
            return Ok(());
        }
        Err(UpdateError::Signature(format!(
            "{SUMS_FILE} is not signed by this app's update key (the list or its signature was changed, or another key signed it)"
        )))
    }
}

/// One public key.
fn parse_one(text: &str) -> Result<VerifyingKey, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("no key".into());
    }
    if text.contains(':') {
        // the private key file is `update-signing-secret:<base64>`: never a public key
        return Err("this is not a public key (a private key file was pasted?)".into());
    }
    let bytes = STANDARD.decode(text).map_err(|e| format!("not base64: {e}"))?;
    let bytes: [u8; 32] = bytes.as_slice().try_into().map_err(|_| format!("{} bytes, not 32", bytes.len()))?;
    VerifyingKey::from_bytes(&bytes).map_err(|_| "not an ed25519 public key".to_string())
}

/// The URL of `name` in the same release folder as `url` (`…/releases/download/v1.2.3/<name>`).
pub fn sibling(url: &str, name: &str) -> Option<String> {
    let path = url.trim().split(['?', '#']).next().unwrap_or("");
    let (dir, file) = path.rsplit_once('/')?;
    // `https://host` alone has no folder to stand in
    if file.is_empty() || dir.ends_with('/') || !dir.contains("://") {
        return None;
    }
    Some(format!("{dir}/{name}"))
}

/// The SHA-256 the release's signed checksum list records for `update`'s file.
///
/// Fetches `SHA256SUMS.txt` and `SHA256SUMS.txt.sig` from the file's own release folder, checks
/// the signature against `key`, and looks the file up in the verified list. Every failure is an
/// error that says which of those went wrong.
pub fn signed_sha256(update: &Update, key: &UpdateKey, cancel: Option<Arc<AtomicBool>>) -> Result<String, UpdateError> {
    let (Some(sums_url), Some(sig_url)) = (sibling(&update.asset.url, SUMS_FILE), sibling(&update.asset.url, SIG_FILE)) else {
        return Err(UpdateError::Signature(format!("the release's {SUMS_FILE} cannot be located from the file's address")));
    };
    let list = crate::check::get_bytes(&sums_url, cancel.clone()).map_err(|e| match e {
        UpdateError::Cancelled => e,
        e => UpdateError::Signature(format!("the release's checksum list ({SUMS_FILE}) could not be read: {e}")),
    })?;
    let signature = match crate::check::get_bytes(&sig_url, cancel) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(UpdateError::Http { code: 404, .. }) => {
            return Err(UpdateError::Signature(format!("the release's checksum list is not signed ({SIG_FILE} is missing)")));
        }
        Err(UpdateError::Cancelled) => return Err(UpdateError::Cancelled),
        Err(e) => return Err(UpdateError::Signature(format!("the checksum list's signature ({SIG_FILE}) could not be read: {e}"))),
    };
    key.verify(&list, &signature)?;
    // the file as it is saved (`dist/app.msi` → `app.msi`), which is the name the list records
    let name = crate::download::safe_file_name(&update.asset.name)?;
    // an older release's list and file are signed too: served under a newer release, they must not
    // install as it, so the file the list vouches for has to be the offered version's
    let version = update.version.to_text();
    if !names_version(&name, &version) {
        return Err(UpdateError::Signature(format!(
            "{name} is not version {version} (its name doesn't carry that version): the release's files belong to another release"
        )));
    }
    let text = String::from_utf8_lossy(&list);
    crate::checksum::lookup(&text, &name).ok_or_else(|| UpdateError::Signature(format!("the release's signed {SUMS_FILE} does not list {name}")))
}

/// Whether an artifact's name, `<binary>-<version>-<os>-<arch>…` (packaging/*/package.sh), carries
/// exactly `version`: a whole `-<version>-<os>-` token, so `1.1.0` matches neither `1.1.0-rc.1`,
/// `1.1.01` nor `11.1.0`. Case is ignored.
pub fn names_version(name: &str, version: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let version = version.trim().to_ascii_lowercase();
    if version.is_empty() {
        return false;
    }
    let token = format!("-{version}-");
    name.match_indices(&token).any(|(at, _)| {
        let rest = name.get(at.saturating_add(token.len())..).unwrap_or_default();
        // a binary name comes first, and an OS slug right after the version
        at > 0 && [Os::Windows, Os::Macos, Os::Linux, Os::Freebsd].iter().any(|os| rest.strip_prefix(os.slug()).is_some_and(|r| r.starts_with('-')))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer as _, SigningKey};

    /// A throwaway key for tests only: never a release key.
    fn test_key(seed: u8) -> (SigningKey, UpdateKey) {
        let signing = SigningKey::from_bytes(&[seed; 32]);
        let public = UpdateKey::parse(&STANDARD.encode(signing.verifying_key().as_bytes())).expect("a key");
        (signing, public)
    }

    fn sign(key: &SigningKey, list: &[u8]) -> String {
        STANDARD.encode(key.sign(list).to_bytes())
    }

    const LIST: &[u8] = b"aa11bb22cc33dd44ee55ff6600112233445566778899aabbccddeeff00112233  app-1.2.3-windows-x64.msi\n";

    #[test]
    fn a_name_carries_a_version_only_as_a_whole_token() {
        for (name, version) in [
            ("app-1.1.0-windows-x64.msi", "1.1.0"),
            ("App-1.1.0-Windows-x64.MSI", "1.1.0"),
            ("lightweight-app-1.1.0-macos-universal.dmg", "1.1.0"),
            ("app-1.1.0-linux-x86_64.AppImage", "1.1.0"),
            ("app-1.1.0-linux-aarch64.tar.gz", "1.1.0"),
            ("app-1.1.0-linux-x86_64.flatpak", "1.1.0"),
            ("app-1.1.0-freebsd-x86_64.tar.gz", "1.1.0"),
            ("app-1.2.0-rc.1-windows-x64.msi", "1.2.0-rc.1"),
            ("app-1.0.0-linux-x86_64-1.1.0-linux-x86_64.tar.gz", "1.1.0"),
        ] {
            assert!(names_version(name, version), "{name} is {version}");
        }
        for (name, version) in [
            ("app-1.0.0-windows-x64.msi", "1.1.0"),
            ("app-1.1.0-rc.1-windows-x64.msi", "1.1.0"),
            ("app-1.1.01-windows-x64.msi", "1.1.0"),
            ("app-11.1.0-windows-x64.msi", "1.1.0"),
            ("app-1.2.0-windows-x64.msi", "1.2.0-rc.1"),
            ("-1.1.0-windows-x64.msi", "1.1.0"),
            ("app-1.1.0-web.zip", "1.1.0"),
            ("app-1.1.0-windowsx-x64.msi", "1.1.0"),
            ("app-1.1.0.msi", "1.1.0"),
            ("app-windows-x64.msi", "1.1.0"),
            ("", "1.1.0"),
            ("app-1.1.0-windows-x64.msi", ""),
            ("app-\u{e9}1.1.0-windows-x64.msi", "1.1.0"),
        ] {
            assert!(!names_version(name, version), "{name} is not {version}");
        }
    }

    #[test]
    fn a_list_signed_by_a_listed_key_verifies() {
        // a list signed by the key verifies
        {
            let (signing, public) = test_key(7);
            public.verify(LIST, &sign(&signing, LIST)).expect("verifies");
            public.verify(LIST, &format!("{}\n", sign(&signing, LIST))).expect("a trailing newline is fine");
        }
        // any key of the list may have signed
        // While the key is being rotated, the build trusts the old and the new one.
        {
            let (old, old_public) = test_key(7);
            let (new, new_public) = test_key(8);
            let (stranger, _) = test_key(9);
            let both = UpdateKey::parse_list(&[&new_public.to_text().join(""), &old_public.to_text().join("")]).expect("two keys");
            both.verify(LIST, &sign(&old, LIST)).expect("the old key");
            both.verify(LIST, &sign(&new, LIST)).expect("the new key");
            assert!(both.verify(LIST, &sign(&stranger, LIST)).is_err(), "no other key");
        }
    }

    #[test]
    fn a_changed_list_another_key_or_a_broken_signature_is_refused() {
        let (signing, public) = test_key(7);
        let (other, _) = test_key(9);
        let mut tampered = LIST.to_vec();
        tampered[0] = b'b';
        for (list, sig, what) in [
            (tampered.as_slice(), sign(&signing, LIST), "a changed list"),
            (LIST, sign(&other, LIST), "another key"),
            (LIST, String::new(), "an empty signature"),
            (LIST, "not base64 !".into(), "not base64"),
            (LIST, STANDARD.encode([1u8; 63]), "a short signature"),
            (LIST, STANDARD.encode([0u8; 64]), "zeros"),
        ] {
            let err = public.verify(list, &sig).expect_err(what);
            assert!(matches!(err, UpdateError::Signature(_)), "{what}: {err}");
        }
    }

    /// The format `cargo xtask update-key` writes: the same seed, list and signature are checked
    /// in xtask's tests, so the tool and the app cannot drift apart.
    #[test]
    fn the_xtask_test_vector_verifies() {
        // throwaway test key: seed [42; 32], never a release key
        let public = UpdateKey::parse(XTASK_PUBLIC).expect("key");
        public.verify(XTASK_LIST.as_bytes(), XTASK_SIG).expect("the xtask signature verifies");
        let (signing, derived) = test_key(42);
        assert_eq!(derived, public);
        assert_eq!(sign(&signing, XTASK_LIST.as_bytes()), XTASK_SIG);
    }

    // the same constants are in xtask/src/update_key.rs's tests
    const XTASK_PUBLIC: &str = "GX9rI+FshTLGq8g4+s1ep4m+DHaykgM0A5v6iz02jWE=";
    const XTASK_LIST: &str = "0000000000000000000000000000000000000000000000000000000000000000  app.msi\n";
    const XTASK_SIG: &str = "r1UAqfvohrdWdsN4DSyKUrEiI+g6DU8WgeI9CwblTMcuEqOXaZ/JPHc7AsL0UXUMdNspO1Kr0ToCYDCUWFINAA==";

    #[test]
    fn keys_that_do_not_parse_are_refused_not_panicked_on() {
        for bad in
            ["", "   ", "not base64", "AAAA", &STANDARD.encode([0u8; 33]), "update-signing-secret:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="]
        {
            assert!(UpdateKey::parse(bad).is_err(), "{bad:?}");
        }
        let (_, public) = test_key(3);
        let text = public.to_text().join("");
        assert_eq!(UpdateKey::parse(&format!("  {text}\n")), Ok(public));
        // a list is all usable keys, or none
        assert!(UpdateKey::parse_list(&[]).is_err());
        assert!(UpdateKey::parse_list(&[&text, "not a key"]).is_err(), "one bad key spoils the list");
    }

    /// Whatever the brand pins must parse: never a typo that silently turns verification off.
    #[test]
    fn the_pinned_keys_parse() {
        if let Err(why) = UpdateKey::parse_list(lcc_brand::UPDATES_PUBLIC_KEYS) {
            assert!(lcc_brand::UPDATES_PUBLIC_KEYS.is_empty(), "brand.toml [updates] public_keys does not parse: {why}");
        }
    }

    #[test]
    fn siblings_stay_in_the_release_folder() {
        assert_eq!(
            sibling("https://github.com/o/r/releases/download/v1.2.3/app.msi", SUMS_FILE).as_deref(),
            Some("https://github.com/o/r/releases/download/v1.2.3/SHA256SUMS.txt")
        );
        assert_eq!(sibling("http://127.0.0.1:9/app.msi?x=1#y", SIG_FILE).as_deref(), Some("http://127.0.0.1:9/SHA256SUMS.txt.sig"));
        for bad in ["", "app.msi", "https://host", "https://host/dir/", "https://host//app.msi"] {
            assert_eq!(sibling(bad, SUMS_FILE), None, "{bad:?}");
        }
    }
}
