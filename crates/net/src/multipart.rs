//! `multipart/form-data` bodies that stream: file parts are read from disk (or any reader) while
//! sending, never loaded whole, and the total length is known up front so the request carries a
//! `Content-Length` and progress has a denominator.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};

use crate::NetError;

enum Data {
    Bytes(Vec<u8>),
    File(PathBuf, u64),
    Reader(Box<dyn Read + Send>, u64),
}

struct Part {
    head: String,
    data: Data,
}

impl Part {
    fn len(&self) -> u64 {
        match &self.data {
            Data::Bytes(b) => b.len() as u64,
            Data::File(_, n) | Data::Reader(_, n) => *n,
        }
    }
}

/// A `multipart/form-data` body under construction.
pub struct Multipart {
    boundary: String,
    parts: Vec<Part>,
}

impl std::fmt::Debug for Multipart {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Multipart").field("parts", &self.parts.len()).field("len", &self.len()).finish()
    }
}

impl Default for Multipart {
    fn default() -> Self {
        Multipart::new()
    }
}

/// Quote a form field or file name: no line breaks, quotes escaped.
fn quoted(s: &str) -> String {
    s.chars().filter(|c| *c != '\r' && *c != '\n').map(|c| if c == '"' { "%22".to_string() } else { c.to_string() }).collect()
}

fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

impl Multipart {
    pub fn new() -> Multipart {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let mut h = Sha256::new();
        h.update(format!("{:?}{}{:?}", std::time::SystemTime::now(), std::process::id(), std::thread::current().id()));
        h.update(COUNTER.fetch_add(1, Ordering::Relaxed).to_le_bytes());
        let hex: String = h.finalize().iter().take(16).map(|b| format!("{b:02x}")).collect();
        Multipart { boundary: format!("----dac-{hex}"), parts: Vec::new() }
    }

    fn push(&mut self, name: &str, file: Option<(&str, &str)>, data: Data) {
        let mut head = format!("--{}\r\nContent-Disposition: form-data; name=\"{}\"", self.boundary, quoted(name));
        if let Some((filename, content_type)) = file {
            head.push_str(&format!("; filename=\"{}\"\r\nContent-Type: {}", quoted(filename), clean(content_type)));
        }
        head.push_str("\r\n\r\n");
        self.parts.push(Part { head, data });
    }

    /// A text field.
    pub fn text(mut self, name: &str, value: &str) -> Multipart {
        self.push(name, None, Data::Bytes(value.as_bytes().to_vec()));
        self
    }

    /// A file part from memory.
    pub fn bytes(mut self, name: &str, filename: &str, content_type: &str, data: Vec<u8>) -> Multipart {
        self.push(name, Some((filename, content_type)), Data::Bytes(data));
        self
    }

    /// A file part streamed from disk (its size is taken now; a file that changes size while
    /// uploading fails the request rather than sending a corrupt body).
    pub fn file(mut self, name: &str, filename: &str, content_type: &str, path: &Path) -> Result<Multipart, NetError> {
        let len = std::fs::metadata(path).map_err(|e| NetError::Io(format!("{}: {e}", path.display())))?.len();
        self.push(name, Some((filename, content_type)), Data::File(path.to_path_buf(), len));
        Ok(self)
    }

    /// A file part streamed from a reader that yields exactly `len` bytes.
    pub fn reader(mut self, name: &str, filename: &str, content_type: &str, reader: Box<dyn Read + Send>, len: u64) -> Multipart {
        self.push(name, Some((filename, content_type)), Data::Reader(reader, len));
        self
    }

    /// The `Content-Type` header value.
    pub fn content_type(&self) -> String {
        format!("multipart/form-data; boundary={}", self.boundary)
    }

    fn closing(&self) -> String {
        format!("--{}--\r\n", self.boundary)
    }

    /// Exact body length in bytes.
    pub fn len(&self) -> u64 {
        let parts: u64 = self.parts.iter().map(|p| (p.head.len() as u64).saturating_add(p.len()).saturating_add(2)).fold(0, u64::saturating_add);
        parts.saturating_add(self.closing().len() as u64)
    }

    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }

    /// Write the body, calling `progress(sent)` as it goes.
    pub(crate) fn write_to(self, w: &mut impl Write, progress: &mut dyn FnMut(u64)) -> Result<(), NetError> {
        let closing = self.closing();
        let mut sent = 0u64;
        let mut buf = vec![0u8; 64 * 1024];
        for part in self.parts {
            w.write_all(part.head.as_bytes())?;
            sent = sent.saturating_add(part.head.len() as u64);
            let expected = part.len();
            match part.data {
                Data::Bytes(b) => {
                    for chunk in b.chunks(64 * 1024) {
                        w.write_all(chunk)?;
                        sent = sent.saturating_add(chunk.len() as u64);
                        progress(sent);
                    }
                }
                Data::File(path, _) => {
                    let f = File::open(&path).map_err(|e| NetError::Io(format!("{}: {e}", path.display())))?;
                    sent = copy_exact(f, expected, w, &mut buf, sent, progress)?;
                }
                Data::Reader(r, _) => sent = copy_exact(r, expected, w, &mut buf, sent, progress)?,
            }
            w.write_all(b"\r\n")?;
            sent = sent.saturating_add(2);
        }
        w.write_all(closing.as_bytes())?;
        progress(sent.saturating_add(closing.len() as u64));
        Ok(())
    }
}

/// Copy exactly `len` bytes from `r` (fewer or more is an error: the Content-Length is promised).
fn copy_exact(mut r: impl Read, len: u64, w: &mut impl Write, buf: &mut [u8], mut sent: u64, progress: &mut dyn FnMut(u64)) -> Result<u64, NetError> {
    let mut left = len;
    while left > 0 {
        let want = usize::try_from(left).unwrap_or(usize::MAX).min(buf.len());
        let chunk = buf.get_mut(..want).unwrap_or_default();
        let n = r.read(chunk).map_err(|e| NetError::Io(e.to_string()))?;
        if n == 0 {
            return Err(NetError::Io("upload source ended early (file changed while uploading?)".into()));
        }
        w.write_all(chunk.get(..n).unwrap_or_default())?;
        left = left.saturating_sub(n as u64);
        sent = sent.saturating_add(n as u64);
        progress(sent);
    }
    let mut probe = [0u8; 1];
    if r.read(&mut probe).map_err(|e| NetError::Io(e.to_string()))? != 0 {
        return Err(NetError::Io("upload source is longer than announced (file changed while uploading?)".into()));
    }
    Ok(sent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn length_matches_bytes_written() {
        let m = Multipart::new().text("deviceId", "x").bytes("a\"b", "f\r\n.jpg", "image/jpeg", vec![7; 100_000]).reader(
            "r",
            "r.bin",
            "application/octet-stream",
            Box::new(std::io::Cursor::new(vec![1u8; 10])),
            10,
        );
        let len = m.len();
        let mut out = Vec::new();
        let mut last = 0;
        m.write_to(&mut out, &mut |s| last = s).unwrap();
        assert_eq!(out.len() as u64, len);
        assert_eq!(last, len);
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("name=\"a%22b\"; filename=\"f.jpg\""));
    }

    #[test]
    fn short_or_long_reader_is_an_error() {
        let m = Multipart::new().reader("r", "r", "x/y", Box::new(std::io::Cursor::new(vec![1u8; 5])), 10);
        assert!(m.write_to(&mut Vec::new(), &mut |_| {}).is_err());
        let m = Multipart::new().reader("r", "r", "x/y", Box::new(std::io::Cursor::new(vec![1u8; 15])), 10);
        assert!(m.write_to(&mut Vec::new(), &mut |_| {}).is_err());
    }

    #[test]
    fn boundaries_differ() {
        assert_ne!(Multipart::new().boundary, Multipart::new().boundary);
    }
}
