//! The Capture One import's messages as data, so each front end can say them in its own language.
//!
//! A [`Notice`] is an English message with `{name}` placeholders and the values that fill them.
//! The app looks the message up in its translation catalogs and fills it; [`Notice::text`] is the
//! English, which the import's `warnings` carry for the command line and agents. A value is shown
//! as it is (a count, a name from the catalog, a path, an error a reader gave) or is itself a
//! notice, translated too (a reason, a field's name).

use std::borrow::Cow;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Notice {
    /// One of [`MESSAGES`], or a LightCraft smart album field's name.
    pub message: Cow<'static, str>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub values: BTreeMap<String, Part>,
}

/// What fills a placeholder.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Part {
    Count(u64),
    /// Shown as it is.
    Text(String),
    /// Said in the reader's language too.
    Message(Notice),
    /// Several, in a list.
    Messages(Vec<Notice>),
}

impl Notice {
    pub(crate) fn new(message: &'static str) -> Self {
        Self { message: Cow::Borrowed(message), values: BTreeMap::new() }
    }

    pub(crate) fn count(mut self, name: &str, n: usize) -> Self {
        self.values.insert(name.into(), Part::Count(u64::try_from(n).unwrap_or(u64::MAX)));
        self
    }

    pub(crate) fn text_value(mut self, name: &str, text: impl Into<String>) -> Self {
        self.values.insert(name.into(), Part::Text(text.into()));
        self
    }

    pub(crate) fn notice(mut self, name: &str, notice: Notice) -> Self {
        self.values.insert(name.into(), Part::Message(notice));
        self
    }

    pub(crate) fn notices(mut self, name: &str, notices: Vec<Notice>) -> Self {
        self.values.insert(name.into(), Part::Messages(notices));
        self
    }

    /// A reason given as English text: one of [`MESSAGES`] when it is one, so it is translated,
    /// else shown as it is.
    pub(crate) fn reason(why: &str) -> Part {
        match MESSAGES.iter().find(|m| **m == why) {
            Some(message) => Part::Message(Self::new(message)),
            None => Part::Text(why.to_string()),
        }
    }

    /// The message in English.
    pub fn text(&self) -> String {
        fill(&self.message, |name| self.values.get(name).map(Part::text))
    }
}

impl Part {
    fn text(&self) -> String {
        match self {
            Self::Count(n) => n.to_string(),
            Self::Text(text) => text.clone(),
            Self::Message(notice) => notice.text(),
            Self::Messages(notices) => notices.iter().map(Notice::text).collect::<Vec<_>>().join(", "),
        }
    }
}

/// `template` with each `{name}` replaced by `value(name)`; a placeholder without a value stays.
/// One pass, so a value that itself holds braces (an album named "{n}") is not filled again.
pub fn fill(template: &str, value: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let (before, from) = rest.split_at(start);
        out.push_str(before);
        let filled = from.find('}').and_then(|end| {
            let name = from.get(1..end)?;
            Some((value(name)?, end))
        });
        match filled {
            Some((text, end)) => {
                out.push_str(&text);
                rest = from.get(end.saturating_add(1)..).unwrap_or("");
            }
            None => {
                out.push('{');
                rest = from.get(1..).unwrap_or("");
            }
        }
    }
    out.push_str(rest);
    out
}

// ---- the catalog
pub(crate) const IN_USE: &str = "Capture One has this catalog open; changes it has not saved yet are not included";
pub(crate) const TABLE_LEFT_OUT: &str = "a table of the catalog can't be read and was left out: {why}";
pub(crate) const KEYWORD_LOOPS: &str = "{n} keyword(s) are nested in a loop and were left out of the keyword list";
pub(crate) const STRAY_VARIANTS: &str = "{n} variant(s) belong to no image in the catalog and were left out";
pub(crate) const UNKNOWN_TAGS: &str = "{n} photo(s) have a colour tag this version doesn't know; imported without one";
pub(crate) const KEYWORDS_CUT: &str =
    "{n} photo(s) have more than {max} keywords, or keywords nested deeper than {depth} levels; those were left out";
pub(crate) const COLLECTION_ORPHANS: &str = "{n} collection(s) are inside a collection the catalog no longer has; they come in at the top level";
pub(crate) const COLLECTION_LOOPS: &str = "{n} collection(s) are nested in a loop and were left out";
// ---- the photos
pub(crate) const TRASHED: &str = "{n} image(s) in Capture One's trash were left out";
pub(crate) const KEPT_COLOUR: &str = "{n} photo(s) have Capture One's orange or pink colour tag, which has no label here yet: the colour is kept with the photo, and the keyword “{keyword}” finds them";
pub(crate) const KEPT_COLOURS: &str = "{n} photo(s) have Capture One's orange or pink colour tag, which has no label here yet: the colour is kept with the photo, and the keywords “{first}” and “{second}” find them";
pub(crate) const ADJUSTED: &str =
    "{n} photo(s) have Capture One adjustments other than crop, rotation, flip and lens corrections; those are not imported yet";
// ---- crop, rotation, perspective and lens settings
pub(crate) const GEOMETRY_LEFT_OUT: &str = "{n} photo(s) use {setting} in Capture One, which isn't imported";
pub(crate) const NO_LENS_PROFILE: &str =
    "{n} photo(s) were lens-corrected in Capture One but have no lens profile in LightCraft yet; the correction applies once LightCraft finds one";
pub(crate) const PERSPECTIVE: &str = "a perspective skew";
pub(crate) const TILT_WITH_CROP: &str = "a perspective tilt together with a crop";
pub(crate) const CROP_OUTSIDE: &str = "a crop reaching past the photo";
pub(crate) const GENERIC_DISTORTION: &str = "distortion correction with the generic lens profile";
pub(crate) const PROFILE_BY_HAND: &str = "a lens profile chosen by hand";
pub(crate) const DISTORTED_AREAS: &str = "distorted areas shown";
pub(crate) const SHARPNESS_FALLOFF: &str = "sharpness falloff";
pub(crate) const DIFFRACTION: &str = "diffraction correction";
pub(crate) const LIGHT_FALLOFF: &str = "light falloff";
pub(crate) const PURPLE_FRINGING: &str = "purple fringing";
pub(crate) const LENS_MOVEMENT: &str = "shift lens movement";
pub(crate) const LCC: &str = "an LCC (lens cast calibration)";
pub(crate) const NOT_OPENED: &str = "image {image}: {path} is not opened: {why}; listed as missing and not added";
pub(crate) const MORE_NOT_OPENED: &str = "{n} more image(s) are not opened; listed as missing and not added";
// ---- why an original is not opened
pub(crate) const NO_FOLDER: &str = "the catalog has no folder for it";
pub(crate) const BAD_FILE_NAME: &str = "its file name is empty or an absolute path";
pub(crate) const NO_FOLDER_RECORDED: &str = "the catalog records no folder for it";
pub(crate) const ABSOLUTE_FOLDER: &str = "its folder is an absolute path where one relative to the catalog is expected";
// the reasons the path checks give (`craft_docpath`, `doc_path`), so they are translated too
const OTHER_COMPUTER: &str = "the path is on another computer";
const DEVICE: &str = "the path names a device, not a file";
const NOT_A_FILE_PATH: &str = "the link is not a file path";
const INVALID_PATH: &str = "the path is not valid";
const NOT_HERE: &str = "the path is not a path on this computer";
// ---- smart albums
pub(crate) const SMART_LEFT_OUT: &str = "smart album “{name}”: not imported; {why}";
pub(crate) const SMART_AS_ALBUM: &str =
    "smart album “{name}”: imported as an album with the {n} photo(s) its rule selects now; LightCraft has no smart album rule for {fields}";
pub(crate) const MORE_SMART: &str = "{n} more smart album(s) were not imported as smart albums";
pub(crate) const RULE_TOO_LONG: &str = "its rule is longer than {kb} KB";
pub(crate) const RULE_UNREADABLE: &str = "its rule can't be read ({error})";
pub(crate) const RULE_FORM: &str = "its rule has a form this version doesn't know";
pub(crate) const RULE_JOIN: &str = "its rule joins conditions in a way this version doesn't know";
pub(crate) const RULE_NO_OPERATOR: &str = "a condition of its rule has no operator";
pub(crate) const RULE_ALL_OFF: &str = "its rule has no condition switched on";
pub(crate) const RULE_NOT_READ: &str = "its rule tests {fields}, which the catalog import can't read";
// ---- the fields of a smart album rule LightCraft has none for (the others go by LightCraft's names)
pub(crate) const FIELD_KEY: &str = "“{key}”";
pub(crate) const KEYWORDS_BY_CASE: &str = "Keywords told apart by upper and lower case";
pub(crate) const ORIENTATION_BY_HAND: &str = "Orientation of photos turned by hand";
pub(crate) const FILE_SIZE: &str = "File Size";
pub(crate) const WIDTH: &str = "Width";
pub(crate) const HEIGHT: &str = "Height";
pub(crate) const ADJUSTED_FIELD: &str = "Adjusted";
pub(crate) const ANNOTATION: &str = "Annotation";
pub(crate) const LIVE_COMMENTS: &str = "Live Comments";
pub(crate) const HEADLINE: &str = "Headline";
pub(crate) const USAGE_TERMS: &str = "Usage Terms";
pub(crate) const ISO_COUNTRY_CODE: &str = "ISO Country Code";
pub(crate) const FLASH_MODE: &str = "Flash Mode";
pub(crate) const FLASH_COMPENSATION: &str = "Flash Compensation";
pub(crate) const CAMERA_OWNER: &str = "Camera Owner";

/// Every message of the Capture One import (with [`smart_field_labels`], the LightCraft field
/// names it uses): each needs a translation in every language the app has.
pub const MESSAGES: &[&str] = &[
    IN_USE,
    TABLE_LEFT_OUT,
    KEYWORD_LOOPS,
    STRAY_VARIANTS,
    UNKNOWN_TAGS,
    KEYWORDS_CUT,
    COLLECTION_ORPHANS,
    COLLECTION_LOOPS,
    TRASHED,
    KEPT_COLOUR,
    KEPT_COLOURS,
    ADJUSTED,
    GEOMETRY_LEFT_OUT,
    NO_LENS_PROFILE,
    PERSPECTIVE,
    TILT_WITH_CROP,
    CROP_OUTSIDE,
    GENERIC_DISTORTION,
    PROFILE_BY_HAND,
    DISTORTED_AREAS,
    SHARPNESS_FALLOFF,
    DIFFRACTION,
    LIGHT_FALLOFF,
    PURPLE_FRINGING,
    LENS_MOVEMENT,
    LCC,
    NOT_OPENED,
    MORE_NOT_OPENED,
    NO_FOLDER,
    BAD_FILE_NAME,
    NO_FOLDER_RECORDED,
    ABSOLUTE_FOLDER,
    OTHER_COMPUTER,
    DEVICE,
    NOT_A_FILE_PATH,
    INVALID_PATH,
    NOT_HERE,
    SMART_LEFT_OUT,
    SMART_AS_ALBUM,
    MORE_SMART,
    RULE_TOO_LONG,
    RULE_UNREADABLE,
    RULE_FORM,
    RULE_JOIN,
    RULE_NO_OPERATOR,
    RULE_ALL_OFF,
    RULE_NOT_READ,
    FIELD_KEY,
    KEYWORDS_BY_CASE,
    ORIENTATION_BY_HAND,
    FILE_SIZE,
    WIDTH,
    HEIGHT,
    ADJUSTED_FIELD,
    ANNOTATION,
    LIVE_COMMENTS,
    HEADLINE,
    USAGE_TERMS,
    ISO_COUNTRY_CODE,
    FLASH_MODE,
    FLASH_COMPENSATION,
    CAMERA_OWNER,
];

/// The LightCraft smart album field names a Capture One rule's condition can be named by.
pub fn smart_field_labels() -> Vec<&'static str> {
    super::smart::lightcraft_labels()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_message_is_listed_once() {
        let source = include_str!("notice.rs");
        let declared = source
            .lines()
            .map(str::trim_start)
            .filter(|line| (line.starts_with("const ") || line.starts_with("pub(crate) const ")) && line.contains(": &str"))
            .count();
        assert_eq!(declared, MESSAGES.len(), "a message constant is missing from MESSAGES");
        let mut seen = std::collections::BTreeSet::new();
        assert!(MESSAGES.iter().all(|m| seen.insert(*m)), "a message is listed twice");
    }

    #[test]
    fn the_path_reasons_are_the_ones_the_checks_give() {
        use crate::doc_path::{NotFollowed, Refused};
        for (reason, text) in [
            (NotFollowed::Refused(Refused::Network), OTHER_COMPUTER),
            (NotFollowed::Refused(Refused::Device), DEVICE),
            (NotFollowed::Refused(Refused::Url), NOT_A_FILE_PATH),
            (NotFollowed::Refused(Refused::Invalid), INVALID_PATH),
            (NotFollowed::NotHere, NOT_HERE),
        ] {
            assert_eq!(reason.to_string(), text);
        }
    }

    #[test]
    fn a_notice_fills_its_message_once() {
        let notice = Notice::new(SMART_AS_ALBUM)
            .text_value("name", "{n} and “{fields}”")
            .count("n", 2)
            .notices("fields", vec![Notice::new(FILE_SIZE), Notice::new(FIELD_KEY).text_value("key", "OWNER")]);
        assert_eq!(
            notice.text(),
            "smart album “{n} and “{fields}””: imported as an album with the 2 photo(s) its rule selects now; LightCraft has no smart album rule for File Size, “OWNER”"
        );
        // as data, and back
        let json = serde_json::to_value(&notice).unwrap();
        assert_eq!(json["values"]["n"], 2);
        assert_eq!(json["values"]["fields"][1]["values"]["key"], "OWNER");
        assert_eq!(serde_json::from_value::<Notice>(json).unwrap(), notice);
        // a placeholder without a value, and stray braces, stay as written
        assert_eq!(fill("{a} {b} {", |name| (name == "a").then(|| "A".to_string())), "A {b} {");
        assert_eq!(Notice::reason(NO_FOLDER), Part::Message(Notice::new(NO_FOLDER)));
        assert_eq!(Notice::reason("other"), Part::Text("other".into()));
    }
}
