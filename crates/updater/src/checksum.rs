//! `SHA256SUMS.txt` from a release: the checksums the pipeline publishes beside every artifact.
//!
//! It is coreutils' format — `<hex sha256>  <file name>` per line, `*` instead of the second space
//! for binary mode, `#` comments — and it may be missing or truncated without that breaking
//! anything (the size and the TLS-verified download still check out).

/// The SHA-256 recorded for `name`, lower-case hex, if the file lists it.
pub fn lookup(text: &str, name: &str) -> Option<String> {
    let want = name.trim();
    if want.is_empty() {
        return None;
    }
    for line in text.lines().take(4_000) {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((digest, rest)) = line.split_once(char::is_whitespace) else { continue };
        if !is_sha256(digest) {
            continue;
        }
        let file = rest.trim_start().strip_prefix('*').unwrap_or(rest.trim_start()).trim();
        // the pipeline writes plain file names; a path is matched on its last segment
        if file == want || file.rsplit('/').next() == Some(want) || file.rsplit('\\').next() == Some(want) {
            return Some(digest.to_ascii_lowercase());
        }
    }
    None
}

fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    const APPIMAGE_SHA: &str = "aa11bb22cc33dd44ee55ff6600112233445566778899aabbccddeeff00112233";
    const APPIMAGE_SHA_UPPER: &str = "AA11BB22CC33DD44EE55FF6600112233445566778899AABBCCDDEEFF00112233";

    fn sums() -> String {
        format!(
            "# release 1.2.3\n\
             d1e8f2  app-1.2.3-windows-x64.msi\n\
             {APPIMAGE_SHA} *app-1.2.3-linux-x86_64.AppImage\n\
             not-a-hash  app-1.2.3-macos-universal.dmg\n"
        )
    }

    #[test]
    fn finds_a_named_file() {
        let text = sums();
        assert_eq!(lookup(&text, "app-1.2.3-windows-x64.msi"), None, "a short digest is not a SHA-256");
        assert_eq!(lookup(&text, "app-1.2.3-linux-x86_64.AppImage").as_deref(), Some(APPIMAGE_SHA));
        assert_eq!(lookup(&text, "app-1.2.3-macos-universal.dmg"), None);
        assert_eq!(lookup(&text, "app-9.9.9-windows-x64.msi"), None);
        assert_eq!(lookup(&text, ""), None);
        assert_eq!(lookup("", "x"), None);
    }

    #[test]
    fn accepts_paths_and_case() {
        let text = format!("{APPIMAGE_SHA_UPPER}  dist/app.msi\n");
        assert_eq!(lookup(&text, "app.msi").as_deref(), Some(APPIMAGE_SHA));
        let nested = format!("{APPIMAGE_SHA}  ./a/b/app.msi");
        assert_eq!(lookup(&nested, "app.msi").as_deref(), Some(APPIMAGE_SHA));
    }
}
