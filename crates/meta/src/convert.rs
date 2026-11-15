use crate::{FocusPoint, Metadata, Orientation, Rect, Region, RegionKind, XmpData};

pub(crate) fn rect_from_craft(r: craft_metadata::Rect) -> Rect {
    Rect { x0: r.x0, y0: r.y0, x1: r.x1, y1: r.y1 }
}

pub(crate) fn rect_to_craft(r: Rect) -> craft_metadata::Rect {
    craft_metadata::Rect { x0: r.x0, y0: r.y0, x1: r.x1, y1: r.y1 }
}

pub(crate) fn orientation_from_craft(o: craft_metadata::Orientation) -> Orientation {
    Orientation::from_exif(o.to_exif())
}

pub(crate) fn orientation_to_craft(o: Orientation) -> craft_metadata::Orientation {
    craft_metadata::Orientation::from_exif(o.to_exif())
}

pub(crate) fn focus_from_craft(f: craft_metadata::FocusPoint) -> FocusPoint {
    FocusPoint { rect: rect_from_craft(f.rect), mode: f.mode, area: f.area, tracking: f.tracking }
}

pub(crate) fn focus_to_craft(f: &FocusPoint) -> craft_metadata::FocusPoint {
    craft_metadata::FocusPoint { rect: rect_to_craft(f.rect), mode: f.mode.clone(), area: f.area.clone(), tracking: f.tracking.clone() }
}

pub(crate) fn region_from_craft(r: craft_metadata::Region) -> Region {
    let kind = match r.kind {
        craft_metadata::RegionKind::Face => RegionKind::Face,
        craft_metadata::RegionKind::Pet => RegionKind::Pet,
        craft_metadata::RegionKind::Focus => RegionKind::Focus,
        craft_metadata::RegionKind::BarCode => RegionKind::BarCode,
        craft_metadata::RegionKind::Other(value) => RegionKind::Other(value),
    };
    Region { rect: rect_from_craft(r.rect), kind, name: r.name, description: r.description }
}

/// Catalog-only regions must not become ordinary regions in interchange/export metadata.
pub(crate) fn region_to_craft(r: &Region) -> Option<craft_metadata::Region> {
    let kind = match &r.kind {
        RegionKind::Face => craft_metadata::RegionKind::Face,
        RegionKind::IgnoredFace => return None,
        RegionKind::Pet => craft_metadata::RegionKind::Pet,
        RegionKind::Focus => craft_metadata::RegionKind::Focus,
        RegionKind::BarCode => craft_metadata::RegionKind::BarCode,
        RegionKind::Other(value) => craft_metadata::RegionKind::Other(value.clone()),
    };
    Some(craft_metadata::Region { rect: rect_to_craft(r.rect), kind, name: r.name.clone(), description: r.description.clone() })
}

pub(crate) fn from_craft(m: craft_metadata::Metadata) -> Metadata {
    Metadata {
        make: m.make,
        model: m.model,
        serial_number: m.serial_number,
        software: m.software,
        lens_make: m.lens_make,
        lens_model: m.lens_model,
        lens_serial_number: m.lens_serial_number,
        lens_spec: m.lens_spec,
        capture_time: m.capture_time,
        exposure_time: m.exposure_time,
        f_number: m.f_number,
        iso: m.iso,
        focal_length: m.focal_length,
        focal_length_35mm: m.focal_length_35mm,
        flash: m.flash,
        exposure_bias: m.exposure_bias,
        exposure_program: m.exposure_program,
        metering_mode: m.metering_mode,
        white_balance: m.white_balance,
        orientation: m.orientation.map(orientation_from_craft),
        gps: m.gps,
        width: m.width,
        height: m.height,
        artist: m.artist,
        copyright: m.copyright,
        copyright_marked: m.copyright_marked,
        usage_terms: m.usage_terms,
        copyright_url: m.copyright_url,
        title: m.title,
        caption: m.caption,
        alt_text: m.alt_text,
        extended_description: m.extended_description,
        sublocation: m.sublocation,
        city: m.city,
        state: m.state,
        country: m.country,
        keywords: m.keywords,
        hierarchical_keywords: m.hierarchical_keywords,
        rating: m.rating,
        label: m.label,
        regions: m.regions.into_iter().map(region_from_craft).collect(),
        focus: m.focus.map(focus_from_craft),
    }
}

pub(crate) fn to_craft(m: &Metadata) -> craft_metadata::Metadata {
    craft_metadata::Metadata {
        make: m.make.clone(),
        model: m.model.clone(),
        serial_number: m.serial_number.clone(),
        software: m.software.clone(),
        lens_make: m.lens_make.clone(),
        lens_model: m.lens_model.clone(),
        lens_serial_number: m.lens_serial_number.clone(),
        lens_spec: m.lens_spec,
        capture_time: m.capture_time,
        exposure_time: m.exposure_time,
        f_number: m.f_number,
        iso: m.iso,
        focal_length: m.focal_length,
        focal_length_35mm: m.focal_length_35mm,
        flash: m.flash,
        exposure_bias: m.exposure_bias,
        exposure_program: m.exposure_program,
        metering_mode: m.metering_mode,
        white_balance: m.white_balance,
        orientation: m.orientation.map(orientation_to_craft),
        gps: m.gps,
        width: m.width,
        height: m.height,
        artist: m.artist.clone(),
        copyright: m.copyright.clone(),
        copyright_marked: m.copyright_marked,
        usage_terms: m.usage_terms.clone(),
        copyright_url: m.copyright_url.clone(),
        title: m.title.clone(),
        caption: m.caption.clone(),
        alt_text: m.alt_text.clone(),
        extended_description: m.extended_description.clone(),
        sublocation: m.sublocation.clone(),
        city: m.city.clone(),
        state: m.state.clone(),
        country: m.country.clone(),
        keywords: m.keywords.clone(),
        hierarchical_keywords: m.hierarchical_keywords.clone(),
        rating: m.rating,
        label: m.label.clone(),
        regions: m.regions.iter().filter_map(region_to_craft).collect(),
        focus: m.focus.as_ref().map(focus_to_craft),
        fuji_recipe: None,
    }
}

pub(crate) fn xmp_from_craft(d: craft_metadata::XmpData) -> XmpData {
    let lc_settings = d.properties.get("lc:settings").and_then(|v| v.first()).cloned();
    XmpData { metadata: from_craft(d.metadata), lc_settings, properties: d.properties, values: d.values }
}

#[cfg(test)]
mod tests {
    use super::{from_craft, region_from_craft, region_to_craft, to_craft};
    use crate::{Metadata, Rect, Region, RegionKind};

    fn region(kind: RegionKind) -> Region {
        Region { rect: Rect { x0: 0.1, y0: 0.2, x1: 0.4, y1: 0.6 }, kind, name: Some("Person".into()), description: Some("retained".into()) }
    }

    /// Every interchange kind, including unknown text, keeps its type, box, name and description.
    #[test]
    fn shared_regions_round_trip_without_changing_their_kind_or_geometry() {
        for kind in [
            RegionKind::Face,
            RegionKind::Pet,
            RegionKind::Focus,
            RegionKind::BarCode,
            RegionKind::Other("IgnoredFace".into()),
            RegionKind::Other(String::new()),
        ] {
            let r = region(kind);
            assert_eq!(region_from_craft(region_to_craft(&r).unwrap()), r);
        }
    }

    /// Export conversion omits ignores without mutating their catalog wire representation.
    #[test]
    fn ignored_faces_stay_in_the_catalog_not_shared_metadata() {
        let ignored = region(RegionKind::IgnoredFace);
        let metadata = Metadata {
            title: Some("Other metadata".into()),
            regions: vec![region(RegionKind::Face), ignored.clone(), region(RegionKind::Pet)],
            ..Default::default()
        };
        let before = serde_json::to_string(&metadata).unwrap();
        assert!(before.contains("\"kind\":\"IgnoredFace\""));
        let shared = to_craft(&metadata);
        assert_eq!(shared.regions.len(), 2);
        let read = from_craft(shared);
        assert_eq!(read.regions, vec![metadata.regions[0].clone(), metadata.regions[2].clone()]);
        assert_eq!(read.title, metadata.title);
        assert_eq!(serde_json::to_string(&metadata).unwrap(), before);
        assert_eq!(metadata.regions[1], ignored);
        assert_eq!(serde_json::from_str::<Metadata>(&before).unwrap(), metadata);
        assert_eq!(crate::write_xmp(&metadata, None), crate::write_xmp(&Metadata { regions: Vec::new(), ..metadata.clone() }, None));
    }
}
