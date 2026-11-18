//! Advanced Search (issue #846): a structured, serializable query over the catalog's fields,
//! organised in collapsible categories, lowered into the library's [`Filter`].
//!
//! An [`AdvancedSearch`] is the query the Advanced Search panel edits and the
//! `library.advancedSearch` command runs. [`AdvancedSearch::to_filter`] turns it into the one
//! [`Filter`] the grid already renders — the global words go to [`Filter::text`], the keyword,
//! people and album picks to their own filter fields, and every technical condition (dimensions,
//! camera, lens, exposure, location…) to a smart-album [`RuleSet`] of mode "all" — so the results
//! are an ordinary filtered grid view, and matching, keeping and follow reuse the filter
//! machinery wholesale.
//!
//! Categories the catalog cannot support yet stay declared in the query ([`MediaCategory`]'s
//! codec and frame rate): setting them is an actionable error from [`AdvancedSearch::to_filter`],
//! never a crash, and the panel shows their inputs disabled.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{AlbumId, Filter, MatchOp, RatingOp, Rule, RuleSet, rules::Match};

/// The file types a [`FileCategory`] can narrow to (the `kind` rule's choices).
pub const KINDS: &[&str] = &["image", "raw", "video"];

/// The aspect ratios a [`PropertiesCategory`] can narrow to (the `aspect` rule's choices).
pub const ASPECTS: &[&str] = &["landscape", "portrait", "square"];

/// The color labels a [`PropertiesCategory`] can narrow to, plus `"none"` (unlabeled photos),
/// in [`crate::ColorLabel::ALL`] order.
pub const LABELS: &[&str] = &["red", "yellow", "green", "blue", "purple", "none"];

/// A structured Advanced Search query: global words plus optional rule categories, all
/// constraints ANDed together. Every field defaults to "no constraint"; a query with none set
/// matches every photo, like an empty [`Filter`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct AdvancedSearch {
    /// Find items that have associated all these words: every whitespace-separated token must
    /// appear in the file name, title, caption, keywords, camera, lens, location or format (the
    /// free-text search, [`Filter::text`]).
    #[serde(default)]
    pub words: String,
    /// File, album and tags.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<FileCategory>,
    /// Item properties: dimensions, rating, color label, aspect.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<PropertiesCategory>,
    /// Caption and title text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<DescriptionCategory>,
    /// Photograph information: camera, lens, exposure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub photograph: Option<PhotographCategory>,
    /// Geographic position as the catalog reads it: the location fields and "has GPS".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<LocationCategory>,
    /// Audio/video properties where supported (duration; codec and frame rate are declared but
    /// unsupported).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media: Option<MediaCategory>,
    /// Face regions: a face count and named people.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub faces: Option<FacesCategory>,
}

/// An inclusive numeric range: both ends optional, `min` may not exceed `max`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Range {
    pub min: Option<f64>,
    pub max: Option<f64>,
}

impl Range {
    /// Whether the range constrains nothing.
    pub fn is_empty(&self) -> bool {
        self.min.is_none() && self.max.is_none()
    }
}

/// A shutter-speed range in camera notation (`1/250`, `2`) or plain seconds (`0.004`); both ends
/// optional.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct ShutterRange {
    pub min: Option<String>,
    pub max: Option<String>,
}

impl ShutterRange {
    fn is_empty(&self) -> bool {
        self.min.is_none() && self.max.is_none()
    }
}

/// File, album and tags: what a photo is and where it lives.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct FileCategory {
    /// One of [`KINDS`] (`image`, `raw`, `video`).
    pub kind: Option<String>,
    /// Part of the file name (case-insensitive).
    #[serde(default)]
    pub name: String,
    /// Part of the folder path (case-insensitive).
    #[serde(default)]
    pub folder: String,
    /// A collection by id: photos in it, not in its folders of albums. The id must name an album
    /// the library has — the caller checks ([`AdvancedSearch::to_filter`] stays catalog-free).
    pub album: Option<AlbumId>,
    /// Keywords; hierarchical keywords match their children too.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    /// How [`FileCategory::keywords`] combine: `any` (a photo with at least one) or `all`
    /// (a photo with every one).
    pub keyword_op: MatchOp,
}

impl FileCategory {
    fn is_empty(&self) -> bool {
        self.kind.is_none()
            && self.name.trim().is_empty()
            && self.folder.trim().is_empty()
            && self.album.is_none()
            && self.keywords.iter().all(|k| k.trim().is_empty())
    }
}

/// Item properties: dimensions and shape, rating, color label.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct PropertiesCategory {
    /// Width in pixels (the sensor's or the crop's, as the rules read it).
    pub width: Range,
    /// Height in pixels.
    pub height: Range,
    /// Megapixels.
    pub megapixels: Range,
    /// Stars, 0–5.
    pub rating: Option<u8>,
    /// How [`PropertiesCategory::rating`] compares.
    pub rating_op: RatingOp,
    /// One of [`LABELS`].
    pub label: Option<String>,
    /// One of [`ASPECTS`].
    pub aspect: Option<String>,
}

impl PropertiesCategory {
    fn is_empty(&self) -> bool {
        self.width.is_empty()
            && self.height.is_empty()
            && self.megapixels.is_empty()
            && self.rating.is_none()
            && self.rating_op == RatingOp::default()
            && self.label.is_none()
            && self.aspect.is_none()
    }
}

/// Caption and title text (case-insensitive substrings).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct DescriptionCategory {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub caption: String,
}

impl DescriptionCategory {
    fn is_empty(&self) -> bool {
        self.title.trim().is_empty() && self.caption.trim().is_empty()
    }
}

/// Photograph information: the camera, the lens and the exposure.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct PhotographCategory {
    /// Camera make or model, as a case-insensitive substring of the camera name.
    #[serde(default)]
    pub camera: String,
    /// Lens, as a case-insensitive substring.
    #[serde(default)]
    pub lens: String,
    /// Focal length, mm.
    pub focal_mm: Range,
    /// ISO speed.
    pub iso: Range,
    /// Aperture, f-number (`2.8` for f/2.8).
    pub aperture: Range,
    /// Shutter speed ([`ShutterRange`]).
    pub shutter: ShutterRange,
}

impl PhotographCategory {
    fn is_empty(&self) -> bool {
        self.camera.trim().is_empty()
            && self.lens.trim().is_empty()
            && self.focal_mm.is_empty()
            && self.iso.is_empty()
            && self.aperture.is_empty()
            && self.shutter.is_empty()
    }
}

/// Geographic position as the catalog reads it: the IPTC location fields and whether a photo
/// carries GPS coordinates. (Coordinate-radius search is not supported yet.)
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct LocationCategory {
    #[serde(default)]
    pub location: String,
    #[serde(default)]
    pub sublocation: String,
    #[serde(default)]
    pub city: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub country: String,
    /// `true`: photos with GPS coordinates, `false`: without.
    pub has_gps: Option<bool>,
}

impl LocationCategory {
    fn is_empty(&self) -> bool {
        self.location.trim().is_empty()
            && self.sublocation.trim().is_empty()
            && self.city.trim().is_empty()
            && self.state.trim().is_empty()
            && self.country.trim().is_empty()
            && self.has_gps.is_none()
    }
}

/// Audio/video properties. Only the duration is supported: a codec or frame-rate condition is an
/// "unsupported" error, and the panel keeps those inputs disabled.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct MediaCategory {
    /// Duration, seconds.
    pub duration_s: Range,
    /// Unsupported: the video codec (declared for forward compatibility).
    pub codec: Option<String>,
    /// Unsupported: the frame rate (declared for forward compatibility).
    pub frame_rate: Option<f64>,
}

impl MediaCategory {
    fn is_empty(&self) -> bool {
        self.duration_s.is_empty() && self.codec.is_none() && self.frame_rate.is_none()
    }
}

/// Face regions: how many detected faces a photo has, and who is named on them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct FacesCategory {
    /// A face-count condition: a count (`2`), a minimum (`3+`, `>=3`), a maximum (`<=2`), a range
    /// (`1-5`) or `none` / `any` — the forms [`crate::parse_face_spec`] reads.
    pub count: Option<String>,
    /// People (named face regions); hierarchical names compare whole, case-insensitively.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub people: Vec<String>,
    /// How [`FacesCategory::people`] combine.
    pub people_op: MatchOp,
}

impl FacesCategory {
    fn is_empty(&self) -> bool {
        self.count.as_deref().is_none_or(|c| c.trim().is_empty()) && self.people.iter().all(|p| p.trim().is_empty())
    }
}

impl AdvancedSearch {
    /// Whether the query constrains nothing at all.
    pub fn is_empty(&self) -> bool {
        self.words.trim().is_empty()
            && self.file.as_ref().is_none_or(FileCategory::is_empty)
            && self.properties.as_ref().is_none_or(PropertiesCategory::is_empty)
            && self.description.as_ref().is_none_or(DescriptionCategory::is_empty)
            && self.photograph.as_ref().is_none_or(PhotographCategory::is_empty)
            && self.location.as_ref().is_none_or(LocationCategory::is_empty)
            && self.media.as_ref().is_none_or(MediaCategory::is_empty)
            && self.faces.as_ref().is_none_or(FacesCategory::is_empty)
    }

    /// The query as the [`Filter`] the grid renders (see the module docs). A value that can't
    /// match anything — a non-finite or negative number, a rating above 5, an unknown label or
    /// aspect, a shutter speed that isn't a time, a face-count spec that can't be read, a range
    /// whose ends are reversed, or an unsupported audio/video condition — is an actionable error
    /// naming the field, never an empty grid or a panic.
    pub fn to_filter(&self) -> Result<Filter, String> {
        let mut rules: Vec<Value> = Vec::new();
        if let Some(f) = &self.file
            && !f.is_empty()
        {
            if let Some(kind) = f.kind.as_deref().map(str::trim).filter(|k| !k.is_empty()) {
                if !KINDS.contains(&kind) {
                    return Err(format!("file kind: unknown type {kind:?} (image, raw or video)"));
                }
                rules.push(json!({"field": "kind", "op": "is", "value": kind}));
            }
            if let Some(rule) = text_rule("fileName", &f.name) {
                rules.push(rule);
            }
            if let Some(rule) = text_rule("folder", &f.folder) {
                rules.push(rule);
            }
            if let Some(list) = trimmed(&f.keywords) {
                for k in list {
                    rules.push(json!({"field": "keywords", "op": "contains", "value": k}));
                }
            }
        }
        if let Some(p) = &self.properties
            && !p.is_empty()
        {
            range_rules(&mut rules, "width", &p.width)?;
            range_rules(&mut rules, "height", &p.height)?;
            range_rules(&mut rules, "megapixels", &p.megapixels)?;
            if let Some(rating) = p.rating {
                if rating > 5 {
                    return Err(format!("rating: {rating} is above 5 stars"));
                }
                let op = match p.rating_op {
                    RatingOp::AtLeast => "gte",
                    RatingOp::Exactly => "is",
                    RatingOp::AtMost => "lte",
                };
                rules.push(json!({"field": "rating", "op": op, "value": rating}));
            }
            if let Some(label) = p.label.as_deref().map(str::trim).filter(|l| !l.is_empty()) {
                if !LABELS.contains(&label) {
                    return Err(format!("color label: unknown label {label:?} (red, yellow, green, blue, purple or none)"));
                }
                rules.push(json!({"field": "label", "op": "is", "value": label}));
            }
            if let Some(aspect) = p.aspect.as_deref().map(str::trim).filter(|a| !a.is_empty()) {
                if !ASPECTS.contains(&aspect) {
                    return Err(format!("aspect: unknown ratio {aspect:?} (landscape, portrait or square)"));
                }
                rules.push(json!({"field": "aspect", "op": "is", "value": aspect}));
            }
        }
        if let Some(d) = &self.description
            && !d.is_empty()
        {
            if let Some(rule) = text_rule("title", &d.title) {
                rules.push(rule);
            }
            if let Some(rule) = text_rule("caption", &d.caption) {
                rules.push(rule);
            }
        }
        if let Some(p) = &self.photograph
            && !p.is_empty()
        {
            if let Some(rule) = text_rule("camera", &p.camera) {
                rules.push(rule);
            }
            if let Some(rule) = text_rule("lens", &p.lens) {
                rules.push(rule);
            }
            range_rules(&mut rules, "focalLength", &p.focal_mm)?;
            range_rules(&mut rules, "iso", &p.iso)?;
            range_rules(&mut rules, "aperture", &p.aperture)?;
            shutter_rules(&mut rules, &p.shutter)?;
        }
        if let Some(l) = &self.location
            && !l.is_empty()
        {
            for (field, value) in
                [("location", &l.location), ("sublocation", &l.sublocation), ("city", &l.city), ("state", &l.state), ("country", &l.country)]
            {
                if let Some(rule) = text_rule(field, value) {
                    rules.push(rule);
                }
            }
            if let Some(has) = l.has_gps {
                rules.push(json!({"field": "hasGps", "op": "is", "value": has}));
            }
        }
        if let Some(m) = &self.media {
            if m.codec.is_some() {
                return Err("audio/video codec: filtering by codec is not supported yet".into());
            }
            if m.frame_rate.is_some() {
                return Err("audio/video frame rate: filtering by frame rate is not supported yet".into());
            }
            if !m.duration_s.is_empty() {
                range_rules(&mut rules, "duration", &m.duration_s)?;
            }
        }
        let mut filter = Filter { text: self.words.trim().to_string(), ..Filter::default() };
        if let Some(f) = &self.file
            && !f.is_empty()
        {
            if let Some(album) = f.album {
                filter.album = Some(album);
            }
            if let Some(list) = trimmed(&f.keywords) {
                filter.keywords = list.iter().map(|k| k.to_string()).collect();
                filter.keyword_op = f.keyword_op;
            }
        }
        if let Some(f) = &self.faces {
            if let Some(spec) = f.count.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
                filter.faces = Some(spec.to_string());
                // checked here, so a spec nothing can read is an error, not an empty grid
                let _ = crate::query::parse_face_spec(spec).map_err(|e| format!("faces count: {e}"))?;
            }
            if let Some(list) = trimmed(&f.people) {
                filter.people = list.iter().map(|p| p.to_string()).collect();
                filter.person_op = f.people_op;
            }
        }
        if !rules.is_empty() {
            filter.rule_set = Some(RuleSet { mode: Match::All, rules: rules.into_iter().map(rule_of).collect() });
        }
        Ok(filter)
    }
}

/// A [`Rule::Field`] over a lowered `{"field", "op", "value"}` triple.
fn rule_of(v: Value) -> Rule {
    let o = v.as_object();
    let field = o.and_then(|o| o.get("field")).and_then(Value::as_str).unwrap_or("").to_string();
    let op = o.and_then(|o| o.get("op")).and_then(Value::as_str).unwrap_or("").to_string();
    let value = o.and_then(|o| o.get("value")).cloned().unwrap_or(Value::Null);
    Rule::Field { field, op, value }
}

/// A Text rule's value, or `None` when the field constrains nothing.
fn text_rule(field: &str, value: &str) -> Option<Value> {
    let v = value.trim();
    (!v.is_empty()).then(|| json!({"field": field, "op": "contains", "value": v}))
}

/// The keywords/people entries worth keeping: trimmed, empties dropped; `None` when none is left.
fn trimmed(list: &[String]) -> Option<Vec<&str>> {
    let v: Vec<&str> = list.iter().map(String::as_str).map(str::trim).filter(|s| !s.is_empty()).collect();
    (!v.is_empty()).then_some(v)
}

/// A hostile number: a whole, non-negative, finite value, or an error naming `field`.
fn non_negative(field: &str, v: f64) -> Result<f64, String> {
    if !v.is_finite() {
        return Err(format!("{field}: {v} is not a usable number"));
    }
    if v < 0.0 {
        return Err(format!("{field}: {v} is negative"));
    }
    Ok(v)
}

/// One rule per end of `range` over `field`, checking both ends.
fn range_rules(rules: &mut Vec<Value>, field: &str, range: &Range) -> Result<(), String> {
    let check = |v: Option<f64>| v.map(|v| non_negative(field, v)).transpose();
    let (min, max) = (check(range.min)?, check(range.max)?);
    if let (Some(min), Some(max)) = (min, max)
        && min > max
    {
        return Err(format!("{field}: the range {min} – {max} is reversed (the minimum is above the maximum)"));
    }
    match (min, max) {
        (Some(v), Some(w)) if v == w => rules.push(json!({"field": field, "op": "is", "value": v})),
        (Some(v), Some(w)) => rules.push(json!({"field": field, "op": "between", "value": [v, w]})),
        (Some(v), None) => rules.push(json!({"field": field, "op": "gte", "value": v})),
        (None, Some(v)) => rules.push(json!({"field": field, "op": "lte", "value": v})),
        (None, None) => {}
    }
    Ok(())
}

/// The shutter-speed range's rules, each end checked as a time (`1/250` or seconds).
fn shutter_rules(rules: &mut Vec<Value>, range: &ShutterRange) -> Result<(), String> {
    let read = |end: &str, v: &Option<String>| -> Result<Option<f64>, String> {
        match v.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            None => Ok(None),
            Some(s) => crate::parse_shutter_seconds(s)
                .filter(|secs| secs.is_finite() && *secs >= 0.0)
                .map(Some)
                .ok_or_else(|| format!("shutter speed {end}: {s:?} is not a time like 1/250 or 2")),
        }
    };
    let (min, max) = (read("minimum", &range.min)?, read("maximum", &range.max)?);
    if let (Some(min), Some(max)) = (min, max)
        && min > max
    {
        return Err(format!("shutter speed: {min} s – {max} s is reversed (the minimum is above the maximum)"));
    }
    match (range.min.as_deref().map(str::trim).filter(|s| !s.is_empty()), range.max.as_deref().map(str::trim).filter(|s| !s.is_empty())) {
        (Some(min), Some(max)) => rules.push(json!({"field": "shutterSpeed", "op": "between", "value": [min, max]})),
        (Some(min), None) => rules.push(json!({"field": "shutterSpeed", "op": "gte", "value": min})),
        (None, Some(max)) => rules.push(json!({"field": "shutterSpeed", "op": "lte", "value": max})),
        (None, None) => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Catalog, Op, Photo, PhotoId, Sort, Source};

    fn catalog() -> Catalog {
        let mut c = Catalog::new();
        let mut p = Photo::new(PhotoId(1), Source::Demo { scene: 1 }, "IMG_4001.jpg", "JPEG", 6000, 4000, "2026-05-01T10:00:00");
        p.rating = 4;
        p.meta.camera = "Canon EOS R6".into();
        p.meta.lens = "RF 85mm F1.2".into();
        p.meta.focal_mm = Some(85.0);
        p.meta.aperture = Some(1.8);
        p.meta.shutter = "1/250".into();
        p.meta.iso = Some(400);
        p.meta.title = "Harbour".into();
        p.meta.caption = "Morning at the pier".into();
        p.meta.keywords = vec!["travel".into(), "travel|italy".into()];
        p.meta.country = "Italy".into();
        p.meta.gps = Some((45.0, 9.0));
        c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        let mut v = Photo::new(PhotoId(2), Source::Demo { scene: 2 }, "clip.mp4", "MP4", 1920, 1080, "2026-06-01T10:00:00");
        v.kind = crate::MediaKind::Video;
        v.duration = Some(12.5);
        c.apply(Op::AddPhoto { photo: Box::new(v) }).unwrap();
        c
    }

    fn matches(c: &Catalog, q: &AdvancedSearch) -> Vec<PhotoId> {
        let f = q.to_filter().unwrap();
        let mut ids = c.query(&f, &Sort::default());
        ids.sort();
        ids
    }

    fn q() -> AdvancedSearch {
        AdvancedSearch::default()
    }

    #[test]
    fn an_empty_query_matches_everything() {
        let c = catalog();
        assert_eq!(matches(&c, &q()), vec![PhotoId(1), PhotoId(2)]);
        assert!(q().is_empty());
        assert_eq!(q().to_filter().unwrap(), Filter::default());
        // categories with nothing set add nothing
        let mut query = q();
        query.file = Some(FileCategory::default());
        query.photograph = Some(PhotographCategory::default());
        query.media = Some(MediaCategory::default());
        assert!(query.is_empty());
        assert_eq!(query.to_filter().unwrap(), Filter::default());
        // an unknown field is a refused query, not a silently ignored one
        assert!(serde_json::from_value::<AdvancedSearch>(serde_json::json!({"words": "x", "magic": true})).is_err());
    }

    #[test]
    fn global_words_match_across_fields() {
        let c = catalog();
        let mut query = q();
        query.words = "harbour italy".into();
        assert_eq!(matches(&c, &query), vec![PhotoId(1)], "title and keyword together");
        query.words = "harbour clip".into();
        assert!(matches(&c, &query).is_empty(), "words must all match one photo");
    }

    #[test]
    fn photograph_information_narrows_by_camera_lens_and_exposure() {
        let c = catalog();
        let mut query = q();
        query.photograph = Some(PhotographCategory { camera: "canon".into(), lens: "85mm".into(), ..Default::default() });
        assert_eq!(matches(&c, &query), vec![PhotoId(1)]);
        query.photograph = Some(PhotographCategory {
            camera: "canon".into(),
            focal_mm: Range { min: Some(50.0), max: Some(100.0) },
            aperture: Range { min: Some(1.8), max: None },
            iso: Range { min: Some(100.0), max: Some(800.0) },
            shutter: ShutterRange { min: Some("1/500".into()), max: None },
            lens: String::new(),
        });
        assert_eq!(matches(&c, &query), vec![PhotoId(1)], "f/1.8 ≥ 1.8, 1/250 s ≥ 1/500 s, ISO 400 in 100–800");
        query.photograph.as_mut().unwrap().shutter.min = Some("1/125".into());
        assert!(matches(&c, &query).is_empty(), "1/250 s is slower than 1/125 s, so it is below the minimum");
        query.photograph.as_mut().unwrap().lens = "nikon".into();
        assert!(matches(&c, &query).is_empty());
    }

    #[test]
    fn item_properties_narrow_by_size_rating_and_label() {
        let c = catalog();
        let mut query = q();
        query.properties = Some(PropertiesCategory {
            megapixels: Range { min: Some(20.0), max: None },
            width: Range { min: Some(1000.0), max: None },
            ..Default::default()
        });
        assert_eq!(matches(&c, &query), vec![PhotoId(1)]);
        query.properties.as_mut().unwrap().rating = Some(3);
        query.properties.as_mut().unwrap().rating_op = RatingOp::AtLeast;
        assert_eq!(matches(&c, &query), vec![PhotoId(1)]);
        query.properties.as_mut().unwrap().rating_op = RatingOp::AtMost;
        assert!(matches(&c, &query).is_empty(), "at most 3 stars excludes a 4-star photo");
        query.properties.as_mut().unwrap().label = Some("red".into());
        assert!(matches(&c, &query).is_empty(), "ANDed: no photo is both at most 3 stars and red");
    }

    #[test]
    fn file_category_narrows_by_type_name_and_keywords() {
        let c = catalog();
        let mut query = q();
        query.file = Some(FileCategory { kind: Some("video".into()), ..Default::default() });
        assert_eq!(matches(&c, &query), vec![PhotoId(2)]);
        query.file = Some(FileCategory { name: "img_".into(), keywords: vec!["travel".into()], keyword_op: MatchOp::Any, ..Default::default() });
        assert_eq!(matches(&c, &query), vec![PhotoId(1)], "hierarchical keywords match their children too");
        query.file.as_mut().unwrap().keyword_op = MatchOp::All;
        query.file.as_mut().unwrap().keywords.push("travel|italy".into());
        assert_eq!(matches(&c, &query), vec![PhotoId(1)]);
        query.file.as_mut().unwrap().keywords.push("ocean".into());
        assert!(matches(&c, &query).is_empty());
    }

    #[test]
    fn description_location_faces_and_media_narrow() {
        let c = catalog();
        let mut query = q();
        query.description = Some(DescriptionCategory { title: "harbour".into(), caption: "pier".into() });
        assert_eq!(matches(&c, &query), vec![PhotoId(1)]);
        query.location = Some(LocationCategory { country: "italy".into(), has_gps: Some(true), ..Default::default() });
        assert_eq!(matches(&c, &query), vec![PhotoId(1)]);
        // faces and media stand on their own: the description and location fields above are cleared
        query.description = None;
        query.location = None;
        query.faces = Some(FacesCategory { count: Some("none".into()), ..Default::default() });
        assert_eq!(matches(&c, &query), vec![PhotoId(1), PhotoId(2)], "no faces on either photo yet");
        query.faces.as_mut().unwrap().count = Some("1+".into());
        assert!(matches(&c, &query).is_empty());
        query.faces = None;
        query.media = Some(MediaCategory { duration_s: Range { min: Some(10.0), max: Some(20.0) }, ..Default::default() });
        assert_eq!(matches(&c, &query), vec![PhotoId(2)]);
    }

    #[test]
    fn a_reversed_or_impossible_value_is_an_error_naming_the_field() {
        let mut query = q();
        query.properties = Some(PropertiesCategory { width: Range { min: Some(100.0), max: Some(50.0) }, ..Default::default() });
        let e = query.to_filter().unwrap_err();
        assert!(e.contains("width"), "{e}");
        let mut query = q();
        query.properties = Some(PropertiesCategory { rating: Some(9), ..Default::default() });
        assert!(query.to_filter().unwrap_err().contains("rating"));
        let mut query = q();
        query.properties = Some(PropertiesCategory { label: Some("crimson".into()), ..Default::default() });
        assert!(query.to_filter().unwrap_err().contains("color label"));
        let mut query = q();
        query.photograph = Some(PhotographCategory { shutter: ShutterRange { min: Some("fast".into()), max: None }, ..Default::default() });
        assert!(query.to_filter().unwrap_err().contains("shutter speed"));
        let mut query = q();
        query.faces = Some(FacesCategory { count: Some("lots".into()), ..Default::default() });
        assert!(query.to_filter().unwrap_err().contains("faces count"));
        let mut query = q();
        query.file = Some(FileCategory { kind: Some("movie".into()), ..Default::default() });
        assert!(query.to_filter().unwrap_err().contains("kind"));
        let mut query = q();
        query.photograph = Some(PhotographCategory { focal_mm: Range { min: Some(-5.0), max: None }, ..Default::default() });
        assert!(query.to_filter().unwrap_err().contains("focalLength"));
        let mut query = q();
        query.photograph = Some(PhotographCategory { iso: Range { min: Some(f64::NAN), max: None }, ..Default::default() });
        assert!(query.to_filter().unwrap_err().contains("iso"));
    }

    #[test]
    fn unsupported_audio_video_conditions_are_rejected_actionably() {
        let mut query = q();
        query.media = Some(MediaCategory { codec: Some("h264".into()), ..Default::default() });
        assert!(query.to_filter().unwrap_err().contains("codec"));
        let mut query = q();
        query.media = Some(MediaCategory { frame_rate: Some(24.0), ..Default::default() });
        assert!(query.to_filter().unwrap_err().contains("frame rate"));
    }
}
