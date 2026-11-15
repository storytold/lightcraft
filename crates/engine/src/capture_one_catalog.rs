//! Capture One catalog import (library data).
//!
//! A Capture One catalog (`Name.cocatalog`: a folder, a package on macOS) keeps its library in
//! `Name.cocatalogdb`, an SQLite database. This module reads it with the same read-only page reader
//! as the Lightroom import and turns it into the same [`CatalogImport`] model, so everything after the
//! read (the job, merge-or-new-library, one undo step, reimport without duplicates, the result window,
//! the archive) is shared with File ▸ Import Lightroom Catalog….
//!
//! What is read: originals (inside the catalog or referenced), variants (the first one is the photo,
//! the others become virtual copies), star ratings, colour tags, IPTC fields, keywords, and user albums
//! with the projects and groups around them, and the variants' develop settings the app has an equivalent for so far
//! ([`crate::capture_one_develop`]: exposure, orientation and straighten, crop, flips, white balance); the report
//! names every other setting the photos use.
//!
//! Sources: own design, from the database's own table and column names (`sqlite_schema`) observed in
//! catalogs written by Capture One 16.9; no Capture One code or documentation was used.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use serde_json::{Value, json};

use crate::lightroom_catalog::{CatalogImport, CatalogPhoto, Original, Row, SkipReason, SkippedImage, ensure_read_active, open_catalog};

/// Deepest collection or keyword chain followed; deeper (or cyclic) chains are cut there.
const MAX_DEPTH: usize = 64;
/// Most keywords kept for one photo; a longer keyword field is cut there.
const MAX_KEYWORDS: usize = 10_000;

/// Whether `path` is a Capture One catalog: its `.cocatalogdb` database, or a folder (a `.cocatalog`
/// package, or the plain folder Windows shows) that holds one.
pub fn is_capture_one(path: &Path) -> bool {
    database_file(path).is_some()
}

fn has_extension(path: &Path, ext: &str) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

/// The catalog database for `path`: the file itself, or the one `.cocatalogdb` in the folder (the one
/// named after the folder first, when there are several).
pub fn database_file(path: &Path) -> Option<PathBuf> {
    if has_extension(path, "cocatalogdb") {
        return path.is_file().then(|| path.to_path_buf());
    }
    if !path.is_dir() {
        return None;
    }
    let named = path.file_stem().map(|stem| path.join(format!("{}.cocatalogdb", stem.to_string_lossy())));
    if let Some(named) = named.filter(|p| p.is_file()) {
        return Some(named);
    }
    let mut found: Vec<PathBuf> = std::fs::read_dir(path)
        .ok()?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && has_extension(p, "cocatalogdb"))
        .collect();
    found.sort();
    found.into_iter().next()
}

/// [`ensure_read_active`], saying the Capture One import was cancelled.
fn active(cancel: &AtomicBool) -> Result<(), String> {
    ensure_read_active(cancel).map_err(|_| "Capture One import cancelled".to_string())
}

fn text<'a>(r: &'a Row, k: &str) -> &'a str {
    r.get(k).and_then(Value::as_str).unwrap_or("")
}

fn int(r: &Row, k: &str) -> Option<i64> {
    let v = r.get(k)?;
    v.as_i64().or_else(|| v.as_f64().filter(|f| f.is_finite() && f.abs() < 9.0e15).map(|f| f.round() as i64))
}

fn number(r: &Row, k: &str) -> i64 {
    int(r, k).unwrap_or(0)
}

fn by_pk(rows: Vec<Row>) -> HashMap<i64, Row> {
    rows.into_iter().filter_map(|r| int(&r, "Z_PK").map(|pk| (pk, r))).collect()
}

/// Capture One's colour tags, by `ZCOLOR_TAG_INDEX`, as the library's label names. Orange (2) and pink
/// (6) have no label in the library.
fn colour_tag(index: i64) -> Result<&'static str, Option<&'static str>> {
    match index {
        1 => Ok("Red"),
        3 => Ok("Yellow"),
        4 => Ok("Green"),
        5 => Ok("Blue"),
        7 => Ok("Purple"),
        2 => Err(Some("orange")),
        6 => Err(Some("pink")),
        _ => Err(None),
    }
}

/// The capture time to import from the catalog (`ZEXP_DATE`, seconds since 1970), as
/// `YYYY-MM-DDTHH:MM:SS`, or nothing. Capture One keeps the camera's wall-clock time as if it were
/// UTC, so it is formatted as UTC, with no offset. A file that is there is read when it is added and
/// keeps its own time (with its offset); any other original (on a drive that isn't connected, on
/// another computer, deleted) has only the catalog's.
fn catalog_capture_time(original_on_disk: bool, seconds: Option<f64>) -> String {
    match seconds {
        Some(s) if !original_on_disk && s.is_finite() && s.abs() < 9.0e15 => {
            chrono::DateTime::from_timestamp(s.trunc() as i64, 0).map(|t| t.naive_utc().format("%Y-%m-%dT%H:%M:%S").to_string()).unwrap_or_default()
        }
        _ => String::new(),
    }
}

/// The camera's make and model as the catalog has them, or nothing for a file without one: Capture
/// One then names the format instead (`Adobe Systems Inc.` / `Tiff File` for a TIFF,
/// `Joint Photographic Experts Group` / `Jpeg File` for a JPEG).
fn camera(image: &Row) -> (&str, &str) {
    let (make, model) = (text(image, "ZCAMERA_MAKE").trim(), text(image, "ZCAMERA_MODEL").trim());
    let format = text(image, "ZEXP_FORMAT").trim();
    let names_the_format = model.strip_suffix(" File").is_some_and(|f| !f.is_empty() && f.eq_ignore_ascii_case(format));
    if names_the_format { ("", "") } else { (make, model) }
}

/// The folder an image's `ZPATHLOCATION` names, as a path string (it may be on another computer).
/// Relative locations are relative to the folder the catalog database is in.
fn location_path(loc: &Row, catalog_dir: &Path) -> Option<String> {
    let relative = text(loc, "ZRELATIVEPATH").replace(['\\', '/'], std::path::MAIN_SEPARATOR_STR);
    if number(loc, "ZISRELATIVE") != 0 {
        let mut dir = catalog_dir.to_path_buf();
        for part in relative.split(std::path::MAIN_SEPARATOR).filter(|p| !p.is_empty() && *p != ".") {
            if part == ".." {
                dir = dir.parent().map(Path::to_path_buf).unwrap_or(dir);
            } else {
                dir.push(part);
            }
        }
        return Some(dir.to_string_lossy().into());
    }
    // An absolute location: the root on the system this runs on first, then the other one, then the
    // volume (macOS).
    let roots = if cfg!(windows) { ["ZWINROOT", "ZMACROOT", "ZVOLUME"] } else { ["ZMACROOT", "ZWINROOT", "ZVOLUME"] };
    let root = roots.iter().map(|k| text(loc, k)).find(|r| !r.is_empty())?;
    let relative = relative.trim_start_matches(std::path::MAIN_SEPARATOR);
    Some(if relative.is_empty() { root.to_string() } else { Path::new(root).join(relative).to_string_lossy().into() })
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            // XML 1.0 has no other control characters
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
    out
}

/// IPTC fields of a variant as an XMP packet, which the sidecar reader merges like any other XMP.
fn iptc_xmp(meta: &dyn Fn(&str) -> Option<String>) -> String {
    let simple = [
        ("photoshop:Headline", "ZCONTENT_HEADLINE"),
        ("photoshop:CaptionWriter", "ZCONTENT_DESCRIPTIONWRITER"),
        ("photoshop:City", "ZIMAGE_CITY"),
        ("photoshop:State", "ZIMAGE_STATE"),
        ("photoshop:Country", "ZIMAGE_COUNTRY"),
        ("photoshop:Instructions", "ZSTATUS_INSTRUCTIONS"),
        ("photoshop:TransmissionReference", "ZSTATUS_JOBIDENTIFIER"),
        ("photoshop:Credit", "ZSTATUS_PROVIDER"),
        ("photoshop:Source", "ZSTATUS_SOURCE"),
        ("photoshop:Category", "ZCONTENT_CATEGORY"),
        ("photoshop:AuthorsPosition", "ZCONTACT_CREATORSTITLE"),
        ("Iptc4xmpCore:Location", "ZIMAGE_LOCATION"),
        ("Iptc4xmpCore:CountryCode", "ZIMAGE_ISOCOUNTRYCODE"),
        ("Iptc4xmpCore:IntellectualGenre", "ZIMAGE_INTELLECTUALGENRE"),
    ];
    let alt = [
        ("dc:title", "ZSTATUS_TITLE"),
        ("dc:description", "ZCONTENT_DESCRIPTION"),
        ("dc:rights", "ZSTATUS_COPYRIGHTNOTICE"),
        ("xmpRights:UsageTerms", "ZSTATUS_USAGETERMS"),
    ];
    let mut body = String::new();
    for (tag, col) in simple {
        if let Some(v) = meta(col) {
            body.push_str(&format!("<{tag}>{}</{tag}>", xml_escape(&v)));
        }
    }
    for (tag, col) in alt {
        if let Some(v) = meta(col) {
            body.push_str(&format!("<{tag}><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></{tag}>", xml_escape(&v)));
        }
    }
    if let Some(v) = meta("ZCONTACT_CREATOR") {
        let names: String =
            v.split([';', '\n']).map(str::trim).filter(|n| !n.is_empty()).map(|n| format!("<rdf:li>{}</rdf:li>", xml_escape(n))).collect();
        if !names.is_empty() {
            body.push_str(&format!("<dc:creator><rdf:Seq>{names}</rdf:Seq></dc:creator>"));
        }
    }
    if body.is_empty() {
        return String::new();
    }
    format!(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
         <rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" \
         xmlns:photoshop=\"http://ns.adobe.com/photoshop/1.0/\" xmlns:Iptc4xmpCore=\"http://iptc.org/std/Iptc4xmpCore/1.0/xmlns/\" \
         xmlns:xmpRights=\"http://ns.adobe.com/xap/1.0/rights/\">{body}</rdf:Description></rdf:RDF></x:xmpmeta>"
    )
}

/// One entry of a variant's keyword field without its order suffix: Capture One writes each keyword
/// as its full path and a position, `Animals|Birds|Owls||4`.
fn without_order(entry: &str) -> &str {
    match entry.rsplit_once("||") {
        Some((path, order)) if !order.is_empty() && order.trim().bytes().all(|b| b.is_ascii_digit()) => path,
        _ => entry,
    }
}

/// Keyword paths (`a|b|c`) for a variant's keyword field (`ZCONTENT_KEYWORDS`). Capture One lists
/// every keyword with its full path, and the parents of an assigned keyword as entries of their own;
/// those parents are dropped, as the library implies them from the path. A bare name that is one
/// keyword of the catalog's keyword tree gets its parents from the tree; anything else is kept as
/// written. The flag says the field held more than [`MAX_KEYWORDS`] and was cut there.
fn keyword_paths(field: &str, tree: &HashMap<i64, Row>, by_name: &HashMap<String, Vec<i64>>) -> (Vec<String>, bool) {
    // (in time linear in the field: a hostile field can hold millions of entries)
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut cut = false;
    for entry in field.split([',', '\n', '\r']).map(without_order).map(str::trim).filter(|n| !n.is_empty()) {
        if out.len() >= MAX_KEYWORDS {
            cut = true;
            break;
        }
        let path = if entry.contains('|') {
            entry.split('|').map(str::trim).filter(|p| !p.is_empty()).collect::<Vec<_>>().join("|")
        } else {
            tree_path(entry, tree, by_name)
        };
        if !path.is_empty() && seen.insert(path.clone()) {
            out.push(path);
        }
    }
    // keep only the deepest keywords: `Animals` goes when `Animals|Birds` is there too
    let parents: HashSet<String> = out.iter().flat_map(|p| p.match_indices('|').filter_map(|(i, _)| p.get(..i)).map(str::to_string)).collect();
    out.retain(|p| !parents.contains(p));
    (out, cut)
}

/// The path of a bare keyword name through the catalog's keyword tree, or the name itself when the
/// tree doesn't hold exactly one keyword of that name.
fn tree_path(name: &str, tree: &HashMap<i64, Row>, by_name: &HashMap<String, Vec<i64>>) -> String {
    let Some([only]) = by_name.get(&name.to_lowercase()).map(Vec::as_slice) else { return name.to_string() };
    let mut parts = Vec::new();
    let mut seen = HashSet::new();
    let mut at = *only;
    while let Some(k) = tree.get(&at) {
        if parts.len() >= MAX_DEPTH || !seen.insert(at) {
            break;
        }
        let part = text(k, "ZNAME").trim();
        if !part.is_empty() {
            parts.push(part.to_string());
        }
        at = number(k, "ZPARENT");
    }
    if parts.is_empty() {
        return name.to_string();
    }
    parts.reverse();
    parts.join("|")
}

/// Read a Capture One catalog for import, counting the tables read in `total` / `done` and stopping
/// when `cancel` is set.
pub fn read_with_progress(path: &Path, cancel: &AtomicBool, total: &AtomicUsize, done: &AtomicUsize) -> Result<CatalogImport, String> {
    total.store(12, Ordering::Relaxed);
    done.store(0, Ordering::Relaxed);
    let db_path = database_file(path).ok_or_else(|| format!("{}: no Capture One catalog database (.cocatalogdb) found", path.display()))?;
    // absolute: originals inside the catalog are found relative to its folder, and resolving that folder's path
    // against the catalog again (`doc_path::resolve`) would join a relative one twice
    let db_path = std::path::absolute(&db_path).unwrap_or(db_path);
    let catalog_dir = db_path.parent().map(Path::to_path_buf).unwrap_or_default();
    let (reader, guard) = open_catalog(&db_path, cancel).map_err(|e| e.replace("Lightroom", "Capture One"))?;
    let table = |name: &str, required: bool| -> Result<Vec<Row>, String> {
        active(cancel)?;
        let rows = reader.table(name, required, cancel).map_err(|e| e.replace("Lightroom", "Capture One"))?;
        done.fetch_add(1, Ordering::Relaxed);
        Ok(rows)
    };
    let mut warnings = Vec::new();
    let entities: HashMap<i64, String> =
        table("ZENTITIES", false)?.iter().filter_map(|r| Some((int(r, "Z_ENT")?, text(r, "ZNAME").to_string()))).collect();
    let images = table("ZIMAGE", true)?;
    let locations = by_pk(table("ZPATHLOCATION", true)?);
    let variants = table("ZVARIANT", true)?;
    let metadata: HashMap<i64, Row> = table("ZVARIANTMETADATA", false)?.into_iter().filter_map(|r| Some((int(&r, "ZLAYER")?, r))).collect();
    let keyword_tree = by_pk(table("ZKEYWORD", false)?);
    let collections = table("ZCOLLECTION", false)?;
    let image_members = table("ZIMAGEINCOLLECTION", false)?;
    let variant_members = table("ZVARIANTINCOLLECTION", false)?;
    let positions = table("ZIMAGEINCOLLECTIONPROPERTIES", false)?;
    let documents = table("ZDOCUMENTCONTENT", false)?;
    active(cancel)?;
    let layers = crate::capture_one_develop::read_layers(&reader, &entities, cancel).map_err(|e| e.replace("Lightroom", "Capture One"))?;
    done.fetch_add(1, Ordering::Relaxed);
    let mut not_imported: BTreeMap<String, usize> = BTreeMap::new();
    let mut keyword_names: HashMap<String, Vec<i64>> = HashMap::new();
    for (pk, k) in &keyword_tree {
        keyword_names.entry(text(k, "ZNAME").trim().to_lowercase()).or_default().push(*pk);
    }

    // ---- photos: one per variant; the first variant of an image is the photo, the others its virtual copies
    let mut by_image: BTreeMap<i64, Vec<&Row>> = BTreeMap::new();
    for v in &variants {
        by_image.entry(number(v, "ZIMAGE")).or_default().push(v);
    }
    for list in by_image.values_mut() {
        list.sort_by_key(|v| (number(v, "ZINDEX"), number(v, "Z_PK")));
    }
    let mut primary: HashMap<i64, i64> = HashMap::new();
    let mut photos = Vec::new();
    let mut skipped = Vec::new();
    let mut trashed = 0usize;
    let mut unlabelled: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut keywords_cut = 0usize;
    for image in &images {
        active(cancel)?;
        // (Core Data keys are positive; 0 would read as "no master" below)
        let Some(image_pk) = int(image, "Z_PK").filter(|pk| *pk > 0) else { continue };
        if number(image, "ZISTRASHED") != 0 {
            trashed = trashed.saturating_add(1);
            continue;
        }
        let file_name = text(image, "ZIMAGEFILENAME");
        let folder = locations.get(&number(image, "ZIMAGELOCATION")).and_then(|loc| location_path(loc, &catalog_dir));
        let Some(folder) = folder else {
            skipped.push(SkippedImage { image: image_pk, reason: SkipReason::MissingFolder });
            continue;
        };
        if file_name.is_empty() || file_name.contains(['/', '\\']) {
            skipped.push(SkippedImage { image: image_pk, reason: SkipReason::IncompletePath });
            continue;
        }
        let stored = Path::new(&folder).join(file_name).to_string_lossy().to_string();
        let (path, original) = match crate::doc_path::resolve(&stored, Some(&db_path)) {
            Ok(found) => (found.to_string_lossy().to_string(), Original::Local),
            Err(crate::doc_path::NotFollowed::NotHere) => (stored, Original::NotHere),
            Err(crate::doc_path::NotFollowed::Refused(why)) => (stored, Original::Refused(why.to_string())),
        };
        let format = Path::new(file_name).extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or_default();
        // ZWIDTH / ZHEIGHT are the file's pixels (ZPIXELWIDTH is the sensor's pixel pitch in µm)
        let width = int(image, "ZWIDTH").unwrap_or(0).max(0);
        let height = int(image, "ZHEIGHT").unwrap_or(0).max(0);
        let on_disk = original == Original::Local && Path::new(&path).is_file();
        let captured = catalog_capture_time(on_disk, image.get("ZEXP_DATE").and_then(Value::as_f64));
        let (camera_make, camera_model) = camera(image);
        let image_variants = by_image.get(&image_pk).cloned().unwrap_or_default();
        if image_variants.is_empty() {
            skipped.push(SkippedImage { image: image_pk, reason: SkipReason::MissingFile });
            continue;
        }
        let mut master = 0;
        for (n, variant) in image_variants.iter().enumerate() {
            let Some(variant_pk) = int(variant, "Z_PK").filter(|pk| *pk > 0) else { continue };
            // Metadata hangs off a layer: the combined layer is what Capture One shows, the
            // adjustment and default layers fill in what it leaves empty.
            let chain: Vec<&Row> =
                ["ZCOMBINEDSETTINGS", "ZADJUSTMENTLAYER", "ZDEFAULTLAYER"].iter().filter_map(|k| metadata.get(&number(variant, k))).collect();
            let meta = |col: &str| -> Option<String> {
                chain.iter().filter_map(|m| m.get(col)).find_map(|v| match v {
                    Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
                    Value::Number(n) => Some(n.to_string()),
                    _ => None,
                })
            };
            let meta_int = |col: &str| chain.iter().find_map(|m| int(m, col));
            let stars = meta_int("ZBASIC_RATING").unwrap_or(0);
            let label = match meta_int("ZCOLOR_TAG_INDEX").map(colour_tag) {
                Some(Ok(name)) => name,
                Some(Err(Some(colour))) => {
                    let count = unlabelled.entry(colour).or_default();
                    *count = count.saturating_add(1);
                    ""
                }
                _ => "",
            };
            let mut row = json!({
                "id_local": variant_pk,
                "masterImage": master,
                "fileFormat": format,
                "fileWidth": width,
                "fileHeight": height,
                "rating": stars.clamp(0, 5),
                "pick": if stars < 0 { -1 } else { 0 },
                "colorLabels": label,
                "captureTime": captured,
                // the camera as the catalog has it, for a photo whose file isn't read (the file's own wins)
                "cameraMake": camera_make,
                "cameraModel": camera_model,
                "lens": text(image, "ZCAMERA_LENS").trim(),
                // when the settings last changed (Core Data seconds since 2001, as Lightroom's touch time): the edit date
                "touchTime": image.get("ZSETTINGSMODIFICATIONDATE").and_then(Value::as_f64),
            });
            if master != 0
                && let Some(obj) = row.as_object_mut()
            {
                obj.insert("copyName".into(), json!(format!("Variant {}", n.saturating_add(1))));
            }
            let image_row: Row = serde_json::from_value(row).map_err(|e| e.to_string())?;
            let (keywords, cut) = meta("ZCONTENT_KEYWORDS").map(|k| keyword_paths(&k, &keyword_tree, &keyword_names)).unwrap_or_default();
            if cut {
                keywords_cut = keywords_cut.saturating_add(1);
            }
            let uuid = text(variant, "ZVARIANTUUID");
            let develop = layers
                .get(&variant_pk)
                .map(|l| crate::capture_one_develop::develop_of(l, [width as f64, height as f64], (camera_make, camera_model)))
                .filter(|d| !d.is_empty() || !d.not_imported.is_empty());
            for tool in develop.iter().flat_map(|d| &d.not_imported) {
                let count = not_imported.entry(tool.clone()).or_default();
                *count = count.saturating_add(1);
            }
            photos.push(CatalogPhoto {
                source_id: variant_pk,
                uuid: if uuid.is_empty() { String::new() } else { format!("c1:{uuid}") },
                path: path.clone(),
                image: image_row,
                settings: String::new(),
                xmp: iptc_xmp(&meta),
                keywords,
                history: Vec::new(),
                snapshots: Vec::new(),
                original: original.clone(),
                as_shot_white: None,
                capture_one: develop.map(Box::new),
            });
            if master == 0 {
                master = variant_pk;
                primary.insert(image_pk, variant_pk);
            }
        }
    }
    if trashed > 0 {
        warnings.push(format!("{trashed} image(s) in Capture One's catalog trash were left out"));
    }
    if keywords_cut > 0 {
        warnings.push(format!(
            "{keywords_cut} photo(s) had more than {MAX_KEYWORDS} keywords in Capture One; only the first {MAX_KEYWORDS} of each were imported"
        ));
    }
    for (colour, count) in unlabelled {
        warnings.push(format!("{count} photo(s) had Capture One's {colour} colour tag, which has no label here; imported without a label"));
    }
    // most used first
    let mut not_imported: Vec<(String, usize)> = not_imported.into_iter().collect();
    not_imported.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    for (tool, count) in not_imported {
        warnings.push(format!("Capture One {tool}: used on {count} photo(s), not imported yet (the photos keep the rest of their settings)"));
    }

    // ---- albums, with the projects and groups that hold them
    let kinds: HashMap<i64, String> = collections
        .iter()
        .filter_map(|c| {
            let ent = int(c, "Z_ENT")?;
            Some((int(c, "Z_PK")?, entities.get(&ent).cloned().unwrap_or_default()))
        })
        .collect();
    let roots: HashSet<i64> = documents.iter().filter_map(|d| int(d, "ZROOTCOLLECTION")).collect();
    let by_id: HashMap<i64, &Row> = collections.iter().filter_map(|c| Some((int(c, "Z_PK")?, c))).collect();
    // Capture One's own folders, named by the collections that use them (the root names Recent
    // Imports, Recent Captures, All Images and the trash this way).
    let system: HashSet<i64> = collections
        .iter()
        .flat_map(|c| {
            [
                "ZLASTIMPORTSCOLLECTION",
                "ZLASTCAPTURESCOLLECTION",
                "ZALLIMAGESCOLLECTION",
                "ZTRASHCOLLECTION",
                "ZCAPTURECOLLECTION",
                "ZPROCESSCOLLECTION",
                "ZMOVETOCOLLECTION",
            ]
            .into_iter()
            .filter_map(|k| int(c, k).filter(|id| *id > 0))
        })
        .collect();
    // A user's group is stored as a VirtualFolderCollection, like Recent Imports; only the ones
    // Capture One doesn't use itself are groups.
    let is_group = |id: i64| match kinds.get(&id).map(String::as_str) {
        Some("ProjectCollection" | "Collection") => true,
        Some("VirtualFolderCollection") => !system.contains(&id),
        _ => false,
    };
    // A collection is imported when it is an album or a group and every collection above it, up to
    // the catalog's root, is a group: albums under Recent Imports and other system folders stay out.
    let wanted = |id: i64| -> bool {
        let mut at = id;
        let mut seen = HashSet::new();
        loop {
            if roots.contains(&at) || at <= 0 {
                return at != id;
            }
            if seen.len() >= MAX_DEPTH || !seen.insert(at) {
                return false;
            }
            let ok = if at == id { is_group(at) || kinds.get(&at).is_some_and(|k| k == "AlbumCollection") } else { is_group(at) };
            if !ok {
                return false;
            }
            at = by_id.get(&at).map_or(0, |c| number(c, "ZPARENT"));
        }
    };
    let mut out_collections = Vec::new();
    let mut kept = HashSet::new();
    for c in &collections {
        let Some(id) = int(c, "Z_PK") else { continue };
        let kind = kinds.get(&id).map(String::as_str).unwrap_or("");
        if kind == "SmartCollection" {
            // only the user's own (under the root or their projects and groups), not the built-in ones
            let parent = number(c, "ZPARENT");
            if roots.contains(&parent) || (is_group(parent) && wanted(parent)) {
                warnings.push(format!("smart album “{}”: its search rules aren't read yet; not imported", text(c, "ZNAME")));
            }
            continue;
        }
        if !wanted(id) {
            continue;
        }
        let parent = number(c, "ZPARENT");
        let parent = if roots.contains(&parent) || parent < 0 { 0 } else { parent };
        let group = is_group(id);
        let creation = if group { "com.adobe.ag.library.group" } else { "com.adobe.ag.library.collection" };
        let name = text(c, "ZNAME").trim();
        let name = if name.is_empty() { "Untitled" } else { name };
        out_collections.push(
            serde_json::from_value::<Row>(json!({"id_local": id, "name": name, "parent": parent, "creationId": creation}))
                .map_err(|e| e.to_string())?,
        );
        // a project or group lists the images of the albums in it as its own; only albums hold photos here
        if !group {
            kept.insert(id);
        }
    }
    let position: HashMap<(i64, i64), i64> =
        positions.iter().filter_map(|p| Some(((number(p, "ZCOLLECTION"), number(p, "ZIMAGE")), int(p, "ZMANUALSORTINDEX")?))).collect();
    let mut members = Vec::new();
    let mut seen_members = HashSet::new();
    let member = |collection: i64, variant: i64, pos: i64| -> Result<Row, String> {
        // positions sort as text: pad so 10 follows 9
        serde_json::from_value(json!({"collection": collection, "image": variant, "positionInCollection": format!("{:020}", pos.max(0))}))
            .map_err(|e| e.to_string())
    };
    for m in &image_members {
        let (collection, image) = (number(m, "ZCOLLECTION"), number(m, "ZIMAGE"));
        if let (true, Some(variant)) = (kept.contains(&collection), primary.get(&image))
            && seen_members.insert((collection, *variant))
        {
            members.push(member(collection, *variant, position.get(&(collection, image)).copied().unwrap_or(number(m, "Z_PK")))?);
        }
    }
    let photo_ids: HashSet<i64> = photos.iter().map(|p| p.source_id).collect();
    for m in &variant_members {
        let (collection, variant) = (number(m, "ZCOLLECTION"), number(m, "ZVARIANT"));
        if kept.contains(&collection) && photo_ids.contains(&variant) && seen_members.insert((collection, variant)) {
            members.push(member(collection, variant, number(m, "Z_PK"))?);
        }
    }
    guard.verify().map_err(|e| e.replace("Lightroom", "Capture One"))?;
    let mut out = CatalogImport {
        source: db_path.to_string_lossy().into(),
        photos,
        collections: out_collections,
        members,
        collection_content: Vec::new(),
        stacks: Vec::new(),
        warnings,
        skipped,
    };
    // Here, as the Lightroom reader does, not only where the records are made: the import job adds
    // every photo it is handed before that, so a refused file would be added without its metadata.
    crate::lightroom_catalog::refuse_non_media(&mut out);
    let kept_photos: HashSet<i64> = out.photos.iter().map(|p| p.source_id).collect();
    out.members.retain(|m| kept_photos.contains(&number(m, "image")));
    done.store(total.load(Ordering::Relaxed), Ordering::Relaxed);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A SQLite file with these tables, one leaf page each; a table's columns are its rows' keys.
    fn sqlite(tables: &[(&str, Vec<Value>)]) -> Vec<u8> {
        use crate::lightroom_sqlite::Value as Sql;
        let specs: Vec<(String, String, Vec<Vec<Sql>>)> = tables
            .iter()
            .map(|(name, rows)| {
                let columns: std::collections::BTreeSet<String> = rows.iter().flat_map(|r| r.as_object().unwrap().keys().cloned()).collect();
                let cells = rows
                    .iter()
                    .map(|r| {
                        columns
                            .iter()
                            .map(|c| match &r[c] {
                                Value::Number(n) if n.is_i64() => Sql::Integer(n.as_i64().unwrap()),
                                Value::Number(n) => Sql::Real(n.as_f64().unwrap()),
                                Value::String(s) => Sql::Text(s.clone()),
                                _ => Sql::Null,
                            })
                            .collect()
                    })
                    .collect();
                (name.to_string(), columns.into_iter().collect::<Vec<_>>().join(", "), cells)
            })
            .collect();
        let refs: Vec<(&str, &str, Vec<Vec<Sql>>)> = specs.iter().map(|(n, c, r)| (n.as_str(), c.as_str(), r.clone())).collect();
        crate::lightroom_catalog::tests::native_catalog_rows(&refs, None)
    }

    #[test]
    fn colour_tags_map_to_library_labels_or_say_why_not() {
        assert_eq!(colour_tag(1), Ok("Red"));
        assert_eq!(colour_tag(7), Ok("Purple"));
        assert_eq!(colour_tag(2), Err(Some("orange")));
        assert_eq!(colour_tag(0), Err(None));
        assert_eq!(colour_tag(-5), Err(None));
    }

    /// The catalog's capture time is imported whenever the file isn't there to be read, whatever
    /// the path looks like; a file that is there keeps its own (which has its UTC offset).
    #[test]
    fn the_catalogs_capture_time_is_used_when_the_file_is_not_there() {
        // 2000-01-01 12:00:00 on the camera's clock, as Capture One stores it
        let seconds = Some(946_728_000.0);
        assert_eq!(catalog_capture_time(false, seconds), "2000-01-01T12:00:00");
        assert_eq!(catalog_capture_time(true, seconds), "", "the file's own time wins");
        assert_eq!(catalog_capture_time(false, None), "");
        // hostile numbers are no time, not a panic
        for bad in [f64::NAN, f64::INFINITY, -f64::INFINITY, 1.0e300, -1.0e300] {
            assert_eq!(catalog_capture_time(false, Some(bad)), "", "{bad}");
        }
    }

    /// End to end through the reader: an image whose folder is on a drive that isn't there gets the
    /// catalog's capture time and camera; the same catalog with the file present leaves the time to
    /// the file.
    #[test]
    fn an_offline_original_gets_the_catalogs_time_and_camera() {
        let dir = std::env::temp_dir().join(format!("c1-offline-time-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Originals")).unwrap();
        let db = dir.join("Test.cocatalogdb");
        let image = |location: i64| {
            json!({"Z_PK": 1, "ZIMAGEFILENAME": "a.NEF", "ZIMAGELOCATION": location, "ZWIDTH": 6000, "ZHEIGHT": 4000,
                   "ZEXP_DATE": 946_728_000.0, "ZCAMERA_MAKE": "EXAMPLE CORP", "ZCAMERA_MODEL": "EXAMPLE X1 ", "ZCAMERA_LENS": "EXAMPLE 50mm"})
        };
        let tables = |location: i64| {
            sqlite(&[
                ("ZIMAGE", vec![image(location)]),
                (
                    "ZPATHLOCATION",
                    vec![
                        json!({"Z_PK": 1, "ZWINROOT": "Q:\\", "ZMACROOT": "/Volumes/Gone", "ZRELATIVEPATH": "Photos"}),
                        json!({"Z_PK": 2, "ZRELATIVEPATH": "./Originals", "ZISRELATIVE": 1}),
                    ],
                ),
                ("ZVARIANT", vec![json!({"Z_PK": 7, "ZIMAGE": 1, "ZVARIANTUUID": "u"})]),
            ])
        };
        let cancel = AtomicBool::new(false);
        let read = |bytes: Vec<u8>| {
            std::fs::write(&db, bytes).unwrap();
            read_with_progress(&db, &cancel, &AtomicUsize::new(0), &AtomicUsize::new(0)).unwrap()
        };
        let offline = read(tables(1));
        let row = &offline.photos[0].image;
        assert_eq!(row["captureTime"], "2000-01-01T12:00:00");
        assert_eq!((&row["cameraMake"], &row["cameraModel"], &row["lens"]), (&json!("EXAMPLE CORP"), &json!("EXAMPLE X1"), &json!("EXAMPLE 50mm")));
        std::fs::write(dir.join("Originals").join("a.NEF"), b"raw").unwrap();
        let here = read(tables(2));
        assert!(here.photos[0].original_is_file());
        assert_eq!(here.photos[0].image["captureTime"], "", "a file that is there keeps its own time");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file type the app doesn't read (here a `.NEFX` next to a `.jpg`) is left out where the
    /// catalog is read, with its variants and album places, so the import job never adds the file:
    /// the photo the job adds is the one the catalog's record becomes, nothing more.
    #[test]
    fn a_file_type_the_app_does_not_read_is_never_added() {
        use lcc_codecs::{ChromaSubsampling, EncodeImage, EncodeMeta, Samples, encode_jpeg};
        let dir = std::env::temp_dir().join(format!("c1-non-media-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Originals")).unwrap();
        let jpeg =
            encode_jpeg(&EncodeImage::new(8, 8, 3, Samples::U8(&[128; 8 * 8 * 3])), 90, ChromaSubsampling::S444, &EncodeMeta::default()).unwrap();
        // the same readable bytes under both names: only the file type decides
        std::fs::write(dir.join("Originals").join("a.jpg"), &jpeg).unwrap();
        std::fs::write(dir.join("Originals").join("b.NEFX"), &jpeg).unwrap();
        let db = dir.join("Test.cocatalogdb");
        let image = |pk: i64, name: &str| json!({"Z_PK": pk, "ZIMAGEFILENAME": name, "ZIMAGELOCATION": 1, "ZWIDTH": 8, "ZHEIGHT": 8});
        std::fs::write(
            &db,
            sqlite(&[
                ("ZPATHLOCATION", vec![json!({"Z_PK": 1, "ZRELATIVEPATH": "./Originals", "ZISRELATIVE": 1})]),
                ("ZIMAGE", vec![image(1, "a.jpg"), image(2, "b.NEFX")]),
                (
                    "ZVARIANT",
                    vec![
                        json!({"Z_PK": 7, "ZIMAGE": 1, "ZVARIANTUUID": "a"}),
                        json!({"Z_PK": 8, "ZIMAGE": 2, "ZVARIANTUUID": "b"}),
                        json!({"Z_PK": 9, "ZIMAGE": 2, "ZVARIANTUUID": "b2"}),
                    ],
                ),
                ("ZENTITIES", vec![json!({"Z_ENT": 2, "ZNAME": "AlbumCollection"})]),
                ("ZCOLLECTION", vec![json!({"Z_PK": 1, "Z_ENT": 2, "ZPARENT": -1, "ZNAME": "Album"})]),
                ("ZDOCUMENTCONTENT", vec![json!({"Z_PK": 1, "ZROOTCOLLECTION": 0})]),
                ("ZIMAGEINCOLLECTION", vec![json!({"Z_PK": 1, "ZCOLLECTION": 1, "ZIMAGE": 1}), json!({"Z_PK": 2, "ZCOLLECTION": 1, "ZIMAGE": 2})]),
            ]),
        )
        .unwrap();
        let cancel = AtomicBool::new(false);
        let read = read_with_progress(&db, &cancel, &AtomicUsize::new(0), &AtomicUsize::new(0)).unwrap();
        assert_eq!(read.photos.iter().map(|p| p.source_id).collect::<Vec<_>>(), vec![7], "the NEFX and its second variant are left out");
        assert!(read.members.iter().all(|m| number(m, "image") == 7), "no album place for a photo that isn't there: {:?}", read.members);
        let warnings = read.warnings.join("\n");
        assert!(warnings.contains("image 8: not imported") && warnings.contains("image 9: not imported"), "{warnings}");
        // through the import job, which adds the originals before the records are made
        let mut s = crate::Session::new();
        let mut job = crate::lightroom_job::LightroomJob::new(&mut s, db.clone(), false).unwrap();
        let prepared = job.prepare(&cancel).unwrap();
        let completion = crate::lightroom_job::commit_prepared(&mut s, prepared).unwrap();
        let names: Vec<String> = s.catalog.photos().map(|p| p.file_name.clone()).collect();
        assert_eq!(names, vec!["a.jpg".to_string()], "only the catalog's readable photo is added: {}", completion.report);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The layer entities as a catalog names them, and a photo's tables with its develop layers.
    fn develop_catalog(layers: Vec<Value>) -> Vec<u8> {
        sqlite(&[
            ("ZPATHLOCATION", vec![json!({"Z_PK": 1, "ZWINROOT": "Q:\\", "ZMACROOT": "/Volumes/Gone", "ZRELATIVEPATH": "Photos"})]),
            (
                "ZIMAGE",
                vec![
                    json!({"Z_PK": 1, "ZIMAGEFILENAME": "a.NEF", "ZIMAGELOCATION": 1, "ZWIDTH": 6000, "ZHEIGHT": 4000, "ZSETTINGSMODIFICATIONDATE": 700_000_000.0,
                        "ZCAMERA_MAKE": "EXAMPLE CORP", "ZCAMERA_MODEL": "EXAMPLE X1"}),
                    json!({"Z_PK": 2, "ZIMAGEFILENAME": "b.NEF", "ZIMAGELOCATION": 1, "ZWIDTH": 6000, "ZHEIGHT": 4000}),
                ],
            ),
            ("ZVARIANT", vec![json!({"Z_PK": 7, "ZIMAGE": 1, "ZVARIANTUUID": "a"}), json!({"Z_PK": 8, "ZIMAGE": 2, "ZVARIANTUUID": "b"})]),
            (
                "ZENTITIES",
                vec![
                    json!({"Z_ENT": 17, "ZNAME": "VariantAdjustmentLayer"}),
                    json!({"Z_ENT": 18, "ZNAME": "VariantDefaultLayer"}),
                    json!({"Z_ENT": 19, "ZNAME": "VariantStyleLayer"}),
                    json!({"Z_ENT": 23, "ZNAME": "LocalAdjustmentsLayer"}),
                ],
            ),
            ("ZVARIANTLAYER", layers),
        ])
    }

    /// A layer row with every column the fixture uses, so the table has them all.
    fn layer(pk: i64, ent: i64, variant: i64, values: Value) -> Value {
        let mut l = json!({"Z_PK": pk, "Z_ENT": ent, "ZVARIANT": variant, "ZINDEX": 0, "ZENABLED": null, "ZEXPOSURE": null,
            "ZROTATION": null, "ZCROP": null, "ZCROPASPECTRATIONAME": null, "ZBRIGHTNESS": null, "ZLENSLCC": null, "ZFLIP": null,
            "ZWHITEBALANCE": null});
        if let (Some(l), Some(v)) = (l.as_object_mut(), values.as_object()) {
            l.extend(v.clone());
        }
        l
    }

    /// End to end through the import job: a variant's exposure, crop and quarter turn land in its settings, dated at
    /// Capture One's last change, and what it uses beyond them is named in the report; a variant the user never
    /// touched keeps the settings any photo gets.
    #[test]
    fn develop_settings_import_and_the_rest_is_reported() {
        let dir = std::env::temp_dir().join(format!("c1-develop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("Test.cocatalogdb");
        let defaults = json!({"ZEXPOSURE": 0, "ZROTATION": 0, "ZBRIGHTNESS": 0, "ZFLIP": 0, "ZWHITEBALANCE": "2.000;1.000;1.400"});
        std::fs::write(
            &db,
            develop_catalog(vec![
                layer(1, 18, 7, defaults.clone()),
                layer(
                    2,
                    17,
                    7,
                    json!({"ZEXPOSURE": 0.75, "ZROTATION": 90.0, "ZCROP": "3000;2000;2000;3000", "ZCROPASPECTRATIONAME": "2x3", "ZBRIGHTNESS": 12.5,
                        "ZWHITEBALANCE": "2.200;1.000;1.200"}),
                ),
                layer(3, 19, 7, json!({"ZINDEX": 1, "ZLENSLCC": "0123abcd"})),
                layer(4, 23, 7, json!({"ZEXPOSURE": 1.0})),
                layer(5, 18, 8, defaults),
                layer(6, 17, 8, json!({"ZEXPOSURE": 0})),
            ]),
        )
        .unwrap();
        let cancel = AtomicBool::new(false);
        let read = read_with_progress(&db, &cancel, &AtomicUsize::new(0), &AtomicUsize::new(0)).unwrap();
        let c1 = read.photos[0].capture_one.as_deref().expect("variant 7 has settings");
        assert_eq!(c1.not_imported, vec!["Brightness", "LCC", "Local adjustment layers"]);
        assert!(read.photos[1].capture_one.is_none(), "an untouched variant has nothing to import");
        let warnings = read.warnings.join("\n");
        assert!(warnings.contains("Capture One Brightness: used on 1 photo(s), not imported yet"), "{warnings}");
        let mut s = crate::Session::new();
        let report = crate::lightroom_catalog::import(&mut s, &db, false).unwrap();
        let photo = |name: &str| s.catalog.photos().find(|p| p.file_name == name).unwrap().clone();
        let a = photo("a.NEF");
        assert_eq!(a.develop.light.exposure, 0.75);
        assert_eq!(a.develop.orientation, lcc_geom::Orientation::from_parts(false, 1));
        let g = a.develop.crop.geometry.rect;
        // the centre (3000, 2000) of the 6000 × 4000 stored image is the centre shown; 2000 × 3000 of 4000 × 6000
        assert!((g.x0 - 0.25).abs() < 1e-9 && (g.x1 - 0.75).abs() < 1e-9 && (g.y0 - 0.25).abs() < 1e-9 && (g.y1 - 0.75).abs() < 1e-9, "{g:?}");
        assert_eq!(a.develop.crop.aspect, Some((2, 3)));
        assert!(a.edited.as_deref().is_some_and(|e| e.starts_with("2023-")), "dated at Capture One's change: {:?}", a.edited);
        // a camera the app edits on the relative white balance scale: the white balance is applied
        // there and the result window says it is approximate, with the photo count
        assert_eq!(a.develop.wb.mode, lcc_develop::WbMode::Custom);
        assert!(a.develop.wb.temp > 6500.0, "a warmer white balance on the relative scale: {:?}", a.develop.wb);
        let warnings: Vec<&str> = report["warnings"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        assert!(warnings.iter().any(|w| w.starts_with("Capture One White Balance: approximate on 1 photo(s)")), "{warnings:?}");
        let unmapped = report["unmapped"]["7"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>());
        assert_eq!(unmapped, Some(vec!["Brightness", "LCC", "Local adjustment layers"]), "{}", report["unmapped"]);
        let b = photo("b.NEF");
        assert_eq!((b.develop.light.exposure, b.develop.orientation), (0.0, lcc_geom::Orientation::Normal));
        assert!(report["unmapped"].get("8").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_format_named_in_place_of_a_camera_is_no_camera() {
        let row = |make: &str, model: &str, format: &str| -> Row {
            serde_json::from_value(json!({"ZCAMERA_MAKE": make, "ZCAMERA_MODEL": model, "ZEXP_FORMAT": format})).unwrap()
        };
        assert_eq!(camera(&row("Adobe Systems Inc.", "Tiff File", "TIFF")), ("", ""));
        assert_eq!(camera(&row("Joint Photographic Experts Group", "Jpeg File", "JPEG")), ("", ""));
        assert_eq!(camera(&row("EXAMPLE CORP", "EXAMPLE X1", "RAW")), ("EXAMPLE CORP", "EXAMPLE X1"));
        // a camera whose model happens to end in "File" is still a camera
        assert_eq!(camera(&row("Acme", "Raw File", "JPEG")), ("Acme", "Raw File"));
    }

    #[test]
    fn relative_locations_resolve_against_the_catalog_folder() {
        let dir = Path::new("/photos/My Catalog");
        let loc: Row = serde_json::from_value(json!({"ZRELATIVEPATH": "./Originals", "ZISRELATIVE": 1})).unwrap();
        assert_eq!(location_path(&loc, dir), Some(dir.join("Originals").to_string_lossy().into()));
        let up: Row = serde_json::from_value(json!({"ZRELATIVEPATH": "..\\..\\Shoots", "ZISRELATIVE": 1})).unwrap();
        assert_eq!(location_path(&up, dir), Some(Path::new("/Shoots").to_string_lossy().into()));
        let nothing: Row = serde_json::from_value(json!({"ZISRELATIVE": 0})).unwrap();
        assert_eq!(location_path(&nothing, dir), None);
    }

    #[test]
    fn keywords_get_their_parents_from_the_tree() {
        let tree = by_pk(vec![
            serde_json::from_value(json!({"Z_PK": 1, "ZNAME": "Nature", "ZPARENT": 0})).unwrap(),
            serde_json::from_value(json!({"Z_PK": 2, "ZNAME": "Trees", "ZPARENT": 1})).unwrap(),
            // a cycle must not hang
            serde_json::from_value(json!({"Z_PK": 3, "ZNAME": "Loop", "ZPARENT": 3})).unwrap(),
        ]);
        let mut names: HashMap<String, Vec<i64>> = HashMap::new();
        for (pk, k) in &tree {
            names.entry(text(k, "ZNAME").to_lowercase()).or_default().push(*pk);
        }
        assert_eq!(keyword_paths("trees, Rivers,Loop", &tree, &names).0, vec!["Nature|Trees", "Rivers", "Loop"]);
        assert_eq!(keyword_paths("a | b|", &tree, &names).0, vec!["a|b"]);
    }

    #[test]
    fn keyword_field_drops_order_suffixes_and_implied_parents() {
        let (tree, names) = (HashMap::new(), HashMap::new());
        let paths = |field: &str| keyword_paths(field, &tree, &names).0;
        // as Capture One 16 writes it: every keyword's full path, its parents too, and a position
        assert_eq!(paths("Animals||2,Animals|Birds||3,Animals|Birds|Owls||4,Weather||0,Night||1"), vec!["Animals|Birds|Owls", "Weather", "Night"]);
        assert_eq!(paths("Weather|Snow||0,Weather||1,Animals||2,Animals|Fish||3"), vec!["Weather|Snow", "Animals|Fish"]);
        // a name that merely starts like another keyword is not its parent; a non-numeric tail stays
        assert_eq!(paths("Wet||0,Weather||1,a||b"), vec!["Wet", "Weather", "a|b"]);
        assert!(paths("||,||7, ,").is_empty());
    }

    #[test]
    fn iptc_fields_become_escaped_xmp() {
        let x = iptc_xmp(&|c: &str| match c {
            "ZSTATUS_TITLE" => Some("Owls <night> & more".into()),
            "ZCONTACT_CREATOR" => Some("A; B".into()),
            _ => None,
        });
        assert!(x.contains("Owls &lt;night&gt; &amp; more"));
        assert!(x.contains("<rdf:li>A</rdf:li><rdf:li>B</rdf:li>"));
        assert!(iptc_xmp(&|_| None).is_empty());
    }

    #[test]
    fn hostile_files_are_errors_not_panics() {
        let dir = std::env::temp_dir().join(format!("c1-hostile-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (n, bytes) in [b"".as_slice(), b"SQLite format 3\0".as_slice(), &[0xffu8; 4096]].iter().enumerate() {
            let p = dir.join(format!("bad{n}.cocatalogdb"));
            std::fs::write(&p, bytes).unwrap();
            assert!(is_capture_one(&p));
            let cancel = AtomicBool::new(false);
            assert!(read_with_progress(&p, &cancel, &AtomicUsize::new(0), &AtomicUsize::new(0)).is_err());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What a hostile catalog must give: an error, or an import of what it can (`Some(n)`: that many
    /// photos), never a panic or a hang.
    enum Want {
        Error,
        Photos(usize),
        Either,
    }

    /// Hostile catalogs, table by table, read and imported into a library: missing tables, NULL and
    /// wrongly typed columns, cyclic collection and keyword parents, huge numbers, empty names.
    #[test]
    fn hostile_catalogs_import_what_they_can_or_fail_without_panicking() {
        let loc = || json!({"Z_PK": 1, "ZISRELATIVE": 1, "ZRELATIVEPATH": "./Originals"});
        let image = |extra: Value| {
            let mut i = json!({"Z_PK": 1, "ZIMAGEFILENAME": "a.NEF", "ZIMAGELOCATION": 1});
            if let (Some(i), Some(extra)) = (i.as_object_mut(), extra.as_object()) {
                i.extend(extra.clone());
            }
            i
        };
        let variant = |pk: i64, image: i64| json!({"Z_PK": pk, "ZIMAGE": image, "ZCOMBINEDSETTINGS": 10});
        let base = || vec![("ZPATHLOCATION", vec![loc()]), ("ZIMAGE", vec![image(json!({}))]), ("ZVARIANT", vec![variant(7, 1)])];
        let with = |mut tables: Vec<(&'static str, Vec<Value>)>, more: Vec<(&'static str, Vec<Value>)>| {
            for (name, rows) in more {
                tables.retain(|(n, _)| *n != name);
                tables.push((name, rows));
            }
            tables
        };
        let album = |pk: i64, parent: i64, ent: i64| json!({"Z_PK": pk, "Z_ENT": ent, "ZPARENT": parent, "ZNAME": format!("c{pk}")});
        let entities = || ("ZENTITIES", vec![json!({"Z_ENT": 2, "ZNAME": "AlbumCollection"}), json!({"Z_ENT": 7, "ZNAME": "ProjectCollection"})]);
        let cases: Vec<(&str, Vec<(&str, Vec<Value>)>, Want)> = vec![
            ("no Capture One tables at all", vec![("ZOTHER", vec![json!({"Z_PK": 1})])], Want::Error),
            ("no ZPATHLOCATION", vec![("ZIMAGE", vec![image(json!({}))]), ("ZVARIANT", vec![variant(7, 1)])], Want::Error),
            ("no ZVARIANT", vec![("ZPATHLOCATION", vec![loc()]), ("ZIMAGE", vec![image(json!({}))])], Want::Error),
            ("only the required tables", base(), Want::Photos(1)),
            ("empty tables", vec![("ZPATHLOCATION", vec![]), ("ZIMAGE", vec![]), ("ZVARIANT", vec![])], Want::Either),
            ("every image column NULL", with(base(), vec![("ZIMAGE", vec![json!({"Z_PK": null, "ZIMAGEFILENAME": null})])]), Want::Photos(0)),
            (
                "wrongly typed image columns",
                with(
                    base(),
                    vec![(
                        "ZIMAGE",
                        vec![image(
                            json!({"Z_PK": "one", "ZIMAGELOCATION": "first", "ZWIDTH": "wide", "ZEXP_DATE": "yesterday", "ZISTRASHED": "no"}),
                        )],
                    )],
                ),
                Want::Photos(0),
            ),
            (
                "wrongly typed metadata",
                with(
                    base(),
                    vec![(
                        "ZVARIANTMETADATA",
                        vec![
                            json!({"Z_PK": 1, "ZLAYER": 10, "ZBASIC_RATING": "five", "ZCOLOR_TAG_INDEX": 1.0e300, "ZCONTENT_KEYWORDS": 42, "ZSTATUS_TITLE": 7}),
                        ],
                    )],
                ),
                Want::Photos(1),
            ),
            (
                "huge numbers",
                with(
                    base(),
                    vec![
                        ("ZIMAGE", vec![image(json!({"Z_PK": i64::MAX, "ZWIDTH": i64::MAX, "ZHEIGHT": -i64::MAX, "ZEXP_DATE": 1.0e300}))]),
                        ("ZVARIANT", vec![json!({"Z_PK": i64::MAX, "ZIMAGE": i64::MAX, "ZINDEX": i64::MIN, "ZCOMBINEDSETTINGS": i64::MAX})]),
                        ("ZVARIANTMETADATA", vec![json!({"Z_PK": 1, "ZLAYER": i64::MAX, "ZBASIC_RATING": i64::MAX, "ZCOLOR_TAG_INDEX": i64::MIN})]),
                    ],
                ),
                Want::Photos(1),
            ),
            (
                "negative and zero keys",
                with(base(), vec![("ZIMAGE", vec![image(json!({"Z_PK": 0}))]), ("ZVARIANT", vec![variant(0, 0), variant(-3, 0)])]),
                Want::Photos(0),
            ),
            (
                "a variant shared by two images",
                with(
                    base(),
                    vec![
                        ("ZIMAGE", vec![image(json!({})), image(json!({"Z_PK": 2, "ZIMAGEFILENAME": "b.NEF"}))]),
                        ("ZVARIANT", vec![variant(7, 1), variant(7, 2)]),
                    ],
                ),
                Want::Either,
            ),
            (
                "empty names",
                with(
                    base(),
                    vec![
                        ("ZIMAGE", vec![image(json!({"ZIMAGEFILENAME": ""})), image(json!({"Z_PK": 2, "ZIMAGEFILENAME": "b.NEF"}))]),
                        ("ZVARIANT", vec![variant(7, 1), variant(8, 2)]),
                        ("ZVARIANTMETADATA", vec![json!({"Z_PK": 1, "ZLAYER": 10, "ZCONTENT_KEYWORDS": "||,|,,||9, | ,"})]),
                        ("ZKEYWORD", vec![json!({"Z_PK": 1, "ZNAME": ""})]),
                        entities(),
                        ("ZCOLLECTION", vec![album(1, -1, 7), json!({"Z_PK": 2, "Z_ENT": 2, "ZPARENT": 1, "ZNAME": ""})]),
                        ("ZDOCUMENTCONTENT", vec![json!({"Z_PK": 1, "ZROOTCOLLECTION": 1})]),
                    ],
                ),
                Want::Photos(1),
            ),
            (
                "cyclic collection parents",
                with(
                    base(),
                    vec![
                        entities(),
                        // a self-parented album, a two-collection loop with an album under it, and a project under the loop
                        ("ZCOLLECTION", vec![album(1, -1, 7), album(2, 2, 2), album(3, 4, 7), album(4, 3, 7), album(5, 3, 2), album(6, 5, 7)]),
                        ("ZDOCUMENTCONTENT", vec![json!({"Z_PK": 1, "ZROOTCOLLECTION": 1})]),
                        (
                            "ZIMAGEINCOLLECTION",
                            vec![json!({"Z_PK": 1, "ZCOLLECTION": 5, "ZIMAGE": 1}), json!({"Z_PK": 2, "ZCOLLECTION": 2, "ZIMAGE": 1})],
                        ),
                    ],
                ),
                Want::Photos(1),
            ),
            (
                "cyclic keyword parents",
                with(
                    base(),
                    vec![
                        (
                            "ZKEYWORD",
                            vec![
                                json!({"Z_PK": 1, "ZNAME": "a", "ZPARENT": 1}),
                                json!({"Z_PK": 2, "ZNAME": "b", "ZPARENT": 3}),
                                json!({"Z_PK": 3, "ZNAME": "c", "ZPARENT": 2}),
                            ],
                        ),
                        ("ZVARIANTMETADATA", vec![json!({"Z_PK": 1, "ZLAYER": 10, "ZCONTENT_KEYWORDS": "a,b,c"})]),
                    ],
                ),
                Want::Photos(1),
            ),
            (
                "hostile develop layers",
                with(
                    base(),
                    vec![
                        (
                            "ZENTITIES",
                            vec![json!({"Z_ENT": 17, "ZNAME": "VariantAdjustmentLayer"}), json!({"Z_ENT": 18, "ZNAME": "VariantDefaultLayer"})],
                        ),
                        (
                            "ZVARIANTLAYER",
                            vec![
                                json!({"Z_PK": 1, "Z_ENT": 18, "ZVARIANT": 7, "ZROTATION": "ninety", "ZEXPOSURE": 0, "ZCROP": null, "ZWHITEBALANCE": "1;1;1"}),
                                json!({"Z_PK": 2, "Z_ENT": 17, "ZVARIANT": 7, "ZROTATION": 1.0e300, "ZEXPOSURE": 1.0e300, "ZCROP": "1e308;-1e308;1e308;1e308", "ZWHITEBALANCE": "nan;0;-1"}),
                                json!({"Z_PK": 3, "Z_ENT": 17, "ZVARIANT": "seven", "ZROTATION": null, "ZEXPOSURE": "x", "ZCROP": ";;;;", "ZWHITEBALANCE": null}),
                                json!({"Z_PK": 4, "Z_ENT": "17", "ZVARIANT": i64::MAX, "ZROTATION": -1.0e300, "ZEXPOSURE": null, "ZCROP": "1;1;1;1", "ZWHITEBALANCE": 5}),
                            ],
                        ),
                    ],
                ),
                Want::Photos(1),
            ),
            (
                "locations with hostile paths",
                with(
                    base(),
                    vec![(
                        "ZPATHLOCATION",
                        vec![
                            json!({"Z_PK": 1, "ZISRELATIVE": 1, "ZRELATIVEPATH": "../../../../../../../../../../../../.."}),
                            json!({"Z_PK": 2, "ZWINROOT": "\\\\?\\UNC\\", "ZMACROOT": 5}),
                        ],
                    )],
                ),
                Want::Either,
            ),
        ];
        let dir = std::env::temp_dir().join(format!("c1-hostile-tables-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (n, (what, tables, want)) in cases.into_iter().enumerate() {
            let db = dir.join(format!("case{n}.cocatalogdb"));
            std::fs::write(&db, sqlite(&tables)).unwrap();
            let cancel = AtomicBool::new(false);
            let read = read_with_progress(&db, &cancel, &AtomicUsize::new(0), &AtomicUsize::new(0));
            let mut s = crate::Session::new();
            let imported = crate::lightroom_catalog::import(&mut s, &db, false);
            match want {
                Want::Error => assert!(read.is_err() && imported.is_err(), "{what}: should fail, read {:?}", read.map(|r| r.photos.len())),
                Want::Photos(count) => {
                    let photos = read.unwrap_or_else(|e| panic!("{what}: {e}")).photos.len();
                    assert_eq!(photos, count, "{what}");
                    assert!(imported.is_ok(), "{what}: {imported:?}");
                }
                Want::Either => {}
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A keyword field with millions of entries takes linear time and keeps at most
    /// [`MAX_KEYWORDS`]; a keyword tree deeper than [`MAX_DEPTH`] is cut there.
    #[test]
    fn a_huge_keyword_field_and_a_deep_tree_stay_bounded() {
        let (tree, names) = (HashMap::new(), HashMap::new());
        let field: String = (0..2_000_000).map(|i| format!("k{i}|x||{i},")).collect();
        let started = std::time::Instant::now();
        let (kept, cut) = keyword_paths(&field, &tree, &names);
        assert_eq!((kept.len(), cut), (MAX_KEYWORDS, true), "cut at the cap, and said so");
        assert!(started.elapsed() < std::time::Duration::from_secs(20), "{:?}", started.elapsed());
        // `s0`, `s0|s1`, … `s0|…|s29`, over and over: only the deepest stays
        let chain: Vec<String> = (1..=30).map(|d| (0..d).map(|j| format!("s{j}")).collect::<Vec<_>>().join("|")).collect();
        let parents: String = (0..50_000).map(|i| format!("{},", chain[i % 30])).collect();
        assert_eq!(keyword_paths(&parents, &tree, &names), (vec![chain[29].clone()], false), "repeats are no reason to cut");
        // exactly the cap is no cut
        let full: String = (0..MAX_KEYWORDS).map(|i| format!("k{i},")).collect();
        assert!(!keyword_paths(&full, &tree, &names).1, "exactly the cap is no cut");
        // a chain of 1,000 keywords, each the child of the one before
        let tree =
            by_pk((1..=1000).map(|i| serde_json::from_value(json!({"Z_PK": i, "ZNAME": format!("n{i}"), "ZPARENT": i - 1})).unwrap()).collect());
        let mut names: HashMap<String, Vec<i64>> = HashMap::new();
        names.insert("n1000".into(), vec![1000]);
        assert_eq!(keyword_paths("n1000", &tree, &names).0[0].matches('|').count(), MAX_DEPTH - 1);
    }

    /// The synthetic catalog the manual's import-result screenshot imports (`docs/manual-site/scripts/fixtures/capture-one`,
    /// shot `capture-one-import-result`): invented names, three small placeholder JPEGs next to it and five raws on a drive
    /// that doesn't exist, with settings that are imported and some that aren't. Its files and their bytes.
    fn manual_shot_fixture() -> Vec<(&'static str, Vec<u8>)> {
        use lcc_codecs::{ChromaSubsampling, EncodeImage, EncodeMeta, Samples, encode_jpeg};
        let placeholder = |rgb: [u8; 3]| {
            let pixels: Vec<u8> = (0..64 * 48).flat_map(|_| rgb).collect();
            encode_jpeg(&EncodeImage::new(64, 48, 3, Samples::U8(&pixels)), 85, ChromaSubsampling::S444, &EncodeMeta::default()).unwrap()
        };
        let edited = 750_000_000.0; // Core Data seconds since 2001: in 2024
        let mut images = Vec::new();
        for (pk, name) in [(1, "meadow-01.jpg"), (2, "meadow-02.jpg"), (3, "harbour-03.jpg")] {
            images.push(
                json!({"Z_PK": pk, "ZIMAGEFILENAME": name, "ZIMAGELOCATION": 1, "ZWIDTH": 64, "ZHEIGHT": 48, "ZSETTINGSMODIFICATIONDATE": edited}),
            );
        }
        for pk in 4..=8 {
            images.push(json!({"Z_PK": pk, "ZIMAGEFILENAME": format!("example-{pk:04}.NEF"), "ZIMAGELOCATION": 2, "ZWIDTH": 6000, "ZHEIGHT": 4000,
                "ZSETTINGSMODIFICATIONDATE": edited, "ZCAMERA_MAKE": "EXAMPLE CORP", "ZCAMERA_MODEL": "EXAMPLE X1"}));
        }
        let variants: Vec<Value> = (1..=8).map(|pk| json!({"Z_PK": 100 + pk, "ZIMAGE": pk, "ZVARIANTUUID": format!("example-{pk}")})).collect();
        let mut layers = Vec::new();
        let mut next = 0;
        let mut add = |ent: i64, variant: i64, values: Value| {
            next += 1;
            layers.push(layer(next, ent, 100 + variant, values));
        };
        for v in 1..=3 {
            add(18, v, json!({"ZEXPOSURE": 0, "ZROTATION": 0, "ZBRIGHTNESS": 0, "ZLEVELSHIGHLIGHT": "1.000;1.000;1.000;1.000"}));
        }
        add(17, 1, json!({"ZEXPOSURE": 0.3, "ZBRIGHTNESS": 10}));
        add(17, 2, json!({"ZROTATION": 1.5, "ZCROP": "32;24;56;40", "ZBRIGHTNESS": 6}));
        add(17, 3, json!({"ZEXPOSURE": -0.2, "ZLEVELSHIGHLIGHT": "1.000;1.000;1.000;0.950"}));
        for v in 4..=8 {
            add(18, v, json!({"ZEXPOSURE": 0, "ZROTATION": 0, "ZBRIGHTNESS": 0, "ZHIGHLIGHTRECOVERY": 0, "ZWHITEBALANCE": "2.000;1.000;1.400"}));
            let mut own = json!({"ZWHITEBALANCE": "2.200;1.000;1.200", "ZEXPOSURE": 0.4});
            if v <= 5 {
                own["ZBRIGHTNESS"] = json!(12);
            }
            if v >= 6 {
                own["ZHIGHLIGHTRECOVERY"] = json!(40);
            }
            add(17, v, own);
        }
        add(19, 4, json!({"ZINDEX": 1, "ZLENSLCC": "example-lcc"}));
        add(19, 5, json!({"ZINDEX": 1, "ZLENSLCC": "example-lcc"}));
        add(23, 6, json!({"ZEXPOSURE": 0.5}));
        let catalog = sqlite(&[
            (
                "ZPATHLOCATION",
                vec![
                    json!({"Z_PK": 1, "ZRELATIVEPATH": "./Originals", "ZISRELATIVE": 1}),
                    json!({"Z_PK": 2, "ZWINROOT": "Q:\\", "ZMACROOT": "/Volumes/Example Archive", "ZRELATIVEPATH": "Example Shoot", "ZISRELATIVE": 0}),
                ],
            ),
            ("ZIMAGE", images),
            ("ZVARIANT", variants),
            (
                "ZENTITIES",
                vec![
                    json!({"Z_ENT": 17, "ZNAME": "VariantAdjustmentLayer"}),
                    json!({"Z_ENT": 18, "ZNAME": "VariantDefaultLayer"}),
                    json!({"Z_ENT": 19, "ZNAME": "VariantStyleLayer"}),
                    json!({"Z_ENT": 23, "ZNAME": "LocalAdjustmentsLayer"}),
                ],
            ),
            ("ZVARIANTLAYER", layers),
        ]);
        vec![
            ("Example.cocatalogdb", catalog),
            ("Originals/meadow-01.jpg", placeholder([96, 140, 72])),
            ("Originals/meadow-02.jpg", placeholder([120, 156, 88])),
            ("Originals/harbour-03.jpg", placeholder([70, 110, 150])),
        ]
    }

    /// The manual's screenshot fixture is what [`manual_shot_fixture`] makes, and its import shows what the shot is
    /// for: the settings not imported yet with their photo counts, and the white balance marked approximate. When the
    /// generator changes, the test writes the new files to the temp folder and says where, to copy over.
    #[test]
    fn the_manual_shot_fixture_is_synthetic_and_current() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/manual-site/scripts/fixtures/capture-one");
        let files = manual_shot_fixture();
        let stale: Vec<&str> =
            files.iter().filter(|(name, bytes)| std::fs::read(fixture.join(name)).ok().as_ref() != Some(bytes)).map(|(n, _)| *n).collect();
        if !stale.is_empty() {
            let out = std::env::temp_dir().join("c1-manual-shot-fixture");
            for (name, bytes) in &files {
                std::fs::create_dir_all(out.join(name).parent().unwrap()).unwrap();
                std::fs::write(out.join(name), bytes).unwrap();
            }
            panic!("{stale:?} differ from the generator: copy {} to {}", out.display(), fixture.display());
        }
        // read by a path relative to the working folder, as the screenshot runner gives it (from the repository root):
        // the originals inside the catalog's folder are found, the five on the missing drive aren't
        let relative = Path::new("../../docs/manual-site/scripts/fixtures/capture-one/Example.cocatalogdb");
        let read = read_with_progress(relative, &AtomicBool::new(false), &AtomicUsize::new(0), &AtomicUsize::new(0)).unwrap();
        let here: Vec<bool> = read.photos.iter().map(CatalogPhoto::original_is_file).collect();
        assert_eq!(here, [true, true, true, false, false, false, false, false], "{:?}", read.photos.iter().map(|p| &p.path).collect::<Vec<_>>());
        let mut s = crate::Session::new();
        let report = crate::lightroom_catalog::import(&mut s, &fixture.join("Example.cocatalogdb"), false).unwrap();
        let warnings: Vec<&str> = report["warnings"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        // all of them fit the result window's eight warnings
        assert!(warnings.len() <= 8, "{warnings:?}");
        for line in [
            "Capture One Brightness: used on 4 photo(s)",
            "Capture One High Dynamic Range: used on 3 photo(s)",
            "Capture One LCC: used on 2 photo(s)",
            "Capture One Levels: used on 1 photo(s)",
            "Capture One Local adjustment layers: used on 1 photo(s)",
            "Capture One White Balance: approximate on 5 photo(s)",
        ] {
            assert!(warnings.iter().any(|w| w.starts_with(line)), "{line}: {warnings:?}");
        }
    }
}
