//! XMP packets (ISO 16684-1 / Adobe XMP Specification Part 1–2): read and write via `craft-metadata`.

use crate::Metadata;
use crate::convert::{to_craft, xmp_from_craft};
pub use craft_metadata::{CRS_NS, XmpError, XmpNamespace, XmpValue, XmpWrite, parse_xmp_with, write_xmp_with};
use std::collections::BTreeMap;

/// LightCraft's XMP namespace URI (prefix `lc`).
pub const LC_NS: &str = "http://ns.lightcraft.app/lc/1.0/";

const LC_NAMESPACES: &[XmpNamespace<'static>] = &[XmpNamespace { prefix: "lc", uri: LC_NS }];

/// Result of [`parse_xmp`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct XmpData {
    pub metadata: Metadata,
    /// The opaque `lc:settings` JSON string, if present.
    pub lc_settings: Option<String>,
    /// Every property found, keyed `prefix:name`.
    pub properties: BTreeMap<String, Vec<String>>,
    /// Every top-level property with its full structure.
    pub values: BTreeMap<String, XmpValue>,
}

impl XmpData {
    /// The pixel size the packet's regions were drawn on (`mwg-rs:AppliedToDimensions`), unless one of them says how
    /// the photo was turned (`mwg-rs:Rotation`).
    pub fn regions_drawn_on(&self) -> Option<(f64, f64)> {
        let regions = self.values.get("mwg-rs:Regions")?;
        let turned = regions.field("mwg-rs:RegionList").is_some_and(|l| l.items().iter().any(|r| r.field("mwg-rs:Rotation").is_some()));
        if turned { None } else { applied_to(regions) }
    }

    /// Which way to turn the regions of a photo whose pixels were turned a quarter turn after they were drawn
    /// ([`crate::regions_onto_turned_pixels`]). Apple's face regions say how each face leans on the frame they were
    /// drawn on (`apple-fi:AngleInfoRoll`, degrees): about 270 is a photo shot upright on a phone held the usual way
    /// (Exif orientation 6), about 90 one held the other way (8). Without that, the usual way.
    pub fn regions_turn(&self) -> crate::Orientation {
        let rolls = self.values.get("mwg-rs:Regions").and_then(|r| r.field("mwg-rs:RegionList")).map_or(&[][..], XmpValue::items);
        let roll = rolls.iter().find_map(|r| {
            let XmpValue::Struct(ext) = r.field("mwg-rs:Extensions")? else { return None };
            ext.iter()
                .find(|(k, _)| k.ends_with(":AngleInfoRoll"))
                .and_then(|(_, v)| v.text())
                .and_then(|t| t.trim().parse::<f64>().ok())
                .filter(|r| r.is_finite())
        });
        match roll.map(|r| r.rem_euclid(360.0)) {
            Some(r) if (45.0..135.0).contains(&r) => crate::Orientation::Rotate270,
            _ => crate::Orientation::Rotate90,
        }
    }
}

fn applied_to(regions: &XmpValue) -> Option<(f64, f64)> {
    let dims = regions.field("mwg-rs:AppliedToDimensions")?;
    let w = dims.field("stDim:w")?.text()?.parse::<f64>().ok().filter(|v| v.is_finite() && *v > 0.0)?;
    let h = dims.field("stDim:h")?.text()?.parse::<f64>().ok().filter(|v| v.is_finite() && *v > 0.0)?;
    let unit = dims.field("stDim:unit").and_then(XmpValue::text).unwrap_or("pixel");
    (unit.is_empty() || unit.eq_ignore_ascii_case("pixel")).then_some((w, h))
}

pub fn parse_xmp(s: &str) -> Result<XmpData, XmpError> {
    parse_xmp_with(s, LC_NAMESPACES).map(xmp_from_craft)
}

pub fn write_xmp(meta: &Metadata, lc_settings: Option<&str>) -> String {
    let settings = lc_settings.map(|s| [("settings", s)]);
    write_xmp_lc(meta, settings.as_ref().map_or(&[], |a| a.as_slice()))
}

pub fn write_xmp_lc(meta: &Metadata, lc: &[(&str, &str)]) -> String {
    write_xmp_with(&to_craft(meta), &XmpWrite { toolkit: "LightCraft", namespace: Some(XmpNamespace { prefix: "lc", uri: LC_NS }), properties: lc })
}
