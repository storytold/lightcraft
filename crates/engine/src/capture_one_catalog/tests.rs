//! Tests on synthetic catalogs: SQLite files built here from the public file format, with the
//! tables and columns observed in catalogs Capture One wrote. No real catalog or photo is used.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use super::*;

// ---- a small SQLite writer: one leaf page per table

#[derive(Clone, Debug)]
enum V {
    N,
    I(i64),
    R(f64),
    T(String),
}

fn t(text: &str) -> V {
    V::T(text.into())
}

/// A table: its name, its columns and its rows as `(column, value)` lists; a column a row
/// leaves out is NULL.
#[derive(Clone)]
struct Spec {
    name: &'static str,
    columns: Vec<&'static str>,
    rows: Vec<Vec<(&'static str, V)>>,
}

fn spec(name: &'static str, columns: &[&'static str], rows: Vec<Vec<(&'static str, V)>>) -> Spec {
    Spec { name, columns: columns.to_vec(), rows }
}

const PAGE: usize = 4096;

fn varint(mut n: u64) -> Vec<u8> {
    let mut out = vec![(n & 127) as u8];
    n >>= 7;
    while n > 0 {
        out.push((n & 127) as u8 | 128);
        n >>= 7;
    }
    out.reverse();
    out
}

fn record(values: &[V]) -> Vec<u8> {
    let (mut header, mut body) = (Vec::new(), Vec::new());
    for value in values {
        let serial = match value {
            V::N => 0,
            V::I(n) => {
                body.extend(n.to_be_bytes());
                6
            }
            V::R(f) => {
                body.extend(f.to_be_bytes());
                7
            }
            V::T(s) => {
                body.extend(s.as_bytes());
                13 + 2 * s.len() as u64
            }
        };
        header.extend(varint(serial));
    }
    let mut out = varint(1 + header.len() as u64);
    out.extend(header);
    out.extend(body);
    out
}

/// Write `rows` as `(rowid, values)` into a table leaf page whose header starts at `offset`.
fn leaf(page: &mut [u8], offset: usize, rows: &[(i64, Vec<V>)]) {
    page[offset] = 13;
    page[offset + 3..offset + 5].copy_from_slice(&(rows.len() as u16).to_be_bytes());
    let mut end = page.len();
    for (i, (rowid, values)) in rows.iter().enumerate() {
        let payload = record(values);
        let mut cell = varint(payload.len() as u64);
        cell.extend(varint(*rowid as u64));
        cell.extend(payload);
        end = end.checked_sub(cell.len()).expect("the test table fits one page");
        assert!(end > offset + 8 + rows.len() * 2, "the test table fits one page");
        page[end..end + cell.len()].copy_from_slice(&cell);
        page[offset + 8 + i * 2..offset + 10 + i * 2].copy_from_slice(&(end as u16).to_be_bytes());
    }
    page[offset + 5..offset + 7].copy_from_slice(&(end as u16).to_be_bytes());
}

/// The key column Capture One declares `INTEGER PRIMARY KEY` (the row id).
fn key_column(table: &str) -> &'static str {
    if table == "ZENTITIES" { "Z_ENT" } else { "Z_PK" }
}

fn database(tables: &[Spec]) -> Vec<u8> {
    let mut bytes = vec![0u8; (tables.len() + 1) * PAGE];
    bytes[..16].copy_from_slice(b"SQLite format 3\0");
    bytes[16..18].copy_from_slice(&(PAGE as u16).to_be_bytes());
    bytes[18] = 1;
    bytes[19] = 1;
    bytes[21] = 64;
    bytes[22] = 32;
    bytes[23] = 32;
    bytes[28..32].copy_from_slice(&(tables.len() as u32 + 1).to_be_bytes());
    bytes[44..48].copy_from_slice(&4u32.to_be_bytes());
    bytes[56..60].copy_from_slice(&1u32.to_be_bytes());
    let mut schema = Vec::new();
    for (i, table) in tables.iter().enumerate() {
        let key = key_column(table.name);
        let columns: Vec<String> =
            table.columns.iter().map(|c| if *c == key { format!("{c} INTEGER PRIMARY KEY") } else { format!("{c} VARCHAR") }).collect();
        let sql = format!("CREATE TABLE {} ( {} )", table.name, columns.join(", "));
        schema.push((i as i64 + 1, vec![t("table"), t(table.name), t(table.name), V::I(i as i64 + 2), V::T(sql)]));
        let mut rows: Vec<(i64, Vec<V>)> = table
            .rows
            .iter()
            .enumerate()
            .map(|(at, row)| {
                let value = |column: &str| row.iter().find(|(name, _)| *name == column).map_or(V::N, |(_, v)| v.clone());
                let rowid = match value(key) {
                    V::I(n) => n,
                    _ => at as i64 + 1,
                };
                // the key is the row id: SQLite stores NULL in its place
                (rowid, table.columns.iter().map(|c| if *c == key { V::N } else { value(c) }).collect())
            })
            .collect();
        // a table leaf lists its cells in row id order; tests that read in another order (a
        // variant's index) give that column values apart from the row ids
        rows.sort_by_key(|(rowid, _)| *rowid);
        leaf(&mut bytes[(i + 1) * PAGE..(i + 2) * PAGE], 0, &rows);
    }
    leaf(&mut bytes[..PAGE], 100, &schema);
    bytes
}

// ---- a catalog on disk

/// A folder under the system's temporary folder, removed when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("lightcraft-c1-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    /// Write `tables` as `Test.cocatalog/Test.cocatalogdb` and return the package folder.
    fn catalog(&self, tables: &[Spec]) -> PathBuf {
        let package = self.0.join("Test.cocatalog");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("Test.cocatalogdb"), database(tables)).unwrap();
        package
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// Entity numbers differ between Capture One versions; these are not the ones of any catalog seen.
const ALBUM: i64 = 3;
const SMART: i64 = 4;
const PROJECT: i64 = 5;
const GROUP: i64 = 6;
const TRASH: i64 = 7;
const FOLDER: i64 = 8;
const ALL: i64 = 9;
const KEYWORD: i64 = 44;
const SYNONYM: i64 = 45;

fn entities() -> Spec {
    let names = [
        (ALBUM, "AlbumCollection"),
        (SMART, "SmartCollection"),
        (PROJECT, "ProjectCollection"),
        (GROUP, "VirtualFolderCollection"),
        (TRASH, "TrashCollection"),
        (FOLDER, "CatalogFolderCollection"),
        (ALL, "CatalogAllImagesCollection"),
        (KEYWORD, "Keyword"),
        (SYNONYM, "Synonym"),
    ];
    spec("ZENTITIES", &["Z_ENT", "ZNAME"], names.iter().map(|(id, name)| vec![("Z_ENT", V::I(*id)), ("ZNAME", t(name))]).collect())
}

fn versions() -> Spec {
    let row = |pk: i64, app: &str, schema: i64| {
        vec![("Z_PK", V::I(pk)), ("ZAUTHOR", t(app)), ("ZVERSION", V::I(schema)), ("ZCOMPATIBLEVERSION", V::I(schema))]
    };
    spec(
        "ZVERSIONINFO",
        &["Z_ENT", "Z_PK", "ZAUTHOR", "ZCOMPATIBLEVERSION", "ZVERSION"],
        vec![row(2, "Datacore CLI", 1621), row(1, "14.0.0.1 Pro Win", 1400), row(3, "16.3.9.9 Pro Mac", 1621)],
    )
}

const LOCATION_COLUMNS: &[&str] = &["Z_ENT", "Z_PK", "ZWINROOT", "ZMACROOT", "ZRELATIVEPATH", "ZISRELATIVE", "ZVOLUME"];
const IMAGE_COLUMNS: &[&str] = &[
    "Z_ENT",
    "Z_PK",
    "ZIMAGEUUID",
    "ZIMPORTDATE",
    "ZIMAGELOCATION",
    "ZIMAGEFILENAME",
    "ZIMAGECLASSIFICATION",
    "ZISTRASHED",
    "ZWIDTH",
    "ZHEIGHT",
    "ZEXP_FOCALLENGTH",
    "ZEXP_EXPOSURECOMPENSATION",
    "ZEXP_APERTURE",
    "ZEXP_SHUTTERSPEED",
    "ZEXP_DATE",
    "ZCAMERA_MODEL",
    "ZCAMERA_MAKE",
    "ZCAMERA_LENS",
    "ZFILE_SIZE",
    "ZISO",
    "ZGPSLATITUDE",
    "ZGPSLONGITUDE",
];
const VARIANT_COLUMNS: &[&str] =
    &["Z_ENT", "Z_PK", "ZINDEX", "ZVARIANTUUID", "ZADJUSTMENTLAYER", "ZCOMBINEDSETTINGS", "ZDEFAULTLAYER", "ZIMAGE", "ZISMODIFIED"];
const METADATA_COLUMNS: &[&str] = &[
    "Z_ENT",
    "Z_PK",
    "ZLAYER",
    "ZBASIC_RATING",
    "ZCONTACT_CREATOR",
    "ZCONTENT_HEADLINE",
    "ZCONTENT_DESCRIPTION",
    "ZCONTENT_KEYWORDS",
    "ZIMAGE_CITY",
    "ZIMAGE_DATECREATED",
    "ZSTATUS_COPYRIGHTNOTICE",
    "ZSTATUS_TITLE",
    "ZCOLOR_TAG_INDEX",
];
const COLLECTION_COLUMNS: &[&str] = &[
    "Z_ENT",
    "Z_PK",
    "ZUUID",
    "ZCOLLECTIONINDEX",
    "ZNAME",
    "ZPARENT",
    "ZTRASHCOLLECTION",
    "ZALLIMAGESCOLLECTION",
    "ZLASTCAPTURESCOLLECTION",
    "ZLASTIMPORTSCOLLECTION",
    "ZFOLDERLOCATION",
];

fn image(pk: i64, location: i64, name: &str) -> Vec<(&'static str, V)> {
    vec![
        ("Z_PK", V::I(pk)),
        ("ZIMAGEUUID", V::T(format!("IMAGE-{pk}"))),
        ("ZIMAGELOCATION", V::I(location)),
        ("ZIMAGEFILENAME", t(name)),
        ("ZISTRASHED", V::I(0)),
    ]
}

/// A variant whose default, adjustment and combined layers are `layer`, `layer + 1`, `layer + 2`.
fn variant(pk: i64, image: i64, index: i64, layer: i64) -> Vec<(&'static str, V)> {
    vec![
        ("Z_PK", V::I(pk)),
        ("ZIMAGE", V::I(image)),
        ("ZINDEX", V::I(index)),
        ("ZVARIANTUUID", V::T(format!("VARIANT-{pk}"))),
        ("ZDEFAULTLAYER", V::I(layer)),
        ("ZADJUSTMENTLAYER", V::I(layer + 1)),
        ("ZCOMBINEDSETTINGS", V::I(layer + 2)),
        ("ZISMODIFIED", V::I(1)),
    ]
}

fn collection(pk: i64, entity: i64, name: &str, parent: i64, index: i64) -> Vec<(&'static str, V)> {
    vec![
        ("Z_ENT", V::I(entity)),
        ("Z_PK", V::I(pk)),
        ("ZUUID", V::T(format!("COLLECTION-{pk}"))),
        ("ZNAME", t(name)),
        ("ZPARENT", V::I(parent)),
        ("ZCOLLECTIONINDEX", V::I(index)),
    ]
}

/// The root's pointers at Capture One's own collections 2 (all images), 3 (recent imports),
/// 4 (recent captures) and 5 (trash).
fn root() -> Vec<(&'static str, V)> {
    let mut row = collection(1, PROJECT, "root", -1, 0);
    row.extend([
        ("ZALLIMAGESCOLLECTION", V::I(2)),
        ("ZLASTIMPORTSCOLLECTION", V::I(3)),
        ("ZLASTCAPTURESCOLLECTION", V::I(4)),
        ("ZTRASHCOLLECTION", V::I(5)),
    ]);
    row
}

fn member(collection: i64, column: &'static str, id: i64) -> Vec<(&'static str, V)> {
    vec![("ZCOLLECTION", V::I(collection)), (column, V::I(id))]
}

/// A catalog with one of everything the reader handles. `photos` is a folder of referenced
/// originals on this computer.
fn sample(photos: &Path) -> Vec<Spec> {
    let photos = photos.to_string_lossy().into_owned();
    let (here, elsewhere, other_root) = if cfg!(windows) { ("ZWINROOT", "ZMACROOT", "/Volumes/Disk  ") } else { ("ZMACROOT", "ZWINROOT", "D:\\") };
    let mut first = image(1, 1, "managed.RAF");
    first.extend([
        ("ZIMAGECLASSIFICATION", V::I(17)),
        ("ZWIDTH", V::I(6000)),
        ("ZHEIGHT", V::I(4000)),
        ("ZFILE_SIZE", V::I(23_000_000)),
        ("ZEXP_DATE", V::R(1_623_752_130.0)),
        ("ZIMPORTDATE", V::R(1_640_995_206.5)),
        ("ZCAMERA_MAKE", t("ACME")),
        ("ZCAMERA_MODEL", t("Model 7")),
        ("ZCAMERA_LENS", t("Zoom 24-70mm ")),
        ("ZISO", V::I(640)),
        ("ZEXP_APERTURE", V::R(5.6)),
        ("ZEXP_SHUTTERSPEED", V::R(0.004)),
        ("ZEXP_FOCALLENGTH", V::R(35.0)),
        ("ZEXP_EXPOSURECOMPENSATION", t("-67/100")),
        ("ZGPSLATITUDE", t("52.5")),
        ("ZGPSLONGITUDE", t("13.4")),
    ]);
    let mut trashed = image(4, 1, "trashed.NEF");
    trashed.retain(|(name, _)| *name != "ZISTRASHED");
    trashed.push(("ZISTRASHED", V::I(1)));
    let metadata = |pk: i64, layer: i64, rating: i64, tag: i64, keywords: &str| {
        vec![
            ("Z_PK", V::I(pk)),
            ("ZLAYER", V::I(layer)),
            ("ZBASIC_RATING", V::I(rating)),
            ("ZCOLOR_TAG_INDEX", V::I(tag)),
            ("ZCONTENT_KEYWORDS", t(keywords)),
        ]
    };
    let mut shown = metadata(1, 12, 4, 3, "Culture||0,Culture|Buildings||1");
    shown.extend([
        ("ZSTATUS_TITLE", t("  A title  ")),
        ("ZCONTENT_HEADLINE", t("A headline")),
        ("ZCONTENT_DESCRIPTION", t("A description")),
        ("ZCONTACT_CREATOR", t("A creator")),
        ("ZSTATUS_COPYRIGHTNOTICE", t("A notice          ")),
        ("ZIMAGE_CITY", t("A city")),
        ("ZIMAGE_DATECREATED", t("20210615")),
    ]);
    let mut folder = collection(40, FOLDER, "", -1, 5);
    folder.push(("ZFOLDERLOCATION", V::I(1)));
    vec![
        entities(),
        versions(),
        spec(
            "ZDOCUMENTCONTENT",
            &["Z_ENT", "Z_PK", "ZDOCUMENTUUID", "ZROOTCOLLECTION"],
            vec![vec![("ZDOCUMENTUUID", t("c1-document-test")), ("ZROOTCOLLECTION", V::I(1))]],
        ),
        spec(
            "ZPATHLOCATION",
            LOCATION_COLUMNS,
            vec![
                vec![("Z_PK", V::I(1)), ("ZRELATIVEPATH", t("./Originals")), ("ZISRELATIVE", V::I(1)), ("ZVOLUME", t("/"))],
                vec![("Z_PK", V::I(2)), (here, V::T(photos)), ("ZRELATIVEPATH", t("2024/Trip")), ("ZISRELATIVE", V::I(0))],
                vec![("Z_PK", V::I(3)), (elsewhere, t(other_root)), ("ZRELATIVEPATH", t("Shoots")), ("ZISRELATIVE", V::I(0))],
            ],
        ),
        spec("ZIMAGE", IMAGE_COLUMNS, vec![first, image(2, 2, "referenced.jpg"), image(3, 3, "elsewhere.ARW"), trashed]),
        spec(
            "ZVARIANT",
            VARIANT_COLUMNS,
            // image 1 has two variants; Capture One numbers them 127, 255, …
            vec![variant(8, 1, 255, 20), variant(1, 1, 127, 10), variant(2, 2, 127, 30), variant(3, 3, 127, 40), variant(4, 4, 127, 50)],
        ),
        spec(
            "ZVARIANTMETADATA",
            METADATA_COLUMNS,
            vec![
                // default layer 10 carries what the file says; the combined layer 12 is what shows
                metadata(9, 10, 0, 0, ""),
                shown,
                metadata(2, 22, 1, 0, "Nature||1,Nature|Animals||0"),
                metadata(3, 32, 0, 2, ""),
                metadata(4, 42, 5, 6, ""),
            ],
        ),
        spec(
            "ZCOLLECTION",
            COLLECTION_COLUMNS,
            vec![
                root(),
                collection(2, ALL, "Alle Bilder", -1, 1),
                collection(3, GROUP, "Letzte Importe", 1, 2),
                collection(4, GROUP, "Letzte Aufnahmen", 1, 1),
                collection(5, TRASH, "Papierkorb", -1, 4),
                collection(6, ALBUM, "An import", 3, 3),
                folder,
                collection(20, GROUP, "A group", 1, 4),
                collection(21, ALBUM, "Second album", 20, 6),
                collection(22, ALBUM, "First album", 20, 5),
                collection(23, PROJECT, "A project", 1, 7),
                collection(24, ALBUM, "In the project", 23, 8),
                collection(25, SMART, "Five stars", 1, 9),
            ],
        ),
        spec(
            "ZIMAGEINCOLLECTION",
            &["Z_ENT", "Z_PK", "ZCOLLECTION", "ZIMAGE"],
            vec![
                member(22, "ZIMAGE", 1),
                member(22, "ZIMAGE", 2),
                member(22, "ZIMAGE", 1),
                member(6, "ZIMAGE", 1),
                member(23, "ZIMAGE", 2),
                member(24, "ZIMAGE", 4),
            ],
        ),
        spec("ZVARIANTINCOLLECTION", &["Z_ENT", "Z_PK", "ZCOLLECTION", "ZVARIANT"], vec![member(22, "ZVARIANT", 8), member(21, "ZVARIANT", 3)]),
        spec("ZKEYWORD", KEYWORD_COLUMNS, keyword_rows()),
    ]
}

const KEYWORD_COLUMNS: &[&str] = &["Z_ENT", "Z_PK", "ZNAME", "ZPARENT", "ZLEFT", "ZRIGHT", "ZISEXPORTABLE"];

/// `Culture` ⊃ `Buildings`, `Nature` ⊃ `Animals` and the unused `Plants`, a keyword not exported,
/// and a synonym row, which is not a keyword.
fn keyword_rows() -> Vec<Vec<(&'static str, V)>> {
    let keyword = |pk: i64, name: &str, parent: i64, left: i64, exportable: i64| {
        vec![
            ("Z_ENT", V::I(KEYWORD)),
            ("Z_PK", V::I(pk)),
            ("ZNAME", t(name)),
            ("ZPARENT", V::I(parent)),
            ("ZLEFT", V::I(left)),
            ("ZISEXPORTABLE", V::I(exportable)),
        ]
    };
    vec![
        keyword(4, "Nature", -1, 5, 1),
        keyword(1, "Culture", -1, 1, 1),
        keyword(2, "Buildings", 1, 2, 1),
        keyword(5, "Animals", 4, 6, 1),
        keyword(6, "Plants", 4, 8, 1),
        keyword(7, "Private", -1, 10, 0),
        vec![("Z_ENT", V::I(SYNONYM)), ("Z_PK", V::I(8)), ("ZNAME", t("Fauna")), ("ZPARENT", V::I(5)), ("ZLEFT", V::I(12))],
    ]
}

/// Replace the table `name` of `tables`.
fn with(mut tables: Vec<Spec>, replacement: Spec) -> Vec<Spec> {
    match tables.iter_mut().find(|t| t.name == replacement.name) {
        Some(table) => *table = replacement,
        None => tables.push(replacement),
    }
    tables
}

fn without(tables: Vec<Spec>, name: &str) -> Vec<Spec> {
    tables.into_iter().filter(|t| t.name != name).collect()
}

fn table<'a>(tables: &'a mut [Spec], name: &str) -> &'a mut Spec {
    tables.iter_mut().find(|t| t.name == name).unwrap()
}

// ---- reading

#[test]
fn photos_come_with_their_originals_variants_and_metadata() {
    let scratch = Scratch::new("photos");
    let package = scratch.catalog(&sample(&scratch.0.join("Photos")));
    let catalog = read(&package).unwrap();

    assert_eq!(catalog.document_uuid.as_deref(), Some("c1-document-test"));
    assert_eq!(catalog.trashed, 1);
    assert!(!catalog.in_use);
    assert_eq!(catalog.warnings, Vec::<String>::new());
    let names: Vec<_> = catalog.photos.iter().map(|p| (p.file_name.as_str(), p.variant_id, p.variant_index)).collect();
    assert_eq!(names, [("managed.RAF", Some(1), 0), ("managed.RAF", Some(8), 1), ("referenced.jpg", Some(2), 0), ("elsewhere.ARW", Some(3), 0)]);

    let first = &catalog.photos[0];
    assert_eq!(Path::new(&first.path), package.join("Originals").join("managed.RAF"));
    assert_eq!((first.original.clone(), first.managed), (Original::Local, true));
    assert_eq!((first.kind, first.width, first.height, first.file_size), (OriginalKind::Raw, 6000, 4000, 23_000_000));
    // the camera's clock, and the moment of the import
    assert_eq!(first.captured.as_deref(), Some("2021-06-15T10:15:30"));
    assert_eq!(first.imported.as_deref(), Some("2022-01-01T00:00:06Z"));
    assert_eq!((first.camera_make.as_str(), first.camera_model.as_str(), first.lens.as_str()), ("ACME", "Model 7", "Zoom 24-70mm"));
    assert_eq!((first.iso, first.aperture, first.shutter_seconds, first.focal_mm), (Some(640), Some(5.6), Some(0.004), Some(35.0)));
    assert_eq!(first.exposure_bias, Some(-0.67));
    assert_eq!(first.gps, Some((52.5, 13.4)));
    assert_eq!((first.image_uuid.as_str(), first.variant_uuid.as_str(), first.adjusted), ("IMAGE-1", "VARIANT-1", true));
    // the combined layer's metadata, not the default layer's
    assert_eq!((first.rating, first.color_tag), (4, ColorTag::Yellow));
    assert_eq!(first.keywords, ["Culture|Buildings"]);
    assert_eq!((first.iptc.title.as_str(), first.iptc.copyright.as_str(), first.iptc.city.as_str()), ("A title", "A notice", "A city"));
    assert_eq!(
        (first.iptc.headline.as_str(), first.iptc.description.as_str(), first.iptc.creator.as_str()),
        ("A headline", "A description", "A creator")
    );
    assert_eq!(first.iptc.other.get("IMAGE_DATECREATED").map(String::as_str), Some("20210615"));

    let copy = &catalog.photos[1];
    assert_eq!((copy.rating, copy.color_tag, copy.keywords.clone()), (1, ColorTag::None, vec!["Nature|Animals".to_string()]));
    assert_eq!(copy.path, first.path);

    let referenced = &catalog.photos[2];
    assert_eq!(Path::new(&referenced.path), scratch.0.join("Photos").join("2024").join("Trip").join("referenced.jpg"));
    assert_eq!((referenced.original.clone(), referenced.managed, referenced.color_tag), (Original::Local, false, ColorTag::Orange));
    assert_eq!((referenced.captured.clone(), referenced.gps, referenced.kind), (None, None, OriginalKind::Unknown));

    // a folder recorded for the other system only is kept as written and never looked at
    let elsewhere = &catalog.photos[3];
    assert_eq!(elsewhere.original, Original::NotHere);
    assert_eq!(elsewhere.path, if cfg!(windows) { "/Volumes/Disk  /Shoots/elsewhere.ARW" } else { "D:/Shoots/elsewhere.ARW" });
    assert_eq!((elsewhere.rating, elsewhere.color_tag), (5, ColorTag::Pink));
}

#[test]
fn the_users_collections_keep_their_nesting_and_capture_ones_own_stay_out() {
    let scratch = Scratch::new("collections");
    let catalog = read(&scratch.catalog(&sample(&scratch.0))).unwrap();
    let listed: Vec<_> = catalog.collections.iter().map(|c| (c.name.as_str(), c.kind, c.parent, c.photos.clone())).collect();
    assert_eq!(
        listed,
        [
            ("A group", CollectionKind::Group, None, vec![]),
            // an image is its first variant; a further variant is listed on its own; nothing twice
            ("First album", CollectionKind::Album, Some(20), vec![0, 2, 1]),
            ("Second album", CollectionKind::Album, Some(20), vec![3]),
            // a project repeats its albums' photos: only albums take photos
            ("A project", CollectionKind::Project, None, vec![]),
            // its only photo is in the trash
            ("In the project", CollectionKind::Album, Some(23), vec![]),
            ("Five stars", CollectionKind::SmartAlbum, None, vec![]),
        ]
    );
    assert_eq!(catalog.collections[1].uuid, "COLLECTION-22");
}

#[test]
fn the_version_history_is_oldest_first() {
    let scratch = Scratch::new("versions");
    let catalog = read(&scratch.catalog(&sample(&scratch.0))).unwrap();
    let apps: Vec<_> = catalog.versions.iter().map(|v| (v.app.as_str(), v.schema)).collect();
    assert_eq!(apps, [("14.0.0.1 Pro Win", 1400), ("Datacore CLI", 1621), ("16.3.9.9 Pro Mac", 1621)]);
}

#[test]
fn an_image_without_a_variant_is_still_a_photo_and_a_stray_variant_is_reported() {
    let scratch = Scratch::new("stray");
    let tables = with(sample(&scratch.0), spec("ZVARIANT", VARIANT_COLUMNS, vec![variant(2, 2, 127, 30), variant(9, 77, 127, 60)]));
    let catalog = read(&scratch.catalog(&tables)).unwrap();
    let photos: Vec<_> = catalog.photos.iter().map(|p| (p.image_id, p.variant_id, p.rating)).collect();
    assert_eq!(photos, [(1, None, 0), (2, Some(2), 0), (3, None, 0)]);
    assert_eq!(catalog.warnings, ["1 variant(s) belong to no image in the catalog and were left out"]);
    // an album still finds the image
    assert_eq!(catalog.collections.iter().find(|c| c.name == "First album").unwrap().photos, [0, 1]);
}

#[test]
fn a_reference_with_a_fraction_points_at_no_row() {
    let scratch = Scratch::new("fraction");
    let mut whole = variant(2, 2, 0, 30);
    whole[1].1 = V::R(2.0);
    let mut broken = variant(9, 1, 0, 60);
    broken[1].1 = V::R(1.5);
    let tables = with(sample(&scratch.0), spec("ZVARIANT", VARIANT_COLUMNS, vec![whole, broken]));
    let catalog = read(&scratch.catalog(&tables)).unwrap();
    let photos: Vec<_> = catalog.photos.iter().map(|p| (p.image_id, p.variant_id)).collect();
    assert_eq!(photos, [(1, None), (2, Some(2)), (3, None)]);
    assert_eq!(catalog.warnings, ["1 variant(s) belong to no image in the catalog and were left out"]);
}

#[test]
fn without_uuids_a_variant_is_known_by_its_keys_not_its_place() {
    let photo = CatalogPhoto { image_id: 3, image_uuid: "IMAGE".into(), variant_id: Some(7), variant_index: 2, ..Default::default() };
    // an earlier variant was deleted: the same variant is now second
    let moved = CatalogPhoto { variant_index: 1, ..photo.clone() };
    assert_eq!(convert::identity(&photo, "DOC"), "captureone:image:IMAGE:7");
    assert_eq!(convert::identity(&moved, "DOC"), convert::identity(&photo, "DOC"));
    let without_variant = CatalogPhoto { variant_id: None, variant_index: 0, ..photo.clone() };
    assert_eq!(convert::identity(&without_variant, "DOC"), "captureone:image:IMAGE");
    assert_eq!(convert::identity(&CatalogPhoto { variant_uuid: "V".into(), ..photo.clone() }, "DOC"), "captureone:V");
    // neither UUID: the rows' keys in this catalog
    let bare = CatalogPhoto { image_uuid: String::new(), ..photo };
    assert_eq!(convert::identity(&bare, "DOC"), "captureone:DOC:row:3:7");
    assert_ne!(convert::identity(&bare, "DOC"), convert::identity(&bare, "OTHER"));
}

#[test]
fn photos_without_uuids_keep_their_keys_when_an_earlier_image_is_deleted() {
    let scratch = Scratch::new("bare");
    let mut tables = sample(&scratch.0);
    for name in ["ZIMAGE", "ZVARIANT"] {
        for row in &mut table(&mut tables, name).rows {
            row.retain(|(column, _)| !column.ends_with("UUID"));
        }
    }
    let keys = |tables: &[Spec]| -> Vec<(i64, String)> {
        let data = convert::to_import(read(&scratch.catalog(tables)).unwrap());
        let catalog = read(&scratch.catalog(tables)).unwrap();
        catalog.photos.iter().map(|p| p.image_id).zip(data.photos.into_iter().map(|p| p.uuid)).collect()
    };
    let before = keys(&tables);
    assert!(before.iter().all(|(_, key)| key.contains(":row:")), "{before:?}");
    table(&mut tables, "ZIMAGE").rows.remove(0);
    let after = keys(&tables);
    assert!(!after.is_empty() && after.iter().all(|kept| before.contains(kept)), "{before:?} {after:?}");
}

// ---- finding the database

#[cfg(unix)]
#[test]
fn the_journal_is_looked_for_beside_a_database_whose_name_is_not_utf8() {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let database = Path::new(std::ffi::OsStr::from_bytes(b"/x/Caf\xe9/Test.cocatalogdb"));
    assert_eq!(locate::beside(database, "-journal").into_os_string().into_vec(), b"/x/Caf\xe9/Test.cocatalogdb-journal");
}

#[test]
fn a_package_a_backup_folder_and_the_database_itself_are_found() {
    let scratch = Scratch::new("locate");
    let package = scratch.catalog(&sample(&scratch.0));
    assert!(is_catalog(&package));
    assert!(is_catalog(&package.join("Test.cocatalogdb")));
    assert_eq!(read(&package.join("Test.cocatalogdb")).unwrap().photos.len(), 4);

    // a catalog backup: a dated folder whose database is not named after it
    let backup = scratch.0.join("2026-11-14 11.50.04");
    std::fs::create_dir_all(&backup).unwrap();
    std::fs::copy(package.join("Test.cocatalogdb"), backup.join("Capture One.cocatalogdb")).unwrap();
    assert_eq!(read(&backup).unwrap().database, backup.join("Capture One.cocatalogdb").to_string_lossy());

    // what an upgrade leaves behind is read when chosen, but never picked from a folder
    std::fs::copy(package.join("Test.cocatalogdb"), package.join("Test.cocatalogdb.backup")).unwrap();
    assert!(read(&package).unwrap().database.ends_with("Test.cocatalogdb"));
    assert_eq!(read(&package.join("Test.cocatalogdb.backup")).unwrap().photos.len(), 4);

    std::fs::copy(package.join("Test.cocatalogdb"), backup.join("Other.cocatalogdb")).unwrap();
    let Err(ReadError::Ambiguous { databases, .. }) = read(&backup) else { panic!("two databases are ambiguous") };
    assert_eq!(databases, ["Capture One.cocatalogdb", "Other.cocatalogdb"]);

    // a folder with more entries than are looked at: the database it holds can't be told apart
    // from one further on
    let crowded = scratch.0.join("Crowded");
    std::fs::create_dir_all(&crowded).unwrap();
    std::fs::copy(package.join("Test.cocatalogdb"), crowded.join("Capture One.cocatalogdb")).unwrap();
    for n in 0..10_000 {
        std::fs::write(crowded.join(format!("{n}.txt")), b"").unwrap();
    }
    assert!(matches!(read(&crowded), Err(ReadError::FolderTooLarge(_))));
    // the database named after its folder is still found at once
    std::fs::rename(crowded.join("Capture One.cocatalogdb"), crowded.join("Crowded.cocatalogdb")).unwrap();
    assert_eq!(read(&crowded).unwrap().photos.len(), 4);

    // an empty package is this reader's to refuse; a folder without a database is not a catalog
    let empty = scratch.0.join("Empty.cocatalog");
    std::fs::create_dir_all(&empty).unwrap();
    assert!(is_catalog(&empty));
    assert!(matches!(read(&empty), Err(ReadError::NotFound(_))));
    assert!(is_catalog(&backup) && !is_catalog(&scratch.0));
    assert!(matches!(read(&scratch.0.join("missing")), Err(ReadError::NotFound(_))));
    std::fs::write(scratch.0.join("notes.txt"), "x").unwrap();
    assert!(matches!(read(&scratch.0.join("notes.txt")), Err(ReadError::NotFound(_))));
}

#[test]
fn a_catalog_capture_one_is_writing_is_refused_and_an_open_one_is_reported() {
    let scratch = Scratch::new("busy");
    let package = scratch.catalog(&sample(&scratch.0));
    let journal = package.join("Test.cocatalogdb-journal");

    // a journal whose header was zeroed holds no transaction
    std::fs::write(&journal, [0u8; 512]).unwrap();
    assert!(read(&package).is_ok());
    let mut hot = vec![0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7];
    hot.resize(512, 0);
    std::fs::write(&journal, hot).unwrap();
    assert_eq!(read(&package), Err(ReadError::Busy));
    std::fs::remove_file(&journal).unwrap();

    std::fs::write(package.join("Test.cocatalogdb-wal"), [1u8; 64]).unwrap();
    assert_eq!(read(&package), Err(ReadError::Busy));
    std::fs::remove_file(package.join("Test.cocatalogdb-wal")).unwrap();

    // a journal shorter than its header holds none either; one that can't be read may
    std::fs::write(&journal, [0xd9, 0xd5, 0x05]).unwrap();
    assert!(read(&package).is_ok());
    std::fs::remove_file(&journal).unwrap();
    std::fs::create_dir(&journal).unwrap();
    assert!(matches!(read(&package), Err(ReadError::WriteStateUnknown(_))), "{:?}", read(&package).err());
    std::fs::remove_dir(&journal).unwrap();
    // a log whose state can't be read either (a link to itself)
    #[cfg(unix)]
    {
        let wal = package.join("Test.cocatalogdb-wal");
        std::os::unix::fs::symlink(&wal, &wal).unwrap();
        assert!(matches!(read(&package), Err(ReadError::WriteStateUnknown(_))), "{:?}", read(&package).err());
        std::fs::remove_file(&wal).unwrap();
    }

    std::fs::write(package.join("writelock"), "someone\n0123").unwrap();
    let catalog = read(&package).unwrap();
    assert!(catalog.in_use);
    assert_eq!(catalog.warnings, ["Capture One has this catalog open; changes it has not saved yet are not included"]);
}

#[test]
fn a_write_capture_one_starts_while_the_file_is_taken_is_refused() {
    let scratch = Scratch::new("race");
    let package = scratch.catalog(&sample(&scratch.0));
    let database = package.join("Test.cocatalogdb");
    let mut hot = vec![0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7];
    hot.resize(512, 0);
    let begin = || std::fs::write(package.join("Test.cocatalogdb-journal"), &hot).unwrap();
    assert!(matches!(tables::Db::open_observed(&database, &begin), Err(ReadError::Busy)));
}

#[test]
fn a_cancel_after_the_tables_are_read_stops_the_photos_being_built() {
    let scratch = Scratch::new("cancel-late");
    let package = scratch.catalog(&sample(&scratch.0));
    let database = package.join("Test.cocatalogdb");
    let db = tables::Db::open(&database).unwrap();
    // the four table reads go ahead; the cancel comes once they are done
    let looks = std::cell::Cell::new(0);
    let active = || {
        looks.set(looks.get() + 1);
        if looks.get() > 4 { Err(ReadError::Cancelled) } else { Ok(()) }
    };
    assert!(matches!(photos::read(&db, &database, &mut Vec::new(), &active), Err(ReadError::Cancelled)));
}

#[test]
fn a_set_cancel_flag_stops_the_read() {
    let scratch = Scratch::new("cancel");
    let package = scratch.catalog(&sample(&scratch.0));
    assert_eq!(read_with_cancel(&package, &AtomicBool::new(true)), Err(ReadError::Cancelled));
}

// ---- catalogs the reader can't or shouldn't trust

#[test]
fn a_catalog_without_the_readers_columns_names_them_and_who_wrote_it() {
    let scratch = Scratch::new("schema");
    let mut tables = without(sample(&scratch.0), "ZVARIANTMETADATA");
    table(&mut tables, "ZVARIANT").columns.retain(|c| *c != "ZCOMBINEDSETTINGS");
    let error = read(&scratch.catalog(&tables)).unwrap_err();
    assert_eq!(
        error,
        ReadError::Unsupported {
            written_by: Some(VersionInfo { app: "16.3.9.9 Pro Mac".into(), schema: 1621, compatible: 1621 }),
            missing: vec!["ZVARIANT.ZCOMBINEDSETTINGS".into(), "ZVARIANTMETADATA".into()],
        }
    );
    assert_eq!(
        error.to_string(),
        "unsupported Capture One catalog (last written by 16.3.9.9 Pro Mac, schema 1621): missing ZVARIANT.ZCOMBINEDSETTINGS, ZVARIANTMETADATA"
    );
    // a catalog that doesn't say who wrote it
    let error = read(&scratch.catalog(&without(tables, "ZVERSIONINFO"))).unwrap_err();
    assert!(error.to_string().starts_with("unsupported Capture One catalog: missing "), "{error}");
}

#[test]
fn a_catalog_without_its_optional_tables_still_gives_its_photos() {
    let scratch = Scratch::new("optional");
    let mut tables = sample(&scratch.0);
    for name in ["ZENTITIES", "ZVERSIONINFO", "ZDOCUMENTCONTENT", "ZCOLLECTION", "ZIMAGEINCOLLECTION", "ZVARIANTINCOLLECTION"] {
        tables = without(tables, name);
    }
    let catalog = read(&scratch.catalog(&tables)).unwrap();
    assert_eq!((catalog.photos.len(), catalog.collections.len(), catalog.versions.len(), catalog.document_uuid), (4, 0, 0, None));
}

#[test]
fn files_that_are_not_a_database_are_errors() {
    let scratch = Scratch::new("invalid");
    let package = scratch.catalog(&sample(&scratch.0));
    let database = package.join("Test.cocatalogdb");
    let bytes = std::fs::read(&database).unwrap();
    for broken in [b"not a database".to_vec(), Vec::new(), bytes[..PAGE + 17].to_vec(), bytes[..100].to_vec()] {
        std::fs::write(&database, broken).unwrap();
        assert!(matches!(read(&package), Err(ReadError::Invalid(_))), "{:?}", read(&package));
    }
    // a table whose root page is outside the file
    let mut cut = bytes.clone();
    cut.truncate(3 * PAGE);
    cut[28..32].copy_from_slice(&3u32.to_be_bytes());
    std::fs::write(&database, cut).unwrap();
    assert!(matches!(read(&package), Err(ReadError::Invalid(_))));
}

/// Catalogs with values no catalog should hold. None may panic; each still gives a result.
#[test]
fn hostile_values_are_read_without_panicking() {
    let scratch = Scratch::new("hostile");
    let big = i64::MAX;
    let odd = |pk: i64| {
        vec![V::I(pk), V::N, V::I(big), V::I(i64::MIN), V::R(1.0e300), V::R(0.5), V::R(-1.0e300), t(""), t("\u{0}../../x"), V::I(0), V::I(-1)]
    };
    // every column of every table takes each odd value in turn
    for (round, _) in odd(0).iter().enumerate() {
        let mut tables = sample(&scratch.0);
        for table in &mut tables {
            if table.name == "ZENTITIES" {
                continue;
            }
            let columns = table.columns.clone();
            for (at, row) in table.rows.iter_mut().enumerate() {
                let key = at as i64 + 1;
                *row = columns
                    .iter()
                    .enumerate()
                    .map(|(c, name)| (*name, if *name == "Z_PK" { V::I(key) } else { odd(key)[(round + c) % 11].clone() }))
                    .collect();
            }
        }
        let package = scratch.catalog(&tables);
        let catalog = read(&package).unwrap_or_else(|e| panic!("round {round}: {e}"));
        assert!(catalog.photos.iter().all(|p| p.rating <= 5 && p.keywords.len() <= 1), "round {round}");
    }
    // the page reader refuses a number that is not one: an error, not a panic
    for not_a_number in [f64::NAN, f64::INFINITY] {
        let mut tables = sample(&scratch.0);
        table(&mut tables, "ZIMAGE").rows[0].push(("ZEXP_APERTURE", V::R(not_a_number)));
        table(&mut tables, "ZIMAGE").rows[0].retain(|(name, v)| *name != "ZEXP_APERTURE" || matches!(v, V::R(f) if !f.is_finite()));
        assert!(matches!(read(&scratch.catalog(&tables)), Err(ReadError::Invalid(_))));
    }
}

#[test]
fn collections_nested_in_a_loop_or_under_a_missing_parent_are_handled() {
    let scratch = Scratch::new("loops");
    let rows = vec![
        root(),
        collection(10, ALBUM, "Itself", 10, 1),
        collection(11, GROUP, "A", 12, 2),
        collection(12, GROUP, "B", 11, 3),
        collection(13, ALBUM, "Orphan", 999, 4),
        collection(14, ALBUM, "Below the orphan's sibling", 15, 5),
        collection(15, GROUP, "Lost group", 998, 6),
        collection(16, ALBUM, "Top", 0, 7),
        collection(17, ALBUM, "No parent", i64::MIN, 8),
    ];
    let catalog = read(&scratch.catalog(&with(sample(&scratch.0), spec("ZCOLLECTION", COLLECTION_COLUMNS, rows)))).unwrap();
    let listed: Vec<_> = catalog.collections.iter().map(|c| (c.name.as_str(), c.parent)).collect();
    assert_eq!(listed, [("Orphan", None), ("Below the orphan's sibling", Some(15)), ("Lost group", None), ("Top", None), ("No parent", None)]);
    assert_eq!(
        catalog.warnings,
        [
            "2 collection(s) are inside a collection the catalog no longer has; they come in at the top level",
            "3 collection(s) are nested in a loop and were left out",
        ]
    );
}

#[test]
fn paths_a_catalog_should_not_hold_are_refused_or_kept_as_written() {
    let scratch = Scratch::new("paths");
    let (here, _) = if cfg!(windows) { ("ZWINROOT", "ZMACROOT") } else { ("ZMACROOT", "ZWINROOT") };
    let root = scratch.0.to_string_lossy().into_owned();
    let locations = vec![
        // a catalog made on Windows: `\` and `..`, relative to the catalog
        vec![("Z_PK", V::I(1)), ("ZRELATIVEPATH", t("..\\..\\Shoots\\.\\2024")), ("ZISRELATIVE", V::I(1))],
        vec![("Z_PK", V::I(2)), ("ZRELATIVEPATH", t("/etc")), ("ZISRELATIVE", V::I(1))],
        vec![("Z_PK", V::I(3)), ("ZISRELATIVE", V::I(0))],
        vec![("Z_PK", V::I(4)), (here, V::T(format!("{root}/"))), ("ZRELATIVEPATH", t("/Photos//2024/")), ("ZISRELATIVE", V::I(0))],
        // the catalog's own folder
        vec![("Z_PK", V::I(5)), ("ZISRELATIVE", V::I(1))],
    ];
    let images = vec![
        image(1, 1, "up.jpg"),
        image(2, 2, "absolute-folder.jpg"),
        image(3, 3, "no-folder.jpg"),
        image(4, 4, "tidy.jpg"),
        image(5, 5, "beside.jpg"),
        image(6, 1, "/etc/passwd"),
        image(7, 1, ""),
        image(8, 99, "no-location.jpg"),
        image(9, 1, "C:\\Windows\\x.jpg"),
    ];
    let tables = with(with(sample(&scratch.0), spec("ZPATHLOCATION", LOCATION_COLUMNS, locations)), spec("ZIMAGE", IMAGE_COLUMNS, images));
    let package = scratch.catalog(&with(tables, spec("ZVARIANT", VARIANT_COLUMNS, Vec::new())));
    let catalog = read(&package).unwrap();
    let by_name = |name: &str| catalog.photos.iter().find(|p| p.file_name == name).unwrap();
    let refused = |name: &str| matches!(by_name(name).original, Original::Refused(_));

    let up = by_name("up.jpg");
    assert_eq!((up.original.clone(), up.managed), (Original::Local, false));
    assert!(Path::new(&up.path).ends_with(Path::new("Shoots").join("2024").join("up.jpg")), "{}", up.path);
    assert!(!up.path.contains('\\') || cfg!(windows), "{}", up.path);
    let beside = by_name("beside.jpg");
    assert_eq!((Path::new(&beside.path), beside.managed), (package.join("beside.jpg").as_path(), true));
    assert_eq!(Path::new(&by_name("tidy.jpg").path), scratch.0.join("Photos").join("2024").join("tidy.jpg"));
    for name in ["absolute-folder.jpg", "no-folder.jpg", "/etc/passwd", "", "no-location.jpg", "C:\\Windows\\x.jpg"] {
        assert!(refused(name), "{name:?}: {:?}", by_name(name).original);
    }
}

// ---- the parts

#[test]
fn keyword_fields_become_paths_without_implied_parents() {
    let parse = |field: &str| keywords::parse(field).0;
    assert_eq!(parse("Nature||0"), ["Nature"]);
    assert_eq!(parse("Culture||0,Culture|Buildings||1"), ["Culture|Buildings"]);
    // the number is the keyword's place, whatever the field's order
    assert_eq!(parse("Nature||1,Nature|Animals||0,Sea||2,Air||3"), ["Nature|Animals", "Sea", "Air"]);
    assert_eq!(parse("B||1,A||0,B||5"), ["A", "B"]);
    assert_eq!(parse("a|b|c||0,a||1,a|b||2,a|x||3"), ["a|b|c", "a|x"]);
    // entries without a number, with blanks and with empty levels
    assert_eq!(parse(" plain , ||3,|,a||x,,deep| |er||1"), ["deep|er", "plain", "a|x"]);
    assert_eq!(parse(""), Vec::<String>::new());
    assert_eq!(parse("9||18446744073709551616"), ["9"]);
}

#[test]
fn a_huge_keyword_field_is_cut_in_linear_time() {
    let field: String = (0..2_000_000).map(|n| format!("k{n}||{n},")).collect();
    let started = std::time::Instant::now();
    let (parsed, cut) = keywords::parse(&field);
    assert_eq!((parsed.len(), cut), (keywords::MAX_KEYWORDS, true));
    assert!(started.elapsed() < std::time::Duration::from_secs(20), "{:?}", started.elapsed());
}

#[test]
fn a_keyword_nested_too_deep_is_left_out_without_quadratic_work() {
    let path = |levels: usize| (0..levels).map(|n| format!("k{n}")).collect::<Vec<_>>().join("|");
    let deepest = path(keywords::MAX_DEPTH);
    assert_eq!(keywords::parse(&format!("{deepest}||0,a||1")), (vec![deepest, "a".to_string()], false));
    let started = std::time::Instant::now();
    let field = format!("{}||0,a||1", path(200_000));
    assert_eq!(keywords::parse(&field), (vec!["a".to_string()], true));
    assert!(started.elapsed() < std::time::Duration::from_secs(5), "{:?}", started.elapsed());
}

#[test]
fn times_are_calendar_times_within_four_digit_years() {
    let scratch = Scratch::new("times");
    let captured = |seconds: f64| {
        let mut tables = sample(&scratch.0);
        let image = &mut table(&mut tables, "ZIMAGE").rows[0];
        image.retain(|(name, _)| *name != "ZEXP_DATE");
        image.push(("ZEXP_DATE", V::R(seconds)));
        read(&scratch.catalog(&tables)).unwrap().photos[0].captured.clone()
    };
    assert_eq!(captured(1_500_000_000.25).as_deref(), Some("2017-07-14T02:40:00"));
    // before 1970, and the last second an ISO date holds
    assert_eq!(captured(-86_400.5).as_deref(), Some("1969-12-30T23:59:59"));
    assert_eq!(captured(-0.5).as_deref(), Some("1969-12-31T23:59:59"));
    assert_eq!(captured(253_402_300_799.0).as_deref(), Some("9999-12-31T23:59:59"));
    assert_eq!(captured(253_402_300_800.0), None);
    assert_eq!(captured(-62_135_596_799.0).as_deref(), Some("0001-01-01T00:00:01"));
    assert_eq!((captured(-62_135_596_800.0), captured(-1.0e18), captured(0.0)), (None, None, None));
}

/// As a catalog written by Capture One 16.3 has them: a file without a camera, a lens Capture One
/// describes from what it knows, and GPS as degrees and decimal minutes.
#[test]
fn stand_ins_for_a_missing_camera_or_lens_are_left_out_and_gps_text_is_read() {
    let scratch = Scratch::new("stand-ins");
    let mut tables = sample(&scratch.0);
    let rows = &mut table(&mut tables, "ZIMAGE").rows;
    rows[0].retain(|(name, _)| !["ZCAMERA_MAKE", "ZCAMERA_MODEL", "ZCAMERA_LENS", "ZGPSLATITUDE", "ZGPSLONGITUDE"].contains(name));
    rows[0].extend([
        ("ZCAMERA_MAKE", t("Joint Photographic Experts Group")),
        ("ZCAMERA_MODEL", t("Jpeg File")),
        ("ZCAMERA_LENS", t("-- mm f/--")),
        ("ZGPSLATITUDE", t("47,30.6N")),
        ("ZGPSLONGITUDE", t("8,15.45W")),
    ]);
    rows[1].extend([("ZCAMERA_MAKE", t("Maker")), ("ZCAMERA_MODEL", t("Maker M1")), ("ZCAMERA_LENS", t("70 mm f/--"))]);
    let catalog = read(&scratch.catalog(&tables)).unwrap();
    let first = &catalog.photos[0];
    assert_eq!((first.camera_make.as_str(), first.camera_model.as_str(), first.lens.as_str()), ("", "", ""));
    let (lat, lon) = first.gps.unwrap();
    assert!((lat - 47.51).abs() < 1e-9 && (lon + 8.2575).abs() < 1e-9, "{lat}, {lon}");
    let named = catalog.photos.iter().find(|p| p.image_id == 2).unwrap();
    assert_eq!((named.camera_make.as_str(), named.camera_model.as_str(), named.lens.as_str()), ("Maker", "Maker M1", ""));
}

#[test]
fn coordinates_in_degrees_minutes_and_seconds() {
    assert_eq!(photos::coordinate("12,30N", 'N', 'S'), Some(12.5));
    assert_eq!(photos::coordinate(" 12,30,36S ", 'N', 'S'), Some(-12.51));
    assert_eq!(photos::coordinate("7E", 'E', 'W'), Some(7.0));
    for bad in ["", "N", "12,30", "12,61N", "12,30,60N", "1,2,3,4N", "x,30N", "-12,30N", "12,30E", "inf,0N", "12,,N", "ñ"] {
        assert_eq!(photos::coordinate(bad, 'N', 'S'), None, "{bad:?}");
    }
}

#[test]
fn colour_tags_follow_capture_ones_order() {
    let names: Vec<_> = (0..8).map(|i| ColorTag::from_index(i).and_then(ColorTag::name)).collect();
    assert_eq!(names, [None, Some("Red"), Some("Orange"), Some("Yellow"), Some("Green"), Some("Blue"), Some("Pink"), Some("Purple")]);
    assert_eq!((ColorTag::from_index(8), ColorTag::from_index(-1)), (None, None));
}

#[test]
fn an_unknown_colour_tag_and_capture_ones_no_date_read_as_none() {
    let scratch = Scratch::new("values");
    let mut tables = sample(&scratch.0);
    let image = &mut table(&mut tables, "ZIMAGE").rows[0];
    image.retain(|(name, _)| !["ZEXP_DATE", "ZIMPORTDATE", "ZEXP_EXPOSURECOMPENSATION", "ZGPSLATITUDE"].contains(name));
    image.extend([
        ("ZEXP_DATE", V::R(-62_135_596_800.0)),
        ("ZIMPORTDATE", V::R(1.0e300)),
        ("ZEXP_EXPOSURECOMPENSATION", t("1/0")),
        ("ZGPSLATITUDE", t("91")),
    ]);
    let shown = &mut table(&mut tables, "ZVARIANTMETADATA").rows[1];
    shown.retain(|(name, _)| !["ZCOLOR_TAG_INDEX", "ZBASIC_RATING"].contains(name));
    shown.extend([("ZCOLOR_TAG_INDEX", V::I(8)), ("ZBASIC_RATING", V::I(99))]);
    let catalog = read(&scratch.catalog(&tables)).unwrap();
    let first = &catalog.photos[0];
    assert_eq!((first.captured.clone(), first.imported.clone(), first.exposure_bias, first.gps), (None, None, None, None));
    assert_eq!((first.color_tag, first.rating), (ColorTag::None, 5));
    assert_eq!(catalog.warnings, ["1 photo(s) have a colour tag this version doesn't know; imported without one"]);
}

// ---- into the library

/// A small JPEG, so an original that is there is a photo the import can read.
fn write_jpeg(path: &Path) {
    use lightcraft_codecs::{ChromaSubsampling, EncodeImage, EncodeMeta, Samples, encode_jpeg};
    let rgb: Vec<u8> = (0..24 * 16).flat_map(|i| [(i % 251) as u8, 90, 160]).collect();
    let bytes = encode_jpeg(&EncodeImage::new(24, 16, 3, Samples::U8(&rgb)), 90, ChromaSubsampling::S444, &EncodeMeta::default()).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// The sample catalog with its referenced original on disk; the managed raw and the photo on
/// another system's drive are not there.
fn sample_on_disk(scratch: &Scratch) -> PathBuf {
    write_jpeg(&scratch.0.join("Photos").join("2024").join("Trip").join("referenced.jpg"));
    scratch.catalog(&sample(&scratch.0.join("Photos")))
}

fn by_name<'a>(s: &'a crate::Session, name: &str, copy: bool) -> &'a lightcraft_catalog::Photo {
    s.catalog.photos().find(|p| p.file_name == name && p.copy_of.is_some() == copy).unwrap_or_else(|| panic!("no photo {name} (copy: {copy})"))
}

fn album<'a>(s: &'a crate::Session, name: &str) -> &'a lightcraft_catalog::Album {
    s.catalog.albums().find(|a| a.name == name).unwrap_or_else(|| panic!("no album {name}"))
}

#[test]
fn a_catalog_becomes_photos_virtual_copies_and_albums_in_one_undo_step() {
    use lightcraft_catalog::{ColorLabel, MediaKind};
    let scratch = Scratch::new("import");
    let package = sample_on_disk(&scratch);
    let mut s = crate::Session::new().with_fs();
    let report = s.execute("library.importLightroom", &serde_json::json!({ "path": package })).unwrap();

    assert_eq!((report["photos"].as_u64(), report["imported"].as_u64(), report["collections"].as_u64()), (Some(4), Some(4), Some(5)));
    // the managed raw isn't in the test's catalog folder, and the other system's drive isn't here
    assert_eq!(report["missing"].as_array().unwrap().len(), 2, "{report}");
    assert_eq!(report["failed"].as_array().map(Vec::len), Some(0), "{report}");
    let warnings: Vec<&str> = report["warnings"].as_array().unwrap().iter().filter_map(|w| w.as_str()).collect();
    assert_eq!(
        warnings,
        [
            "1 image(s) in Capture One's trash were left out",
            "2 photo(s) have Capture One's orange or pink colour tag, which has no label here yet: the colour is kept with the photo, and the keywords “Capture One|Orange” and “Capture One|Pink” find them",
            "4 photo(s) have Capture One adjustments; adjustments are not imported yet",
            "smart album “Five stars”: not imported; smart album rules can't be converted yet",
        ]
    );

    // an original that isn't there is kept with what the catalog knows about it
    let master = by_name(&s, "managed.RAF", false);
    assert_eq!((master.rating, master.label, master.kind), (4, Some(ColorLabel::Yellow), MediaKind::Raw));
    assert_eq!(master.meta.keywords, ["Culture|Buildings"]);
    assert_eq!(master.captured.as_deref(), Some("2021-06-15T10:15:30"));
    let m = &master.meta;
    assert_eq!(
        (m.title.as_str(), m.caption.as_str(), m.creator.as_str(), m.copyright.as_str(), m.city.as_str()),
        ("A title", "A description", "A creator", "A notice", "A city")
    );
    assert!(master.edited.is_none(), "no adjustments are imported");
    assert_eq!(master.meta.label_text, "", "a colour the library has is its label, nothing is kept beside it");
    // its second variant is a virtual copy with a rating and keywords of its own
    let copy = by_name(&s, "managed.RAF", true);
    assert_eq!((copy.copy_of, copy.copy_name.as_deref()), (Some(master.id), Some("Variant 2")));
    assert_eq!((copy.rating, copy.label, copy.meta.keywords.clone()), (1, None, vec!["Nature|Animals".to_string()]));
    // an original that is there is read like any imported file
    let referenced = by_name(&s, "referenced.jpg", false);
    assert_eq!((referenced.width, referenced.height, referenced.rating, referenced.label), (24, 16, 0, None));
    let elsewhere = by_name(&s, "elsewhere.ARW", false);
    assert_eq!((elsewhere.rating, elsewhere.label, elsewhere.kind), (5, None, MediaKind::Raw));
    assert_eq!(s.catalog.len(), 4);

    let group = album(&s, "A group");
    assert!(group.folder && group.parent.is_none() && album(&s, "A project").folder);
    let first = album(&s, "First album");
    assert_eq!((first.parent, first.photos.clone()), (Some(group.id), vec![master.id, referenced.id, copy.id]));
    assert_eq!(album(&s, "Second album").photos, [elsewhere.id]);
    assert_eq!(album(&s, "In the project").parent, Some(album(&s, "A project").id));
    assert!(s.catalog.albums().all(|a| a.name != "Five stars" && a.name != "An import"));

    assert_eq!(s.undo.last().map(|step| step.label.as_str()), Some("Import Capture One Catalog"));
    s.execute("edit.undo", &serde_json::json!({})).unwrap();
    assert!(s.catalog.is_empty() && s.catalog.albums().count() == 0);
}

/// Capture One has seven colour tags, the library five labels. Orange and pink are not lost: the
/// colour's name stays with the photo, goes into its XMP, and a keyword finds the photo.
#[test]
fn orange_and_pink_are_kept_found_by_a_keyword_and_replaced_by_a_label_set_here() {
    use lightcraft_catalog::ColorLabel;
    let scratch = Scratch::new("kept-label");
    let package = sample_on_disk(&scratch);
    let mut s = crate::Session::new().with_fs();
    let params = serde_json::json!({ "path": package });
    s.execute("library.importLightroom", &params).unwrap();
    let kept = |s: &crate::Session, name: &str| {
        let p = by_name(s, name, false);
        (p.label, p.meta.label_text.clone(), p.meta.keywords.clone())
    };
    assert_eq!(kept(&s, "referenced.jpg"), (None, "Orange".to_string(), vec!["Capture One|Orange".to_string()]));
    assert_eq!(kept(&s, "elsewhere.ARW"), (None, "Pink".to_string(), vec!["Capture One|Pink".to_string()]));
    let id = by_name(&s, "referenced.jpg", false).id;
    let packet = |s: &crate::Session| crate::sidecar::sidecar_packet(s.catalog.photo(id).unwrap(), &s.catalog);
    assert!(packet(&s).contains("<xmp:Label>Orange</xmp:Label>"), "{}", packet(&s));

    // (the photo commands want a selection, also when they are given ids)
    s.execute("library.select", &serde_json::json!({ "ids": [id.0] })).unwrap();
    // other edits of the photo's metadata leave the kept colour alone
    s.execute("photo.setMeta", &serde_json::json!({ "ids": [id.0], "title": "A new title" })).unwrap();
    assert_eq!(kept(&s, "referenced.jpg").1, "Orange");

    // a label set here replaces it, in one undo step
    s.execute("photo.label", &serde_json::json!({ "ids": [id.0], "label": "red" })).unwrap();
    assert_eq!((kept(&s, "referenced.jpg").0, kept(&s, "referenced.jpg").1.as_str()), (Some(ColorLabel::Red), ""));
    // only the photo labelled: another photo's kept colour isn't in the step
    assert_eq!(kept(&s, "elsewhere.ARW").1, "Pink");
    assert!(
        matches!(&s.undo.last().unwrap().op, lightcraft_catalog::Op::Batch { ops } if ops.len() == 1),
        "{:?}",
        s.undo.last().map(|step| &step.op)
    );
    // (the keyword still names the colour; the label is the one set here)
    assert!(packet(&s).contains("<xmp:Label>Red</xmp:Label>") && !packet(&s).contains("<xmp:Label>Orange"), "{}", packet(&s));
    s.execute("edit.undo", &serde_json::json!({})).unwrap();
    assert_eq!((kept(&s, "referenced.jpg").0, kept(&s, "referenced.jpg").1.as_str()), (None, "Orange"));
    // so does taking the label away, which the photo doesn't show it has
    s.execute("photo.clearLabel", &serde_json::json!({ "ids": [id.0] })).unwrap();
    assert_eq!((kept(&s, "referenced.jpg").0, kept(&s, "referenced.jpg").1.as_str()), (None, ""));
    // a photo without a kept colour takes a label as before: one operation, no metadata touched
    let other = by_name(&s, "managed.RAF", false).id;
    s.execute("photo.label", &serde_json::json!({ "ids": [other.0], "label": "none" })).unwrap();
    assert!(
        matches!(&s.undo.last().unwrap().op, lightcraft_catalog::Op::Batch { ops } if matches!(ops.as_slice(), [lightcraft_catalog::Op::SetLabel { .. }]))
    );

    // the catalog says orange again when it is imported again
    s.execute("library.importLightroom", &params).unwrap();
    assert_eq!(kept(&s, "referenced.jpg"), (None, "Orange".to_string(), vec!["Capture One|Orange".to_string()]));
}

#[test]
fn the_keyword_list_is_read_with_parents_first_and_without_loops_or_synonyms() {
    let scratch = Scratch::new("keyword-list");
    let catalog = read(&scratch.catalog(&sample(&scratch.0))).unwrap();
    let list: Vec<_> = catalog.keywords.iter().map(|k| (k.path.as_str(), k.exportable)).collect();
    assert_eq!(
        list,
        [("Culture", true), ("Culture|Buildings", true), ("Nature", true), ("Nature|Animals", true), ("Nature|Plants", true), ("Private", false)]
    );

    let rows = vec![
        vec![("Z_ENT", V::I(KEYWORD)), ("Z_PK", V::I(1)), ("ZNAME", t("A")), ("ZPARENT", V::I(2))],
        vec![("Z_ENT", V::I(KEYWORD)), ("Z_PK", V::I(2)), ("ZNAME", t("B")), ("ZPARENT", V::I(1))],
        vec![("Z_ENT", V::I(KEYWORD)), ("Z_PK", V::I(3)), ("ZNAME", t("  ")), ("ZPARENT", V::I(-1))],
        vec![("Z_ENT", V::I(KEYWORD)), ("Z_PK", V::I(4)), ("ZNAME", t("a|b")), ("ZPARENT", V::I(99))],
    ];
    let catalog = read(&scratch.catalog(&with(sample(&scratch.0), spec("ZKEYWORD", KEYWORD_COLUMNS, rows)))).unwrap();
    let list: Vec<_> = catalog.keywords.iter().map(|k| k.path.as_str()).collect();
    assert_eq!(list, ["a b"], "a `|` in a name is not a level; an empty name is no keyword");
    assert!(
        catalog.warnings.iter().any(|w| w == "2 keyword(s) are nested in a loop and were left out of the keyword list"),
        "{:?}",
        catalog.warnings
    );
}

#[test]
fn the_keyword_list_joins_the_library_and_offline_originals_keep_their_camera() {
    let scratch = Scratch::new("camera-keywords");
    let package = sample_on_disk(&scratch);
    let mut s = crate::Session::new().with_fs();
    // a keyword the library has keeps what the library says about it
    s.commit(
        "list",
        lightcraft_catalog::Op::SetKeyword {
            path: "Private".into(),
            info: Some(lightcraft_catalog::keywords::KeywordInfo { include_on_export: true, person: true, ..Default::default() }),
        },
    )
    .unwrap();
    s.execute("library.importLightroom", &serde_json::json!({ "path": package })).unwrap();
    let info = |path: &str| s.catalog.keyword_info(path).cloned();
    assert!(info("Nature|Plants").is_some_and(|i| i.include_on_export), "an unused keyword comes in");
    assert!(info("Private").is_some_and(|i| i.include_on_export && i.person), "the library's own settings stay");
    assert!(info("Fauna").is_none() && info("Nature|Animals|Fauna").is_none(), "a synonym is not a keyword");

    // the managed raw is not in the test's catalog folder: the catalog's camera and exposure stay
    let raw = by_name(&s, "managed.RAF", false);
    let m = &raw.meta;
    assert_eq!((m.camera.as_str(), m.make.as_str(), m.lens.as_str()), ("ACME Model 7", "ACME", "Zoom 24-70mm"));
    assert_eq!((m.iso, m.aperture, m.focal_mm, m.exposure_bias, m.gps), (Some(640), Some(5.6), Some(35.0), Some(-0.67), Some((52.5, 13.4))));
    assert_eq!(m.shutter, lightcraft_meta::format_exposure_time(0.004).unwrap());
    // its virtual copy has them too
    assert_eq!(by_name(&s, "managed.RAF", true).meta.camera, "ACME Model 7");
}

#[test]
fn a_file_s_own_camera_data_is_kept_and_only_gaps_are_filled() {
    let mut meta = lightcraft_catalog::Meta { camera: "From the file".into(), iso: Some(100), ..Default::default() };
    let camera = serde_json::json!({ "make": "Make", "model": "Model", "lens": "Lens", "iso": 3200, "aperture": f64::NAN, "exposureTime": -1, "focalLength": 1.0e9, "gps": [91, 0] });
    crate::lightroom_catalog::fill_camera(&mut meta, &camera);
    assert_eq!((meta.camera.as_str(), meta.make.as_str(), meta.lens.as_str(), meta.iso), ("From the file", "Make", "Lens", Some(100)));
    assert_eq!((meta.aperture, meta.shutter.as_str(), meta.focal_mm, meta.gps), (None, "", None, None), "implausible values are left out");
    // the exact time comes with its text, as from a file
    let mut offline = lightcraft_catalog::Meta::default();
    crate::lightroom_catalog::fill_camera(&mut offline, &serde_json::json!({ "exposureTime": 1.0 / 3.0 }));
    assert_eq!((offline.shutter.as_str(), offline.exposure_time, offline.shutter_seconds()), ("1/3", Some(1.0 / 3.0), Some(1.0 / 3.0)));
    // a catalog row without the block changes nothing (Lightroom's)
    let before = meta.clone();
    crate::lightroom_catalog::fill_camera(&mut meta, &serde_json::json!(null));
    assert_eq!(meta, before);
}

/// A library written before the field existed reads as before, and one without a kept colour
/// is written as before.
#[test]
fn the_kept_label_is_left_out_of_the_catalog_when_empty() {
    let mut meta = lightcraft_catalog::Meta::default();
    assert!(serde_json::to_value(&meta).unwrap().get("label_text").is_none());
    let old: lightcraft_catalog::Meta = serde_json::from_value(serde_json::json!({ "title": "x" })).unwrap();
    assert_eq!((old.title.as_str(), old.label_text.as_str()), ("x", ""));
    meta.label_text = "Pink".into();
    let json = serde_json::to_value(&meta).unwrap();
    assert_eq!(json["label_text"], "Pink");
    assert_eq!(serde_json::from_value::<lightcraft_catalog::Meta>(json).unwrap().label_text, "Pink");
}

#[test]
fn importing_a_catalog_again_adds_nothing_twice() {
    let scratch = Scratch::new("reimport");
    let package = sample_on_disk(&scratch);
    let library = scratch.0.join("Library");
    let mut s = crate::Session::new().with_fs();
    s.open_library(&library, false).unwrap();
    let params = serde_json::json!({ "path": package });
    let first = s.execute("library.importLightroom", &params).unwrap();
    let rated = by_name(&s, "elsewhere.ARW", false).id;
    s.commit("Rate", lightcraft_catalog::Op::SetRating { id: rated, rating: 2 }).unwrap();
    s.close_library().unwrap();
    s.open_library(&library, false).unwrap();

    // the database itself, and its folder, are the same catalog
    let second = s.execute("library.importLightroom", &serde_json::json!({ "path": package.join("Test.cocatalogdb") })).unwrap();
    assert_eq!(first["mapping"], second["mapping"]);
    assert_eq!((first["imported"].as_u64(), second["imported"].as_u64()), (Some(4), Some(0)));
    assert_eq!((s.catalog.len(), s.catalog.albums().count()), (4, 5));
    assert_eq!(album(&s, "First album").photos.len(), 3);
    // the catalog's rating comes in again
    assert_eq!(s.catalog.photo(rated).unwrap().rating, 5);
    s.close_library().unwrap();
}

#[test]
fn inspecting_a_catalog_adds_nothing() {
    let scratch = Scratch::new("inspect");
    let package = sample_on_disk(&scratch);
    let mut s = crate::Session::new().with_fs();
    let report = s.execute("library.inspectLightroom", &serde_json::json!({ "path": package })).unwrap();
    assert_eq!((report["photos"].as_u64(), report["collections"].as_u64()), (Some(4), Some(5)));
    assert_eq!(report["missing"].as_array().unwrap().len(), 3, "the raw, its second variant and the other system's photo: {report}");
    assert!(s.catalog.is_empty());
    // a catalog the reader refuses says why, in Capture One's words, also when it is not there
    let error = s.execute("library.inspectLightroom", &serde_json::json!({ "path": scratch.0.join("Missing.cocatalogdb") })).unwrap_err();
    assert!(error.to_string().ends_with("Missing.cocatalogdb: no Capture One catalog database (.cocatalogdb) found"), "{error}");
    assert!(!error.to_string().contains("Lightroom"), "{error}");
    std::fs::create_dir_all(scratch.0.join("Empty.cocatalog")).unwrap();
    let error = s.execute("library.importLightroom", &serde_json::json!({ "path": scratch.0.join("Empty.cocatalog") })).unwrap_err();
    assert!(error.to_string().contains("no Capture One catalog database (.cocatalogdb) found"), "{error}");
    assert!(!error.to_string().contains("Lightroom"), "a Capture One catalog fails in its own words: {error}");
}

#[test]
fn an_original_that_is_not_a_photo_and_a_refused_path_never_become_records() {
    let scratch = Scratch::new("refused");
    let mut tables = sample(&scratch.0.join("Photos"));
    table(&mut tables, "ZIMAGE").rows = vec![image(1, 1, "payload.exe"), image(2, 1, "/etc/passwd.jpg"), image(3, 1, "kept.jpg")];
    let tables = with(tables, spec("ZVARIANT", VARIANT_COLUMNS, vec![variant(1, 1, 127, 10), variant(2, 2, 127, 30), variant(3, 3, 127, 40)]));
    let package = scratch.catalog(&tables);
    write_jpeg(&package.join("Originals").join("kept.jpg"));
    std::fs::write(package.join("Originals").join("payload.exe"), b"MZ").unwrap();
    let mut s = crate::Session::new().with_fs();
    let report = s.execute("library.importLightroom", &serde_json::json!({ "path": package })).unwrap();
    let names: Vec<_> = s.catalog.photos().map(|p| p.file_name.clone()).collect();
    assert_eq!(names, ["kept.jpg"], "{report}");
    let warnings = report["warnings"].to_string();
    assert!(warnings.contains("payload.exe isn't one of the file types LightCraft reads"), "{warnings}");
    assert!(warnings.contains("/etc/passwd.jpg is not opened"), "{warnings}");
}

/// Opt-in: read a real catalog named by `LIGHTCRAFT_C1_CATALOG` and check what any catalog has
/// to satisfy. Nothing about the catalog's content is asserted or kept.
#[test]
fn a_real_catalog_reads() {
    let Some(path) = std::env::var_os("LIGHTCRAFT_C1_CATALOG").filter(|p| !p.is_empty()) else {
        lightcraft_testkit::skip!(Input, "set LIGHTCRAFT_C1_CATALOG to a Capture One catalog to read it");
        return;
    };
    let catalog = read(Path::new(&path)).unwrap();
    eprintln!(
        "{} photo(s) ({} with a camera, {} with a lens, {} with GPS), {} trashed, {} collection(s), {} keyword(s), written by {:?}, warnings: {:?}",
        catalog.photos.len(),
        catalog.photos.iter().filter(|p| !p.camera_model.is_empty()).count(),
        catalog.photos.iter().filter(|p| !p.lens.is_empty()).count(),
        catalog.photos.iter().filter(|p| p.gps.is_some()).count(),
        catalog.trashed,
        catalog.collections.len(),
        catalog.keywords.len(),
        catalog.versions.last(),
        catalog.warnings
    );
    assert!(!catalog.photos.is_empty());
    assert!(catalog.photos.iter().all(|p| !p.file_name.is_empty() && p.rating <= 5));
    for collection in &catalog.collections {
        assert!(collection.parent.is_none_or(|parent| catalog.collections.iter().any(|c| c.id == parent)));
        assert!(collection.photos.iter().all(|at| *at < catalog.photos.len()));
    }
}
