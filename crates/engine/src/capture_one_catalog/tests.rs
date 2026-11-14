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
        let rows: Vec<(i64, Vec<V>)> = table
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

fn entities() -> Spec {
    let names = [
        (ALBUM, "AlbumCollection"),
        (SMART, "SmartCollection"),
        (PROJECT, "ProjectCollection"),
        (GROUP, "VirtualFolderCollection"),
        (TRASH, "TrashCollection"),
        (FOLDER, "CatalogFolderCollection"),
        (ALL, "CatalogAllImagesCollection"),
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
        ("ZEXP_DATE", V::R(1_601_230_617.0)),
        ("ZIMPORTDATE", V::R(1_604_310_308.92)),
        ("ZCAMERA_MAKE", t("FUJIFILM")),
        ("ZCAMERA_MODEL", t("X-T2")),
        ("ZCAMERA_LENS", t("XF18-55mm ")),
        ("ZISO", V::I(200)),
        ("ZEXP_APERTURE", V::R(8.0)),
        ("ZEXP_SHUTTERSPEED", V::R(0.004)),
        ("ZEXP_FOCALLENGTH", V::R(18.0)),
        ("ZEXP_EXPOSURECOMPENSATION", t("-33/100")),
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
        ("ZIMAGE_DATECREATED", t("20200927")),
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
    assert_eq!(first.captured.as_deref(), Some("2020-09-27T18:16:57"));
    assert_eq!(first.imported.as_deref(), Some("2020-11-02T09:45:08Z"));
    assert_eq!((first.camera_make.as_str(), first.camera_model.as_str(), first.lens.as_str()), ("FUJIFILM", "X-T2", "XF18-55mm"));
    assert_eq!((first.iso, first.aperture, first.shutter_seconds, first.focal_mm), (Some(200), Some(8.0), Some(0.004), Some(18.0)));
    assert_eq!(first.exposure_bias, Some(-0.33));
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
    assert_eq!(first.iptc.other.get("IMAGE_DATECREATED").map(String::as_str), Some("20200927"));

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
    assert_eq!(elsewhere.path, if cfg!(windows) { "/Volumes/Disk  /Shoots/elsewhere.ARW" } else { "D:\\Shoots\\elsewhere.ARW" });
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

// ---- finding the database

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

    let empty = scratch.0.join("Empty.cocatalog");
    std::fs::create_dir_all(&empty).unwrap();
    assert!(!is_catalog(&empty));
    assert!(matches!(read(&empty), Err(ReadError::NotFound(_))));
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

    std::fs::write(package.join("writelock"), "someone\n0123").unwrap();
    let catalog = read(&package).unwrap();
    assert!(catalog.in_use);
    assert_eq!(catalog.warnings, ["Capture One has this catalog open; changes it has not saved yet are not included"]);
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
    let deep = format!("{}||0", vec!["x"; 50_000].join("|"));
    assert_eq!(keywords::parse(&deep).0.len(), 1);
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
        "{} photo(s), {} trashed, {} collection(s), written by {:?}, warnings: {:?}",
        catalog.photos.len(),
        catalog.trashed,
        catalog.collections.len(),
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
