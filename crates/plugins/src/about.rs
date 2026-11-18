//! Where a plug-in comes from (host feature `about`): the manifest's `homepage`, `repository`,
//! `changelog` and `license`, named as in `Cargo.toml`. All optional and read leniently: a value
//! the host can't use is dropped with a reason, never an install error. Nothing here touches the
//! network; the links open only when the user (or an agent) asks. `lcc-filters` keeps a copy of
//! this file for pixel filters (it doesn't depend on this crate); `lcc-engine` tests that both agree.

use serde::Serialize;
use serde_json::Value;

/// Longest link (`homepage`, `repository`, `changelog`) the host keeps.
pub const MAX_LINK_BYTES: usize = 2048;
/// Longest `license` the host keeps (an SPDX expression: `MIT OR Apache-2.0`).
pub const MAX_LICENSE_BYTES: usize = 256;

/// A plug-in's About fields, as [`About::parse`] kept them.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct About {
    /// The plug-in's web page (`https://`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    /// Its source code (`https://`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// What changed in each version (`https://`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changelog: Option<String>,
    /// Its licence, as text (an SPDX expression, never a link).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
}

/// The manifest fields [`About`] reads.
pub const ABOUT_FIELDS: &[&str] = &["homepage", "repository", "changelog", "license"];

impl About {
    /// The About fields of manifest JSON `raw`, and a line per value dropped (`"homepage: not an
    /// https:// link"`). A field left out, `null` or an empty string is simply absent.
    pub fn parse(raw: &Value) -> (About, Vec<String>) {
        let mut dropped = Vec::new();
        let mut text = |key: &str, max: usize| -> Option<String> {
            let t = match raw.get(key)? {
                Value::Null => return None,
                Value::String(s) => s.trim(),
                _ => {
                    dropped.push(format!("{key}: not text"));
                    return None;
                }
            };
            if t.is_empty() {
                return None;
            }
            if t.len() > max {
                dropped.push(format!("{key}: {} bytes (limit {max})", t.len()));
                return None;
            }
            if t.chars().any(hidden) {
                dropped.push(format!("{key}: has control or invisible characters"));
                return None;
            }
            Some(t.to_string())
        };
        let homepage = text("homepage", MAX_LINK_BYTES);
        let repository = text("repository", MAX_LINK_BYTES);
        let changelog = text("changelog", MAX_LINK_BYTES);
        let license = text("license", MAX_LICENSE_BYTES);
        let mut link = |key: &str, v: Option<String>| {
            let v = v?;
            if link_host(&v).is_some() {
                return Some(v);
            }
            dropped.push(format!("{key}: not an https:// link"));
            None
        };
        let about = About {
            homepage: link("homepage", homepage),
            repository: link("repository", repository),
            changelog: link("changelog", changelog),
            license,
        };
        (about, dropped)
    }

    /// The repository's issues page, for a repository on `https://github.com/<owner>/<repo>` (or a
    /// page inside one); `None` for any other host.
    pub fn issues(&self) -> Option<String> {
        github_issues(self.repository.as_deref()?)
    }

    /// The fields as `plugin.list` shows them: each one present, plus `issues` when
    /// [`About::issues`] has one.
    pub fn info(&self) -> Value {
        let mut v = serde_json::to_value(self).unwrap_or_else(|_| Value::Object(Default::default()));
        if let (Some(issues), Some(o)) = (self.issues(), v.as_object_mut()) {
            o.insert("issues".into(), Value::from(issues));
        }
        v
    }
}

/// A control character, or an invisible one that changes how the text around it reads: direction
/// overrides and isolates (`U+202E` shows `txt.exe` as `exe.txt`), zero-width spaces and joiners,
/// the byte-order mark, a soft hyphen.
fn hidden(c: char) -> bool {
    c.is_control() || matches!(c, '\u{AD}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}')
}

/// The host of `https://` link `url`, lower-cased (`github.com`), or `None` when `url` isn't one
/// the host shows: another scheme, no host, user info (`https://a.com@b.com/` hides its real
/// host), a host that isn't ASCII letters, digits, dots and dashes (no look-alike letters), or
/// whitespace anywhere.
pub fn link_host(url: &str) -> Option<String> {
    if url.len() > MAX_LINK_BYTES || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let rest = url.strip_prefix("https://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.contains('@') {
        return None;
    }
    let host = match authority.rsplit_once(':') {
        Some((h, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => h,
        Some(_) => return None,
        None => authority,
    };
    let ok = !host.is_empty()
        && host.len() <= 253
        && host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        && host.split('.').all(|label| !label.is_empty() && !label.starts_with('-') && !label.ends_with('-'));
    ok.then(|| host.to_ascii_lowercase())
}

/// `https://github.com/<owner>/<repo>/issues` for a link to that repository or a page inside it.
fn github_issues(url: &str) -> Option<String> {
    if !matches!(link_host(url)?.as_str(), "github.com" | "www.github.com") {
        return None;
    }
    let path = url.strip_prefix("https://")?.split_once('/')?.1;
    let path = path.split(['?', '#']).next().unwrap_or_default();
    let mut parts = path.split('/');
    let owner = parts.next().filter(|s| valid_name(s))?;
    let repo = parts.next()?;
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    valid_name(repo).then(|| format!("https://github.com/{owner}/{repo}/issues"))
}

/// A GitHub owner or repository name: 1-100 ASCII letters, digits, `.`, `_`, `-`, and not `.` or `..`.
fn valid_name(s: &str) -> bool {
    (1..=100).contains(&s.len()) && s != "." && s != ".." && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Every hostile value is dropped with a reason that names its field; none panics.
    #[test]
    fn drops_what_it_cannot_use_and_says_why() {
        let huge = format!("https://example.com/{}", "a".repeat(MAX_LINK_BYTES));
        let cases = [
            json!({"homepage": "http://example.com"}),
            json!({"homepage": "javascript:alert(1)"}),
            json!({"homepage": "https://"}),
            json!({"homepage": "https://github.com@evil.example/"}),
            json!({"homepage": "https://exa mple.com/"}),
            json!({"homepage": "https://éxample.com/"}),
            json!({"homepage": "https://example.com:x/"}),
            json!({"homepage": "https://-a.com/"}),
            json!({"homepage": "https://a..com/"}),
            json!({"homepage": 7}),
            json!({"homepage": {"url": "https://example.com"}}),
            json!({"homepage": ["https://example.com"]}),
            json!({"homepage": huge}),
            json!({"repository": "file:///etc/passwd"}),
            json!({"changelog": "https://example.com/\u{0}"}),
            json!({"license": "x".repeat(MAX_LICENSE_BYTES + 1)}),
            json!({"license": "MIT\nOR GPL"}),
            json!({"license": "MIT\u{202E}LPG"}),
            json!({"homepage": "https://example.com/\u{200B}a"}),
            json!({"repository": "https://github.com/me/x\u{2066}"}),
            json!({"license": true}),
        ];
        for raw in cases {
            let (a, dropped) = About::parse(&raw);
            assert_eq!(a, About::default(), "{raw}");
            let key = raw.as_object().and_then(|o| o.keys().next()).cloned().unwrap_or_default();
            assert!(dropped.len() == 1 && dropped[0].starts_with(&format!("{key}: ")), "{raw}: {dropped:?}");
        }
        // one bad field leaves the others
        let (a, dropped) = About::parse(&json!({"homepage": "http://x.com", "license": "MIT"}));
        assert_eq!((a.homepage, a.license.as_deref(), dropped.len()), (None, Some("MIT"), 1));
    }

    #[test]
    fn link_host_is_the_lower_cased_host() {
        assert_eq!(link_host("https://GitHub.com/a/b").as_deref(), Some("github.com"));
        assert_eq!(link_host("https://example.com:8443?x#y").as_deref(), Some("example.com"));
        assert_eq!(link_host("https://xn--bcher-kva.example/").as_deref(), Some("xn--bcher-kva.example"));
    }

    /// Report a Problem comes only from a GitHub repository, with or without a path after it.
    #[test]
    fn issues_only_for_a_github_repository() {
        let issues = |r: &str| About { repository: Some(r.into()), ..About::default() }.issues();
        for r in [
            "https://github.com/me/geo",
            "https://github.com/me/geo/",
            "https://github.com/me/geo.git",
            "https://github.com/me/geo/tree/main/x",
            "https://www.github.com/me/geo?tab=1",
        ] {
            assert_eq!(issues(r).as_deref(), Some("https://github.com/me/geo/issues"), "{r}");
        }
        for r in [
            "https://gitlab.com/me/geo",
            "https://github.com/me",
            "https://github.com/",
            "https://github.com/../x",
            "https://github.com.evil.example/me/geo",
            "https://gist.github.com/me/geo",
        ] {
            assert_eq!(issues(r), None, "{r}");
        }
        let a = About { repository: Some("https://github.com/me/geo".into()), homepage: Some("https://example.com".into()), ..About::default() };
        assert_eq!(
            a.info(),
            json!({"repository": "https://github.com/me/geo", "homepage": "https://example.com", "issues": "https://github.com/me/geo/issues"})
        );
    }
}
