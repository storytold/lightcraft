//! `http://` and `https://` URLs: just enough parsing to connect, send a request line and follow
//! redirects. Hostile input never panics; anything odd is a [`NetError::BadUrl`].

use crate::NetError;

/// Longest URL accepted.
const MAX_URL: usize = 16 * 1024;

/// A parsed `http://` or `https://` URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    /// `https://`.
    pub tls: bool,
    /// Host name or IP address (an IPv6 address without brackets).
    pub host: String,
    pub port: u16,
    /// Path and query, starting with `/`.
    pub path: String,
}

impl Url {
    pub fn parse(s: &str) -> Result<Url, NetError> {
        let bad = |why: &str| NetError::BadUrl(why.to_string());
        if s.len() > MAX_URL {
            return Err(bad("longer than 16 KiB"));
        }
        let s = s.trim();
        if s.chars().any(|c| c.is_control() || c == ' ') {
            return Err(bad("contains spaces or control characters"));
        }
        let lower = s.get(..8).unwrap_or(s).to_ascii_lowercase();
        let (tls, rest) = if lower.starts_with("https://") {
            (true, s.get(8..).unwrap_or_default())
        } else if lower.starts_with("http://") {
            (false, s.get(7..).unwrap_or_default())
        } else {
            return Err(bad("only http:// and https:// are supported"));
        };
        let rest = rest.split('#').next().unwrap_or_default();
        let (authority, path) = match rest.find(['/', '?']) {
            Some(i) => (rest.get(..i).unwrap_or_default(), rest.get(i..).unwrap_or_default()),
            None => (rest, ""),
        };
        if authority.contains('@') {
            return Err(bad("credentials in URLs are not supported"));
        }
        let default_port = if tls { 443 } else { 80 };
        let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
            let end = v6.find(']').ok_or_else(|| bad("unterminated IPv6 address"))?;
            let host = v6.get(..end).unwrap_or_default();
            let after = v6.get(end + 1..).unwrap_or_default();
            let port = match after.strip_prefix(':') {
                Some(p) => parse_port(p)?,
                None if after.is_empty() => default_port,
                None => return Err(bad("text after the IPv6 address")),
            };
            (host.to_string(), port)
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) => (h.to_string(), parse_port(p)?),
                None => (authority.to_string(), default_port),
            }
        };
        if host.is_empty() {
            return Err(bad("no host"));
        }
        if !host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':')) {
            return Err(bad("host has unsupported characters (use the punycode form)"));
        }
        let path = if path.starts_with('?') {
            format!("/{path}")
        } else if path.is_empty() {
            "/".to_string()
        } else {
            path.to_string()
        };
        Ok(Url { tls, host: host.to_ascii_lowercase(), port, path })
    }

    /// The `Host` header value (port only when not the default).
    pub fn host_header(&self) -> String {
        let host = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        if self.port == self.default_port() { host } else { format!("{host}:{}", self.port) }
    }

    fn default_port(&self) -> u16 {
        if self.tls { 443 } else { 80 }
    }

    /// `scheme://host[:port]`, to compare origins (credentials never cross origins).
    pub fn origin(&self) -> String {
        format!("{}://{}", if self.tls { "https" } else { "http" }, self.host_header())
    }

    /// Resolve a `Location` header against this URL.
    pub fn join(&self, location: &str) -> Result<Url, NetError> {
        let l = location.trim();
        let lower = l.get(..8).unwrap_or(l).to_ascii_lowercase();
        if lower.starts_with("http://") || lower.starts_with("https://") {
            return Url::parse(l);
        }
        if let Some(rest) = l.strip_prefix("//") {
            return Url::parse(&format!("{}://{rest}", if self.tls { "https" } else { "http" }));
        }
        let path = if l.starts_with('/') {
            l.to_string()
        } else {
            let base = self.path.split('?').next().unwrap_or("/");
            let dir = base.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
            format!("{dir}/{l}")
        };
        Url::parse(&format!("{}{path}", self.origin()))
    }
}

impl std::fmt::Display for Url {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}{}", self.origin(), self.path)
    }
}

fn parse_port(p: &str) -> Result<u16, NetError> {
    match p.parse::<u16>() {
        Ok(n) if n > 0 => Ok(n),
        _ => Err(NetError::BadUrl(format!("bad port {p:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_joins() {
        let u = Url::parse("HTTPS://Photos.Example:8443/api/x?y=1#frag").unwrap();
        assert_eq!((u.tls, u.host.as_str(), u.port, u.path.as_str()), (true, "photos.example", 8443, "/api/x?y=1"));
        assert_eq!(u.host_header(), "photos.example:8443");
        assert_eq!(u.join("/b").unwrap().to_string(), "https://photos.example:8443/b");
        assert_eq!(u.join("c?d").unwrap().path, "/api/c?d");
        assert_eq!(u.join("//other/z").unwrap().to_string(), "https://other/z");
        let v6 = Url::parse("http://[::1]:2283").unwrap();
        assert_eq!((v6.host.as_str(), v6.port, v6.path.as_str()), ("::1", 2283, "/"));
        assert_eq!(v6.host_header(), "[::1]:2283");
        assert_eq!(Url::parse("http://h?q").unwrap().path, "/?q");
    }

    #[test]
    fn hostile_urls_are_errors() {
        for s in ["ftp://x", "http://", "http://a b", "http://u:p@h/", "http://h:0", "http://h:99999", "http://[::1", "http://h\r\n/", "http://é/"] {
            assert!(Url::parse(s).is_err(), "{s}");
        }
        assert!(Url::parse(&"h".repeat(20_000)).is_err());
    }
}
