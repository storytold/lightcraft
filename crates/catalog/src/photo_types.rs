//! Photo types: selfies, panoramas, long exposures, screenshots, drone and Portrait photos, worked out
//! from what a photo's record already holds (lens and camera names, Exif `CustomRendered`, pixel size,
//! shutter speed, format, file name, recorded depth). No type is stored: each is derived when asked for, so a photo
//! whose metadata is corrected changes type with it.
//!
//! Long exposure means a long shutter time. Apple Photos' Long Exposure effect, made from a Live
//! Photo's frames, is an edit in the Photos library that the original file does not record, so those
//! photos are not long exposures here.
//!
//! Search uses them as `type:selfie` (beside the file types `type:raw` / `type:video`) and smart
//! albums as the `photoType` rule.

use crate::{MediaKind, Photo, query::merged_kind};

/// A kind of photo the library can be filtered by, independent of its file type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PhotoType {
    /// Taken with a phone's front camera.
    Selfie,
    /// A panorama by the camera's own Exif `CustomRendered`, a Photo Merge panorama, or a still at least
    /// [`PANORAMA_ASPECT`] times as long as it is wide; never a screenshot.
    Panorama,
    /// A still exposed for [`LONG_EXPOSURE_SECONDS`] or longer.
    LongExposure,
    /// A screen capture: named as one, or a PNG that names no camera.
    Screenshot,
    /// Taken by a drone's camera, from its maker.
    Drone,
    /// A still with recorded depth ([`crate::Meta::has_depth`]): an iPhone Portrait photo, or one the phone gave depth
    /// to by itself, as recent iPhones do for people and pets.
    Portrait,
}

/// A still this many times as long as it is wide counts as a panorama (2.5:1 — wider than 21:9
/// crops, narrower than the phone panoramas, which run from about 3:1).
pub const PANORAMA_ASPECT: f64 = 2.5;

/// An exposure this long (seconds) or longer counts as a long exposure.
pub const LONG_EXPOSURE_SECONDS: f64 = 1.0;

/// Drone makers as Exif `Make` names them (lower case, matched as a prefix).
const DRONE_MAKERS: &[&str] = &["dji", "autel", "parrot", "skydio"];

/// Hand-held cameras sold by drone makers (DJI Osmo, Pocket, Action; lower case, matched within the
/// model name): their photos are not aerial.
const HANDHELD_MODELS: &[&str] = &["osmo", "pocket", "action", "ronin"];

impl PhotoType {
    /// Every type, in the order menus list them.
    pub const ALL: [PhotoType; 6] =
        [PhotoType::Selfie, PhotoType::Panorama, PhotoType::LongExposure, PhotoType::Screenshot, PhotoType::Drone, PhotoType::Portrait];

    /// The id commands, searches and rules use.
    pub fn id(self) -> &'static str {
        match self {
            PhotoType::Selfie => "selfie",
            PhotoType::Panorama => "panorama",
            PhotoType::LongExposure => "longExposure",
            PhotoType::Screenshot => "screenshot",
            PhotoType::Drone => "drone",
            PhotoType::Portrait => "portrait",
        }
    }

    /// The name people see.
    pub fn label(self) -> &'static str {
        match self {
            PhotoType::Selfie => "Selfie",
            PhotoType::Panorama => "Panorama",
            PhotoType::LongExposure => "Long Exposure",
            PhotoType::Screenshot => "Screenshot",
            PhotoType::Drone => "Drone",
            PhotoType::Portrait => "Portrait",
        }
    }

    /// The type an id or label names, ignoring case, spaces, hyphens and underscores, and a plural
    /// `s` (`selfies`, `long-exposure`, `Long Exposure`); `None` for anything else.
    pub fn parse(s: &str) -> Option<PhotoType> {
        let key: String = s.chars().filter(|c| !matches!(c, ' ' | '-' | '_')).flat_map(char::to_lowercase).collect();
        let key = key.strip_suffix('s').filter(|k| !k.is_empty()).unwrap_or(&key);
        PhotoType::ALL.into_iter().find(|t| t.id().eq_ignore_ascii_case(key))
    }
}

impl Photo {
    /// Whether this photo is of type `t` (see [`PhotoType`] for each rule).
    pub fn is_type(&self, t: PhotoType) -> bool {
        let still = self.kind != MediaKind::Video;
        let m = &self.meta;
        match t {
            PhotoType::Selfie => {
                // "iPhone 15 Pro front camera 2.69mm f/1.9", "iPhone 15 Pro Max front TrueDepth camera 2.69mm f/1.9"
                let lens = m.lens.to_lowercase();
                let words: Vec<&str> = lens.split(|c: char| !c.is_alphanumeric()).collect();
                words.contains(&"front") && (words.contains(&"camera") || words.contains(&"truedepth"))
            }
            PhotoType::Panorama => {
                let (long, short) = (self.width.max(self.height), self.width.min(self.height));
                // the camera says so (iPhone panoramas run from about 2:1), else a merge's name, else the
                // shape; a long screenshot of a scrolled page is not a panorama
                still
                    && !self.is_type(PhotoType::Screenshot)
                    && (m.custom_rendered == Some(lightcraft_meta::CUSTOM_RENDERED_PANORAMA)
                        || matches!(merged_kind(&self.file_name), Some("panorama" | "hdrPanorama"))
                        || (short > 0 && f64::from(long) >= PANORAMA_ASPECT * f64::from(short)))
            }
            PhotoType::LongExposure => still && m.shutter_seconds().is_some_and(|s| s >= LONG_EXPOSURE_SECONDS),
            PhotoType::Screenshot => {
                let name = self.file_name.to_lowercase();
                self.kind == MediaKind::Image
                    && (["screenshot", "screen shot", "screen_shot"].iter().any(|w| name.contains(w))
                        || (self.format.eq_ignore_ascii_case("png") && m.make.trim().is_empty() && m.camera.trim().is_empty()))
            }
            PhotoType::Drone => {
                let make = m.make.trim().to_lowercase();
                // `camera` is maker and model together; older records may have no `make` of their own
                let camera = m.camera.trim().to_lowercase();
                let maker = if make.is_empty() { &camera } else { &make };
                // stills only: drone videos come with video import (gap 18)
                still && DRONE_MAKERS.iter().any(|d| maker.starts_with(d)) && !HANDHELD_MODELS.iter().any(|h| camera.contains(h))
            }
            PhotoType::Portrait => still && m.has_depth,
        }
    }

    /// The types this photo is, in [`PhotoType::ALL`] order (a drone panorama is both).
    pub fn photo_types(&self) -> Vec<PhotoType> {
        PhotoType::ALL.into_iter().filter(|t| self.is_type(*t)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PhotoId, Source};

    fn photo(name: &str, format: &str, w: u32, h: u32) -> Photo {
        Photo::new(PhotoId(1), Source::File { path: name.to_string() }, name, format, w, h, "2026-11-15")
    }

    fn jpeg() -> Photo {
        let mut p = photo("IMG_0001.JPG", "JPEG", 4032, 3024);
        p.meta.make = "Apple".into();
        p.meta.camera = "Apple iPhone 15 Pro".into();
        p.meta.lens = "iPhone 15 Pro back triple camera 6.765mm f/1.78".into();
        p.meta.shutter = "1/120".into();
        p
    }

    #[test]
    fn an_ordinary_photo_has_no_type() {
        assert!(jpeg().photo_types().is_empty());
    }

    #[test]
    fn the_front_camera_makes_a_selfie() {
        let mut p = jpeg();
        for lens in [
            "iPhone 15 Pro front camera 2.69mm f/1.9",
            // newer iPhones name the front camera after its depth sensor
            "iPhone 15 Pro Max front TrueDepth camera 2.69mm f/1.9",
            "Pixel 8 Front Camera",
        ] {
            p.meta.lens = lens.into();
            assert_eq!(p.photo_types(), [PhotoType::Selfie], "{lens}");
        }
        // a word that only contains "front" is not the front camera
        p.meta.lens = "Storefront camera 24mm".into();
        assert!(!p.is_type(PhotoType::Selfie));
    }

    #[test]
    fn a_long_frame_or_a_merge_is_a_panorama() {
        let mut p = jpeg();
        (p.width, p.height) = (12000, 4000);
        assert!(p.is_type(PhotoType::Panorama));
        // tall panoramas count too
        (p.width, p.height) = (3000, 9000);
        assert!(p.is_type(PhotoType::Panorama));
        // a 21:9 crop is not one
        (p.width, p.height) = (2100, 900);
        assert!(!p.is_type(PhotoType::Panorama));
        let merged = photo("DSC_1-Pano.dng", "DNG", 6000, 4000);
        assert!(merged.is_type(PhotoType::Panorama));
        let mut video = photo("clip.mov", "MOV", 7680, 1080);
        video.kind = MediaKind::Video;
        assert!(!video.is_type(PhotoType::Panorama));
    }

    #[test]
    fn the_camera_can_say_it_is_a_panorama() {
        // an iPhone panorama of a short sweep is barely 2:1, but its Exif says what it is
        let mut p = jpeg();
        (p.width, p.height) = (7910, 3788);
        assert!(!p.is_type(PhotoType::Panorama));
        p.meta.custom_rendered = Some(lightcraft_meta::CUSTOM_RENDERED_PANORAMA);
        assert!(p.is_type(PhotoType::Panorama));
        // other values (Apple's portrait = 8, normal = 0) are not
        for other in [0, 1, 8] {
            p.meta.custom_rendered = Some(other);
            assert!(!p.is_type(PhotoType::Panorama), "{other}");
        }
    }

    #[test]
    fn a_long_screenshot_is_not_a_panorama() {
        let shot = photo("Screenshot 2026-10-19 at 10.05.29 AM.png", "PNG", 688, 1820);
        assert_eq!(shot.photo_types(), [PhotoType::Screenshot]);
    }

    #[test]
    fn a_zero_sized_photo_is_not_a_panorama() {
        assert!(!photo("broken.jpg", "JPEG", 4000, 0).is_type(PhotoType::Panorama));
        assert!(!photo("broken.jpg", "JPEG", 0, 0).is_type(PhotoType::Panorama));
    }

    #[test]
    fn a_second_or_longer_is_a_long_exposure() {
        let mut p = jpeg();
        for (shutter, long) in [("1/2", false), ("0.9", false), ("1", true), ("1\"", true), ("30", true), ("", false), ("bogus", false)] {
            p.meta.shutter = shutter.into();
            p.meta.exposure_time = None;
            assert_eq!(p.is_type(PhotoType::LongExposure), long, "{shutter}");
        }
    }

    #[test]
    fn screenshots_by_name_or_as_cameraless_pngs() {
        assert!(photo("Screenshot 2026-11-15 at 10.00.00.png", "PNG", 2940, 1912).is_type(PhotoType::Screenshot));
        assert!(photo("Screen Shot 2020-01-01.jpg", "JPEG", 100, 100).is_type(PhotoType::Screenshot));
        assert!(photo("IMG_0002.PNG", "PNG", 1179, 2556).is_type(PhotoType::Screenshot));
        // a PNG a camera wrote is not one
        let mut png = photo("IMG_0003.PNG", "PNG", 1179, 2556);
        png.meta.make = "Apple".into();
        assert!(!png.is_type(PhotoType::Screenshot));
        assert!(!jpeg().is_type(PhotoType::Screenshot));
    }

    #[test]
    fn drone_makers_but_not_their_handheld_cameras() {
        let mut p = jpeg();
        p.meta.make = "DJI".into();
        p.meta.camera = "DJI FC3582".into();
        assert!(p.is_type(PhotoType::Drone));
        let mut video = p.clone();
        video.kind = MediaKind::Video;
        assert!(!video.is_type(PhotoType::Drone), "a drone's video is not a drone photo");
        p.meta.camera = "DJI Osmo Pocket 3".into();
        assert!(!p.is_type(PhotoType::Drone));
        // no Make of its own: the camera name says it
        p.meta.make.clear();
        p.meta.camera = "Autel Robotics XT705".into();
        assert!(p.is_type(PhotoType::Drone));
    }

    /// Issue #4331: a still with recorded depth is a Portrait photo, a front-camera one a Portrait selfie too; a
    /// video never is, and neither is a photo without depth.
    #[test]
    fn recorded_depth_makes_a_portrait_photo() {
        let mut p = jpeg();
        assert!(!p.is_type(PhotoType::Portrait));
        p.meta.has_depth = true;
        assert!(p.is_type(PhotoType::Portrait));
        p.meta.lens = "iPhone 15 Pro Max front TrueDepth camera 2.69mm f/1.9".into();
        assert_eq!(p.photo_types(), [PhotoType::Selfie, PhotoType::Portrait]);
        p.kind = MediaKind::Video;
        assert!(!p.is_type(PhotoType::Portrait));
        for s in ["portrait", "Portraits", "PORTRAIT"] {
            assert_eq!(PhotoType::parse(s), Some(PhotoType::Portrait), "{s}");
        }
    }

    #[test]
    fn a_photo_can_have_several_types() {
        let mut p = jpeg();
        p.meta.make = "DJI".into();
        p.meta.camera = "DJI FC8482".into();
        (p.width, p.height) = (16000, 4000);
        assert_eq!(p.photo_types(), [PhotoType::Panorama, PhotoType::Drone]);
    }

    /// A library with an ordinary photo, a selfie, a panorama and a raw, by name.
    fn library() -> (crate::Catalog, [PhotoId; 5]) {
        let mut c = crate::Catalog::new();
        let mut add = |mut p: Photo| {
            let id = c.alloc_photo_id();
            p.id = id;
            c.apply(crate::Op::AddPhoto { photo: Box::new(p) }).map(|_| id)
        };
        let mut selfie = jpeg();
        selfie.meta.lens = "iPhone 15 Pro front camera 2.69mm f/1.9".into();
        let mut pano = jpeg();
        (pano.width, pano.height) = (12000, 4000);
        let mut raw = photo("DSC_0001.ARW", "ARW", 6000, 4000);
        raw.kind = MediaKind::Raw;
        let mut portrait = jpeg();
        portrait.meta.has_depth = true;
        let ids = [add(jpeg()).unwrap(), add(selfie).unwrap(), add(pano).unwrap(), add(raw).unwrap(), add(portrait).unwrap()];
        (c, ids)
    }

    #[test]
    fn search_finds_photo_types_beside_file_types() {
        let (c, [_, selfie, pano, raw, portrait]) = library();
        let q = |text: &str| c.query(&crate::Filter { text: text.into(), ..Default::default() }, &crate::Sort::default());
        assert_eq!(q("type:selfie"), [selfie]);
        assert_eq!(q("type:selfies"), [selfie]);
        assert_eq!(q("kind:panorama"), [pano]);
        assert_eq!(q("type:\"long exposure\""), []);
        // file types still work, and the two combine
        assert_eq!(q("type:raw"), [raw]);
        assert_eq!(q("type:selfie type:raw"), []);
        assert_eq!(q("type:nonsense"), []);
        assert_eq!(q("type:portrait"), [portrait]);
        assert_eq!(q("type:portraits"), [portrait]);
    }

    #[test]
    fn the_photo_type_rule_matches_and_checks_its_value() {
        let (c, [plain, selfie, pano, raw, portrait]) = library();
        let rule = |op: &str, value: &str| crate::RuleSet {
            mode: crate::Match::All,
            rules: vec![crate::Rule::Field { field: "photoType".into(), op: op.into(), value: value.into() }],
        };
        let q = |rs: crate::RuleSet| c.query(&crate::Filter { rule_set: Some(rs), ..Default::default() }, &crate::Sort::default());
        assert_eq!(q(rule("is", "panorama")), [pano]);
        assert_eq!(q(rule("is", "Selfie")), [selfie]);
        let mut not_selfie = q(rule("isNot", "selfie"));
        not_selfie.sort();
        let mut want = vec![plain, pano, raw, portrait];
        want.sort();
        assert_eq!(not_selfie, want);
        assert_eq!(q(rule("is", "portrait")), [portrait]);
        assert!(rule("is", "longExposure").check(&c).is_empty());
        assert!(!rule("is", "macro").check(&c).is_empty(), "an unknown photo type is a problem");
    }

    /// The rule's choices are written out for the field menu; they must stay the photo types.
    #[test]
    fn the_rule_choices_are_the_photo_types() {
        let Some(crate::rules::Kind::Choice(choices)) = crate::rules::field_kind("photoType") else {
            panic!("photoType is a choice field");
        };
        let from_types: Vec<(&str, &str)> = PhotoType::ALL.iter().map(|t| (t.id(), t.label())).collect();
        assert_eq!(choices.to_vec(), from_types);
    }

    #[test]
    fn ids_and_labels_parse_back() {
        for t in PhotoType::ALL {
            assert_eq!(PhotoType::parse(t.id()), Some(t));
            assert_eq!(PhotoType::parse(t.label()), Some(t));
        }
        assert_eq!(PhotoType::parse("selfies"), Some(PhotoType::Selfie));
        assert_eq!(PhotoType::parse("long-exposure"), Some(PhotoType::LongExposure));
        assert_eq!(PhotoType::parse("LONGEXPOSURES"), Some(PhotoType::LongExposure));
        for bad in ["", "s", "raw", "video", "selfiee"] {
            assert_eq!(PhotoType::parse(bad), None, "{bad}");
        }
    }
}
