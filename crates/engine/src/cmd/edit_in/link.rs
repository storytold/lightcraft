//! The link between an Edit In version and the photo it was made from, kept in the file itself
//! (XMP Media Management, as Photoshop and Lightroom do): the edit gets its own
//! `xmpMM:DocumentID` / `InstanceID` and an `xmpMM:DerivedFrom` reference (`stRef:documentID` of
//! the source, `stRef:filePath` = the source's file name). The relationship then survives a
//! re-import, a moved catalog or an upload to a server: importing a folder stacks each edit on the
//! photo it came from again ([`stack_derived`]).
//!
//! Reading is cheap and bounded: only TIFF/PSD/JPEG files are opened, and only the part holding
//! the XMP packet is read (TIFF: IFD0 tag 700; PSD: the image resources; JPEG: the first MiB).
//!
//! Sources: XMP Specification Part 2 (xmpMM namespace, stRef ResourceRef type); TIFF 6.0 (IFD
//! layout); Adobe Photoshop File Formats Specification (image resources section, as published).

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use lcc_catalog::{PhotoId, Source};

use crate::Session;

const XMPMM_NS: &str = "http://ns.adobe.com/xap/1.0/mm/";
const STREF_NS: &str = "http://ns.adobe.com/xap/1.0/sType/ResourceRef#";
/// The largest XMP packet read from a file.
const MAX_PACKET: u64 = 16 << 20;
/// How much of a JPEG (or an unknown layout) is scanned for the packet.
const PREFIX: u64 = 1 << 20;
/// Most IFD0 entries looked at.
const MAX_ENTRIES: u64 = 4096;

/// What a file's XMP says about where it came from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Link {
    pub document_id: Option<String>,
    pub derived_document: Option<String>,
    pub derived_path: Option<String>,
}

/// A fresh `xmp.did:` id (128 bits from the time, the process and `seed`).
pub fn new_document_id(seed: &str) -> String {
    use std::hash::{BuildHasher, Hasher};
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let mut parts = [0u64; 2];
    for (i, p) in parts.iter_mut().enumerate() {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write(seed.as_bytes());
        h.write_u128(now);
        h.write_u32(std::process::id());
        h.write_usize(i);
        *p = h.finish();
    }
    format!("xmp.did:{:016x}{:016x}", parts[0], parts[1])
}

/// The document id of a catalog photo without reading its file: from its SHA-1 or content hash
/// (what the link of an edit made from a raw or a JPEG names).
pub fn hash_document_id(ph: &lcc_catalog::Photo) -> Option<String> {
    let h = ph.sha1.as_deref().or(ph.content_hash.as_deref())?;
    let clean: String = h.chars().filter(char::is_ascii_alphanumeric).take(64).collect();
    (!clean.is_empty()).then(|| format!("xmp.did:{clean}"))
}

/// The document id an edit of `ph` names as its source: the id in the file's own XMP when it has
/// one (an edit of an edit), else [`hash_document_id`], else one made from its path.
pub fn source_document_id(ph: &lcc_catalog::Photo) -> String {
    if let Source::File { path } = &ph.source
        && let Some(id) = file_xmp(Path::new(path)).and_then(|x| read_link(&x).document_id)
    {
        return id;
    }
    hash_document_id(ph).unwrap_or_else(|| new_document_id(&ph.file_name))
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// `packet` with the edit's ids and its `DerivedFrom` reference added (an existing link is
/// replaced). A packet without an `rdf:Description` gets none.
pub fn add_link(packet: &str, document_id: &str, from_document: &str, from_path: &str) -> String {
    let Some(at) = packet.find("<rdf:Description") else { return packet.to_string() };
    let mut out = String::with_capacity(packet.len() + 600);
    let head_end = at + "<rdf:Description".len();
    out.push_str(packet.get(..head_end).unwrap_or_default());
    if !packet.contains("xmlns:xmpMM=") {
        out.push_str(&format!(" xmlns:xmpMM=\"{XMPMM_NS}\""));
    }
    if !packet.contains("xmlns:stRef=") {
        out.push_str(&format!(" xmlns:stRef=\"{STREF_NS}\""));
    }
    let rest = strip_link(packet.get(head_end..).unwrap_or_default());
    let body = format!(
        "   <xmpMM:DocumentID>{d}</xmpMM:DocumentID>\n   <xmpMM:InstanceID>{i}</xmpMM:InstanceID>\n   <xmpMM:DerivedFrom rdf:parseType=\"Resource\">\n    <stRef:documentID>{f}</stRef:documentID>\n    <stRef:filePath>{p}</stRef:filePath>\n   </xmpMM:DerivedFrom>\n",
        d = escape(document_id),
        i = escape(&document_id.replacen("xmp.did:", "xmp.iid:", 1)),
        f = escape(from_document),
        p = escape(from_path),
    );
    match rest.rfind("</rdf:Description>") {
        Some(end) => {
            out.push_str(rest.get(..end).unwrap_or_default());
            out.push_str(&body);
            out.push_str("  ");
            out.push_str(rest.get(end..).unwrap_or_default().trim_start());
        }
        None => out.push_str(&rest),
    }
    out
}

/// `s` without the element forms of a previous link.
fn strip_link(s: &str) -> String {
    let mut s = s.to_string();
    for tag in ["xmpMM:DocumentID", "xmpMM:InstanceID", "xmpMM:DerivedFrom"] {
        let (open, close) = (format!("<{tag}"), format!("</{tag}>"));
        for _ in 0..8 {
            let Some(a) = s.find(&open) else { break };
            let Some(b) = s.get(a..).and_then(|t| t.find(&close)).map(|b| a + b + close.len()) else { break };
            let line_start = s.get(..a).and_then(|t| t.rfind('\n')).map(|i| i + 1).unwrap_or(a);
            let start = if s.get(line_start..a).is_some_and(|w| w.trim().is_empty()) { line_start } else { a };
            let end = if s.get(b..).is_some_and(|t| t.starts_with('\n')) { b + 1 } else { b };
            s.replace_range(start..end, "");
        }
    }
    s
}

/// The value of property `key` in `xml`: an attribute (`key="v"`) or an element (`<key>v</key>`).
fn prop(xml: &str, key: &str) -> Option<String> {
    let attr = format!("{key}=\"");
    if let Some(i) = xml.find(&attr) {
        let v = xml.get(i + attr.len()..)?;
        return Some(unescape(v.get(..v.find('"')?)?)).filter(|v| !v.trim().is_empty());
    }
    let (open, close) = (format!("<{key}>"), format!("</{key}>"));
    let i = xml.find(&open)? + open.len();
    let v = xml.get(i..)?;
    Some(unescape(v.get(..v.find(&close)?)?.trim())).filter(|v| !v.is_empty())
}

/// What an XMP packet links to.
pub fn read_link(xmp: &str) -> Link {
    let derived = xmp.find("<xmpMM:DerivedFrom").and_then(|a| {
        let t = xmp.get(a..)?;
        let end = t.find("</xmpMM:DerivedFrom>").or_else(|| t.find("/>").map(|e| e + 2))?;
        t.get(..end)
    });
    // the edit's own id is outside the DerivedFrom block
    let own = match derived {
        Some(d) => xmp.replacen(d, "", 1),
        None => xmp.to_string(),
    };
    Link {
        document_id: prop(&own, "xmpMM:DocumentID"),
        derived_document: derived.and_then(|d| prop(d, "stRef:documentID")),
        derived_path: derived.and_then(|d| prop(d, "stRef:filePath")),
    }
}

fn read_range(f: &mut std::fs::File, at: u64, len: u64) -> Option<Vec<u8>> {
    if len > MAX_PACKET {
        return None;
    }
    f.seek(SeekFrom::Start(at)).ok()?;
    let mut v = Vec::new();
    f.take(len).read_to_end(&mut v).ok()?;
    Some(v)
}

fn scan_packet(b: &[u8]) -> Option<String> {
    let s = String::from_utf8_lossy(b);
    let a = s.find("<x:xmpmeta")?;
    let e = s.get(a..)?.find("</x:xmpmeta>")? + a + "</x:xmpmeta>".len();
    s.get(a..e).map(str::to_string)
}

/// TIFF: the XMP tag (700) of IFD0, read by seeking (classic TIFF only).
fn tiff_xmp(f: &mut std::fs::File) -> Option<String> {
    let h = read_range(f, 0, 8)?;
    let le = match h.get(..4)? {
        b"II*\0" => true,
        b"MM\0*" => false,
        _ => return None,
    };
    let u16_at = |b: &[u8], i: usize| -> Option<u64> {
        let x = [*b.get(i)?, *b.get(i + 1)?];
        Some(u64::from(if le { u16::from_le_bytes(x) } else { u16::from_be_bytes(x) }))
    };
    let u32_at = |b: &[u8], i: usize| -> Option<u64> {
        let x = [*b.get(i)?, *b.get(i + 1)?, *b.get(i + 2)?, *b.get(i + 3)?];
        Some(u64::from(if le { u32::from_le_bytes(x) } else { u32::from_be_bytes(x) }))
    };
    let ifd = u32_at(&h, 4)?;
    let n = u16_at(&read_range(f, ifd, 2)?, 0)?.min(MAX_ENTRIES);
    let entries = read_range(f, ifd + 2, n * 12)?;
    for e in entries.as_chunks::<12>().0 {
        let e: &[u8] = e;
        if u16_at(e, 0)? != 700 {
            continue;
        }
        let count = u32_at(e, 4)?;
        let bytes = if count <= 4 { e.get(8..8 + count as usize)?.to_vec() } else { read_range(f, u32_at(e, 8)?, count)? };
        return scan_packet(&bytes).or_else(|| Some(String::from_utf8_lossy(&bytes).to_string()).filter(|s| s.contains("xmpmeta")));
    }
    None
}

/// PSD/PSB: the XMP image resource (1060), scanned within the image resources section.
fn psd_xmp(f: &mut std::fs::File) -> Option<String> {
    let h = read_range(f, 0, 30)?;
    if h.get(..4)? != b"8BPS" {
        return None;
    }
    let be =
        |b: &[u8], i: usize| -> Option<u64> { Some(u64::from(u32::from_be_bytes([*b.get(i)?, *b.get(i + 1)?, *b.get(i + 2)?, *b.get(i + 3)?]))) };
    let color_mode = be(&h, 26)?;
    let res_at = 30u64.checked_add(color_mode)?;
    let len = be(&read_range(f, res_at, 4)?, 0)?;
    scan_packet(&read_range(f, res_at + 4, len)?)
}

/// The main XMP packet embedded in a TIFF, PSD or JPEG file (`None` for other files or when it
/// can't be read).
pub fn file_xmp(path: &Path) -> Option<String> {
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let mut f = std::fs::File::open(path).ok()?;
    match ext.as_str() {
        "tif" | "tiff" => tiff_xmp(&mut f),
        "psd" | "psb" => psd_xmp(&mut f),
        "jpg" | "jpeg" => scan_packet(&read_range(&mut f, 0, PREFIX)?),
        _ => None,
    }
}

/// After an import: stack each new TIFF/PSD whose XMP says it was derived from a photo in the
/// library on that photo (found by file name in the same folder, else by document id), as Edit In
/// would have. Photos already stacked with their source are left alone. → the photos stacked
pub fn stack_derived(s: &mut Session, imported: &[PhotoId]) -> Vec<PhotoId> {
    let mut wanted: Vec<(PhotoId, String, Link)> = Vec::new();
    for id in imported {
        let Some(ph) = s.catalog.photo(*id) else { continue };
        let Source::File { path } = &ph.source else { continue };
        let p = Path::new(path);
        if !matches!(p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).as_deref(), Some("tif" | "tiff" | "psd" | "psb")) {
            continue;
        }
        let Some(link) = file_xmp(p).map(|x| read_link(&x)) else { continue };
        if link.derived_document.is_none() && link.derived_path.is_none() {
            continue;
        }
        let dir = p.parent().map(|d| d.to_string_lossy().to_string()).unwrap_or_default();
        wanted.push((*id, dir, link));
    }
    if wanted.is_empty() {
        return Vec::new();
    }
    // (folder, file name) and document id → photo, built once
    let mut by_name: HashMap<(String, String), PhotoId> = HashMap::new();
    let mut by_doc: HashMap<String, PhotoId> = HashMap::new();
    for ph in s.catalog.photos().filter(|p| !p.deleted && p.copy_of.is_none()) {
        if let Source::File { path } = &ph.source {
            let p = Path::new(path);
            let dir = p.parent().map(|d| d.to_string_lossy().to_string()).unwrap_or_default();
            by_name.entry((dir, ph.file_name.clone())).or_insert(ph.id);
        }
        if let Some(d) = hash_document_id(ph) {
            by_doc.entry(d).or_insert(ph.id);
        }
    }
    let mut pairs: Vec<(PhotoId, PhotoId)> = Vec::new();
    for (id, dir, link) in wanted {
        let src = link
            .derived_path
            .as_ref()
            .and_then(|n| Path::new(n).file_name().map(|f| f.to_string_lossy().to_string()))
            .and_then(|n| by_name.get(&(dir.clone(), n)).copied())
            .or_else(|| link.derived_document.as_ref().and_then(|d| by_doc.get(d).copied()))
            .filter(|src| *src != id);
        if let Some(src) = src {
            pairs.push((id, src));
        }
    }
    // sources first: an edit of an edit is placed once the edit it came from sits in its stack
    let mut ordered: Vec<(PhotoId, PhotoId)> = Vec::with_capacity(pairs.len());
    while !pairs.is_empty() {
        let pending: Vec<PhotoId> = pairs.iter().map(|(id, _)| *id).collect();
        let (ready, wait): (Vec<_>, Vec<_>) = pairs.into_iter().partition(|(_, src)| !pending.contains(src));
        if ready.is_empty() {
            // a cycle (hand-edited files): take them as they come
            ordered.extend(wait);
            break;
        }
        ordered.extend(ready);
        pairs = wait;
    }
    let mut done = Vec::new();
    for (id, src) in ordered {
        if s.catalog.stack_of(src).is_some_and(|st| st.photos.contains(&id)) {
            continue;
        }
        match super::stack_edit(s, id, src) {
            Ok(()) => done.push(id),
            Err(e) => log::warn!("import: stack {} on {}: {e}", id.0, src.0),
        }
    }
    done
}

#[cfg(test)]
mod tests {
    use super::*;

    const PACKET: &str = "<?xpacket begin=\"\"?>\n<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n  <rdf:Description rdf:about=\"\"\n    xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\">\n   <xmp:Rating>3</xmp:Rating>\n  </rdf:Description>\n </rdf:RDF>\n</x:xmpmeta>\n<?xpacket end=\"w\"?>";

    #[test]
    fn link_round_trips_and_replaces() {
        let x = add_link(PACKET, "xmp.did:aa", "xmp.did:src&1", "IMG_1.CR3");
        let l = read_link(&x);
        assert_eq!(l.document_id.as_deref(), Some("xmp.did:aa"));
        assert_eq!(l.derived_document.as_deref(), Some("xmp.did:src&1"));
        assert_eq!(l.derived_path.as_deref(), Some("IMG_1.CR3"));
        assert!(x.contains("<xmp:Rating>3</xmp:Rating>"));
        assert!(lcc_meta::parse_xmp(&x).is_ok(), "still a valid packet: {x}");
        // a second link replaces the first
        let y = add_link(&x, "xmp.did:bb", "xmp.did:aa", "IMG_1-Edit.psd");
        assert_eq!(y.matches("<xmpMM:DerivedFrom").count(), 1, "{y}");
        assert_eq!(y.matches("xmlns:xmpMM=").count(), 1);
        let l = read_link(&y);
        assert_eq!((l.document_id.as_deref(), l.derived_document.as_deref()), (Some("xmp.did:bb"), Some("xmp.did:aa")));
    }

    #[test]
    fn photoshop_attribute_form_is_read() {
        let x = r#"<rdf:Description xmpMM:DocumentID="xmp.did:me" xmlns:xmpMM="m"><xmpMM:DerivedFrom stRef:instanceID="i" stRef:documentID="xmp.did:parent"/></rdf:Description>"#;
        let l = read_link(x);
        assert_eq!(l.document_id.as_deref(), Some("xmp.did:me"));
        assert_eq!(l.derived_document.as_deref(), Some("xmp.did:parent"));
        assert_eq!(l.derived_path, None);
    }

    #[test]
    fn hostile_input_never_panics() {
        for x in [
            "",
            "<rdf:Description",
            "<xmpMM:DerivedFrom",
            "<xmpMM:DerivedFrom stRef:documentID=\"",
            "</rdf:Description><rdf:Description",
            "<xmpMM:DocumentID>é</xmpMM:DocumentID",
        ] {
            let _ = read_link(x);
            let _ = add_link(x, "a", "b", "c");
        }
        let d = std::env::temp_dir().join(format!("edit-link-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        for (name, bytes) in [
            ("a.tif", b"II*\0\xff\xff\xff\xff".to_vec()),
            ("b.tif", b"MM\0*\0\0\0\x08\xff\xff".to_vec()),
            ("c.psd", b"8BPS\0\x01\0\0\0\0\0\0\0\x03\0\0\0\x01\0\0\0\x01\0\x08\0\x03\xff\xff\xff\xff".to_vec()),
            ("d.jpg", b"\xff\xd8<x:xmpmeta".to_vec()),
        ] {
            let p = d.join(name);
            std::fs::write(&p, bytes).unwrap();
            assert!(file_xmp(&p).is_none(), "{name}");
        }
    }
}
