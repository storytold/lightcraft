//! `multipart/form-data` for Immich's `POST /api/assets`: the file, an optional XMP sidecar and a
//! JSON `options` part. The framing is a boundary line, the part's headers, its bytes, repeat —
//! small enough to write here instead of taking a dependency.
//!
//! Inputs are treated as hostile: a file name arrives from a file system or another machine, so it
//! is flattened before it reaches a header, and a boundary that appears anywhere in the payload is
//! refused rather than silently corrupting the body.

/// One part of the form. `data` is written verbatim, so it must not contain [`boundary`] output.
pub struct Part<'a> {
    /// The form field name Immich expects (`file`, `sidecarData`, `options`).
    pub name: &'static str,
    /// Sent only when set; flattened with [`safe_file_name`].
    pub filename: Option<&'a str>,
    pub content_type: Option<&'a str>,
    pub data: &'a [u8],
}

/// A boundary that cannot collide with anything we generate, from a caller-supplied seed
/// (date-plus-counter is enough; nothing here needs a CSPRNG).
#[must_use]
pub fn boundary(seed: u64) -> String {
    format!("----LightCraftImmich{seed:016x}")
}

/// A file name that cannot break out of its header: no CR, LF, quote, backslash or control
/// character, at most 200 bytes, never empty.
#[must_use]
pub fn safe_file_name(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if out.len() >= 512 {
            break; // never size an allocation by an input we do not trust
        }
        if c.is_control() || c == '"' || c == '\\' || c == '/' {
            out.push('_');
        } else {
            out.push(c);
        }
    }
    out.truncate(floor_char_boundary(&out, 200));
    let trimmed = out.trim();
    if trimmed.is_empty() {
        return "file".to_string();
    }
    trimmed.to_string()
}

fn floor_char_boundary(s: &str, max: usize) -> usize {
    let mut i = max.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn valid_boundary(b: &str) -> bool {
    !b.is_empty() && b.len() <= 70 && b.bytes().all(|c| !c.is_ascii_control() && c != b' ' && c != b'\t')
}

/// Encode `parts` as a `multipart/form-data` body. The error text never quotes payload bytes.
pub fn encode(b: &str, parts: &[Part<'_>]) -> Result<Vec<u8>, String> {
    if !valid_boundary(b) {
        return Err("the multipart boundary is unusable".to_string());
    }
    let mut out: Vec<u8> = Vec::new();
    for part in parts {
        if part.name.is_empty() || part.name.chars().any(|c| c.is_control() || c == '"' || c == '\\') {
            return Err(format!("unusable form field name `{}`", part.name));
        }
        if part.content_type.is_some_and(|ct| ct.is_empty() || ct.chars().any(|c| c.is_control() || c == '"')) {
            return Err("unusable content type".to_string());
        }
        if part.data.windows(b.len()).any(|w| w == b.as_bytes()) {
            return Err("the boundary occurs in the payload".to_string());
        }
        out.extend_from_slice(b"--");
        out.extend_from_slice(b.as_bytes());
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(b"Content-Disposition: form-data; name=\"");
        out.extend_from_slice(part.name.as_bytes());
        out.push(b'"');
        if let Some(f) = part.filename {
            out.extend_from_slice(b"; filename=\"");
            out.extend_from_slice(safe_file_name(f).as_bytes());
            out.push(b'"');
        }
        out.extend_from_slice(b"\r\n");
        if let Some(ct) = part.content_type {
            out.extend_from_slice(b"Content-Type: ");
            out.extend_from_slice(ct.as_bytes());
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(part.data);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"--");
    out.extend_from_slice(b.as_bytes());
    out.extend_from_slice(b"--\r\n");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{Part, boundary, encode, safe_file_name};

    #[test]
    fn encodes_the_three_parts_in_order() {
        let b = boundary(7);
        let body = encode(
            &b,
            &[
                Part { name: "file", filename: Some("IMG 1.jpg"), content_type: Some("image/jpeg"), data: b"jpegbytes" },
                Part { name: "sidecarData", filename: Some("IMG 1.xmp"), content_type: None, data: b"<x/>" },
                Part { name: "options", filename: None, content_type: Some("application/json"), data: br#"{"isFavorite":true}"# },
            ],
        )
        .unwrap_or_default();
        let s = String::from_utf8_lossy(&body).into_owned();
        assert!(s.starts_with(&format!(
            "--{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"IMG 1.jpg\"\r\nContent-Type: image/jpeg\r\n\r\njpegbytes\r\n"
        )));
        assert!(s.contains(&format!("--{b}\r\nContent-Disposition: form-data; name=\"sidecarData\"; filename=\"IMG 1.xmp\"\r\n\r\n<x/>\r\n")));
        assert!(s.contains("Content-Type: application/json\r\n\r\n{\"isFavorite\":true}"));
        assert!(s.ends_with(&format!("--{b}--\r\n")));
    }

    #[test]
    fn a_boundary_inside_the_payload_is_refused() {
        let b = boundary(1);
        let e = encode(&b, &[Part { name: "file", filename: None, content_type: None, data: b.as_bytes() }]).unwrap_err();
        assert!(e.contains("boundary"), "{e}");
    }

    #[test]
    fn file_names_cannot_write_headers() {
        assert_eq!(safe_file_name("a\r\nX-Evil: 1\"b\\c"), "a__X-Evil: 1_b_c");
        assert_eq!(safe_file_name("   "), "file");
        assert_eq!(safe_file_name(""), "file");
        let long = safe_file_name(&"é".repeat(400));
        assert!(long.len() <= 202 && long.is_char_boundary(long.len()));
    }

    #[test]
    fn rejects_an_unusable_boundary_or_field_name() {
        assert!(encode("", &[]).is_err());
        assert!(encode("has space", &[]).is_err());
        let b = boundary(2);
        assert!(encode(&b, &[Part { name: "a\nb", filename: None, content_type: None, data: b"" }]).is_err());
    }
}
