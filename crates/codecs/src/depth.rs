//! Recorded depth: the depth (or disparity) map and the portrait matte an iPhone Portrait HEIC carries as
//! auxiliary images of the photo, read through `craft-heif` (the `heif` feature).
//!
//! The depth map's samples are stored as integers; the XMP packet that describes it (Apple's
//! `http://ns.apple.com/pixeldatainfo/1.0/` namespace) says what they mean: `AuxiliaryImageType` (`disparity`, in
//! 1/metres, or `depth`, in metres) and the linear map from `IntMinValue..IntMaxValue` to
//! `FloatMinValue..FloatMaxValue`. Both are returned as disparity, larger nearer, which is what a lens blur
//! needs (the blur grows with the disparity difference from the focal plane).
//!
//! Recorded depth is best effort: a file without it, a build without the decoder, or a map that is malformed or
//! doesn't say how to read it gives `None` (the last two with a logged reason), never an error that would stop the
//! photo from opening.

/// A photo's recorded depth, at the depth map's own size (smaller than the photo: a quarter on iPhones) and in the
/// photo's orientation. The caller scales it to the photo.
#[derive(Debug, Clone, PartialEq)]
pub struct Depth {
    pub width: u32,
    pub height: u32,
    /// Disparity in 1/metres, row-major: larger is nearer. Never negative, NaN or infinite.
    pub disparity: Vec<f32>,
    /// The portrait matte (the person or pet), when the file has one.
    pub matte: Option<Matte>,
}

/// A matte at its own size (on iPhones half the photo's, larger than the depth map), in the photo's orientation.
#[derive(Debug, Clone, PartialEq)]
pub struct Matte {
    pub width: u32,
    pub height: u32,
    /// Coverage from 0 (background) to 1 (subject), row-major.
    pub alpha: Vec<f32>,
}

/// Apple's namespace for how an auxiliary image's samples are stored.
pub const PIXEL_DATA_INFO_NS: &str = "http://ns.apple.com/pixeldatainfo/1.0/";

/// The `auxC` type of an iPhone Portrait photo's matte.
#[cfg(feature = "heif")]
const PORTRAIT_MATTE: &str = "urn:com:apple:photo:2018:aux:portraiteffectsmatte";

/// How a depth map's stored samples map to physical values.
#[cfg(any(feature = "heif", test))]
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Scale {
    /// The samples are disparity (1/metres); otherwise depth (metres).
    disparity: bool,
    int_min: f32,
    int_max: f32,
    float_min: f32,
    float_max: f32,
}

#[cfg(any(feature = "heif", test))]
impl Scale {
    /// The scale an `apdi:` packet states, matched by namespace URI whatever the prefix. `None` when the packet
    /// doesn't name the type or the ranges, or they make no sense (empty or reversed, non-finite).
    pub(crate) fn from_xmp(xmp: &str) -> Option<Scale> {
        let ns = [craft_metadata::XmpNamespace { prefix: "apdi", uri: PIXEL_DATA_INFO_NS }];
        let data = craft_metadata::parse_xmp_with(xmp, &ns).ok()?;
        let text = |name: &str| data.properties.get(&format!("apdi:{name}")).and_then(|v| v.first()).map(|s| s.trim().to_string());
        let number = |name: &str| text(name)?.parse::<f32>().ok().filter(|v| v.is_finite());
        let disparity = match text("AuxiliaryImageType")?.as_str() {
            "disparity" => true,
            "depth" => false,
            _ => return None,
        };
        let s = Scale {
            disparity,
            int_min: number("IntMinValue")?,
            int_max: number("IntMaxValue")?,
            float_min: number("FloatMinValue")?,
            float_max: number("FloatMaxValue")?,
        };
        (s.int_max > s.int_min && s.float_max >= s.float_min && s.float_min >= 0.0).then_some(s)
    }

    /// One stored sample as disparity (1/metres), clamped to the stated range. A depth of zero (no measurement) is
    /// disparity zero: infinitely far, never infinite.
    pub(crate) fn disparity(&self, sample: u16) -> f32 {
        let t = ((f32::from(sample) - self.int_min) / (self.int_max - self.int_min)).clamp(0.0, 1.0);
        let v = self.float_min + t * (self.float_max - self.float_min);
        if self.disparity {
            v
        } else if v > 0.0 {
            1.0 / v
        } else {
            0.0
        }
    }
}

/// The photo's recorded depth, or `None` (see the module docs).
#[cfg(feature = "heif")]
pub fn read_depth(bytes: &[u8]) -> Option<Depth> {
    let aux = match craft_heif::auxiliary_images(bytes) {
        Ok(aux) => aux,
        Err(e) => {
            log::warn!("recorded depth: the auxiliary images can't be listed: {e}");
            return None;
        }
    };
    let map = aux.iter().find(|a| a.kind == craft_heif::AuxKind::Depth)?;
    let Some(scale) = map.xmp.as_deref().and_then(Scale::from_xmp) else {
        log::warn!("recorded depth: the depth map doesn't say how to read its samples, so it is left out");
        return None;
    };
    let options = craft_heif::Options::default();
    let plane = match craft_heif::decode_auxiliary(bytes, map.id, &options) {
        Ok(p) => p,
        Err(e) => {
            log::warn!("recorded depth: the depth map can't be decoded, so it is left out: {e}");
            return None;
        }
    };
    let disparity = plane.data.iter().map(|&s| scale.disparity(s)).collect();
    let matte = aux.iter().find(|a| a.urn == PORTRAIT_MATTE).and_then(|m| match craft_heif::decode_auxiliary(bytes, m.id, &options) {
        Ok(p) => Some(matte_from(&p)),
        Err(e) => {
            log::warn!("recorded depth: the portrait matte can't be decoded, so it is left out: {e}");
            None
        }
    });
    Some(Depth { width: plane.width, height: plane.height, disparity, matte })
}

/// The photo's recorded depth: always `None` in a build without the HEIF decoder.
#[cfg(not(feature = "heif"))]
pub fn read_depth(_bytes: &[u8]) -> Option<Depth> {
    None
}

/// A matte's samples as coverage from 0 to 1, by its bit depth.
#[cfg(feature = "heif")]
fn matte_from(p: &craft_heif::Plane) -> Matte {
    let max = f32::from(((1u32 << u32::from(p.bit_depth).clamp(1, 16)) - 1) as u16);
    Matte { width: p.width, height: p.height, alpha: p.data.iter().map(|&s| (f32::from(s) / max).clamp(0.0, 1.0)).collect() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(body: &str) -> String {
        format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:pd="{PIXEL_DATA_INFO_NS}">{body}</rdf:Description></rdf:RDF></x:xmpmeta>"#
        )
    }

    fn fields(kind: &str, ints: (&str, &str), floats: (&str, &str)) -> String {
        packet(&format!(
            "<pd:AuxiliaryImageType>{kind}</pd:AuxiliaryImageType><pd:IntMinValue>{}</pd:IntMinValue><pd:IntMaxValue>{}</pd:IntMaxValue><pd:FloatMinValue>{}</pd:FloatMinValue><pd:FloatMaxValue>{}</pd:FloatMaxValue>",
            ints.0, ints.1, floats.0, floats.1
        ))
    }

    /// Issue #4330: as an iPhone writes it (8-bit samples, here 0.153931 to 0.598633 1/m), samples map linearly onto
    /// the stated range, by namespace whatever the prefix; values outside the integer range are clamped.
    #[test]
    fn disparity_samples_map_onto_the_stated_range() {
        let s = Scale::from_xmp(&fields("disparity", ("0", "255"), ("0.153931", "0.598633"))).unwrap();
        assert!((s.disparity(0) - 0.153931).abs() < 1e-6);
        assert!((s.disparity(255) - 0.598633).abs() < 1e-6);
        assert!((s.disparity(51) - (0.153931 + 0.2 * (0.598633 - 0.153931))).abs() < 1e-5);
        assert!((s.disparity(1000) - 0.598633).abs() < 1e-6, "clamped");
    }

    /// A depth map in metres is turned into disparity; no measurement (0 m) is infinitely far, not infinite.
    #[test]
    fn depth_in_metres_becomes_disparity() {
        let s = Scale::from_xmp(&fields("depth", ("0", "255"), ("0", "4"))).unwrap();
        assert!((s.disparity(255) - 0.25).abs() < 1e-6, "4 m");
        assert!((s.disparity(127) - 1.0 / (127.0 / 255.0 * 4.0)).abs() < 1e-5);
        assert_eq!(s.disparity(0), 0.0);
        assert!((0..=255).all(|v| s.disparity(v).is_finite()));
    }

    /// A packet that doesn't say how to read the samples gives no scale: the depth map is left out rather than
    /// guessed at.
    #[test]
    fn an_incomplete_or_nonsensical_packet_gives_no_scale() {
        for bad in [
            fields("normals", ("0", "255"), ("0", "1")),
            fields("disparity", ("255", "0"), ("0", "1")),
            fields("disparity", ("0", "0"), ("0", "1")),
            fields("disparity", ("0", "255"), ("1", "0")),
            fields("disparity", ("0", "255"), ("-1", "1")),
            fields("disparity", ("0", "255"), ("0", "inf")),
            fields("disparity", ("0", "x"), ("0", "1")),
            packet("<pd:AuxiliaryImageType>disparity</pd:AuxiliaryImageType>"),
            "not xml".to_string(),
        ] {
            assert_eq!(Scale::from_xmp(&bad), None, "{bad}");
        }
        let other_ns = fields("disparity", ("0", "255"), ("0", "1")).replace(PIXEL_DATA_INFO_NS, "http://example.com/");
        assert_eq!(Scale::from_xmp(&other_ns), None, "the prefix alone doesn't make it Apple's");
    }

    #[cfg(not(feature = "heif"))]
    #[test]
    fn without_the_decoder_there_is_no_depth() {
        assert_eq!(read_depth(b"\0\0\0\x18ftypheic\0\0\0\0mif1heic"), None);
    }

    /// Issue #4330: a synthetic Portrait HEIC (a quarter-size disparity map with its `apdi:` packet and a matte, both
    /// turned like the photo) gives disparity in the stated range and coverage from 0 to 1, in the photo's orientation.
    #[cfg(feature = "heif")]
    #[test]
    fn a_portrait_heic_gives_its_disparity_and_matte() {
        use craft_heif::testdata::{Aux, Prop, Spec, build};
        let grey = |_: u32, _: u32| [128u16, 128, 128];
        let depth = |x: u32, _: u32| (x * 8) as u16; // 0 at the left, 248 at the right, as coded
        let matte = |x: u32, _: u32| if x < 32 { 255 } else { 0 };
        let xmp = fields("disparity", ("0", "255"), ("0.1", "0.6"));
        let turn = vec![Prop::Irot(2)];
        let spec = Spec {
            props: turn.clone(),
            aux: vec![
                Aux { urn: "urn:mpeg:hevc:2015:auxid:2", width: 32, height: 16, sample: &depth, props: turn.clone(), xmp: Some(xmp) },
                Aux { urn: PORTRAIT_MATTE, width: 64, height: 32, sample: &matte, props: turn, xmp: None },
            ],
            ..Spec::new(128, 64, &grey)
        };
        let d = read_depth(&build(&spec)).expect("recorded depth");
        assert_eq!((d.width, d.height, d.disparity.len()), (32, 16, 32 * 16));
        // turned half a turn: the coded right edge (nearest) is now the left
        assert!((d.disparity[0] - (0.1 + 248.0 / 255.0 * 0.5)).abs() < 1e-4, "{}", d.disparity[0]);
        assert!((d.disparity[31] - 0.1).abs() < 1e-4);
        let m = d.matte.expect("a matte");
        assert_eq!((m.width, m.height), (64, 32));
        assert_eq!((m.alpha[0], m.alpha[63]), (0.0, 1.0), "the subject was on the coded left");
        // no packet saying how to read the map: no depth
        let mut bare = spec;
        bare.aux[0].xmp = None;
        assert_eq!(read_depth(&build(&bare)), None);
        // a photo without depth, and garbage
        assert_eq!(read_depth(&build(&Spec::new(64, 32, &grey))), None);
        assert_eq!(read_depth(b"not a heic"), None);
    }
}
