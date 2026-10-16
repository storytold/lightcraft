//! Explicit, read-only runtime resolution of a user-supplied XMP HSV table.
//!
//! No directories are searched and no table payload is saved. This is deliberately separate
//! from the lenient general metadata reader: a requested table must be one unambiguous scalar
//! in the actual CRS namespace, with an independently verified canonical fingerprint.

use std::{fs::File, io::Read, path::Path};

use lightcraft_raw::profile::big_table::{DecodedLookTable, LookFingerprint, MAX_ENCODED_BYTES, TableError, decode_type0};
use quick_xml::{events::Event, name::ResolveResult, reader::NsReader};

pub const MAX_XMP_BYTES: usize = 2 << 20;
const MAX_DEPTH: usize = 64;
const MAX_EVENTS: usize = 200_000;
const CRS: &[u8] = b"http://ns.adobe.com/camera-raw-settings/1.0/";
const RDF: &[u8] = b"http://www.w3.org/1999/02/22-rdf-syntax-ns#";

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("table XMP input exceeds its size or complexity bound")]
    Limit,
    #[error("table XMP input is not valid UTF-8 XML")]
    Xml,
    #[error("requested table is absent from the supplied XMP")]
    Missing,
    #[error("requested table occurs more than once in the supplied XMP")]
    Duplicate,
    #[error("requested table must be a scalar RDF Description property")]
    Shape,
    #[error("table XMP input is not a regular file")]
    NotFile,
    #[error("cannot read supplied table XMP file: {0:?}")]
    Io(std::io::ErrorKind),
    #[error(transparent)]
    Table(#[from] TableError),
}

fn namespace_is(result: &ResolveResult<'_>, wanted: &[u8]) -> Result<bool, ResolveError> {
    let ResolveResult::Bound(ns) = result else { return Ok(false) };
    // NsReader retains raw xmlns attribute bytes; aliases may spell the URI with entities.
    let raw = std::str::from_utf8(ns.as_ref()).map_err(|_| ResolveError::Xml)?;
    let uri = quick_xml::escape::unescape(raw).map_err(|_| ResolveError::Xml)?;
    if !xml_chars(&uri) {
        return Err(ResolveError::Xml);
    }
    Ok(uri.as_bytes() == wanted)
}

fn xml_chars(text: &str) -> bool {
    text.chars().all(|c| matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}'))
}

fn target_name(name: &[u8], expected: LookFingerprint) -> bool {
    name.strip_prefix(b"Table_").and_then(|s| std::str::from_utf8(s).ok()).and_then(|s| s.parse::<LookFingerprint>().ok()) == Some(expected)
}

fn start_value(value: &mut Option<String>, text: String) -> Result<(), ResolveError> {
    if value.is_some() {
        return Err(ResolveError::Duplicate);
    }
    if text.len() > MAX_ENCODED_BYTES {
        return Err(ResolveError::Limit);
    }
    *value = Some(text);
    Ok(())
}

fn append(value: &mut Option<String>, text: &str) -> Result<(), ResolveError> {
    let value = value.as_mut().ok_or(ResolveError::Shape)?;
    if text.len() > MAX_ENCODED_BYTES.saturating_sub(value.len()) {
        return Err(ResolveError::Limit);
    }
    value.push_str(text);
    Ok(())
}

fn scalar(xmp: &[u8], expected: LookFingerprint) -> Result<String, ResolveError> {
    if xmp.len() > MAX_XMP_BYTES {
        return Err(ResolveError::Limit);
    }
    let text = std::str::from_utf8(xmp).map_err(|_| ResolveError::Xml)?;
    if !xml_chars(text) {
        return Err(ResolveError::Xml);
    }
    let mut reader = NsReader::from_str(text.strip_prefix('\u{feff}').unwrap_or(text));
    reader.config_mut().enable_all_checks(true);
    let mut stack = Vec::<bool>::new(); // Whether each open element is an RDF Description.
    let (mut value, mut capture_depth) = (None, None);
    let (mut roots, mut events) = (0usize, 0usize);
    let mut seen_declaration = false;
    loop {
        events += 1;
        if events > MAX_EVENTS {
            return Err(ResolveError::Limit);
        }
        match reader.read_event().map_err(|_| ResolveError::Xml)? {
            event @ (Event::Start(_) | Event::Empty(_)) => {
                if capture_depth.is_some() {
                    return Err(ResolveError::Shape);
                }
                if stack.len() >= MAX_DEPTH {
                    return Err(ResolveError::Limit);
                }
                if stack.is_empty() {
                    roots += 1;
                    if roots != 1 {
                        return Err(ResolveError::Xml);
                    }
                }
                let (e, empty) = match event {
                    Event::Start(e) => (e, false),
                    Event::Empty(e) => (e, true),
                    _ => return Err(ResolveError::Xml),
                };
                let (ns, local) = reader.resolve_element(e.name());
                if matches!(ns, ResolveResult::Unknown(_)) {
                    return Err(ResolveError::Xml);
                }
                let description = namespace_is(&ns, RDF)? && local.as_ref() == b"Description";
                let table = namespace_is(&ns, CRS)? && target_name(local.as_ref(), expected);
                if table {
                    if stack.last() != Some(&true) {
                        return Err(ResolveError::Shape);
                    }
                    start_value(&mut value, String::new())?;
                }
                for attribute in e.attributes() {
                    let attribute = attribute.map_err(|_| ResolveError::Xml)?;
                    let text = attribute.decode_and_unescape_value(reader.decoder()).map_err(|_| ResolveError::Xml)?;
                    if !xml_chars(&text) {
                        return Err(ResolveError::Xml);
                    }
                    if attribute.key.as_ref() == b"xmlns" || attribute.key.as_ref().starts_with(b"xmlns:") {
                        continue;
                    }
                    if table {
                        return Err(ResolveError::Shape);
                    }
                    let (ns, name) = reader.resolve_attribute(attribute.key);
                    if matches!(ns, ResolveResult::Unknown(_)) {
                        return Err(ResolveError::Xml);
                    }
                    if namespace_is(&ns, CRS)? && target_name(name.as_ref(), expected) {
                        if !description {
                            return Err(ResolveError::Shape);
                        }
                        start_value(&mut value, text.into_owned())?;
                    }
                }
                if !empty {
                    stack.push(description);
                    if table {
                        capture_depth = Some(stack.len());
                    }
                }
            }
            Event::End(_) => {
                if capture_depth == Some(stack.len()) {
                    capture_depth = None;
                }
                if stack.pop().is_none() {
                    return Err(ResolveError::Xml);
                }
            }
            Event::Text(text) => {
                let text = text.decode().map_err(|_| ResolveError::Xml)?;
                if capture_depth.is_some() {
                    append(&mut value, &text)?;
                } else if stack.is_empty() && !text.trim().is_empty() {
                    return Err(ResolveError::Xml);
                }
            }
            Event::CData(text) => {
                if stack.is_empty() {
                    return Err(ResolveError::Xml);
                }
                if capture_depth.is_some() {
                    append(&mut value, &text.decode().map_err(|_| ResolveError::Xml)?)?;
                }
            }
            Event::GeneralRef(reference) => {
                let character = reference.resolve_char_ref().map_err(|_| ResolveError::Xml)?;
                let name = reference.decode().map_err(|_| ResolveError::Xml)?;
                let resolved = if let Some(c) = character {
                    c.to_string()
                } else {
                    quick_xml::escape::resolve_predefined_entity(&name).ok_or(ResolveError::Xml)?.to_string()
                };
                if !xml_chars(&resolved) {
                    return Err(ResolveError::Xml);
                }
                if capture_depth.is_some() {
                    append(&mut value, &resolved)?;
                } else if stack.is_empty() && !resolved.trim().is_empty() {
                    return Err(ResolveError::Xml);
                }
            }
            Event::DocType(_) => return Err(ResolveError::Xml),
            Event::Decl(decl) => {
                if roots != 0 || seen_declaration || decl.version().map_err(|_| ResolveError::Xml)?.as_ref() != b"1.0" {
                    return Err(ResolveError::Xml);
                }
                seen_declaration = true;
                if let Some(encoding) = decl.encoding() {
                    let encoding = encoding.map_err(|_| ResolveError::Xml)?;
                    if !encoding.eq_ignore_ascii_case(b"utf-8") {
                        return Err(ResolveError::Xml);
                    }
                }
            }
            Event::Eof => break,
            Event::PI(_) | Event::Comment(_) => {}
        }
    }
    if roots != 1 || !stack.is_empty() || capture_depth.is_some() {
        return Err(ResolveError::Xml);
    }
    value.ok_or(ResolveError::Missing)
}

/// Resolve only the requested fingerprint in the supplied bytes. Never fall back to another table.
pub fn resolve_type0_xmp(xmp: &[u8], expected: LookFingerprint) -> Result<DecodedLookTable, ResolveError> {
    Ok(decode_type0(&scalar(xmp, expected)?, expected)?)
}

/// Read one explicitly supplied XMP file with a bounded read. No paths or payloads are persisted.
pub fn load_type0_xmp_file(path: &Path, expected: LookFingerprint) -> Result<DecodedLookTable, ResolveError> {
    // Avoid opening a supplied device or FIFO. Recheck the opened handle as well.
    if !std::fs::metadata(path).map_err(|e| ResolveError::Io(e.kind()))?.is_file() {
        return Err(ResolveError::NotFile);
    }
    let file = File::open(path).map_err(|e| ResolveError::Io(e.kind()))?;
    if !file.metadata().map_err(|e| ResolveError::Io(e.kind()))?.is_file() {
        return Err(ResolveError::NotFile);
    }
    let mut bytes = Vec::new();
    file.take((MAX_XMP_BYTES + 1) as u64).read_to_end(&mut bytes).map_err(|e| ResolveError::Io(e.kind()))?;
    resolve_type0_xmp(&bytes, expected)
}

#[cfg(test)]
#[path = "tests_profile_tables.rs"]
mod tests;

/// One explicit, session-only resource. Neither path nor decoded samples enter the catalog.
#[derive(Clone, Debug)]
pub(crate) struct LoadedTable {
    pub table: std::sync::Arc<DecodedLookTable>,
    pub path: std::path::PathBuf,
    pub content: lightcraft_preview::Hash128,
}

fn content_key(table: &DecodedLookTable) -> lightcraft_preview::Hash128 {
    let grid = table.hsv();
    let mut hash = lightcraft_preview::Hasher128::new();
    hash.str("runtime-hsv-table-1")
        .u64(grid.hue_divisions as u64)
        .u64(grid.sat_divisions as u64)
        .u64(grid.val_divisions as u64)
        .u64(table.encoding() as u64);
    let [minimum, maximum] = table.amount_range();
    hash.u64(minimum.to_bits()).u64(maximum.to_bits()).u64(u64::from(table.embed_never()));
    for entry in &grid.data {
        for sample in entry {
            hash.update(&sample.to_bits().to_le_bytes());
        }
    }
    hash.finish()
}

impl crate::Session {
    pub(crate) fn profile_table(&self, settings: &lightcraft_develop::DevelopSettings) -> Option<std::sync::Arc<DecodedLookTable>> {
        let reference = lightcraft_pipeline::active_look_table(settings)?;
        self.profile_tables.get(reference.as_str()).map(|loaded| loaded.table.clone())
    }

    /// Salt only final renders; source pixels and spatial preparation do not contain this resource.
    pub(crate) fn profile_table_key(&self, settings: &lightcraft_develop::DevelopSettings) -> Option<lightcraft_preview::Hash128> {
        let reference = lightcraft_pipeline::active_look_table(settings)?;
        let mut hash = lightcraft_preview::Hasher128::new();
        hash.str("runtime-hsv-pre-tone-entry-amount-v1").str(reference.as_str());
        if let Some(loaded) = self.profile_tables.get(reference.as_str()) {
            hash.str("resolved").update(&loaded.content.0.to_le_bytes());
        } else {
            hash.str("missing");
        }
        Some(hash.finish())
    }

    /// Saved import diagnostics plus current session resource availability. Never persisted.
    pub fn unsupported_edits(&self, id: lightcraft_catalog::PhotoId) -> Vec<String> {
        let Some(photo) = self.catalog.photo(id) else { return Vec::new() };
        let mut request = lightcraft_pipeline::RenderRequest::fit(1, 1);
        request.imported_table = self.profile_table(&photo.develop);
        let runtime = lightcraft_pipeline::profile_table_diagnostics(&photo.develop, &request);
        lightcraft_develop::unsupported_edits::bounded_names(photo.unsupported_edits.iter().chain(runtime.iter()).map(String::as_str))
    }
}

pub(crate) fn specs() -> Vec<crate::cmd::CommandSpec> {
    use crate::cmd::{always, bad, cmd, str_param};
    use serde_json::json;
    vec![
        cmd!("profile.loadTable", "Load Profile Table", [], None, "{fingerprint:string,path:string}", always, |s, p| {
            const C: &str = "profile.loadTable";
            let expected = str_param(p, "fingerprint")
                .ok_or_else(|| bad(C, "missing fingerprint"))?
                .parse::<LookFingerprint>()
                .map_err(|e| bad(C, e.to_string()))?;
            let path = str_param(p, "path").ok_or_else(|| bad(C, "missing path"))?;
            if path.len() > 4096 {
                return Err(bad(C, "table path exceeds 4096 bytes"));
            }
            let id = expected.to_string();
            if s.profile_tables.len() >= 32 && !s.profile_tables.contains_key(&id) {
                return Err(bad(C, "32 profile tables are loaded; unload one before loading another"));
            }
            // Validate completely before replacing an existing resource or invalidating any job.
            let table = load_type0_xmp_file(Path::new(path), expected).map_err(|e| bad(C, e.to_string()))?;
            let content = content_key(&table);
            let changed = s.profile_tables.get(&id).is_none_or(|previous| previous.table.as_ref() != &table);
            s.profile_tables.insert(id.clone(), LoadedTable { table: std::sync::Arc::new(table), path: path.into(), content });
            if changed {
                s.media.rendered.invalidate_memory();
            }
            Ok(json!({"loaded":id,"changed":changed,"sessionOnly":true}))
        }),
        cmd!("profile.unloadTable", "Unload Profile Table", [], None, "{fingerprint:string}", always, |s, p| {
            const C: &str = "profile.unloadTable";
            let id = str_param(p, "fingerprint")
                .ok_or_else(|| bad(C, "missing fingerprint"))?
                .parse::<LookFingerprint>()
                .map_err(|e| bad(C, e.to_string()))?
                .to_string();
            let removed = s.profile_tables.remove(&id).is_some();
            if removed {
                s.media.rendered.invalidate_memory();
            }
            Ok(json!({"unloaded":id,"removed":removed}))
        }),
    ]
}
