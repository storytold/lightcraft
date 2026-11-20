//! A read Capture One catalog as the catalog import's model ([`CatalogImport`]).
//!
//! The Lightroom catalog import turns that model into library records: it reads the originals
//! that are there, keeps the ones that are not as offline records, makes virtual copies and
//! albums, recognises an earlier import of the same catalog and does all of it in one undo step.
//! A Capture One catalog goes the same way, written in that model's terms: an image's first
//! variant is the master and its further variants are virtual copies, a project or group is a
//! collection set, and IPTC fields travel as an XMP packet.
//!
//! A smart album keeps its rule where LightCraft has a rule for every condition, else it holds
//! the photos its rule selects (see [`super::smart`]). Not carried yet, and said so in the
//! import's warnings: adjustments. The orange and pink colour tags have no label in the library: the colour is kept with the photo
//! (`Meta::label_text`) and a keyword finds it. Each photo's row keeps what was read under
//! `captureOne`, so the import's recovery archive has it.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::atomic::AtomicBool;

use serde_json::{Value, json};

use super::notice::{self, Notice};
use super::{CaptureOneCatalog, CatalogPhoto, Collection, CollectionKind, ColorTag, Original, OriginalKind, smart};
use crate::lightroom_catalog::{CatalogImport, CatalogPhoto as ImportPhoto};

/// Most warnings that name a single photo or album; the rest are counted.
const MAX_NAMED: usize = 50;
/// What marks a catalog id and a photo's identity as Capture One's in the import's index.
const PREFIX: &str = "captureone:";
/// The keyword the colour keywords are inside.
pub(super) const KEYWORD_PARENT: &str = "Capture One";

/// Read the Capture One catalog at `path` for the catalog import.
pub(crate) fn read_for_import(path: &Path, cancel: &AtomicBool) -> Result<CatalogImport, String> {
    let mut data = super::read_with_cancel(path, cancel).map(to_import).map_err(|e| e.to_string())?;
    // before the import reads any original: a catalog must not name a program as a photo (#2135)
    crate::lightroom_catalog::refuse_non_media(&mut data);
    Ok(data)
}

/// The program whose catalog `data` was read from, for the import's own messages.
pub(crate) fn program(data: &CatalogImport) -> &'static str {
    if is_capture_one(data) { "Capture One" } else { "Lightroom" }
}

/// Whether `data` was read from a Capture One catalog.
pub(crate) fn is_capture_one(data: &CatalogImport) -> bool {
    data.catalog_id.as_deref().is_some_and(|id| id.starts_with(PREFIX)) || super::locate::is_database_name(&data.source)
}

pub(crate) fn to_import(catalog: CaptureOneCatalog) -> CatalogImport {
    let mut warnings = catalog.warnings;
    if catalog.trashed > 0 {
        warnings.push(Notice::new(notice::TRASHED).count("n", catalog.trashed));
    }
    let count = |test: fn(&CatalogPhoto) -> bool| catalog.photos.iter().filter(|p| test(p)).count();
    // the colours the library has no label for, each named with the keyword that finds it
    let kept: Vec<&str> = [ColorTag::Orange, ColorTag::Pink]
        .into_iter()
        .filter(|tag| catalog.photos.iter().any(|p| p.color_tag == *tag))
        .filter_map(kept_label)
        .collect();
    let unlabelled = count(|p| kept_label(p.color_tag).is_some());
    if unlabelled > 0 {
        let keyword = |name: &str| format!("{KEYWORD_PARENT}|{name}");
        warnings.push(match kept.as_slice() {
            [first, second, ..] => {
                Notice::new(notice::KEPT_COLOURS).count("n", unlabelled).text_value("first", keyword(first)).text_value("second", keyword(second))
            }
            names => Notice::new(notice::KEPT_COLOUR).count("n", unlabelled).text_value("keyword", keyword(names.first().copied().unwrap_or(""))),
        });
    }
    let adjusted = count(|p| p.adjusted);
    if adjusted > 0 {
        warnings.push(Notice::new(notice::ADJUSTED).count("n", adjusted));
    }

    // an image's first variant is the master of its further ones
    let mut masters: HashMap<i64, i64> = HashMap::new();
    let mut photos = Vec::with_capacity(catalog.photos.len());
    let mut refused = 0usize;
    // whose rows a photo without UUIDs is keyed on: the catalog's id, which its backups share
    let rows_of = catalog.document_uuid.clone().unwrap_or_else(|| catalog.database.clone());
    for (at, photo) in catalog.photos.iter().enumerate() {
        let source_id = source_id(at);
        let master = if photo.variant_index == 0 {
            masters.insert(photo.image_id, source_id);
            0
        } else {
            masters.get(&photo.image_id).copied().unwrap_or(0)
        };
        if let Original::Refused(why) = &photo.original {
            refused = refused.saturating_add(1);
            if refused <= MAX_NAMED {
                let mut line = Notice::new(notice::NOT_OPENED).text_value("image", source_id.to_string()).text_value("path", photo.path.clone());
                line.values.insert("why".into(), Notice::reason(why));
                warnings.push(line);
            }
        }
        photos.push(import_photo(photo, source_id, master, &rows_of));
    }
    if refused > MAX_NAMED {
        warnings.push(Notice::new(notice::MORE_NOT_OPENED).count("n", refused - MAX_NAMED));
    }

    let (collections, members) = collections(&catalog.collections, &catalog.photos, &catalog.metadata_columns, &mut warnings);
    CatalogImport {
        source: catalog.database,
        catalog_id: catalog.document_uuid.map(|id| format!("{PREFIX}{id}")),
        catalog_path: None,
        photos,
        collections,
        members,
        collection_content: Vec::new(),
        warnings: warnings.iter().map(Notice::text).collect(),
        notices: warnings,
        skipped: Vec::new(),
        keyword_list: catalog
            .keywords
            .iter()
            .map(|k| crate::lightroom_catalog::CatalogKeyword { path: k.path.clone(), include_on_export: k.exportable })
            .collect(),
    }
}

/// A photo's id in the import: its place in the catalog, from 1 (0 means "no master").
fn source_id(at: usize) -> i64 {
    i64::try_from(at).unwrap_or(i64::MAX - 1).saturating_add(1)
}

fn import_photo(photo: &CatalogPhoto, source_id: i64, master: i64, rows_of: &str) -> ImportPhoto {
    let here = photo.original == Original::Local && Path::new(&photo.path).is_file();
    let mut image: BTreeMap<String, Value> = BTreeMap::new();
    let mut set = |key: &str, value: Value| {
        image.insert(key.to_string(), value);
    };
    set("id_local", json!(source_id));
    set("masterImage", json!(master));
    if master != 0 {
        set("copyName", json!(format!("Variant {}", photo.variant_index.saturating_add(1))));
    }
    set("fileFormat", json!(file_format(photo)));
    set("fileWidth", json!(photo.width));
    set("fileHeight", json!(photo.height));
    set("rating", json!(photo.rating));
    set("pick", json!(0));
    set("colorLabels", json!(label(photo.color_tag)));
    // (always there, so a tag Capture One has since changed replaces what an earlier import kept)
    set("labelText", json!(kept_label(photo.color_tag).unwrap_or("")));
    // Capture One keeps the camera's clock as the file gives it and has no command to change it,
    // so a file that is there says it better (with its zone, when it has one)
    if !here && let Some(captured) = &photo.captured {
        set("captureTime", json!(captured));
    }
    // what the file says when it is there; the catalog's camera and exposure fill what it leaves
    // empty, so an offline original keeps them too
    set("camera", camera(photo));
    set("captureOne", kept(photo));
    ImportPhoto {
        source_id,
        uuid: identity(photo, rows_of),
        path: photo.path.clone(),
        image,
        settings: String::new(),
        xmp: iptc_packet(photo),
        keywords: keywords(photo),
        history: Vec::new(),
        snapshots: Vec::new(),
        original: photo.original.clone(),
    }
}

/// What tells an earlier import of the same variant apart from a new photo: the variant's UUID,
/// else the image's with the variant's key, else the image's and the variant's keys in the
/// catalog `rows_of` names (its id, or its database's path). Never a photo's place in the catalog,
/// which moves when an earlier image or variant is deleted.
pub(super) fn identity(photo: &CatalogPhoto, rows_of: &str) -> String {
    let variant = photo.variant_id.map(|id| format!(":{id}")).unwrap_or_default();
    match (photo.variant_uuid.is_empty(), photo.image_uuid.is_empty()) {
        (false, _) => format!("{PREFIX}{}", photo.variant_uuid),
        (true, false) => format!("{PREFIX}image:{}{variant}", photo.image_uuid),
        (true, true) => format!("{PREFIX}{rows_of}:row:{}{variant}", photo.image_id),
    }
}

/// The format of a record whose file isn't read: `RAW` makes it a raw photo and `VIDEO` a video,
/// as in a Lightroom catalog; anything else is the file's extension.
fn file_format(photo: &CatalogPhoto) -> String {
    match photo.kind {
        OriginalKind::Raw => "RAW".into(),
        OriginalKind::Movie => "VIDEO".into(),
        OriginalKind::Raster | OriginalKind::Unknown => {
            let extension = Path::new(&photo.file_name).extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default();
            if extension.len() <= 8 && extension.chars().all(|c| c.is_ascii_alphanumeric()) { extension } else { String::new() }
        }
    }
}

/// The library's label for a colour tag. Orange and pink have none.
fn label(tag: ColorTag) -> &'static str {
    match tag {
        ColorTag::Red | ColorTag::Yellow | ColorTag::Green | ColorTag::Blue | ColorTag::Purple => tag.name().unwrap_or(""),
        ColorTag::None | ColorTag::Orange | ColorTag::Pink => "",
    }
}

/// The name of a colour tag the library has no label for: kept with the photo as its
/// `xmp:Label` text ([`lightcraft_catalog::Meta::label_text`]).
fn kept_label(tag: ColorTag) -> Option<&'static str> {
    matches!(tag, ColorTag::Orange | ColorTag::Pink).then(|| tag.name()).flatten()
}

/// The photo's keywords, with one that finds an orange or pink photo while the library can't
/// show or filter those colours: `Capture One|Orange`, `Capture One|Pink`.
fn keywords(photo: &CatalogPhoto) -> Vec<String> {
    let mut keywords = photo.keywords.clone();
    if let Some(name) = kept_label(photo.color_tag) {
        let keyword = format!("{KEYWORD_PARENT}|{name}");
        if !keywords.contains(&keyword) {
            keywords.push(keyword);
        }
    }
    keywords
}

/// The camera, lens and exposure the catalog records, as the catalog import's `camera` row field.
fn camera(photo: &CatalogPhoto) -> Value {
    json!({
        "make": photo.camera_make,
        "model": photo.camera_model,
        "serial": photo.camera_serial,
        "lens": photo.lens,
        "iso": photo.iso,
        "aperture": photo.aperture,
        "exposureTime": photo.shutter_seconds,
        "focalLength": photo.focal_mm,
        "exposureBias": photo.exposure_bias,
        "gps": photo.gps.map(|(latitude, longitude)| [latitude, longitude]),
    })
}

/// What the import doesn't carry into the library yet, kept with the photo's row.
fn kept(photo: &CatalogPhoto) -> Value {
    json!({
        "imageUuid": photo.image_uuid,
        "variantUuid": photo.variant_uuid,
        "variant": photo.variant_index,
        "managed": photo.managed,
        "adjusted": photo.adjusted,
        "colorTag": photo.color_tag.name(),
        "cameraMake": photo.camera_make,
        "cameraModel": photo.camera_model,
        "lens": photo.lens,
        "imported": photo.imported,
        "headline": photo.iptc.headline,
        "countryCode": photo.iptc.country_code,
        "iptc": photo.iptc.other,
        "exif": photo.exif,
        "annotated": photo.annotated,
    })
}

/// The IPTC fields the library has a place for, as an XMP packet; empty when there are none.
fn iptc_packet(photo: &CatalogPhoto) -> String {
    let field = |value: &String| (!value.is_empty()).then(|| value.clone());
    let iptc = &photo.iptc;
    let meta = lightcraft_meta::Metadata {
        title: field(&iptc.title),
        caption: field(&iptc.description),
        artist: field(&iptc.creator),
        copyright: field(&iptc.copyright),
        usage_terms: field(&iptc.usage_terms),
        sublocation: field(&iptc.location),
        city: field(&iptc.city),
        state: field(&iptc.state),
        country: field(&iptc.country),
        ..Default::default()
    };
    if meta == lightcraft_meta::Metadata::default() { String::new() } else { lightcraft_meta::write_xmp(&meta, None) }
}

/// Albums, projects, groups and smart albums as the import's collection rows and album
/// memberships. A smart album whose rule LightCraft can express keeps it (`smartRules`); one whose
/// rule tests something LightCraft has no rule for holds the photos its rule selects now; one
/// whose rule can't be read or evaluated is named in a warning instead.
fn collections(
    all: &[Collection],
    photos: &[CatalogPhoto],
    columns: &smart::Columns,
    warnings: &mut Vec<Notice>,
) -> (Vec<BTreeMap<String, Value>>, Vec<BTreeMap<String, Value>>) {
    let row = |pairs: &[(&str, Value)]| pairs.iter().map(|(key, value)| ((*key).to_string(), value.clone())).collect::<BTreeMap<_, _>>();
    let (mut rows, mut members) = (Vec::new(), Vec::new());
    let mut named = 0usize;
    let mut warn = |line: Notice| {
        named = named.saturating_add(1);
        if named <= MAX_NAMED {
            warnings.push(line);
        }
    };
    for collection in all {
        let name = if collection.name.is_empty() { "Untitled" } else { collection.name.as_str() };
        let mut album = row(&[
            ("id_local", json!(collection.id)),
            ("name", json!(name)),
            ("parent", json!(collection.parent.unwrap_or(0))),
            ("creationId", json!("com.adobe.ag.library.collection")),
            ("systemOnly", json!(0)),
        ]);
        let mut photos_in = collection.photos.clone();
        match collection.kind {
            CollectionKind::Album => {}
            CollectionKind::Project | CollectionKind::Group => {
                album.insert("creationId".into(), json!("com.adobe.ag.library.group"));
            }
            CollectionKind::SmartAlbum => {
                // a smart album left out still goes to the recovery archive, with its rule: as a
                // row the library skips (`systemOnly`)
                let mut left_out = |mut album: BTreeMap<String, Value>| {
                    album.insert("systemOnly".into(), json!(1));
                    album.insert("captureOneLeftOut".into(), json!(true));
                    rows.push(album);
                };
                let rule = match smart::parse(&collection.rule) {
                    Ok(rule) => rule,
                    Err(why) => {
                        warn(Notice::new(notice::SMART_LEFT_OUT).text_value("name", name).notice("why", why));
                        // as written, up to the length a rule is read at
                        if collection.rule.len() <= smart::MAX_RULE_BYTES {
                            album.insert("captureOneRuleText".into(), json!(collection.rule));
                        }
                        left_out(album);
                        continue;
                    }
                };
                album.insert("captureOneRule".into(), smart::archived(&rule));
                match smart::outcome(&rule, photos, columns) {
                    smart::Outcome::Smart(rules) => {
                        album.insert("smartRules".into(), json!(rules));
                        let exact = smart::exact_keywords(&rule);
                        if !exact.is_empty() {
                            album.insert("exactKeywords".into(), json!(exact));
                        }
                        // what it shows now, should the library refuse the rules; when that can't
                        // be told, the album says so rather than pass an empty one off as it
                        match smart::select(&rule, photos, columns) {
                            Some(selected) => photos_in = selected,
                            None => {
                                album.insert("selectionUnknown".into(), json!(true));
                                photos_in = Vec::new();
                            }
                        }
                    }
                    smart::Outcome::Selected(selected, missing) => {
                        warn(Notice::new(notice::SMART_AS_ALBUM).text_value("name", name).count("n", selected.len()).notices("fields", missing));
                        photos_in = selected;
                    }
                    smart::Outcome::Skipped(why) => {
                        warn(Notice::new(notice::SMART_LEFT_OUT).text_value("name", name).notice("why", why));
                        left_out(album);
                        continue;
                    }
                }
            }
        }
        rows.push(album);
        members.extend(photos_in.iter().map(|at| row(&[("collection", json!(collection.id)), ("image", json!(source_id(*at)))])));
    }
    if named > MAX_NAMED {
        warnings.push(Notice::new(notice::MORE_SMART).count("n", named - MAX_NAMED));
    }
    (rows, members)
}
