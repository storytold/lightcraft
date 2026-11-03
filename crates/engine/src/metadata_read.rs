//! A read-only metadata request that can be moved to a UI worker.

use crate::{Session, merge::ByteReader, sidecar::SidecarLookup};
use lightcraft_catalog::{PhotoId, Source};
use serde_json::{Value, json};

pub struct Request {
    source: Source,
    read: Option<ByteReader>,
    sidecar: SidecarLookup,
}

impl Request {
    pub fn new(session: &Session, id: PhotoId) -> Result<Self, String> {
        let photo = session.catalog.photo(id).ok_or("no such photo")?;
        Ok(Self { source: photo.source.clone(), read: session.media.file_bytes.clone(), sidecar: session.sidecar_lookup(id) })
    }

    pub fn read(self) -> Result<Value, String> {
        self.read_with_diagnostics().map(|(value, _)| value)
    }

    pub fn read_with_diagnostics(self) -> Result<(Value, Option<String>), String> {
        let Source::File { path } = self.source else {
            return Ok((json!({"exif": [], "xmp": [], "note": "a generated demo photo has no file"}), None));
        };
        let bytes = match self.read {
            Some(read) => read(&path),
            None => std::fs::read(&path).map_err(|e| format!("{path}: {e}")),
        }?;
        let exif: Vec<Value> = lightcraft_meta::file_tag_rows(&bytes)
            .into_iter()
            .map(|row| json!({"group": row.group, "tag": row.tag, "name": row.name, "value": row.value}))
            .collect();
        let packet = match self.sidecar.find(&path) {
            Some(sidecar) => std::fs::read_to_string(&sidecar).map(Some).map_err(|error| format!("{}: {error}", sidecar.display())),
            None => Ok(lightcraft_meta::embedded(&bytes).xmp),
        };
        let parsed =
            packet.and_then(|packet| packet.map(|xmp| lightcraft_meta::parse_xmp(&xmp).map_err(|error| format!("XMP: {error}"))).transpose());
        let warning = parsed.as_ref().err().cloned();
        let xmp: Vec<Value> = parsed
            .ok()
            .flatten()
            .map(|d| {
                d.properties
                    .into_iter()
                    .filter(|(key, _)| !key.starts_with("lc:"))
                    .map(|(key, values)| json!({"name": key, "value": values.join("; ")}))
                    .collect()
            })
            .unwrap_or_default();
        Ok((json!({"exif": exif, "xmp": xmp}), warning))
    }
}
