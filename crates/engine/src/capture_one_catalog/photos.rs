//! Images, their variants and each variant's metadata.

use std::collections::HashMap;
use std::path::Path;

use super::tables::{Db, Row, Table};
use super::{CatalogPhoto, ColorTag, Iptc, OriginalKind, ReadError, keywords, paths};

/// Capture One's "no date": 0001-01-01 as Unix seconds.
const NO_DATE: f64 = -62_135_596_800.0;
/// 9999-12-31T23:59:59 as Unix seconds: the last time an ISO date with a four-digit year holds.
const LAST_DATE: f64 = 253_402_300_799.0;
/// The metadata columns with a field of their own in [`Iptc`], and the keyword field.
const NAMED: &[&str] = &[
    "ZSTATUS_TITLE",
    "ZCONTENT_HEADLINE",
    "ZCONTENT_DESCRIPTION",
    "ZCONTACT_CREATOR",
    "ZSTATUS_COPYRIGHTNOTICE",
    "ZSTATUS_USAGETERMS",
    "ZIMAGE_LOCATION",
    "ZIMAGE_CITY",
    "ZIMAGE_STATE",
    "ZIMAGE_COUNTRY",
    "ZIMAGE_ISOCOUNTRYCODE",
    "ZCONTENT_KEYWORDS",
];

/// The photos of a catalog and where collections find them.
pub(super) struct Read {
    pub photos: Vec<CatalogPhoto>,
    pub trashed: usize,
    /// `ZVARIANT.Z_PK` to its index in `photos`.
    pub by_variant: HashMap<i64, usize>,
    /// `ZIMAGE.Z_PK` to the index of its first variant in `photos`.
    pub first_of_image: HashMap<i64, usize>,
}

/// What was left out, counted while reading and reported once each.
#[derive(Default)]
struct Notes {
    stray_variants: usize,
    unknown_tags: usize,
    cut_keywords: usize,
}

pub(super) fn read(db: &Db, database: &Path, warnings: &mut Vec<String>, active: &dyn Fn() -> Result<(), ReadError>) -> Result<Read, ReadError> {
    let locations = db.table("ZPATHLOCATION")?;
    active()?;
    let images = db.table("ZIMAGE")?;
    active()?;
    let variants = db.table("ZVARIANT")?;
    active()?;
    let metadata = db.table("ZVARIANTMETADATA")?;
    active()?;

    let location_of: HashMap<i64, Row<'_>> = locations.rows().filter_map(|r| Some((r.id("Z_PK")?, r))).collect();
    let metadata_of: HashMap<i64, Row<'_>> = metadata.rows().filter_map(|r| Some((r.id("ZLAYER")?, r))).collect();
    let mut image_rows: Vec<(i64, Row<'_>)> = images.rows().filter_map(|r| Some((r.id("Z_PK")?, r))).collect();
    image_rows.sort_by_key(|(id, _)| *id);
    let mut notes = Notes::default();
    let mut variants_of = variants_by_image(&variants, &image_rows, &mut notes);

    let mut out = Read { photos: Vec::new(), trashed: 0, by_variant: HashMap::new(), first_of_image: HashMap::new() };
    for (image_id, image) in image_rows {
        if image.flag("ZISTRASHED") {
            out.trashed = out.trashed.saturating_add(1);
            continue;
        }
        let location = image.id("ZIMAGELOCATION").and_then(|id| location_of.get(&id).copied());
        let base = image_fields(image_id, image, location, database);
        out.first_of_image.insert(image_id, out.photos.len());
        let own = variants_of.remove(&image_id).unwrap_or_default();
        if own.is_empty() {
            // an image without a variant has no rating or tag, but its file is still a photo
            out.photos.push(base);
            continue;
        }
        for (index, (variant_id, variant)) in own.into_iter().enumerate() {
            let mut photo = base.clone();
            photo.variant_id = Some(variant_id);
            photo.variant_index = index;
            photo.variant_uuid = variant.text("ZVARIANTUUID").trim().to_string();
            photo.adjusted = variant.flag("ZISMODIFIED");
            if let Some(row) = shown_metadata(variant, &metadata_of) {
                variant_metadata(&mut photo, row, &metadata, &mut notes);
            }
            out.by_variant.insert(variant_id, out.photos.len());
            out.photos.push(photo);
        }
    }
    notes.report(warnings);
    Ok(out)
}

/// Each image's variants in Capture One's order (`ZINDEX`, then key). Variants of an image the
/// catalog doesn't have are counted and left out.
fn variants_by_image<'a>(variants: &'a Table, images: &[(i64, Row<'_>)], notes: &mut Notes) -> HashMap<i64, Vec<(i64, Row<'a>)>> {
    let mut by_image: HashMap<i64, Vec<(i64, Row<'a>)>> = images.iter().map(|(id, _)| (*id, Vec::new())).collect();
    for row in variants.rows() {
        let Some(id) = row.id("Z_PK") else { continue };
        match row.id("ZIMAGE").and_then(|image| by_image.get_mut(&image)) {
            Some(own) => own.push((id, row)),
            None => notes.stray_variants = notes.stray_variants.saturating_add(1),
        }
    }
    for own in by_image.values_mut() {
        own.sort_by_key(|(id, row)| (row.int("ZINDEX").unwrap_or(0), *id));
    }
    by_image
}

/// The metadata Capture One shows for a variant: the combined layer's, else the adjustment
/// layer's, else the default layer's.
fn shown_metadata<'a>(variant: Row<'_>, metadata_of: &HashMap<i64, Row<'a>>) -> Option<Row<'a>> {
    ["ZCOMBINEDSETTINGS", "ZADJUSTMENTLAYER", "ZDEFAULTLAYER"]
        .into_iter()
        .find_map(|layer| variant.id(layer).and_then(|id| metadata_of.get(&id).copied()))
}

fn image_fields(image_id: i64, image: Row<'_>, location: Option<Row<'_>>, database: &Path) -> CatalogPhoto {
    let file_name = image.text("ZIMAGEFILENAME").to_string();
    let place = paths::original(location, &file_name, database);
    let positive = |column: &str| image.real(column).filter(|n| *n > 0.0);
    CatalogPhoto {
        image_id,
        image_uuid: image.text("ZIMAGEUUID").trim().to_string(),
        file_name,
        path: place.path,
        original: place.original,
        managed: place.managed,
        kind: match image.int("ZIMAGECLASSIFICATION") {
            Some(17) => OriginalKind::Raw,
            Some(19) => OriginalKind::Raster,
            Some(6) => OriginalKind::Movie,
            _ => OriginalKind::Unknown,
        },
        width: image.int("ZWIDTH").and_then(|n| u32::try_from(n).ok()).unwrap_or(0),
        height: image.int("ZHEIGHT").and_then(|n| u32::try_from(n).ok()).unwrap_or(0),
        file_size: image.int("ZFILE_SIZE").and_then(|n| u64::try_from(n).ok()).unwrap_or(0),
        captured: time(image.real("ZEXP_DATE"), ""),
        imported: time(image.real("ZIMPORTDATE"), "Z"),
        camera_make: image.text("ZCAMERA_MAKE").trim().to_string(),
        camera_model: image.text("ZCAMERA_MODEL").trim().to_string(),
        camera_serial: image.text("ZCAMERA_SERIAL").trim().to_string(),
        lens: image.text("ZCAMERA_LENS").trim().to_string(),
        iso: image.int("ZISO").filter(|n| *n > 0).and_then(|n| u32::try_from(n).ok()),
        aperture: positive("ZEXP_APERTURE").map(|n| n as f32),
        shutter_seconds: positive("ZEXP_SHUTTERSPEED"),
        focal_mm: positive("ZEXP_FOCALLENGTH").map(|n| n as f32),
        exposure_bias: fraction(image.text("ZEXP_EXPOSURECOMPENSATION")),
        gps: gps(image),
        ..CatalogPhoto::default()
    }
}

/// Unix seconds as a calendar time in UTC (`YYYY-MM-DDTHH:MM:SS`) followed by `zone`; `None`
/// for Capture One's "no date", zero and times outside the years 0001 to 9999.
fn time(seconds: Option<f64>, zone: &str) -> Option<String> {
    let seconds = seconds.filter(|s| *s > NO_DATE && *s != 0.0 && *s <= LAST_DATE)?;
    Some(format!("{}{zone}", lightcraft_catalog::dates::civil(seconds.trunc() as i64)))
}

/// A value Capture One writes as a fraction (`-33/100`) or a plain number.
fn fraction(text: &str) -> Option<f32> {
    let text = text.trim();
    let value = match text.split_once('/') {
        Some((n, d)) => {
            let d: f64 = d.trim().parse().ok()?;
            if d == 0.0 {
                return None;
            }
            n.trim().parse::<f64>().ok()? / d
        }
        None => text.parse().ok()?,
    };
    value.is_finite().then_some(value as f32)
}

fn gps(image: Row<'_>) -> Option<(f64, f64)> {
    let latitude = image.real("ZGPSLATITUDE")?;
    let longitude = image.real("ZGPSLONGITUDE")?;
    let on_earth = (-90.0..=90.0).contains(&latitude) && (-180.0..=180.0).contains(&longitude);
    (on_earth && (latitude != 0.0 || longitude != 0.0)).then_some((latitude, longitude))
}

fn variant_metadata(photo: &mut CatalogPhoto, row: Row<'_>, table: &Table, notes: &mut Notes) {
    photo.rating = row.int("ZBASIC_RATING").unwrap_or(0).clamp(0, 5) as u8;
    photo.color_tag = match row.int("ZCOLOR_TAG_INDEX").map(ColorTag::from_index) {
        Some(Some(tag)) => tag,
        Some(None) => {
            notes.unknown_tags = notes.unknown_tags.saturating_add(1);
            ColorTag::None
        }
        None => ColorTag::None,
    };
    let (keywords, cut) = keywords::parse(row.text("ZCONTENT_KEYWORDS"));
    photo.keywords = keywords;
    notes.cut_keywords = notes.cut_keywords.saturating_add(usize::from(cut));
    let field = |column: &str| row.text(column).trim().to_string();
    photo.iptc = Iptc {
        title: field("ZSTATUS_TITLE"),
        headline: field("ZCONTENT_HEADLINE"),
        description: field("ZCONTENT_DESCRIPTION"),
        creator: field("ZCONTACT_CREATOR"),
        copyright: field("ZSTATUS_COPYRIGHTNOTICE"),
        usage_terms: field("ZSTATUS_USAGETERMS"),
        location: field("ZIMAGE_LOCATION"),
        city: field("ZIMAGE_CITY"),
        state: field("ZIMAGE_STATE"),
        country: field("ZIMAGE_COUNTRY"),
        country_code: field("ZIMAGE_ISOCOUNTRYCODE"),
        other: table
            .columns()
            .filter(|column| !NAMED.contains(column))
            .filter_map(|column| {
                let value = row.text(column).trim();
                (!value.is_empty()).then(|| (column.trim_start_matches('Z').to_string(), value.to_string()))
            })
            .collect(),
    };
}

impl Notes {
    fn report(&self, warnings: &mut Vec<String>) {
        if self.stray_variants > 0 {
            warnings.push(format!("{} variant(s) belong to no image in the catalog and were left out", self.stray_variants));
        }
        if self.unknown_tags > 0 {
            warnings.push(format!("{} photo(s) have a colour tag this version doesn't know; imported without one", self.unknown_tags));
        }
        if self.cut_keywords > 0 {
            warnings.push(format!("{} photo(s) have more than {} keywords; the rest were left out", self.cut_keywords, keywords::MAX_KEYWORDS));
        }
    }
}
