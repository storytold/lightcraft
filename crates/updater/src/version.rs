//! Version numbers as release tags spell them (`v1.2.3`, `1.2.3-rc.1`, `1.2.3+build.5`), and the
//! comparison that decides whether a release is newer than the running build.
//!
//! Semantic Versioning 2.0.0 precedence: numeric identifiers compare numerically, a prerelease is
//! lower than the release it leads to, and build metadata is ignored. A tag that is not a version
//! is not a candidate (see [`crate::release::Release::version_of`]).

/// A parsed version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    major: u64,
    minor: u64,
    patch: u64,
    /// Dot-separated prerelease identifiers (`rc.1` → `["rc", "1"]`); empty for a release.
    pre: Vec<String>,
    /// Dot-separated build metadata (`+build.5`); ignored by the ordering.
    build: Vec<String>,
}

impl Version {
    /// Parse `1.2.3`, `1.2.3-rc.1`, `1.2.3+build`, with an optional leading `v`/`V`.
    ///
    /// `major.minor.patch` is required: a tag whose shape we do not understand is not guessed at,
    /// it is refused (`"1.2"`, `"nightly"`, `"1.2.3.4"`).
    pub fn parse(text: &str) -> Result<Version, String> {
        let s = text.trim();
        let s = s.strip_prefix(['v', 'V']).unwrap_or(s);
        if s.is_empty() || s.len() > 128 {
            return Err(format!("`{text}` is not a version"));
        }
        let (rest, build) = match s.split_once('+') {
            Some((head, meta)) => (head, ids(meta)?),
            None => (s, Vec::new()),
        };
        let (core, pre) = match rest.split_once('-') {
            Some((head, tail)) => (head, ids(tail)?),
            None => (rest, Vec::new()),
        };
        let mut parts = core.split('.');
        let mut next = |what: &str| -> Result<u64, String> {
            let part = parts.next().ok_or_else(|| format!("`{text}` has no {what} number"))?;
            // leading zeros and huge numbers are not versions we would ever publish
            if part.is_empty() || part.len() > 18 || !part.bytes().all(|b| b.is_ascii_digit()) {
                return Err(format!("`{text}`: `{part}` is not a {what} number"));
            }
            if part.len() > 1 && part.starts_with('0') {
                return Err(format!("`{text}`: `{part}` has a leading zero"));
            }
            part.parse::<u64>().map_err(|_| format!("`{text}`: `{part}` is too large"))
        };
        let major = next("major")?;
        let minor = next("minor")?;
        let patch = next("patch")?;
        if parts.next().is_some() {
            return Err(format!("`{text}` has more than three numbers"));
        }
        Ok(Version { major, minor, patch, pre, build })
    }

    /// The running build's version, from the crate that was compiled.
    pub fn current() -> Version {
        Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or(Version { major: 0, minor: 0, patch: 0, pre: Vec::new(), build: Vec::new() })
    }

    /// `1.2.3`, or `1.2.3-rc.1` (build metadata is not shown: it never orders anything).
    pub fn to_text(&self) -> String {
        let mut out = format!("{}.{}.{}", self.major, self.minor, self.patch);
        if !self.pre.is_empty() {
            out.push('-');
            out.push_str(&self.pre.join("."));
        }
        out
    }

    /// A release (`1.2.3`), not a prerelease (`1.2.3-rc.1`).
    pub fn is_release(&self) -> bool {
        self.pre.is_empty()
    }

    /// The prerelease identifiers (`["rc", "1"]`), empty for a release.
    pub fn prerelease(&self) -> &[String] {
        &self.pre
    }
}

fn ids(text: &str) -> Result<Vec<String>, String> {
    if text.is_empty() {
        return Err("an empty prerelease or build part".into());
    }
    let mut out = Vec::new();
    for part in text.split('.') {
        if part.is_empty() || part.len() > 64 || !part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return Err(format!("`{part}` is not a version identifier"));
        }
        out.push(part.to_string());
    }
    Ok(out)
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_text())
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    /// SemVer precedence: numbers, then "a prerelease is lower", then the identifiers.
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let core = (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch));
        if core != std::cmp::Ordering::Equal {
            return core;
        }
        match (self.pre.is_empty(), other.pre.is_empty()) {
            (true, true) => std::cmp::Ordering::Equal,
            (true, false) => std::cmp::Ordering::Greater,
            (false, true) => std::cmp::Ordering::Less,
            (false, false) => pre_cmp(&self.pre, &other.pre),
        }
    }
}

/// Identifier by identifier: numeric ones compare as numbers and rank below alphanumeric ones; a
/// shorter list of otherwise equal identifiers ranks lower (`1.0.0-rc` < `1.0.0-rc.1`).
fn pre_cmp(a: &[String], b: &[String]) -> std::cmp::Ordering {
    for (x, y) in a.iter().zip(b) {
        let nx = x.bytes().all(|c| c.is_ascii_digit());
        let ny = y.bytes().all(|c| c.is_ascii_digit());
        let ord = match (nx, ny) {
            (true, true) => match (x.parse::<u64>(), y.parse::<u64>()) {
                (Ok(a), Ok(b)) => a.cmp(&b),
                _ => x.cmp(y),
            },
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            (false, false) => x.cmp(y),
        };
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
    }
    a.len().cmp(&b.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap_or_else(|e| panic!("{s}: {e}"))
    }

    #[test]
    fn parses_the_shapes_we_publish() {
        assert_eq!(v("1.2.3").to_text(), "1.2.3");
        assert_eq!(v("v1.2.3").to_text(), "1.2.3");
        assert_eq!(v("V1.2.3").to_text(), "1.2.3");
        assert_eq!(v(" 1.2.3 ").to_text(), "1.2.3");
        assert_eq!(v("1.2.3-rc.1").to_text(), "1.2.3-rc.1");
        assert_eq!(v("1.2.3+build.5").to_text(), "1.2.3");
        assert_eq!(v("1.2.0-rc.1+sha.abc").to_text(), "1.2.0-rc.1");
        assert_eq!(v("0.7.0").to_text(), "0.7.0");
    }

    #[test]
    fn refuses_what_is_not_a_version() {
        for bad in [
            "",
            "v",
            "1",
            "1.2",
            "1.2.3.4",
            "nightly",
            "1.2.x",
            "01.2.3",
            "1.02.3",
            "1.2.03",
            "1.2.3-",
            "1.2.3+",
            "1.2.3-rc..1",
            "1.2.3-rc.1!",
            "1.2.3+ab cd",
        ] {
            assert!(Version::parse(bad).is_err(), "{bad} should not parse");
        }
    }

    #[test]
    fn orders_like_semver() {
        assert!(v("1.2.3") > v("1.2.2"));
        assert!(v("2.0.0") > v("1.99.99"));
        assert!(v("1.2.3") > v("1.2.3-rc.1"), "a release outranks its prerelease");
        assert!(v("1.2.3-rc.2") > v("1.2.3-rc.1"));
        assert!(v("1.2.3-rc.1") > v("1.2.3-beta"));
        assert!(v("1.2.3-beta.2") > v("1.2.3-beta.1"));
        assert!(v("1.2.3-beta.10") > v("1.2.3-beta.9"), "numeric identifiers compare as numbers");
        assert!(v("1.2.3-rc") < v("1.2.3-rc.1"));
        assert!(v("1.2.3-1") < v("1.2.3-alpha"), "numeric identifiers rank below alphanumeric ones");
        assert_eq!(v("1.2.3+a").cmp(&v("1.2.3+b")), std::cmp::Ordering::Equal, "build metadata is ignored");
    }

    #[test]
    fn current_is_this_build() {
        assert_eq!(Version::current().to_text(), env!("CARGO_PKG_VERSION"));
        assert!(Version::current() >= Version::current());
    }

    #[test]
    fn prerelease_is_reported() {
        assert!(v("1.2.3").is_release());
        assert!(!v("1.2.3-rc.1").is_release());
        assert_eq!(v("1.2.3-rc.1").prerelease(), ["rc".to_string(), "1".to_string()]);
    }
}
