//! The app's URL-scheme links (`<scheme>://…`, the scheme from `lcc_brand::URL_SCHEME`), as
//! another app or a script hands them over. A link names one of a few actions, never an
//! arbitrary command: a link can come from anywhere.
//!
//! - `<scheme>://sync?path=<folder>` — synchronize that folder of the library now
//!   (`folder.synchronize`; issue #123). `path` is percent-encoded.

use serde_json::{Value, json};

/// The command a link dispatches and its parameters.
#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    pub command: &'static str,
    pub params: Value,
}

/// Whether `s` looks like one of the app's links (so a launch argument isn't taken for a file).
pub fn is_link(s: &str) -> bool {
    s.get(..lcc_brand::URL_SCHEME.len()).is_some_and(|p| p.eq_ignore_ascii_case(lcc_brand::URL_SCHEME))
        && s.get(lcc_brand::URL_SCHEME.len()..).is_some_and(|r| r.starts_with(':'))
}

/// A `file://` URL's local path (a Linux file manager hands files over as URLs to an app whose
/// desktop file takes `%U`, as ours does for its links); `None` for anything else, a remote host
/// or a path that doesn't decode.
pub fn file_url_path(s: &str) -> Option<String> {
    let rest = s.get(..7).filter(|p| p.eq_ignore_ascii_case("file://")).and_then(|_| s.get(7..))?;
    // `file:///p` or `file://localhost/p`
    let path = if rest.starts_with('/') { rest } else { rest.strip_prefix("localhost").filter(|r| r.starts_with('/'))? };
    let path = decode_path(path).ok()?;
    if path.contains('\0') {
        return None;
    }
    // (Windows: `file:///C:/x` is `C:/x`)
    let windows_drive = path.get(1..3).is_some_and(|d| d.ends_with(':') && d.starts_with(|c: char| c.is_ascii_alphabetic()));
    Some(if windows_drive { path.get(1..).unwrap_or_default().to_string() } else { path })
}

/// The link that synchronizes `folder`.
pub fn sync_link(folder: &str) -> String {
    format!("{}://sync?path={}", lcc_brand::URL_SCHEME, encode(folder))
}

/// Read a link: which command it runs, with what.
pub fn parse(url: &str) -> Result<Link, String> {
    let url = url.trim();
    if !is_link(url) {
        return Err(format!("not a {}:// link", lcc_brand::URL_SCHEME));
    }
    let rest = url.get(lcc_brand::URL_SCHEME.len() + 1..).unwrap_or_default();
    let rest = rest.trim_start_matches('/');
    let (action, query) = rest.split_once('?').unwrap_or((rest, ""));
    let mut path = None;
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if k == "path" {
            path = Some(decode(v)?);
        }
    }
    match action.trim_end_matches('/') {
        "sync" | "folder/sync" => {
            let path = path.filter(|p| !p.trim().is_empty()).ok_or("the link names no folder (`?path=`)")?;
            Ok(Link { command: "folder.synchronize", params: json!({"path": path}) })
        }
        other => Err(format!("unknown link action `{other}` (sync)")),
    }
}

fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~/".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `%`-escapes only (a `+` in a path is a plus).
fn decode_path(s: &str) -> Result<String, String> {
    decode_with(s, false)
}

fn decode(s: &str) -> Result<String, String> {
    decode_with(s, true)
}

fn decode_with(s: &str, plus_is_space: bool) -> Result<String, String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        match b {
            b'%' => {
                let hex = s.get(i + 1..i + 3).ok_or("bad %-escape in the link")?;
                out.push(u8::from_str_radix(hex, 16).map_err(|_| "bad %-escape in the link")?);
                i += 3;
            }
            b'+' if plus_is_space => {
                out.push(b' ');
                i += 1;
            }
            _ => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| "the link's path is not UTF-8".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_folder() {
        for f in ["/Volumes/NAS/Photos 2026/Trip", r"C:\Users\me\Pictures\a&b=c", "/ü/%41"] {
            let l = parse(&sync_link(f)).unwrap();
            assert_eq!(l.command, "folder.synchronize");
            assert_eq!(l.params["path"], f);
        }
    }

    #[test]
    fn file_urls_are_paths() {
        assert_eq!(file_url_path("file:///home/me/a%20b+c.jpg").as_deref(), Some("/home/me/a b+c.jpg"));
        assert_eq!(file_url_path("FILE://localhost/p/x").as_deref(), Some("/p/x"));
        assert_eq!(file_url_path("file:///C:/Users/x.jpg").as_deref(), Some("C:/Users/x.jpg"));
        for bad in
            ["", "file:", "file://", "file://host/p", "file:///%", "file:///%FF", "file:///a%00b", "/p/x", "http://x/y", "file://é", "filé:///x"]
        {
            assert_eq!(file_url_path(bad), None, "{bad}");
        }
    }

    #[test]
    fn hostile_links_are_errors() {
        let s = lcc_brand::URL_SCHEME;
        for bad in [
            "".to_string(),
            "http://sync?path=/a".into(),
            format!("{s}://sync"),
            format!("{s}://sync?path="),
            format!("{s}://sync?path=%"),
            format!("{s}://sync?path=%G1"),
            format!("{s}://sync?path=%FF"),
            format!("{s}://sync?path=%e2%82"),
            format!("{s}://develop.reset?path=/a"),
            format!("{s}:"),
        ] {
            assert!(parse(&bad).is_err(), "{bad}");
        }
        assert!(is_link(&format!("{}://x", s.to_uppercase())));
        assert!(!is_link("/tmp/a.jpg"));
    }
}
