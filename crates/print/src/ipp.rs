//! A small IPP/1.1 client (RFC 8010 encoding, RFC 8011 operations) over plain HTTP, enough to
//! print to CUPS on Linux and macOS: list the printers of a CUPS server, read a printer's media
//! sizes and margins, and send a PDF or JPEG with Print-Job.
//!
//! Pure Rust (no libcups): `ipp://` and `http://` over `std::net`; `ipps://` and `https://` over
//! TLS through `lcc-net` (rustls). A printer's certificate is trusted when it chains to the Mozilla
//! roots or when its fingerprint was trusted earlier ([`trust_certificate`], or on first use when
//! [`set_trust_on_first_use`] is on); otherwise the request fails with
//! [`PrintError::UntrustedCertificate`], which names the fingerprint to check. The printer's own
//! options (paper type, tray, quality, two-sided: [`DriverOptions`]) are read with its attributes
//! and sent with the job. Every length read from the network is bounded and checked.
//!
//! Sources: RFC 8010 (IPP/1.1 encoding and transport), RFC 8011 (model and semantics), PWG
//! 5100.13 (media-col, margins), PWG 5101.1 (self-describing media size names), CUPS
//! "CUPS-Get-Printers" operation (CUPS IPP extensions documentation), RFC 7472 (IPP over HTTPS,
//! the `ipps` scheme), PWG 5100.7 / 5100.13 (media-type, media-source, print-quality, sides).
//! Own design.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use lcc_layout::Size;

use crate::{PrintError, Result};

/// IPP operation ids.
pub const OP_PRINT_JOB: u16 = 0x0002;
pub const OP_GET_JOB_ATTRIBUTES: u16 = 0x0009;
pub const OP_GET_PRINTER_ATTRIBUTES: u16 = 0x000B;
pub const OP_CUPS_GET_PRINTERS: u16 = 0x4002;

/// Delimiter tags.
pub(crate) const TAG_OPERATION: u8 = 0x01;
pub(crate) const TAG_JOB: u8 = 0x02;
pub(crate) const TAG_END: u8 = 0x03;
pub(crate) const TAG_PRINTER: u8 = 0x04;
pub(crate) const TAG_UNSUPPORTED: u8 = 0x05;

/// Value tags.
pub(crate) const V_INTEGER: u8 = 0x21;
pub(crate) const V_BOOLEAN: u8 = 0x22;
pub(crate) const V_ENUM: u8 = 0x23;
pub(crate) const V_RESOLUTION: u8 = 0x32;
pub(crate) const V_RANGE: u8 = 0x33;
pub(crate) const V_BEG_COLLECTION: u8 = 0x34;
pub(crate) const V_END_COLLECTION: u8 = 0x37;
pub(crate) const V_NAME_WO_LANG: u8 = 0x42;
pub(crate) const V_KEYWORD: u8 = 0x44;
pub(crate) const V_URI: u8 = 0x45;
pub(crate) const V_CHARSET: u8 = 0x47;
pub(crate) const V_LANGUAGE: u8 = 0x48;
pub(crate) const V_MIME: u8 = 0x49;
pub(crate) const V_MEMBER_NAME: u8 = 0x4A;

/// Largest IPP response accepted (attributes only; printers answer in kilobytes).
const MAX_RESPONSE: usize = 16 * 1024 * 1024;
/// Deepest collection nesting accepted.
const MAX_DEPTH: usize = 8;
/// Network timeout.
const TIMEOUT: Duration = Duration::from_secs(30);

/// An attribute value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Int(i32),
    Bool(bool),
    Text(String),
    Range(i32, i32),
    Resolution(i32, i32, u8),
    Collection(Vec<(String, Vec<Value>)>),
    /// Out-of-band or unknown tag, with its raw bytes.
    Other(u8, Vec<u8>),
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Text(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_int(&self) -> Option<i32> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }
    /// A member of a collection value.
    pub fn member(&self, name: &str) -> Option<&Value> {
        match self {
            Value::Collection(m) => m.iter().find(|(n, _)| n == name).and_then(|(_, v)| v.first()),
            _ => None,
        }
    }
}

/// One attribute: a name and one or more values, with their value tag.
#[derive(Clone, Debug, PartialEq)]
pub struct Attribute {
    pub tag: u8,
    pub name: String,
    pub values: Vec<Value>,
}

/// An attribute group (operation, job, printer…).
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub tag: u8,
    pub attrs: Vec<Attribute>,
}

impl Group {
    pub fn get(&self, name: &str) -> Option<&Attribute> {
        self.attrs.iter().find(|a| a.name == name)
    }
}

/// An IPP message (request or response).
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub version: (u8, u8),
    /// Operation id (request) or status code (response).
    pub code: u16,
    pub request_id: u32,
    pub groups: Vec<Group>,
}

impl Message {
    pub fn request(op: u16, request_id: u32, printer_uri: &str) -> Message {
        let ops = Group {
            tag: TAG_OPERATION,
            attrs: vec![
                Attribute { tag: V_CHARSET, name: "attributes-charset".into(), values: vec![Value::Text("utf-8".into())] },
                Attribute { tag: V_LANGUAGE, name: "attributes-natural-language".into(), values: vec![Value::Text("en".into())] },
                Attribute { tag: V_URI, name: "printer-uri".into(), values: vec![Value::Text(printer_uri.into())] },
            ],
        };
        Message { version: (1, 1), code: op, request_id, groups: vec![ops] }
    }

    /// Adds an operation attribute.
    pub fn with(mut self, tag: u8, name: &str, values: Vec<Value>) -> Message {
        if let Some(g) = self.groups.iter_mut().find(|g| g.tag == TAG_OPERATION) {
            g.attrs.push(Attribute { tag, name: name.into(), values });
        }
        self
    }

    /// Status code of a response: `successful-ok…` is 0x0000–0x00FF.
    pub fn is_success(&self) -> bool {
        self.code < 0x0100
    }

    /// The first attribute `name` in a group with `tag`.
    pub fn attr(&self, tag: u8, name: &str) -> Option<&Attribute> {
        self.groups.iter().filter(|g| g.tag == tag).find_map(|g| g.get(name))
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut b = vec![self.version.0, self.version.1];
        b.extend_from_slice(&self.code.to_be_bytes());
        b.extend_from_slice(&self.request_id.to_be_bytes());
        for g in &self.groups {
            b.push(g.tag);
            for a in &g.attrs {
                for (i, v) in a.values.iter().enumerate() {
                    let name = if i == 0 { a.name.as_str() } else { "" };
                    encode_value(&mut b, a.tag, name, v);
                }
            }
        }
        b.push(TAG_END);
        b
    }

    /// Parses a message; trailing document data is ignored.
    pub fn decode(b: &[u8]) -> Result<Message> {
        Message::decode_prefix(b).map(|(m, _)| m)
    }

    /// Parses a message and returns where the document data after it starts.
    pub fn decode_prefix(b: &[u8]) -> Result<(Message, usize)> {
        let mut r = Reader { b, at: 0 };
        let version = (r.u8()?, r.u8()?);
        let code = r.u16()?;
        let request_id = r.u32()?;
        let mut groups: Vec<Group> = Vec::new();
        loop {
            let tag = r.u8()?;
            if tag == TAG_END {
                break;
            }
            if tag < 0x10 {
                groups.push(Group { tag, attrs: Vec::new() });
                continue;
            }
            let name = r.string()?;
            let value = r.value(tag, 0)?;
            let Some(g) = groups.last_mut() else { return Err(bad("attribute outside a group")) };
            if name.is_empty() {
                match g.attrs.last_mut() {
                    Some(a) => a.values.push(value),
                    None => return Err(bad("additional value without an attribute")),
                }
            } else {
                g.attrs.push(Attribute { tag, name, values: vec![value] });
            }
        }
        Ok((Message { version, code, request_id, groups }, r.at))
    }
}

fn bad(m: &str) -> PrintError {
    PrintError::Ipp(format!("malformed IPP message: {m}"))
}

fn put_str(b: &mut Vec<u8>, s: &[u8]) {
    let s = s.get(..s.len().min(u16::MAX as usize)).unwrap_or(&[]);
    b.extend_from_slice(&(s.len() as u16).to_be_bytes());
    b.extend_from_slice(s);
}

fn encode_value(b: &mut Vec<u8>, tag: u8, name: &str, v: &Value) {
    match v {
        Value::Collection(members) => {
            b.push(V_BEG_COLLECTION);
            put_str(b, name.as_bytes());
            put_str(b, &[]);
            for (m, vals) in members {
                b.push(V_MEMBER_NAME);
                put_str(b, &[]);
                put_str(b, m.as_bytes());
                for v in vals {
                    let t = match v {
                        Value::Int(_) => V_INTEGER,
                        Value::Bool(_) => V_BOOLEAN,
                        Value::Range(..) => V_RANGE,
                        Value::Resolution(..) => V_RESOLUTION,
                        Value::Collection(_) => V_BEG_COLLECTION,
                        Value::Other(t, _) => *t,
                        Value::Text(_) => V_KEYWORD,
                    };
                    encode_value(b, t, "", v);
                }
            }
            b.push(V_END_COLLECTION);
            put_str(b, &[]);
            put_str(b, &[]);
        }
        _ => {
            b.push(tag);
            put_str(b, name.as_bytes());
            match v {
                Value::Int(i) => put_str(b, &i.to_be_bytes()),
                Value::Bool(x) => put_str(b, &[u8::from(*x)]),
                Value::Text(s) => put_str(b, s.as_bytes()),
                Value::Range(lo, hi) => {
                    let mut v = lo.to_be_bytes().to_vec();
                    v.extend_from_slice(&hi.to_be_bytes());
                    put_str(b, &v);
                }
                Value::Resolution(x, y, u) => {
                    let mut v = x.to_be_bytes().to_vec();
                    v.extend_from_slice(&y.to_be_bytes());
                    v.push(*u);
                    put_str(b, &v);
                }
                Value::Other(_, raw) => put_str(b, raw),
                Value::Collection(_) => {}
            }
        }
    }
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        let end = self.at.checked_add(n).ok_or_else(|| bad("length overflow"))?;
        let s = self.b.get(self.at..end).ok_or_else(|| bad("truncated"))?;
        self.at = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?.first().copied().unwrap_or(0))
    }
    fn u16(&mut self) -> Result<u16> {
        let s = self.take(2)?;
        Ok(u16::from_be_bytes([s.first().copied().unwrap_or(0), s.get(1).copied().unwrap_or(0)]))
    }
    fn u32(&mut self) -> Result<u32> {
        let s = self.take(4)?;
        let mut a = [0u8; 4];
        a.copy_from_slice(s);
        Ok(u32::from_be_bytes(a))
    }
    fn bytes(&mut self) -> Result<Vec<u8>> {
        let n = self.u16()? as usize;
        Ok(self.take(n)?.to_vec())
    }
    fn string(&mut self) -> Result<String> {
        Ok(String::from_utf8_lossy(&self.bytes()?).into_owned())
    }

    /// The value of an attribute whose tag and name were read.
    fn value(&mut self, tag: u8, depth: usize) -> Result<Value> {
        let raw = self.bytes()?;
        let i32_at = |o: usize| raw.get(o..o + 4).map(|s| i32::from_be_bytes([s[0], s[1], s[2], s[3]]));
        Ok(match tag {
            V_INTEGER | V_ENUM => Value::Int(i32_at(0).ok_or_else(|| bad("short integer"))?),
            V_BOOLEAN => Value::Bool(raw.first().is_some_and(|v| *v != 0)),
            V_RANGE => Value::Range(i32_at(0).ok_or_else(|| bad("short range"))?, i32_at(4).ok_or_else(|| bad("short range"))?),
            V_RESOLUTION => Value::Resolution(
                i32_at(0).ok_or_else(|| bad("short resolution"))?,
                i32_at(4).ok_or_else(|| bad("short resolution"))?,
                raw.get(8).copied().unwrap_or(3),
            ),
            V_BEG_COLLECTION => {
                if depth >= MAX_DEPTH {
                    return Err(bad("collections nested too deeply"));
                }
                let mut members: Vec<(String, Vec<Value>)> = Vec::new();
                loop {
                    let t = self.u8()?;
                    let _name = self.string()?;
                    if t == V_END_COLLECTION {
                        let _ = self.bytes()?;
                        break;
                    }
                    if t == V_MEMBER_NAME {
                        let m = self.string()?;
                        members.push((m, Vec::new()));
                        continue;
                    }
                    let v = self.value(t, depth + 1)?;
                    match members.last_mut() {
                        Some(m) => m.1.push(v),
                        None => return Err(bad("collection value without a member name")),
                    }
                }
                Value::Collection(members)
            }
            0x30 | 0x35 | 0x36 | 0x41..=0x49 => Value::Text(String::from_utf8_lossy(&raw).into_owned()),
            t => Value::Other(t, raw),
        })
    }
}

/// Why a spooler printer name (`\\server\printer`, `HP OfficeJet`) is refused.
pub const WINDOWS_SPOOLER_UNSUPPORTED: &str = "printing to a printer by its system name (the Windows print spooler) is not supported: it needs the native print API, which the app doesn't use. Enter the printer's ipp:// address (most network printers and Windows print servers with IPP printing on answer at ipp://<host>/ipp/print or ipp://<host>/printers/<name>), or print to a PDF file and print that";

/// A parsed `ipp://host:port/path` (or `ipps://`) URI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IppUri {
    pub host: String,
    pub port: u16,
    pub path: String,
    /// `ipps://` / `https://`: IPP over TLS.
    pub tls: bool,
}

impl IppUri {
    pub fn parse(uri: &str) -> Result<IppUri> {
        let uri = uri.trim();
        let (rest, tls) = if let Some(r) = uri.strip_prefix("ipp://").or_else(|| uri.strip_prefix("http://")) {
            (r, false)
        } else if let Some(r) = uri.strip_prefix("ipps://").or_else(|| uri.strip_prefix("https://")) {
            (r, true)
        } else if uri.starts_with("\\\\") || !uri.contains("://") {
            return Err(PrintError::Ipp(format!("{uri:?}: {WINDOWS_SPOOLER_UNSUPPORTED}")));
        } else {
            return Err(PrintError::Ipp(format!("not an ipp:// URI: {uri}")));
        };
        let (hostport, path) = match rest.find('/') {
            Some(i) => (rest.get(..i).unwrap_or(""), rest.get(i..).unwrap_or("/")),
            None => (rest, "/"),
        };
        let (host, port) = if let Some(h) = hostport.strip_prefix('[') {
            // [v6]:port
            let end = h.find(']').ok_or_else(|| PrintError::Ipp("bad IPv6 host".into()))?;
            let port = h.get(end + 1..).and_then(|p| p.strip_prefix(':')).map(|p| p.parse::<u16>()).transpose();
            (h.get(..end).unwrap_or("").to_string(), port.map_err(|_| PrintError::Ipp("bad port".into()))?.unwrap_or(631))
        } else {
            match hostport.rsplit_once(':') {
                Some((h, p)) => (h.to_string(), p.parse::<u16>().map_err(|_| PrintError::Ipp("bad port".into()))?),
                None => (hostport.to_string(), 631),
            }
        };
        if host.is_empty() || host.chars().any(|c| c.is_whitespace() || c.is_control()) || path.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(PrintError::Ipp(format!("bad printer URI: {uri}")));
        }
        Ok(IppUri { host, port, path: path.to_string(), tls })
    }

    /// `host:port` (`[v6]:port`): what a trusted certificate is remembered under.
    pub fn host_header(&self) -> String {
        if self.host.contains(':') { format!("[{}]:{}", self.host, self.port) } else { format!("{}:{}", self.host, self.port) }
    }
}

/// Printer certificates trusted by fingerprint: (`host:port`, `AB:CD:…`).
static TRUSTED: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());
/// Whether an `ipps://` printer's unknown certificate is trusted (and remembered) the first time.
static TOFU: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn trusted() -> std::sync::MutexGuard<'static, Vec<(String, String)>> {
    TRUSTED.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Trust the certificate with SHA-256 `fingerprint` (`AB:CD:…`, colons optional) for the printer
/// at `uri` (or a bare `host:port`). Refused when the fingerprint is not 32 hex bytes.
pub fn trust_certificate(uri: &str, fingerprint: &str) -> Result<()> {
    let fp = parse_fingerprint(fingerprint)
        .ok_or_else(|| PrintError::Invalid(format!("{fingerprint:?} is not a SHA-256 fingerprint (64 hex digits, colons optional)")))?;
    let key = if uri.contains("://") { IppUri::parse(uri)?.host_header() } else { uri.trim().to_string() };
    let mut t = trusted();
    if !t.iter().any(|(k, f)| *k == key && *f == fp) {
        t.push((key, fp));
    }
    Ok(())
}

/// Whether `s` is a SHA-256 fingerprint (`AB:CD:…`, colons optional).
pub fn is_fingerprint(s: &str) -> bool {
    parse_fingerprint(s).is_some()
}

/// `s` normalised as an upper-case, colon-separated SHA-256 fingerprint, or `None`.
#[cfg(not(target_arch = "wasm32"))]
fn parse_fingerprint(s: &str) -> Option<String> {
    lcc_net::Fingerprint::parse(s.trim()).map(|f| f.to_string())
}

/// `s` normalised as an upper-case, colon-separated SHA-256 fingerprint, or `None` (wasm builds
/// have no TLS client; the check matches the native one).
#[cfg(target_arch = "wasm32")]
fn parse_fingerprint(s: &str) -> Option<String> {
    let hex: String = s.trim().chars().filter(|c| *c != ':').collect();
    if hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let up = hex.to_ascii_uppercase();
    let pairs: Vec<&str> = (0..32).filter_map(|i| up.get(i * 2..i * 2 + 2)).collect();
    Some(pairs.join(":"))
}

/// Forget every trusted printer certificate, then trust `list` (`host:port`, fingerprint): what the
/// app loads from its settings. Unusable entries are skipped.
pub fn set_trusted_certificates(list: &[(String, String)]) {
    trusted().clear();
    for (k, f) in list {
        let _ = trust_certificate(k, f);
    }
}

/// The trusted printer certificates, including those trusted on first use (to save them).
pub fn trusted_certificates() -> Vec<(String, String)> {
    trusted().clone()
}

/// Trust an `ipps://` printer's certificate the first time it is seen (like an SSH host key),
/// remembering its fingerprint; a different certificate later is still refused.
pub fn set_trust_on_first_use(on: bool) {
    TOFU.store(on, std::sync::atomic::Ordering::SeqCst);
}

/// IPP over TLS: one HTTPS POST through `lcc-net`.
#[cfg(not(target_arch = "wasm32"))]
fn send_tls(u: &IppUri, msg: &Message, document: &[u8]) -> Result<Message> {
    let key = u.host_header();
    let mut trust = lcc_net::Trust::new();
    let pins: Vec<String> = trusted().iter().filter(|(k, _)| *k == key).map(|(_, f)| f.clone()).collect();
    for f in &pins {
        if let Some(fp) = lcc_net::Fingerprint::parse(f) {
            trust = trust.with_pinned(fp);
        }
    }
    // first use: only for a printer with no trusted certificate yet (a changed one is refused)
    if pins.is_empty() && TOFU.load(std::sync::atomic::Ordering::SeqCst) {
        let k = key.clone();
        trust = trust.with_tofu(std::sync::Arc::new(move |info: &lcc_net::CertInfo| {
            let mut t = trusted();
            if !t.iter().any(|(h, _)| *h == k) {
                t.push((k.clone(), info.fingerprint.to_string()));
            }
            true
        }));
    }
    let config = lcc_net::ClientConfig {
        connect_timeout: TIMEOUT,
        stall_timeout: TIMEOUT,
        max_redirects: 0,
        trust,
        max_body: MAX_RESPONSE as u64,
        ..lcc_net::ClientConfig::default()
    };
    let client = lcc_net::Client::new(config).map_err(|e| PrintError::Ipp(format!("{key}: {e}")))?;
    let mut body = msg.encode();
    body.extend_from_slice(document);
    let url = format!("https://{key}{}", u.path);
    let resp = client.post(&url).identity_encoding().body("application/ipp", body).send().map_err(|e| match e {
        lcc_net::NetError::UntrustedCertificate { fingerprint, .. } => PrintError::UntrustedCertificate { printer: key.clone(), fingerprint },
        e => PrintError::Ipp(format!("{key}: {e}")),
    })?;
    if resp.status != 200 {
        return Err(PrintError::Ipp(format!("printer answered HTTP {}", resp.status)));
    }
    let bytes = resp.bytes().map_err(|e| PrintError::Ipp(format!("{key}: {e}")))?;
    Message::decode(&bytes)
}

#[cfg(target_arch = "wasm32")]
fn send_tls(_: &IppUri, _: &Message, _: &[u8]) -> Result<Message> {
    Err(PrintError::Ipp("printing to a printer is not available in the web app".into()))
}

/// Sends `msg` followed by `document` to `uri` and returns the parsed response.
pub fn send(uri: &str, msg: &Message, document: &[u8]) -> Result<Message> {
    let u = IppUri::parse(uri)?;
    if u.tls {
        return send_tls(&u, msg, document);
    }
    let addr = (u.host.as_str(), u.port)
        .to_socket_addrs()
        .map_err(|e| PrintError::Ipp(format!("{}: {e}", u.host)))?
        .next()
        .ok_or_else(|| PrintError::Ipp(format!("{}: no address", u.host)))?;
    let mut s = TcpStream::connect_timeout(&addr, TIMEOUT).map_err(|e| PrintError::Ipp(format!("connect {}: {e}", u.host_header())))?;
    let _ = s.set_read_timeout(Some(TIMEOUT));
    let _ = s.set_write_timeout(Some(TIMEOUT));
    let body = msg.encode();
    let len = body.len().saturating_add(document.len());
    let head = format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/ipp\r\nContent-Length: {len}\r\nUser-Agent: {}\r\nConnection: close\r\n\r\n",
        u.path,
        u.host_header(),
        "IPP-client/1.1"
    );
    let io = |e: std::io::Error| PrintError::Ipp(format!("{}: {e}", u.host_header()));
    s.write_all(head.as_bytes()).map_err(io)?;
    s.write_all(&body).map_err(io)?;
    s.write_all(document).map_err(io)?;
    s.flush().map_err(io)?;
    let mut resp = Vec::new();
    s.take(MAX_RESPONSE as u64 + 64 * 1024).read_to_end(&mut resp).map_err(io)?;
    let body = http_body(&resp)?;
    Message::decode(&body)
}

/// The body of an HTTP/1.1 response (Content-Length, chunked or read-to-close).
pub fn http_body(resp: &[u8]) -> Result<Vec<u8>> {
    let end = resp.windows(4).position(|w| w == b"\r\n\r\n").ok_or_else(|| PrintError::Ipp("no HTTP response".into()))?;
    let head = String::from_utf8_lossy(resp.get(..end).unwrap_or(&[]));
    let rest = resp.get(end + 4..).unwrap_or(&[]);
    let mut lines = head.lines();
    let status = lines.next().unwrap_or("");
    let code = status.split_whitespace().nth(1).and_then(|c| c.parse::<u16>().ok()).unwrap_or(0);
    if code != 200 {
        return Err(PrintError::Ipp(format!("printer answered {}", status.trim())));
    }
    let header =
        |n: &str| lines.clone().find_map(|l| l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case(n)).map(|(_, v)| v.trim().to_string()));
    if header("transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked")) {
        let mut out = Vec::new();
        let mut at = 0usize;
        loop {
            let line_end = rest.get(at..).and_then(|r| r.windows(2).position(|w| w == b"\r\n")).ok_or_else(|| PrintError::Ipp("bad chunk".into()))?;
            let size_s = String::from_utf8_lossy(rest.get(at..at + line_end).unwrap_or(&[])).to_string();
            let size =
                usize::from_str_radix(size_s.split(';').next().unwrap_or("").trim(), 16).map_err(|_| PrintError::Ipp("bad chunk size".into()))?;
            at += line_end + 2;
            if size == 0 {
                break;
            }
            if out.len().saturating_add(size) > MAX_RESPONSE {
                return Err(PrintError::Ipp("response too large".into()));
            }
            let end = at.checked_add(size).ok_or_else(|| PrintError::Ipp("bad chunk".into()))?;
            out.extend_from_slice(rest.get(at..end).ok_or_else(|| PrintError::Ipp("truncated chunk".into()))?);
            at = end + 2;
        }
        return Ok(out);
    }
    if let Some(n) = header("content-length").and_then(|v| v.parse::<usize>().ok()) {
        return rest.get(..n).map(<[u8]>::to_vec).ok_or_else(|| PrintError::Ipp("truncated response".into()));
    }
    Ok(rest.to_vec())
}

/// A printer on a CUPS server.
#[derive(Clone, Debug, PartialEq)]
pub struct PrinterEntry {
    pub name: String,
    pub uri: String,
    pub info: String,
}

/// The printers a CUPS server shares (`server` = `host[:port]`, e.g. `localhost:631`).
pub fn cups_printers(server: &str) -> Result<Vec<PrinterEntry>> {
    let uri = format!("ipp://{server}/");
    let msg = Message::request(OP_CUPS_GET_PRINTERS, 1, &uri).with(
        V_KEYWORD,
        "requested-attributes",
        vec![Value::Text("printer-name".into()), Value::Text("printer-uri-supported".into()), Value::Text("printer-info".into())],
    );
    let resp = send(&uri, &msg, &[])?;
    if !resp.is_success() {
        return Err(PrintError::Ipp(format!("CUPS-Get-Printers failed: status 0x{:04x}", resp.code)));
    }
    Ok(resp
        .groups
        .iter()
        .filter(|g| g.tag == TAG_PRINTER)
        .filter_map(|g| {
            let name = g.get("printer-name")?.values.first()?.as_str()?.to_string();
            let uri = g
                .get("printer-uri-supported")
                .and_then(|a| a.values.first())
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("ipp://{server}/printers/{name}"));
            let info = g.get("printer-info").and_then(|a| a.values.first()).and_then(Value::as_str).unwrap_or("").to_string();
            Some(PrinterEntry { name, uri, info })
        })
        .collect())
}

/// A paper size a printer offers.
#[derive(Clone, Debug, PartialEq)]
pub struct Media {
    /// PWG name (`na_letter_8.5x11in`) or the size in hundredths of millimetres.
    pub name: String,
    pub size: Size,
    /// Unprintable margins (points): top, right, bottom, left.
    pub margins: [f32; 4],
}

/// What a printer reports.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PrinterInfo {
    pub name: String,
    /// 3 idle, 4 processing, 5 stopped.
    pub state: i32,
    pub formats: Vec<String>,
    pub media: Vec<Media>,
    pub media_default: Option<String>,
    /// The printer's own options: what it offers and what it uses when the job doesn't say.
    pub options: PrinterOptions,
}

/// The driver options a printer offers (keywords as the printer names them).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PrinterOptions {
    /// `media-type-supported`: paper types (`stationery`, `photographic-glossy`, …).
    pub media_types: Vec<String>,
    pub media_type_default: Option<String>,
    /// `media-source-supported`: trays (`auto`, `main`, `manual`, `tray-2`, …).
    pub media_sources: Vec<String>,
    pub media_source_default: Option<String>,
    /// `print-quality-supported`: 3 draft, 4 normal, 5 high.
    pub qualities: Vec<i32>,
    pub quality_default: Option<i32>,
    /// `sides-supported`: `one-sided`, `two-sided-long-edge`, `two-sided-short-edge`.
    pub sides: Vec<String>,
    pub sides_default: Option<String>,
}

/// The printer options a job asks for; `None` leaves each to the printer's default.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DriverOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_source: Option<String>,
    /// 3 draft, 4 normal, 5 high.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub print_quality: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sides: Option<String>,
}

impl DriverOptions {
    pub fn is_empty(&self) -> bool {
        *self == DriverOptions::default()
    }

    /// These options with those `printer` doesn't offer removed, and one warning for each.
    /// A printer that lists nothing for an option is sent it anyway (it may still know it).
    pub fn supported_by(&self, printer: &PrinterOptions) -> (DriverOptions, Vec<String>) {
        let mut warnings = Vec::new();
        let mut keep = |v: &Option<String>, list: &[String], what: &str| match v {
            Some(k) if !list.is_empty() && !list.iter().any(|x| x == k) => {
                warnings.push(format!("the printer offers no {what} {k:?}; its default is used"));
                None
            }
            other => other.clone(),
        };
        let media_type = keep(&self.media_type, &printer.media_types, "paper type");
        let media_source = keep(&self.media_source, &printer.media_sources, "paper tray");
        let sides = keep(&self.sides, &printer.sides, "two-sided setting");
        let print_quality = match self.print_quality {
            Some(q) if !printer.qualities.is_empty() && !printer.qualities.contains(&q) => {
                warnings.push(format!("the printer offers no print quality {q}; its default is used"));
                None
            }
            q => q,
        };
        (DriverOptions { media_type, media_source, print_quality, sides }, warnings)
    }

    /// The job attributes for these options (PWG 5100.7: media-col, print-quality, sides).
    /// Keywords with characters a keyword can't have are left out.
    fn job_attributes(&self) -> Vec<Attribute> {
        let ok = |k: &&String| !k.is_empty() && k.len() <= 255 && k.chars().all(|c| c.is_ascii_graphic());
        let mut out = Vec::new();
        let mut col = Vec::new();
        if let Some(t) = self.media_type.as_ref().filter(ok) {
            col.push(("media-type".to_string(), vec![Value::Text(t.clone())]));
        }
        if let Some(t) = self.media_source.as_ref().filter(ok) {
            col.push(("media-source".to_string(), vec![Value::Text(t.clone())]));
        }
        if !col.is_empty() {
            out.push(Attribute { tag: V_BEG_COLLECTION, name: "media-col".into(), values: vec![Value::Collection(col)] });
        }
        if let Some(q) = self.print_quality.filter(|q| (3..=5).contains(q)) {
            out.push(Attribute { tag: V_ENUM, name: "print-quality".into(), values: vec![Value::Int(q)] });
        }
        if let Some(sd) = self.sides.as_ref().filter(ok) {
            out.push(Attribute { tag: V_KEYWORD, name: "sides".into(), values: vec![Value::Text(sd.clone())] });
        }
        out
    }
}

/// A print quality's name.
pub fn quality_label(q: i32) -> &'static str {
    match q {
        3 => "Draft",
        4 => "Normal",
        5 => "High",
        _ => "Unknown",
    }
}

impl PrinterInfo {
    /// Whether the printer takes `mime` directly.
    pub fn accepts(&self, mime: &str) -> bool {
        self.formats.iter().any(|f| f.eq_ignore_ascii_case(mime) || f == "application/octet-stream")
    }
}

/// Size of a PWG 5101.1 self-describing media name (`iso_a4_210x297mm`, `na_letter_8.5x11in`).
pub fn pwg_media_size(name: &str) -> Option<Size> {
    let dims = name.rsplit('_').next()?;
    let (nums, unit) = if let Some(d) = dims.strip_suffix("mm") { (d, 72.0 / 25.4) } else { (dims.strip_suffix("in")?, 72.0) };
    let (w, h) = nums.split_once('x')?;
    let (w, h) = (w.parse::<f32>().ok()?, h.parse::<f32>().ok()?);
    (w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0 && w < 10_000.0 && h < 10_000.0).then(|| Size::new(w * unit, h * unit))
}

/// Get-Printer-Attributes: formats, media sizes and margins.
pub fn printer_info(uri: &str) -> Result<PrinterInfo> {
    let req = [
        "printer-name",
        "printer-state",
        "document-format-supported",
        "media-supported",
        "media-default",
        "media-col-database",
        "media-bottom-margin-supported",
        "media-left-margin-supported",
        "media-right-margin-supported",
        "media-top-margin-supported",
        "media-type-supported",
        "media-source-supported",
        "media-col-default",
        "print-quality-supported",
        "print-quality-default",
        "sides-supported",
        "sides-default",
    ];
    let msg = Message::request(OP_GET_PRINTER_ATTRIBUTES, 1, uri).with(
        V_KEYWORD,
        "requested-attributes",
        req.iter().map(|s| Value::Text((*s).into())).collect(),
    );
    let resp = send(uri, &msg, &[])?;
    if !resp.is_success() {
        return Err(PrintError::Ipp(format!("Get-Printer-Attributes failed: status 0x{:04x}", resp.code)));
    }
    Ok(parse_printer_info(&resp))
}

/// [`PrinterInfo`] from a Get-Printer-Attributes response.
pub fn parse_printer_info(resp: &Message) -> PrinterInfo {
    let strs = |n: &str| -> Vec<String> {
        resp.attr(TAG_PRINTER, n).map(|a| a.values.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default()
    };
    let first_int = |n: &str| resp.attr(TAG_PRINTER, n).and_then(|a| a.values.iter().filter_map(Value::as_int).min());
    // hundredths of mm → points
    let hmm = |v: i32| v as f32 / 100.0 * 72.0 / 25.4;
    let default_margins = [
        first_int("media-top-margin-supported").map_or(0.0, hmm),
        first_int("media-right-margin-supported").map_or(0.0, hmm),
        first_int("media-bottom-margin-supported").map_or(0.0, hmm),
        first_int("media-left-margin-supported").map_or(0.0, hmm),
    ];
    let mut media: Vec<Media> = Vec::new();
    if let Some(db) = resp.attr(TAG_PRINTER, "media-col-database") {
        for col in &db.values {
            let Some(size) = col.member("media-size") else { continue };
            let (Some(x), Some(y)) = (size.member("x-dimension").and_then(Value::as_int), size.member("y-dimension").and_then(Value::as_int)) else {
                continue;
            };
            if x <= 0 || y <= 0 {
                continue;
            }
            let m = |n: &str| col.member(n).and_then(Value::as_int).map(hmm);
            let margins = [
                m("media-top-margin").unwrap_or(default_margins[0]),
                m("media-right-margin").unwrap_or(default_margins[1]),
                m("media-bottom-margin").unwrap_or(default_margins[2]),
                m("media-left-margin").unwrap_or(default_margins[3]),
            ];
            let name = col
                .member("media-key")
                .or_else(|| col.member("media-size-name"))
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("{x}x{y}"));
            media.push(Media { name, size: Size::new(hmm(x), hmm(y)), margins });
        }
    }
    for name in strs("media-supported") {
        if media.iter().any(|m| m.name == name) {
            continue;
        }
        if let Some(size) = pwg_media_size(&name) {
            media.push(Media { name, size, margins: default_margins });
        }
    }
    PrinterInfo {
        name: strs("printer-name").into_iter().next().unwrap_or_default(),
        state: first_int("printer-state").unwrap_or(0),
        formats: strs("document-format-supported"),
        media,
        media_default: strs("media-default").into_iter().next(),
        options: parse_printer_options(resp),
    }
}

/// [`PrinterOptions`] from a Get-Printer-Attributes response.
pub fn parse_printer_options(resp: &Message) -> PrinterOptions {
    let strs = |n: &str| -> Vec<String> {
        resp.attr(TAG_PRINTER, n).map(|a| a.values.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default()
    };
    let ints = |n: &str| -> Vec<i32> { resp.attr(TAG_PRINTER, n).map(|a| a.values.iter().filter_map(Value::as_int).collect()).unwrap_or_default() };
    let col_default = |member: &str| {
        resp.attr(TAG_PRINTER, "media-col-default")
            .and_then(|a| a.values.first())
            .and_then(|c| c.member(member))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let cap = |mut v: Vec<String>| {
        v.truncate(256);
        v
    };
    let mut qualities: Vec<i32> = ints("print-quality-supported").into_iter().filter(|q| (3..=5).contains(q)).collect();
    qualities.dedup();
    PrinterOptions {
        media_types: cap(strs("media-type-supported")),
        media_type_default: col_default("media-type"),
        media_sources: cap(strs("media-source-supported")),
        media_source_default: col_default("media-source"),
        qualities,
        quality_default: ints("print-quality-default").into_iter().next(),
        sides: cap(strs("sides-supported")),
        sides_default: strs("sides-default").into_iter().next(),
    }
}

/// Print-Job: sends `document` (`format` = `application/pdf` or `image/jpeg`); returns the job id.
pub fn print_job(uri: &str, user: &str, job_name: &str, format: &str, copies: u32, document: &[u8]) -> Result<i32> {
    print_job_with(uri, user, job_name, format, copies, &DriverOptions::default(), document)
}

/// [`print_job`] with the printer's own options (paper type, tray, quality, two-sided).
pub fn print_job_with(uri: &str, user: &str, job_name: &str, format: &str, copies: u32, driver: &DriverOptions, document: &[u8]) -> Result<i32> {
    let mut msg = Message::request(OP_PRINT_JOB, 1, uri)
        .with(V_NAME_WO_LANG, "requesting-user-name", vec![Value::Text(user.into())])
        .with(V_NAME_WO_LANG, "job-name", vec![Value::Text(job_name.into())])
        .with(V_MIME, "document-format", vec![Value::Text(format.into())]);
    let mut job = driver.job_attributes();
    if copies > 1 {
        job.insert(0, Attribute { tag: V_INTEGER, name: "copies".into(), values: vec![Value::Int(copies.min(999) as i32)] });
    }
    if !job.is_empty() {
        msg.groups.push(Group { tag: TAG_JOB, attrs: job });
    }
    let resp = send(uri, &msg, document)?;
    if !resp.is_success() {
        let why = resp.attr(TAG_OPERATION, "status-message").and_then(|a| a.values.first()).and_then(Value::as_str).unwrap_or("").to_string();
        return Err(PrintError::Ipp(format!("Print-Job failed: status 0x{:04x} {why}", resp.code)));
    }
    Ok(resp.attr(TAG_JOB, "job-id").and_then(|a| a.values.first()).and_then(Value::as_int).unwrap_or(0))
}

/// What a printer says about a job (Get-Job-Attributes).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct JobState {
    /// RFC 8011 job-state: 3 pending, 4 held, 5 processing, 6 stopped, 7 canceled, 8 aborted, 9 completed.
    pub state: i32,
    pub reasons: Vec<String>,
    pub message: String,
}

impl JobState {
    pub const PENDING: i32 = 3;
    pub const CANCELED: i32 = 7;
    pub const ABORTED: i32 = 8;
    pub const COMPLETED: i32 = 9;

    /// Canceled, aborted or completed: the job will not change any more.
    pub fn is_final(&self) -> bool {
        self.state >= JobState::CANCELED
    }

    /// A short word for the state.
    pub fn label(&self) -> &'static str {
        match self.state {
            3 => "pending",
            4 => "held",
            5 => "processing",
            6 => "stopped",
            7 => "canceled",
            8 => "aborted",
            9 => "completed",
            _ => "unknown",
        }
    }
}

/// Get-Job-Attributes: the state of job `job_id` on the printer at `uri`.
pub fn job_state(uri: &str, user: &str, job_id: i32) -> Result<JobState> {
    let msg = Message::request(OP_GET_JOB_ATTRIBUTES, 1, uri)
        .with(V_INTEGER, "job-id", vec![Value::Int(job_id)])
        .with(V_NAME_WO_LANG, "requesting-user-name", vec![Value::Text(user.into())])
        .with(
            V_KEYWORD,
            "requested-attributes",
            vec![Value::Text("job-state".into()), Value::Text("job-state-reasons".into()), Value::Text("job-state-message".into())],
        );
    let resp = send(uri, &msg, &[])?;
    if !resp.is_success() {
        return Err(PrintError::Ipp(format!("Get-Job-Attributes failed: status 0x{:04x}", resp.code)));
    }
    Ok(parse_job_state(&resp))
}

/// [`JobState`] from a Get-Job-Attributes response.
pub fn parse_job_state(resp: &Message) -> JobState {
    let strs = |n: &str| -> Vec<String> {
        resp.attr(TAG_JOB, n).map(|a| a.values.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default()
    };
    JobState {
        state: resp.attr(TAG_JOB, "job-state").and_then(|a| a.values.first()).and_then(Value::as_int).unwrap_or(0),
        reasons: strs("job-state-reasons"),
        message: strs("job-state-message").into_iter().next().unwrap_or_default(),
    }
}

/// The group tag constant of unsupported attributes (for callers inspecting responses).
pub const UNSUPPORTED_GROUP: u8 = TAG_UNSUPPORTED;

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn printer_response() -> Message {
        let size = Value::Collection(vec![("x-dimension".into(), vec![Value::Int(21000)]), ("y-dimension".into(), vec![Value::Int(29700)])]);
        let col = Value::Collection(vec![
            ("media-key".into(), vec![Value::Text("iso_a4_210x297mm".into())]),
            ("media-size".into(), vec![size]),
            ("media-top-margin".into(), vec![Value::Int(500)]),
        ]);
        let printer = Group {
            tag: TAG_PRINTER,
            attrs: vec![
                Attribute { tag: V_NAME_WO_LANG, name: "printer-name".into(), values: vec![Value::Text("PDF".into())] },
                Attribute { tag: V_ENUM, name: "printer-state".into(), values: vec![Value::Int(3)] },
                Attribute {
                    tag: V_MIME,
                    name: "document-format-supported".into(),
                    values: vec![Value::Text("application/pdf".into()), Value::Text("image/jpeg".into())],
                },
                Attribute {
                    tag: V_KEYWORD,
                    name: "media-supported".into(),
                    values: vec![Value::Text("na_letter_8.5x11in".into()), Value::Text("iso_a4_210x297mm".into())],
                },
                Attribute { tag: V_INTEGER, name: "media-left-margin-supported".into(), values: vec![Value::Int(635), Value::Int(0)] },
                Attribute { tag: V_BEG_COLLECTION, name: "media-col-database".into(), values: vec![col] },
            ],
        };
        let mut m = Message::request(0, 1, "ipp://x/");
        m.code = 0;
        m.groups.push(printer);
        m
    }

    #[test]
    fn encode_decode_round_trip_with_collections() {
        let m = printer_response();
        let back = Message::decode(&m.encode()).unwrap();
        assert_eq!(back.groups.len(), 2);
        let info = parse_printer_info(&back);
        assert_eq!(info.name, "PDF");
        assert!(info.accepts("image/jpeg"));
        assert_eq!(info.media.len(), 2);
        let a4 = info.media.iter().find(|m| m.name.starts_with("iso_a4")).unwrap();
        assert!((a4.size.w - 595.3).abs() < 0.5);
        assert!((a4.margins[0] - 14.17).abs() < 0.1);
        assert!(a4.margins[3].abs() < 0.01); // smallest supported margin: borderless
        let letter = info.media.iter().find(|m| m.name.starts_with("na_letter")).unwrap();
        assert_eq!(letter.size, Size::new(612.0, 792.0));
    }

    #[test]
    fn hostile_messages_are_errors() {
        let good = printer_response().encode();
        for cut in [0, 3, 9, 20, good.len() / 2, good.len() - 1] {
            assert!(Message::decode(&good[..cut]).is_err(), "cut {cut}");
        }
        // a value claiming 65535 bytes
        let mut m = good[..8].to_vec();
        m.extend_from_slice(&[TAG_PRINTER, V_INTEGER, 0, 1, b'a', 0xff, 0xff, 1]);
        assert!(Message::decode(&m).is_err());
        // deep collections
        let mut deep = good[..8].to_vec();
        deep.push(TAG_PRINTER);
        for _ in 0..20 {
            deep.extend_from_slice(&[V_BEG_COLLECTION, 0, 1, b'c', 0, 0, V_MEMBER_NAME, 0, 0, 0, 1, b'm']);
        }
        assert!(Message::decode(&deep).is_err());
        assert!(pwg_media_size("custom_x_1e30x5in").is_none());
        assert!(pwg_media_size("garbage").is_none());
        assert!(http_body(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n").is_err());
        assert!(http_body(b"HTTP/1.1 200 OK\r\nContent-Length: 99\r\n\r\nab").is_err());
        assert!(http_body(b"HTTP/1.1 426 Upgrade Required\r\n\r\n").is_err());
        assert_eq!(http_body(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nab\r\n1\r\nc\r\n0\r\n\r\n").unwrap(), b"abc");
    }

    #[test]
    fn uris() {
        assert_eq!(
            IppUri::parse("ipp://localhost/printers/PDF").unwrap(),
            IppUri { host: "localhost".into(), port: 631, path: "/printers/PDF".into(), tls: false }
        );
        assert_eq!(IppUri::parse("ipp://[::1]:8631/ipp/print").unwrap().port, 8631);
        let u = IppUri::parse("ipps://h/p").unwrap();
        assert!(u.tls && u.port == 631);
        assert!(!IppUri::parse("ipp://h/p").unwrap().tls);
        assert!(IppUri::parse("ipp://h:99999/p").is_err());
        assert!(IppUri::parse("ftp://h").is_err());
        assert!(IppUri::parse("ipp://h/a b").is_err());
    }

    /// A fake printer: answers one request with `reply`, returns what it received.
    fn fake_printer(reply: Message) -> (String, std::thread::JoinHandle<Vec<u8>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut got = Vec::new();
            let mut buf = [0u8; 65536];
            loop {
                let n = s.read(&mut buf).unwrap();
                got.extend_from_slice(&buf[..n]);
                if let Some(end) = got.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&got[..end]).to_string();
                    let len: usize = head.lines().find_map(|l| l.strip_prefix("Content-Length: ")).unwrap().parse().unwrap();
                    if got.len() >= end + 4 + len {
                        break;
                    }
                }
            }
            let body = reply.encode();
            let _ = s.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/ipp\r\nContent-Length: {}\r\n\r\n", body.len()).as_bytes());
            let _ = s.write_all(&body);
            got
        });
        (format!("ipp://127.0.0.1:{port}/printers/Test"), h)
    }

    #[test]
    fn print_job_reaches_a_printer() {
        let mut reply = Message::request(0, 1, "ipp://x/");
        reply.code = 0;
        reply.groups.push(Group { tag: TAG_JOB, attrs: vec![Attribute { tag: V_INTEGER, name: "job-id".into(), values: vec![Value::Int(42)] }] });
        let (uri, h) = fake_printer(reply);
        let id = print_job(&uri, "me", "Print", "application/pdf", 2, b"%PDF-1.7 test").unwrap();
        assert_eq!(id, 42);
        let got = h.join().unwrap();
        let end = got.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        assert!(got.starts_with(b"POST /printers/Test HTTP/1.1"));
        let req = Message::decode(&got[end + 4..]).unwrap();
        assert_eq!(req.code, OP_PRINT_JOB);
        assert_eq!(req.attr(TAG_OPERATION, "document-format").unwrap().values[0], Value::Text("application/pdf".into()));
        assert_eq!(req.attr(TAG_JOB, "copies").unwrap().values[0], Value::Int(2));
        assert!(got.ends_with(b"%PDF-1.7 test"));
    }

    #[test]
    fn printer_attributes_and_refusals() {
        let (uri, h) = fake_printer(printer_response());
        let info = printer_info(&uri).unwrap();
        assert_eq!(info.state, 3);
        h.join().unwrap();
        let mut refuse = Message::request(0, 1, "ipp://x/");
        refuse.code = 0x040A; // client-error-document-format-not-supported
        let (uri, h) = fake_printer(refuse);
        assert!(print_job(&uri, "me", "x", "image/jpeg", 1, b"x").is_err());
        h.join().unwrap();
        // nobody listening
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        assert!(printer_info(&format!("ipp://127.0.0.1:{port}/p")).is_err());
    }
}
