//! All Metadata (`photo.allMetadata`) for one photo or several: every EXIF / TIFF / GPS tag of
//! the files and their XMP properties. With several photos the rows are merged into the fields
//! they share, the rest marked `mixed`.
//!
//! Reading every selected file can take a while (raws on a slow drive), so the work is a
//! [`MetadataJob`] detached from the session: the UI runs it on a worker thread with a row in
//! the activity stack; the command runs it inline.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lightcraft_catalog::{PhotoId, Source};
use serde_json::{Value, json};

use crate::merge::ByteReader;
use crate::{EngineError, Result, Session};

/// The most photos one comparison reads: every one of them is read from disk.
pub const MAX_PHOTOS: usize = 100;

/// One photo to read: its file name and, for a photo with a file, the file and its sidecar.
struct MetadataSource {
    file_name: String,
    file: Option<(String, Option<PathBuf>)>,
}

/// The All Metadata reads for a set of photos, detached from the session ([`Session::all_metadata_job`]).
pub struct MetadataJob {
    photos: Vec<MetadataSource>,
    read: ByteReader,
}

/// One photo's rows: (EXIF / TIFF / GPS rows, XMP rows).
type Rows = (Vec<Value>, Vec<Value>);

impl Session {
    /// The All Metadata job for `ids`: one to [`MAX_PHOTOS`] photos of the library.
    pub fn all_metadata_job(&self, ids: &[PhotoId]) -> Result<MetadataJob> {
        const C: &str = "photo.allMetadata";
        if ids.is_empty() {
            return Err(crate::cmd::bad(C, "no photo"));
        }
        if ids.len() > MAX_PHOTOS {
            return Err(crate::cmd::bad(C, format!("compares at most {MAX_PHOTOS} photos ({} given)", ids.len())));
        }
        let mut photos = Vec::with_capacity(ids.len());
        for &id in ids {
            let ph = self.catalog.photo(id).ok_or_else(|| crate::cmd::bad(C, format!("no such photo {}", id.0)))?;
            let file = match &ph.source {
                Source::File { path } => Some((path.clone(), crate::sidecar::find_sidecar(path, self.sidecar_naming(id)))),
                _ => None,
            };
            photos.push(MetadataSource { file_name: ph.file_name.clone(), file });
        }
        let read: ByteReader = match &self.media.file_bytes {
            Some(r) => r.clone(),
            None => Arc::new(|path: &str| std::fs::read(path).map_err(|e| format!("{path}: {e}"))),
        };
        Ok(MetadataJob { photos, read })
    }
}

impl MetadataJob {
    /// How many photos the job reads.
    pub fn len(&self) -> usize {
        self.photos.len()
    }

    /// Whether the job has no photo (never, for a job from [`Session::all_metadata_job`]).
    pub fn is_empty(&self) -> bool {
        self.photos.is_empty()
    }

    /// Read the photos' files, reporting `progress(done, total)` before the first and after each, and stop with an
    /// error once `cancel` is set. Returns `{exif, xmp, photos}` (see `photo.allMetadata`):
    /// `photos` counts the photos compared. A single photo whose file can't be read is an error;
    /// among several such a photo is left out of the comparison and named in `unreadable`, and
    /// it is an error only when no photo could be read.
    pub fn run(&self, cancel: &AtomicBool, progress: &dyn Fn(u64, u64)) -> Result<Value> {
        let total = u64::try_from(self.photos.len()).unwrap_or(u64::MAX);
        let mut each: Vec<Option<Rows>> = Vec::with_capacity(self.photos.len());
        let mut compared: Vec<&str> = Vec::with_capacity(self.photos.len());
        let mut unreadable: Vec<(&str, String)> = Vec::new();
        progress(0, total);
        for (i, photo) in self.photos.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Err(EngineError::Cancelled("All Metadata cancelled".into()));
            }
            match &photo.file {
                None => {
                    each.push(None);
                    compared.push(&photo.file_name);
                }
                Some((path, sidecar)) => match self.rows(path, sidecar.as_ref()) {
                    Ok(rows) => {
                        each.push(Some(rows));
                        compared.push(&photo.file_name);
                    }
                    Err(e) => unreadable.push((photo.file_name.as_str(), e)),
                },
            }
            progress(u64::try_from(i).unwrap_or(u64::MAX).saturating_add(1), total);
        }
        if each.is_empty() {
            let msg = unreadable.into_iter().next().map_or_else(|| "no photo".to_string(), |(_, e)| e);
            return Err(crate::cmd::bad("photo.allMetadata", msg));
        }
        let mut out = if each.iter().all(Option::is_none) {
            json!({"exif": [], "xmp": [], "photos": each.len(), "note": "a generated demo photo has no file"})
        } else {
            let each: Vec<Rows> = each.into_iter().map(Option::unwrap_or_default).collect();
            let exif = shared_rows(each.iter().map(|(e, _)| e.as_slice()), &["group", "tag"]);
            let xmp = shared_rows(each.iter().map(|(_, x)| x.as_slice()), &["name"]);
            json!({"exif": exif, "xmp": xmp, "photos": each.len()})
        };
        if let [one] = compared.as_slice() {
            out["fileName"] = json!(one);
        }
        if !unreadable.is_empty() {
            out["unreadable"] = json!(unreadable.iter().map(|(name, e)| json!({"fileName": name, "error": e})).collect::<Vec<_>>());
        }
        Ok(out)
    }

    /// One file's rows, its XMP from the sidecar when there is one, else embedded in the file.
    fn rows(&self, path: &str, sidecar: Option<&PathBuf>) -> std::result::Result<Rows, String> {
        let bytes = (self.read)(path)?;
        let exif: Vec<Value> = lightcraft_meta::file_tag_rows(&bytes)
            .into_iter()
            .map(|r| json!({"group": r.group, "tag": r.tag, "name": r.name, "value": r.value}))
            .collect();
        // Keep sidecar precedence; reuse the loaded bytes for embedded XMP.
        let packet = match sidecar {
            Some(sc) => std::fs::read_to_string(sc).ok(),
            None => lightcraft_meta::embedded(&bytes).xmp,
        };
        let xmp: Vec<Value> = packet
            .and_then(|x| lightcraft_meta::parse_xmp(&x).ok())
            .map(|d| {
                d.properties.into_iter().filter(|(k, _)| !k.starts_with("lc:")).map(|(k, v)| json!({"name": k, "value": v.join("; ")})).collect()
            })
            .unwrap_or_default();
        Ok((exif, xmp))
    }
}

/// The rows of several photos as one list, a row per field (`key` names the fields that tell
/// rows apart), in the order the fields first appear. A field every photo has with the same
/// value keeps it; one whose values differ, or that some photos lack, gets `mixed: true` and an
/// empty value. One photo's rows come back as they are.
fn shared_rows<'a>(photos: impl Iterator<Item = &'a [Value]> + Clone, key: &[&str]) -> Vec<Value> {
    let n = photos.clone().count();
    if n <= 1 {
        return photos.flat_map(<[Value]>::to_vec).collect();
    }
    let key_of = |row: &Value| -> String { Value::Array(key.iter().map(|k| row.get(*k).cloned().unwrap_or(Value::Null)).collect()).to_string() };
    // per field: its first row, how many photos have it with that row's value, whether one differs
    let mut fields: Vec<(Value, usize, bool)> = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    for rows in photos {
        // a field a file lists twice counts once for that photo
        let mut seen: Vec<usize> = Vec::new();
        for row in rows {
            let i = *at.entry(key_of(row)).or_insert_with(|| {
                fields.push((row.clone(), 0, false));
                fields.len().saturating_sub(1)
            });
            if seen.contains(&i) {
                continue;
            }
            seen.push(i);
            let Some(f) = fields.get_mut(i) else { continue };
            if f.0.get("value") == row.get("value") {
                f.1 = f.1.saturating_add(1);
            } else {
                f.2 = true;
            }
        }
    }
    fields
        .into_iter()
        .map(|(mut row, same, differs)| {
            if differs || same < n {
                row["value"] = json!("");
                row["mixed"] = json!(true);
            }
            row
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rows that differ, are missing from one photo or are listed twice by one file.
    #[test]
    fn shared_rows_marks_what_the_photos_do_not_share() {
        let a = vec![json!({"name": "A", "value": "1"}), json!({"name": "B", "value": "x"}), json!({"name": "B", "value": "x"})];
        let b = vec![json!({"name": "A", "value": "1"}), json!({"name": "B", "value": "y"}), json!({"name": "C", "value": "z"})];
        let rows = shared_rows([a.as_slice(), b.as_slice()].into_iter(), &["name"]);
        let got: Vec<(String, String, bool)> = rows
            .iter()
            .map(|r| (r["name"].as_str().unwrap().into(), r["value"].as_str().unwrap().into(), r["mixed"].as_bool().unwrap_or(false)))
            .collect();
        assert_eq!(got, vec![("A".into(), "1".into(), false), ("B".into(), "".into(), true), ("C".into(), "".into(), true)]);
        // one photo: its rows untouched, duplicates included
        assert_eq!(shared_rows([a.as_slice()].into_iter(), &["name"]), a);
    }
}
