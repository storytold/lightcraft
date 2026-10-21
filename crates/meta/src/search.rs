//! Disposable, bounded search values. Original packets and workflow JSON are never retained.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SEARCH_BYTES: usize = 16 * 1024;
const VALUE_BYTES: usize = 4096;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchMetadata {
    pub fields: BTreeMap<String, Vec<String>>,
    pub comfy: bool,
}

impl SearchMetadata {
    pub fn values(&self, field: &str) -> impl Iterator<Item = &str> {
        self.fields.get(field).into_iter().flatten().map(String::as_str)
    }

    /// The query is already lower-case.
    pub fn contains(&self, query: &str) -> bool {
        self.fields.iter().any(|(k, vs)| k.contains(query) || vs.iter().any(|v| v.contains(query)))
    }

    /// `key=value` limits a match to a metadata property.
    pub fn matches(&self, query: &str) -> bool {
        let (key, value) = query.split_once('=').map_or(("", query), |(k, v)| (k, v));
        self.fields.iter().any(|(k, vs)| k.contains(key) && (key.is_empty() && k.contains(value) || vs.iter().any(|v| v.contains(value))))
    }

    pub fn byte_len(&self) -> usize {
        self.fields.iter().map(|(k, vs)| k.len() + vs.iter().map(String::len).sum::<usize>()).sum()
    }

    /// Cache files are untrusted too; reject entries outside the extractor's limits.
    pub fn valid(&self) -> bool {
        self.fields.len() <= 512
            && self.byte_len() <= SEARCH_BYTES
            && self.fields.iter().all(|(k, vs)| k.len() <= 256 && vs.len() <= 512 && vs.iter().all(|v| v.len() <= VALUE_BYTES))
    }

    fn add(&mut self, key: &str, value: &str) {
        if self.fields.len() >= 512 || self.byte_len() >= SEARCH_BYTES {
            return;
        }
        let key = limited(key, 256);
        let value = limited(value.trim(), VALUE_BYTES);
        if key.is_empty() || value.is_empty() || self.byte_len() + key.len() + value.len() > SEARCH_BYTES {
            return;
        }
        let values = self.fields.entry(key).or_default();
        if values.len() < 512 && !values.contains(&value) {
            values.push(value);
        }
    }

    fn json(&mut self, key: &str, value: &Value, depth: usize, visits: &mut usize) {
        if depth > 24 || key.len() > 256 || *visits >= 10_000 || self.byte_len() >= SEARCH_BYTES {
            return;
        }
        *visits += 1;
        match value {
            Value::String(v) => self.add(key, v),
            Value::Number(v) => self.add(key, &v.to_string()),
            Value::Bool(v) => self.add(key, &v.to_string()),
            Value::Object(o) => {
                for (k, v) in o.iter().take(512) {
                    if !opaque(k) {
                        self.json(&format!("{key}.{k}"), v, depth + 1, visits);
                    }
                }
            }
            Value::Array(a) => {
                for v in a.iter().take(512) {
                    self.json(key, v, depth + 1, visits);
                }
            }
            Value::Null => {}
        }
    }

    fn text(&mut self, key: &str, text: &str) {
        if text.len() > crate::png_text::TEXT_LIMIT {
            return;
        }
        let trimmed = text.trim_start();
        if trimmed.starts_with(['{', '[']) {
            if let Ok(v) = serde_json::from_str::<Value>(text) {
                self.json(key, &v, 0, &mut 0);
            }
        } else {
            self.add(key, text);
        }
    }

    /// Add custom XMP properties, preserving catalog ownership of editable standard fields.
    pub fn add_xmp(&mut self, packet: &str) {
        self.packet(packet, "xmp");
    }

    pub fn replace_sidecar(&mut self, packet: Option<&str>) {
        self.fields.retain(|key, _| !key.starts_with("sidecar."));
        if let Some(packet) = packet {
            self.packet(packet, "sidecar");
        }
    }

    fn packet(&mut self, packet: &str, prefix: &str) {
        if packet.len() > crate::png_text::TEXT_LIMIT {
            return;
        }
        let Ok(xmp) = crate::parse_xmp(packet) else { return };
        for (key, values) in xmp.properties {
            if key.len() > 248 {
                continue;
            }
            let k = key.to_lowercase();
            if editable_xmp(&k) || opaque(&k) {
                continue;
            }
            for value in values.iter().take(512) {
                self.text(&format!("{prefix}.{k}"), value);
            }
        }
    }
}

fn limited(text: &str, bytes: usize) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .scan(0, |n, c| {
            *n += c.len_utf8();
            (*n <= bytes).then_some(c)
        })
        .collect()
}

fn opaque(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.contains("base64") || key.contains("thumbnail") || key.contains("iccprofile") || key == "lc:settings" || key.starts_with("crs:")
}

fn editable_xmp(key: &str) -> bool {
    matches!(
        key,
        "dc:subject"
            | "lr:hierarchicalsubject"
            | "dc:title"
            | "dc:description"
            | "dc:creator"
            | "dc:rights"
            | "xmp:rating"
            | "xmp:label"
            | "photoshop:city"
            | "photoshop:state"
            | "photoshop:country"
            | "iptc4xmpcore:location"
            | "xmprights:marked"
            | "xmprights:usageterms"
            | "xmprights:webstatement"
            | "lc:location"
            | "iptc4xmpcore:alttextaccessibility"
            | "iptc4xmpcore:extdescraccessibility"
    ) || key.starts_with("mwg-rs:")
        || key.starts_with("iptc4xmpext:")
}

/// Read during import/refresh only. Uses the existing container, EXIF, XMP and PNG decoders.
pub fn search_metadata(bytes: &[u8]) -> SearchMetadata {
    let embedded = crate::embedded(bytes);
    from_embedded(bytes, &embedded)
}

pub(crate) fn from_embedded(bytes: &[u8], embedded: &crate::Embedded) -> SearchMetadata {
    let mut index = SearchMetadata::default();
    if let Some(comfy) = &embedded.comfy {
        index.comfy = true;
        for (key, values) in &comfy.fields {
            for value in values {
                index.add(key, value);
            }
        }
    }
    for row in crate::file_tag_rows(embedded.exif.as_deref().unwrap_or_default()) {
        // Editable descriptive fields are searched from the catalog; binary blobs have no text.
        if matches!(row.tag, 0x010e | 0x013b | 0x8298 | 0x927c) || row.value.starts_with('[') || row.value.ends_with(" bytes)") {
            continue;
        }
        index.text(&format!("{}.{}", row.group, row.name), &row.value);
    }
    for packet in [embedded.xmp.as_deref(), embedded.xmp_extended.as_deref()].into_iter().flatten() {
        index.add_xmp(packet);
    }
    for field in crate::png_text_fields(bytes) {
        // The semantic graph index already covers ComfyUI without indexing a graph as prompt text.
        if index.comfy && matches!(field.name.as_str(), "prompt" | "workflow") {
            continue;
        }
        if !opaque(&field.name) {
            index.text(&format!("png.{}", field.name), &field.value);
        }
    }
    index
}
