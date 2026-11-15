//! A catalogue photo as sync sees it: a handful of fields, each merged on its own.
//!
//! | field | what |
//! |---|---|
//! | `develop` | the develop settings (with the flat-field map they reference) |
//! | `rating`, `flag`, `label` | stars, pick / reject, colour label |
//! | `meta` | what a person types: title, caption, keywords, location, creator, copyright |
//! | `deleted` | in Recently Deleted |
//!
//! History and versions stay on each device. Virtual copies don't travel (yet).

use std::collections::BTreeMap;
use std::sync::Arc;

use lcc_catalog::{Catalog, Op, Photo, PhotoId};
use lcc_develop::DevelopSettings;
use serde_json::{Value, json};

use crate::{EngineError, Result, Session};

pub(super) const FIELDS: [&str; 6] = ["develop", "rating", "flag", "label", "meta", "deleted"];

/// The metadata a person edits (the rest is read from the file, the same on every device).
const META: [&str; 23] = [
    "title",
    "headline",
    "caption",
    "alt_text",
    "extended_description",
    "description_writer",
    "category",
    "supplemental_categories",
    "subject_code",
    "keywords",
    "creator",
    "contact",
    "copyright",
    "copyright_status",
    "usage_terms",
    "copyright_url",
    "location",
    "city",
    "state",
    "country",
    "country_code",
    "gps",
    "gps_removed",
];

/// What a photo's key starts with when it is named by its original's SHA-1.
pub(crate) const SHA1_KEY: &str = "sha1:";

/// The key that names the same photo on every device (`docs/sync.md` ▸ Photo identity):
///
/// - `sha1:<hex>`, the SHA-1 of the original's bytes, once it is known: hashed here (every
///   catalogue hashes its originals in the background), or, for an Immich link-only photo whose
///   original isn't here, the checksum Immich reported for it. A device holding the file and a
///   device holding only the Immich link so give the same photo the same key.
/// - otherwise the catalogue's content hash of the original (a photo not hashed yet); it moves to
///   the SHA-1 key once that is known ([`lcc_sync::Replica::rekey`]).
///
/// Virtual copies, a second import of the same bytes and link-only photos without a checksum
/// have none.
pub(crate) fn key_of(p: &Photo, cat: &Catalog) -> Option<String> {
    if p.copy_of.is_some() {
        return None;
    }
    if p.preview_only.as_deref() == Some(crate::remote::LINK_ONLY) {
        // the file here is Immich's preview: only the checksum of the original names it
        return cat
            .remote_of(p.id)
            .filter(|r| r.service == lcc_immich::link::SERVICE)
            .find_map(|r| r.remote_checksum.as_deref().and_then(sha1_key));
    }
    if let Some(k) = p.sha1.as_deref().and_then(sha1_key) {
        return Some(k);
    }
    content_key(p).map(str::to_string)
}

/// The photo's content-hash key, which another device may still use for it (`None` for a
/// link-only photo: its content hash is the preview's).
pub(crate) fn content_key(p: &Photo) -> Option<&str> {
    if p.copy_of.is_some() || p.preview_only.as_deref() == Some(crate::remote::LINK_ONLY) {
        return None;
    }
    p.content_hash.as_deref().filter(|h| ok_content_key(h))
}

fn ok_content_key(h: &str) -> bool {
    !h.is_empty() && h.len() <= 128 && !h.contains(":dup") && !h.starts_with(SHA1_KEY) && !h.chars().any(char::is_control)
}

/// `sha1:<lowercase hex>` of a SHA-1 given as hex or base64 (`None` if it isn't one).
pub(crate) fn sha1_key(checksum: &str) -> Option<String> {
    lcc_hash::Sha1Digest::parse(checksum).map(|d| format!("{SHA1_KEY}{}", d.to_hex()))
}

/// The SHA-1 (hex) a key names, if it is a SHA-1 key.
pub(crate) fn sha1_of_key(key: &str) -> Option<String> {
    key.strip_prefix(SHA1_KEY).and_then(lcc_hash::Sha1Digest::parse).map(|d| d.to_hex())
}

fn develop_is_default(d: &DevelopSettings) -> bool {
    // (new photos start with Constrain Crop on: that is not an edit)
    let plain = DevelopSettings { geometry: lcc_develop::Geometry { constrain_crop: false, ..d.geometry.clone() }, ..d.clone() };
    plain == DevelopSettings::default()
}

fn meta_value(p: &Photo) -> Value {
    let all = serde_json::to_value(&p.meta).unwrap_or(Value::Null);
    let plain = serde_json::to_value(lcc_catalog::Meta::default()).unwrap_or(Value::Null);
    let mut out = serde_json::Map::new();
    for k in META {
        // (only what differs from an empty record travels, so a field a build adds changes nothing)
        if let Some(v) = all.get(k).filter(|v| !is_empty(v) && plain.get(k) != Some(*v)) {
            out.insert(k.to_string(), v.clone());
        }
    }
    Value::Object(out)
}

fn is_empty(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        _ => false,
    }
}

/// One field's value, and whether it is what a photo nobody touched has.
pub(crate) fn value(p: &Photo, cat: &Catalog, field: &str) -> Option<(Value, bool)> {
    Some(match field {
        "develop" => {
            let maps: Vec<_> =
                p.develop.optics.vignetting_calibration.as_ref().and_then(|id| cat.vignetting_map(id)).map(|r| r.map.clone()).into_iter().collect();
            let mut v = json!({"s": &*p.develop});
            if !maps.is_empty() {
                v["maps"] = serde_json::to_value(&maps).unwrap_or(Value::Null);
            }
            (v, develop_is_default(&p.develop))
        }
        "rating" => (json!(p.rating), p.rating == 0),
        "flag" => (serde_json::to_value(p.flag).unwrap_or(Value::Null), p.flag == lcc_catalog::Flag::None),
        "label" => (serde_json::to_value(p.label).unwrap_or(Value::Null), p.label.is_none()),
        "meta" => {
            let v = meta_value(p);
            let empty = is_empty(&v);
            (v, empty)
        }
        "deleted" => (json!(p.deleted), !p.deleted),
        _ => return None,
    })
}

pub(super) fn values(p: &Photo, cat: &Catalog) -> BTreeMap<String, Value> {
    FIELDS.iter().filter_map(|f| value(p, cat, f).map(|(v, _)| (f.to_string(), v))).collect()
}

/// `(field, fingerprint, is the default)` of every field.
pub(super) fn digests(p: &Photo, cat: &Catalog) -> Vec<(&'static str, u64, bool)> {
    FIELDS.iter().filter_map(|f| value(p, cat, f).map(|(v, default)| (*f, lcc_sync::digest(&v), default))).collect()
}

pub(super) fn digest(p: &Photo, cat: &Catalog, field: &str) -> u64 {
    value(p, cat, field).map_or(0, |(v, _)| lcc_sync::digest(&v))
}

/// What another device needs to list a photo it doesn't hold: the photo without what belongs to
/// this device (where its file is, its history) and without the fields that travel on their own.
pub(super) fn info(p: &Photo) -> Value {
    let mut q = p.clone();
    q.source = lcc_catalog::Source::File { path: String::new() };
    q.history.clear();
    q.history_cursor = None;
    q.versions.clear();
    q.develop = Arc::new(DevelopSettings::default());
    q.before = None;
    q.import_look = None;
    q.import_state = None;
    q.exported = None;
    q.local = false;
    q.local_baseline = None;
    q.deleted = false;
    q.rating = 0;
    q.flag = lcc_catalog::Flag::None;
    q.label = None;
    if q.preview_only.as_deref() == Some(crate::remote::LINK_ONLY) {
        // what is here is Immich's preview: its hash, size and SHA-1 aren't the original's
        q.content_hash = None;
        q.sha1 = None;
        q.preview_only = None;
    }
    serde_json::to_value(&q).unwrap_or(Value::Null)
}

/// The largest facts record taken from another device (a photo's own is a few kB).
const MAX_INFO: usize = 4 << 20;

/// A catalogue photo from another device's facts, to live at `path`.
pub(super) fn photo_from(info: &Value, key: &str, id: PhotoId, path: &str, imported: &str) -> std::result::Result<Photo, String> {
    if serde_json::to_string(info).map_or(usize::MAX, |s| s.len()) > MAX_INFO {
        return Err("the photo's record is too large".into());
    }
    let mut p: Photo = serde_json::from_value(info.clone()).map_err(|e| format!("the other device's photo record can't be read: {e}"))?;
    let name: String = p.file_name.chars().filter(|c| !c.is_control() && !matches!(c, '/' | '\\' | ':')).take(200).collect();
    if name.trim_matches('.').trim().is_empty() {
        return Err("the photo has no usable file name".into());
    }
    p.file_name = name;
    p.id = id;
    p.source = lcc_catalog::Source::File { path: path.to_string() };
    p.imported = imported.to_string();
    match sha1_of_key(key) {
        // named by its SHA-1: the original's content hash comes with it when the sender knows it
        Some(sha1) => {
            p.sha1 = Some(sha1);
            p.content_hash = p.content_hash.take().filter(|h| ok_content_key(h));
        }
        None => {
            p.content_hash = Some(key.to_string());
            p.sha1 = p.sha1.take().and_then(|h| lcc_hash::Sha1Digest::parse(&h)).map(|d| d.to_hex());
        }
    }
    // (what the other device shows of it is no business of this one)
    p.preview_only = None;
    p.copy_of = None;
    p.copy_name = None;
    p.local = false;
    p.local_baseline = None;
    p.deleted = false;
    p.history.clear();
    p.history_cursor = None;
    p.versions.clear();
    p.before = None;
    p.exported = None;
    p.develop = Arc::new(DevelopSettings::default());
    p.rating = 0;
    p.flag = lcc_catalog::Flag::None;
    p.label = None;
    p.width = p.width.min(1 << 20);
    p.height = p.height.min(1 << 20);
    Ok(p)
}

fn bad(field: &str, e: impl std::fmt::Display) -> EngineError {
    EngineError::Other(format!("the other device's `{field}` can't be read: {e}"))
}

/// Writes another device's value of one field onto photo `id`. Not an undo step: undoing a
/// change another device made would only send the old state back as a new edit.
pub(super) fn apply(s: &mut Session, id: PhotoId, field: &str, v: &Value) -> Result<()> {
    let Some(p) = s.catalog.photo(id).cloned() else { return Ok(()) };
    let op = match field {
        "develop" => {
            let settings: DevelopSettings = serde_json::from_value(v.get("s").cloned().unwrap_or(Value::Null)).map_err(|e| bad(field, e))?;
            if let Some(maps) = v.get("maps") {
                let maps: Vec<lcc_develop::EmbeddedVignettingMap> = serde_json::from_value(maps.clone()).map_err(|e| bad(field, e))?;
                // the other catalogue's flat-field map comes with its settings: stored first
                for m in maps.into_iter().take(4) {
                    s.offer_vignetting_calibration(m);
                }
            }
            if *p.develop == settings {
                return Ok(());
            }
            let edited = (!develop_is_default(&settings)).then(|| (s.clock)());
            Op::SetDevelop { id, settings: Arc::new(settings), label: "Sync".into(), edited }
        }
        "rating" => {
            let rating = v.as_u64().map_or(0, |r| r.min(5) as u8);
            if p.rating == rating {
                return Ok(());
            }
            Op::SetRating { id, rating }
        }
        "flag" => {
            let flag: lcc_catalog::Flag = serde_json::from_value(v.clone()).map_err(|e| bad(field, e))?;
            if p.flag == flag {
                return Ok(());
            }
            Op::SetFlag { id, flag }
        }
        "label" => {
            let label: Option<lcc_catalog::ColorLabel> = serde_json::from_value(v.clone()).map_err(|e| bad(field, e))?;
            if p.label == label {
                return Ok(());
            }
            Op::SetLabel { id, label }
        }
        "meta" => {
            let mut all = serde_json::to_value(&p.meta).map_err(|e| bad(field, e))?;
            let Some(obj) = all.as_object_mut() else { return Ok(()) };
            for k in META {
                match v.get(k) {
                    Some(x) => {
                        obj.insert(k.to_string(), x.clone());
                    }
                    None => {
                        obj.remove(k);
                    }
                }
            }
            let meta: lcc_catalog::Meta = serde_json::from_value(all).map_err(|e| bad(field, e))?;
            if p.meta == meta {
                return Ok(());
            }
            Op::SetMeta { id, meta: Box::new(meta) }
        }
        "deleted" => {
            let deleted = v.as_bool().unwrap_or(false);
            if p.deleted == deleted {
                return Ok(());
            }
            Op::SetDeleted { id, deleted }
        }
        // a field of a newer build: not this build's to write
        _ => return Ok(()),
    };
    s.apply_system(op)
}

/// Keeps the photo's current settings as a version before another device's later edit replaces
/// an edit made here that the other device never saw.
pub(super) fn keep_version(s: &mut Session, id: PhotoId, from: &str) {
    let Some(p) = s.catalog.photo(id) else { return };
    if develop_is_default(&p.develop) || p.versions.last().is_some_and(|v| *v.settings == *p.develop) {
        return;
    }
    let mut versions = p.versions.clone();
    let created = (s.clock)();
    let name: String = format!("Before sync from {from}").chars().take(120).collect();
    versions.push(lcc_catalog::Version { name, created, settings: p.develop.clone(), auto: false });
    let _ = s.apply_system(Op::SetVersions { id, versions });
}
