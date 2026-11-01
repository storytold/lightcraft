use super::*;
use crate::{ColorLabel, Flag, PhotoId, Source};
use serde_json::json;

fn photo(id: u64) -> Photo {
    let mut p = Photo::new(PhotoId(id), Source::Demo { scene: 0 }, "IMG_0042.CR2", "CR2", 6000, 4000, "2026-09-20T10:00:00");
    p.rating = 4;
    p.flag = Flag::Pick;
    p.label = Some(ColorLabel::Red);
    p.captured = Some("2026-08-14T18:30:00".into());
    p.meta.keywords = vec!["travel|italy|rome".into(), "food".into()];
    p.meta.camera = "Model X2".into();
    p.meta.iso = Some(1600);
    p.meta.aperture = Some(2.8);
    p
}

fn rs(v: serde_json::Value) -> RuleSet {
    serde_json::from_value(v).unwrap()
}

#[test]
fn f32_boundary_values_match_gte_lte() {
    let cat = Catalog::new();
    let matches = |p: &Photo, field: &str, op: &str, value: f64| rs(json!({"rules": [{"field": field, "op": op, "value": value}]})).matches(p, &cat);
    for v in [1.4f32, 1.8, 2.8, 5.6, 0.95] {
        let mut p = photo(1);
        p.meta.aperture = Some(v);
        p.meta.focal_mm = Some(v);
        let w = v.to_string().parse::<f64>().unwrap();
        for field in ["aperture", "focalLength"] {
            assert!(matches(&p, field, "gte", w), "{field} gte {w}");
            assert!(matches(&p, field, "lte", w), "{field} lte {w}");
            assert!(!matches(&p, field, "gt", w), "{field} gt {w}");
            assert!(!matches(&p, field, "lt", w), "{field} lt {w}");
        }
    }
}

#[test]
fn fields_ops_and_groups() {
    let cat = Catalog::new();
    let p = photo(1);
    let yes = |v: serde_json::Value| assert!(rs(v.clone()).matches(&p, &cat), "{v}");
    let no = |v: serde_json::Value| assert!(!rs(v.clone()).matches(&p, &cat), "{v}");
    yes(json!({"rules": [{"field": "rating", "op": "gte", "value": 3}, {"field": "flag", "op": "is", "value": "pick"}]}));
    no(json!({"rules": [{"field": "rating", "op": "gt", "value": 4}]}));
    yes(json!({"match": "any", "rules": [{"field": "rating", "op": "is", "value": 1}, {"field": "label", "op": "is", "value": "red"}]}));
    no(json!({"match": "none", "rules": [{"field": "label", "op": "is", "value": "red"}]}));
    yes(json!({"rules": [{"field": "keywords", "op": "is", "value": "italy"}]}));
    yes(json!({"rules": [{"field": "keywords", "op": "contains", "value": "ROME"}, {"field": "keywords", "op": "notContains", "value": "beach"}]}));
    no(json!({"rules": [{"field": "keywords", "op": "isEmpty"}]}));
    yes(json!({"rules": [{"field": "fileName", "op": "startsWith", "value": "img_"}, {"field": "fileName", "op": "endsWith", "value": ".cr2"}]}));
    // file path: the whole string (spaces included), either separator, case-insensitive
    let mut fp = photo(3);
    fp.source = Source::File { path: "D:\\Photos\\Lake Como Wedding-Day\\2026\\IMG_0042.CR2".into() };
    let fpm = |v: serde_json::Value| rs(v).matches(&fp, &cat);
    assert!(fpm(json!({"rules": [{"field": "filePath", "op": "contains", "value": "/Lake Como Wedding-Day/"}]})));
    assert!(!fpm(json!({"rules": [{"field": "filePath", "op": "contains", "value": "/Como Lake/"}]})));
    assert!(!fpm(json!({"rules": [{"field": "filePath", "op": "contains", "value": "Wedding Day"}]})));
    assert!(fpm(json!({"rules": [{"field": "filePath", "op": "notContains", "value": "/Other/"}]})));
    assert!(fpm(json!({"rules": [{"field": "filePath", "op": "startsWith", "value": "d:/photos/"}]})));
    no(json!({"rules": [{"field": "filePath", "op": "contains", "value": "/Lake/"}]})); // demo photo has no path
    yes(json!({"rules": [{"field": "filePath", "op": "isEmpty"}]}));
    yes(json!({"rules": [{"field": "camera", "op": "contains", "value": "x2"}, {"field": "title", "op": "isEmpty"}]}));
    yes(json!({"rules": [{"field": "iso", "op": "between", "value": [800, 3200]}, {"field": "aperture", "op": "lte", "value": "f/4"}]}));
    yes(json!({"rules": [{"field": "megapixels", "op": "gte", "value": 24}]}));
    yes(
        json!({"rules": [{"field": "captureDate", "op": "is", "value": "2026-08"}, {"field": "captureDate", "op": "before", "value": "2026-09-01"}]}),
    );
    yes(json!({"rules": [{"field": "captureDate", "op": "between", "value": ["2026-08-01", "2026-08"]}]}));
    no(json!({"rules": [{"field": "captureDate", "op": "after", "value": "2026-08"}]}));
    yes(json!({"rules": [{"field": "kind", "op": "isNot", "value": "video"}, {"field": "edited", "op": "is", "value": false}]}));
    // copyright status (unknown until set)
    yes(json!({"rules": [{"field": "copyrightStatus", "op": "is", "value": "unknown"}]}));
    no(json!({"rules": [{"field": "copyrightStatus", "op": "is", "value": "copyrighted"}]}));
    let mut pd = photo(2);
    pd.meta.copyright_status = crate::CopyrightStatus::PublicDomain;
    assert!(rs(json!({"rules": [{"field": "copyrightStatus", "op": "is", "value": "publicDomain"}]})).matches(&pd, &cat));
    assert!(rs(json!({"rules": [{"field": "copyrightStatus", "op": "isNot", "value": "copyrighted"}]})).matches(&pd, &cat));
    // nested: rating ≥ 4 and (label is blue or keywords contain food)
    yes(json!({"rules": [{"field": "rating", "op": "gte", "value": 4}, {"group": {"match": "any", "rules": [
        {"field": "label", "op": "is", "value": "blue"}, {"field": "keywords", "op": "contains", "value": "food"}]}}]}));
    // in the last N days, relative to now
    set_now(Some("2026-09-01T00:00:00".into()));
    yes(json!({"rules": [{"field": "captureDate", "op": "inLast", "value": {"n": 30, "unit": "days"}}]}));
    no(json!({"rules": [{"field": "captureDate", "op": "inLast", "value": {"n": 1, "unit": "weeks"}}]}));
    yes(json!({"rules": [{"field": "captureDate", "op": "notInLast", "value": 7}]}));
    set_now(None);
    // an empty rule list: all → everything, any → nothing
    yes(json!({"rules": []}));
    no(json!({"match": "any", "rules": []}));
}

#[test]
fn problems_and_description() {
    let r = rs(json!({"rules": [{"field": "rating", "op": "contains", "value": 1}, {"group": {"rules": [{"field": "nope", "op": "is"}]}}]}));
    let messages: Vec<String> = r.check(&Catalog::new()).into_iter().map(|p| p.message).collect();
    assert_eq!(messages, vec!["`rating` has no operator `contains`".to_string(), "unknown field `nope`".to_string()]);
    let r = rs(
        json!({"match": "any", "rules": [{"field": "rating", "op": "gte", "value": 3}, {"field": "captureDate", "op": "inLast", "value": {"n": 2, "unit": "weeks"}}]}),
    );
    assert_eq!(r.describe(), "rating is ≥ 3 or capture date is in the last 2 weeks");
    // every field has a kind with operators
    for (f, _, k) in FIELDS {
        assert!(!ops_for(*k).is_empty(), "{f}");
    }
}

/// Keyword Count: a photo with 2 keywords is in "≥ 2", "≤ 2" and "is 2"; a hierarchical path
/// (travel|italy|rome) is one keyword, and the same keyword twice in different case is one.
#[test]
fn keyword_count_rules() {
    let cat = Catalog::new();
    let mut p = photo(1); // travel|italy|rome, food
    let m =
        |p: &Photo, op: &str, value: serde_json::Value| rs(json!({"rules": [{"field": "keywordCount", "op": op, "value": value}]})).matches(p, &cat);
    assert!(m(&p, "gte", json!(2)) && m(&p, "lte", json!(2)) && m(&p, "is", json!(2)));
    assert!(!m(&p, "gte", json!(3)) && !m(&p, "lt", json!(2)) && m(&p, "between", json!([1, 2])));
    p.meta.keywords.push("Food".into());
    assert!(m(&p, "is", json!(2)), "Food and food are one keyword");
    p.meta.keywords.clear();
    assert!(m(&p, "is", json!(0)) && m(&p, "lte", json!(2)));
    assert_eq!(field_group("keywordCount"), Some("Keywords & People"));
}

/// People: the names on face regions. "People contains ana" finds Ana Lima; pets and unnamed
/// faces are not people; People Count counts each person once.
#[test]
fn people_rules() {
    use lightcraft_meta::{Rect, Region, RegionKind};
    let cat = Catalog::new();
    let region = |name: Option<&str>, kind: RegionKind| Region {
        rect: Rect { x0: 0.4, y0: 0.4, x1: 0.6, y1: 0.6 },
        kind,
        name: name.map(str::to_string),
        description: None,
    };
    let mut p = photo(1);
    let m = |p: &Photo, field: &str, op: &str, value: serde_json::Value| {
        rs(json!({"rules": [{"field": field, "op": op, "value": value}]})).matches(p, &cat)
    };
    assert!(m(&p, "person", "isEmpty", json!(null)) && m(&p, "personCount", "is", json!(0)));
    p.meta.regions = vec![
        region(Some("Ana Lima"), RegionKind::Face),
        region(Some(" ana lima "), RegionKind::Face),
        region(Some("Rex"), RegionKind::Pet),
        region(None, RegionKind::Face),
        region(Some("Bo"), RegionKind::Face),
    ];
    assert!(m(&p, "person", "contains", json!("ana")));
    assert!(m(&p, "person", "is", json!("ANA LIMA")));
    assert!(!m(&p, "person", "is", json!("rex")), "a pet isn't a person");
    assert!(m(&p, "person", "notContains", json!("rex")));
    assert!(m(&p, "person", "isNotEmpty", json!(null)));
    assert!(m(&p, "personCount", "is", json!(2)), "Ana Lima once, Bo; not the pet or the unnamed face");
    assert_eq!(field_group("person"), Some("Keywords & People"));
}

/// Shutter Speed compares exposure times in seconds, written as the camera shows them: a
/// photo at 1/250 is in "≤ 1/60" (faster) and not in "≥ 1/60"; "between 1/1000 and 1/125".
#[test]
fn shutter_speed_rules() {
    let cat = Catalog::new();
    let mut p = photo(1);
    let m =
        |p: &Photo, op: &str, value: serde_json::Value| rs(json!({"rules": [{"field": "shutterSpeed", "op": op, "value": value}]})).matches(p, &cat);
    assert!(!m(&p, "gte", json!(0)), "no shutter speed recorded");
    p.meta.shutter = "1/250".into();
    assert!(m(&p, "is", json!("1/250")) && m(&p, "lte", json!("1/60")) && !m(&p, "gte", json!("1/60")));
    assert!(m(&p, "between", json!(["1/1000", "1/125"])) && m(&p, "lt", json!(0.01)));
    assert!(!m(&p, "is", json!("1/0")) && !m(&p, "is", json!("fast")), "a value that isn't a time matches nothing");
    p.meta.shutter = "2\"".into();
    assert!(m(&p, "gte", json!("1")) && m(&p, "is", json!(2)));
    assert_eq!(field_group("shutterSpeed"), Some("Camera Info"));
}

#[test]
fn shutter_seconds_parses_camera_notation() {
    for (s, want) in [("1/250", Some(0.004)), (" 1/250 s", Some(0.004)), ("0.5", Some(0.5)), ("2\"", Some(2.0)), ("30s", Some(30.0))] {
        assert_eq!(crate::parse_shutter_seconds(s), want, "{s}");
    }
    for s in ["", "1/0", "0", "-1/250", "fast", "inf", "NaN", "1/x"] {
        assert_eq!(crate::parse_shutter_seconds(s), None, "{s}");
    }
}

/// File Extension ignores case and a leading dot; Copy Name, Alt Text, City, State /
/// Province and Country are text fields of their own (Location still matches all of them).
#[test]
fn file_source_description_and_location_rules() {
    let cat = Catalog::new();
    let mut p = photo(1); // IMG_0042.CR2
    let m = |p: &Photo, field: &str, op: &str, value: serde_json::Value| {
        rs(json!({"rules": [{"field": field, "op": op, "value": value}]})).matches(p, &cat)
    };
    assert!(m(&p, "extension", "is", json!("cr2")) && m(&p, "extension", "is", json!(".CR2")));
    assert!(!m(&p, "extension", "is", json!("jpg")));
    p.file_name = "README".into();
    assert!(m(&p, "extension", "isEmpty", json!(null)), "no dot, no extension");
    p.file_name = "archive.tar.gz".into();
    assert!(m(&p, "extension", "is", json!("gz")));
    p.file_name = ".hidden".into();
    assert!(m(&p, "extension", "isEmpty", json!(null)), "a dot file has no extension");
    assert!(m(&p, "copyName", "isEmpty", json!(null)));
    p.copy_name = Some("Black and white".into());
    assert!(m(&p, "copyName", "contains", json!("black")));
    p.meta.alt_text = "A red kite over a hill".into();
    assert!(m(&p, "altText", "contains", json!("kite")));
    p.meta.city = "Porto".into();
    p.meta.state = "Porto District".into();
    p.meta.country = "Portugal".into();
    assert!(m(&p, "city", "is", json!("porto")) && m(&p, "state", "startsWith", json!("porto")) && m(&p, "country", "is", json!("portugal")));
    assert!(!m(&p, "city", "is", json!("portugal")), "City tests only the city");
    assert!(m(&p, "location", "contains", json!("portugal")), "Location still covers the country");
}

/// In a Stack, Video Duration (seconds; photos have none), Cropped and Treatment.
#[test]
fn source_file_and_develop_rules() {
    use crate::{Op, Stack, StackId};
    let mut cat = Catalog::new();
    let a = photo(1);
    let b = photo(2);
    let c = photo(3);
    for p in [&a, &b, &c] {
        cat.apply(Op::AddPhoto { photo: Box::new(p.clone()) }).unwrap();
    }
    cat.apply(Op::AddStack { stack: Stack { id: StackId(1), photos: vec![a.id, b.id], collapsed: false } }).unwrap();
    let m = |p: &Photo, field: &str, op: &str, value: serde_json::Value| {
        rs(json!({"rules": [{"field": field, "op": op, "value": value}]})).matches(p, &cat)
    };
    assert!(m(&a, "stacked", "is", json!(true)) && m(&b, "stacked", "is", json!(true)));
    assert!(m(&c, "stacked", "is", json!(false)));
    let mut v = photo(4);
    assert!(!m(&v, "duration", "gte", json!(0)), "a photo has no duration");
    v.duration = Some(95.0);
    assert!(m(&v, "duration", "gt", json!(60)) && m(&v, "duration", "between", json!([90, 120])));
    let mut d = photo(5);
    assert!(m(&d, "cropped", "is", json!(false)) && m(&d, "treatment", "is", json!("color")));
    let mut s = (*d.develop).clone();
    s.crop.geometry.rect = lightcraft_geom::Rect::new(0.1, 0.0, 0.9, 1.0);
    s.treatment = lightcraft_develop::Treatment::Bw;
    d.develop = std::sync::Arc::new(s);
    assert!(m(&d, "cropped", "is", json!(true)) && m(&d, "treatment", "is", json!("monochrome")));
    assert!(m(&d, "treatment", "isNot", json!("color")));
    let mut s = (*d.develop).clone();
    s.crop = Default::default();
    s.crop.geometry.angle = 2.0;
    d.develop = std::sync::Arc::new(s);
    assert!(m(&d, "cropped", "is", json!(true)), "straightening counts as a crop");
}

/// Size follows the photo as shown: rotated a quarter turn a landscape frame is a portrait, and
/// a crop changes its edges and its megapixels. Long / Short Edge are in pixels.
#[test]
fn size_rules_follow_orientation_and_crop() {
    let cat = Catalog::new();
    let mut p = photo(1); // 6000 × 4000
    let m = |p: &Photo, field: &str, op: &str, value: serde_json::Value| {
        rs(json!({"rules": [{"field": field, "op": op, "value": value}]})).matches(p, &cat)
    };
    assert!(m(&p, "aspect", "is", json!("landscape")) && m(&p, "longEdge", "is", json!(6000)) && m(&p, "shortEdge", "is", json!(4000)));
    assert!(m(&p, "megapixels", "is", json!(24)));
    let mut s = (*p.develop).clone();
    s.orientation = lightcraft_geom::Orientation::Rotate90;
    p.develop = std::sync::Arc::new(s.clone());
    assert!(m(&p, "aspect", "is", json!("portrait")) && m(&p, "longEdge", "is", json!(6000)));
    // a square crop of the rotated frame: 4000 wide, 4000 of its 6000 tall
    s.crop.geometry.rect = lightcraft_geom::Rect::new(0.0, 1.0 / 6.0, 1.0, 5.0 / 6.0);
    p.develop = std::sync::Arc::new(s);
    assert!(m(&p, "aspect", "is", json!("square")) && m(&p, "longEdge", "is", json!(4000)));
    assert!(m(&p, "megapixels", "is", json!(16)), "a 4000 × 4000 crop is 16 MP, not the sensor's 24");
    assert!(m(&p, "megapixels", "lt", json!(20)) && !m(&p, "megapixels", "gte", json!(24)));
    assert!(m(&p, "aspect", "isNot", json!("portrait")));
    let empty = Photo::new(PhotoId(9), Source::Demo { scene: 0 }, "x.jpg", "JPEG", 0, 0, "2026-09-20T10:00:00");
    assert!(!m(&empty, "aspect", "is", json!("landscape")) && !m(&empty, "aspect", "is", json!("square")) && m(&empty, "longEdge", "is", json!(0)));
    assert!(m(&empty, "megapixels", "is", json!(0)));
}

/// Any Searchable Text also finds the state / province, the alt text, a person on a face and
/// a virtual copy's name, as it already finds the city and country.
#[test]
fn any_searchable_text_covers_new_fields() {
    let cat = Catalog::new();
    let mut p = photo(1);
    let m = |p: &Photo, value: &str| rs(json!({"rules": [{"field": "text", "op": "contains", "value": value}]})).matches(p, &cat);
    for word in ["oregon", "kite", "ana", "bluish"] {
        assert!(!m(&p, word), "{word}");
    }
    p.meta.state = "Oregon".into();
    p.meta.alt_text = "A kite".into();
    p.meta.regions = vec![lightcraft_meta::Region {
        rect: lightcraft_meta::Rect { x0: 0.4, y0: 0.4, x1: 0.6, y1: 0.6 },
        kind: lightcraft_meta::RegionKind::Face,
        name: Some("Ana".into()),
        description: None,
    }];
    p.copy_name = Some("Bluish".into());
    for word in ["oregon", "kite", "ana", "bluish"] {
        assert!(m(&p, word), "{word}");
    }
}

/// Choice values have readable labels: the editor and summaries show "Public Domain" and
/// "Black & White", never ids like `publicDomain`; rules still store and accept the ids.
#[test]
fn choices_have_readable_labels() {
    assert_eq!(choice_label("copyrightStatus", "publicDomain"), Some("Public Domain"));
    assert_eq!(choice_label("treatment", "monochrome"), Some("Black & White"));
    // own labels where a shared word would translate wrongly: "Color" is the colour noun (the
    // Color panel) and "None" agrees with other nouns in gendered languages
    assert_eq!(choice_label("treatment", "color"), Some("In Color"));
    assert_eq!(choice_label("label", "none"), Some("No Label"));
    assert_eq!(choice_label("flag", "pick"), Some("Picked"));
    assert_eq!(choice_label("aspect", "landscape"), Some("Landscape (wide)"));
    assert_eq!(choice_label("flag", "nope"), None);
    assert_eq!(choice_label("rating", "pick"), None, "not a choice field");
    for (field, _, kind) in FIELDS {
        let Kind::Choice(choices) = kind else { continue };
        for (id, label) in *choices {
            assert!(!label.is_empty() && label != id, "{field}: {id} has no label of its own");
            assert!(label.starts_with(|c: char| c.is_uppercase()), "{field}: {label}");
        }
    }
    let r = rs(
        json!({"rules": [{"field": "copyrightStatus", "op": "is", "value": "publicDomain"}, {"field": "flag", "op": "isNot", "value": "reject"}]}),
    );
    assert_eq!(r.describe(), "copyright status is public domain and pick flag isn't rejected");
    let mut p = photo(1);
    p.meta.copyright_status = crate::CopyrightStatus::PublicDomain;
    assert!(r.matches(&p, &Catalog::new()), "ids still match");
}

/// Yes/no fields read "Yes" / "No", and a value means the same to matching and to every
/// summary: `false`, `"false"`, `"no"` and `0` are no; `true`, `"yes"`, `1` and a missing value
/// are yes. Anything else is reported, not silently taken as yes.
#[test]
fn yes_no_values() {
    for v in [json!(true), json!("true"), json!("Yes"), json!(1), json!(1.0), json!(null)] {
        assert_eq!(bool_value(&v), Some(true), "{v}");
    }
    for v in [json!(false), json!("false"), json!(" NO "), json!(0), json!(0.0)] {
        assert_eq!(bool_value(&v), Some(false), "{v}");
    }
    for v in [json!("maybe"), json!(2), json!(0.5), json!(-1), json!([true]), json!({"x": 1})] {
        assert_eq!(bool_value(&v), None, "{v}");
    }
    assert_eq!((bool_label(true), bool_label(false)), ("Yes", "No"));
    let cat = Catalog::new();
    let p = photo(1); // unedited
    assert!(rs(json!({"rules": [{"field": "edited", "op": "is", "value": "false"}]})).matches(&p, &cat), "\"false\" means no");
    assert!(
        !rs(json!({"rules": [{"field": "edited", "op": "is", "value": "false"}, {"field": "hasGps", "op": "is", "value": "yes"}]})).matches(&p, &cat)
    );
    assert!(!rs(json!({"rules": [{"field": "edited", "op": "is", "value": "maybe"}]})).matches(&p, &cat), "an unreadable value matches nothing");
    let r = rs(json!({"rules": [{"field": "edited", "op": "is", "value": true}, {"field": "cropped", "op": "is", "value": "false"}]}));
    assert_eq!(r.describe(), "has edits is yes and cropped is no");
    let bad = rs(json!({"rules": [{"field": "edited", "op": "is", "value": "maybe"}]}));
    let messages: Vec<String> = bad.check(&cat).into_iter().map(|p| p.message).collect();
    assert_eq!(messages, vec!["`edited` is yes or no, not \"maybe\"".to_string()]);
}

/// "In the last N" never overflows, whatever N an agent or a saved file holds: a zero,
/// negative or unreadable N matches nothing (either way round); an N beyond 10,000 years means
/// "ever", so every dated photo is in it and none is outside it.
#[test]
fn in_the_last_survives_hostile_counts() {
    set_now(Some("2026-09-01T00:00:00".into()));
    let cat = Catalog::new();
    let p = photo(1); // captured 2026-08-14
    let m = |op: &str, n: serde_json::Value, unit: &str| {
        rs(json!({"rules": [{"field": "captureDate", "op": op, "value": {"n": n, "unit": unit}}]})).matches(&p, &cat)
    };
    for n in [json!(0), json!(-5), json!(-1e30), json!(-9.3e18), json!("soon")] {
        for op in ["inLast", "notInLast"] {
            assert!(!m(op, n.clone(), "years"), "{op} {n}");
        }
    }
    for n in [json!(1e30), json!(9.3e18), json!(u64::MAX), json!(20_000)] {
        assert!(m("inLast", n.clone(), "years"), "{n} years is ever");
        assert!(!m("notInLast", n.clone(), "years"), "{n}");
    }
    assert!(m("inLast", json!(1e30), "hours") && m("inLast", json!(30), "days"));
    set_now(None);
}

/// Every rule's value is checked against its field, so a rule that can't mean anything is
/// reported instead of quietly matching nothing (or everything). Each problem says which rule
/// it is about: "rule 2", or "rule 2.1" inside a group.
#[test]
fn values_are_checked_against_their_field() {
    use crate::{Album, AlbumId, Op};
    let mut cat = Catalog::new();
    cat.apply(Op::AddAlbum { album: Album::new(AlbumId(1), "Trip") }).unwrap();
    let smart = Album { smart: Some(Box::default()), ..Album::new(AlbumId(2), "Best") };
    cat.apply(Op::AddAlbum { album: smart }).unwrap();
    cat.apply(Op::SetLabelName { label: crate::ColorLabel::Red, name: Some("Client".into()) }).unwrap();
    let check = |rule: serde_json::Value| rs(json!({"rules": [rule]})).check(&cat);
    let ok = |rule: serde_json::Value| assert!(check(rule.clone()).is_empty(), "{rule}: {:?}", check(rule.clone()));
    let bad = |rule: serde_json::Value, want: &str| {
        let p = check(rule.clone());
        assert!(p.iter().any(|p| p.message.contains(want)), "{rule}: wanted {want:?}, got {p:?}");
    };
    let r = |field: &str, op: &str, value: serde_json::Value| json!({"field": field, "op": op, "value": value});
    // numbers
    ok(r("rating", "gte", json!(3)));
    ok(r("rating", "gte", json!("4")));
    ok(r("rating", "between", json!([1, 5])));
    ok(r("aperture", "lte", json!("f/4")));
    ok(r("focalLength", "is", json!("50mm")));
    bad(r("rating", "gte", json!("abc")), "needs a number");
    bad(r("rating", "gte", json!(7)), "no rating 0–5 is ≥ 7");
    bad(r("iso", "between", json!([100])), "two");
    bad(r("iso", "between", json!("100")), "two");
    ok(r("shutterSpeed", "lte", json!("1/60")));
    bad(r("shutterSpeed", "lte", json!("fast")), "1/250");
    // dates
    for d in ["2026", "2026-08", "2026-08-14", "2026-08-14T10:00:00"] {
        ok(r("captureDate", "is", json!(d)));
    }
    ok(r("captureDate", "between", json!(["2026-01", "2026-12"])));
    for d in [json!("banana"), json!("2026-13"), json!("2026-02-30"), json!(""), json!(2026)] {
        bad(r("captureDate", "after", d), "needs a date");
    }
    bad(r("captureDate", "between", json!(["2026-12", "2026-01"])), "after the second");
    bad(r("captureDate", "between", json!(["", "2026"])), "needs a date");
    ok(r("captureDate", "inLast", json!({"n": 2, "unit": "weeks"})));
    ok(r("captureDate", "inLast", json!(7)));
    bad(r("captureDate", "inLast", json!({"n": 0, "unit": "days"})), "more than 0");
    bad(r("captureDate", "notInLast", json!({"n": 3, "unit": "fortnights"})), "fortnights");
    ok(r("captureDate", "isEmpty", json!(null)));
    // choices: the ids, the flag's aliases and a colour label's custom name
    ok(r("kind", "is", json!("raw")));
    ok(r("flag", "is", json!("picked")));
    ok(r("label", "is", json!("client")));
    bad(r("label", "is", json!("pink")), "pink");
    bad(r("kind", "isNot", json!("photo")), "photo");
    // text needs something to look for
    ok(r("title", "contains", json!("kite")));
    ok(r("title", "isEmpty", json!(null)));
    bad(r("title", "contains", json!("  ")), "something to look for");
    bad(r("keywords", "startsWith", json!("")), "something to look for");
    // albums: one that exists, plain or smart
    ok(r("album", "is", json!(1)));
    bad(r("album", "is", json!(999)), "no album 999");
    ok(r("album", "is", json!(2)));
    // groups: an empty one is a mistake; problems inside one say where they are
    let p = rs(json!({"rules": [r("rating", "gte", json!(3)), {"group": {"match": "any", "rules": []}}]})).check(&cat);
    assert_eq!(p.iter().map(ToString::to_string).collect::<Vec<_>>(), vec!["rule 2: an empty group".to_string()]);
    let p = rs(json!({"rules": [r("rating", "gte", json!(3)), {"group": {"rules": [r("iso", "is", json!(100)), r("rating", "is", json!(9))]}}]}))
        .check(&cat);
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].path, vec![1, 1]);
    assert_eq!(p[0].to_string(), "rule 2.2: no rating 0–5 is 9");
    // an empty rule list at the top is "all photos" (or none, for any), not a mistake
    assert!(rs(json!({"rules": []})).check(&cat).is_empty());
}

/// The check and the matcher agree. Units are any case, singular or plural, and missing means
/// days; an unknown unit is refused and matches nothing (it used to count as days, so "week" was
/// one day). A rating rule is refused only when no rating 0–5 could meet it. A colour label's
/// custom name matches in any case, accents included. Empty text hints at the right operator.
#[test]
fn check_and_matcher_agree() {
    use crate::Op;
    set_now(Some("2026-09-01T00:00:00".into()));
    let mut cat = Catalog::new();
    cat.apply(Op::SetLabelName { label: crate::ColorLabel::Red, name: Some("Été".into()) }).unwrap();
    let p = photo(1); // captured 2026-08-14 (18 days before now), red label, rating 4
    let rule = |field: &str, op: &str, value: serde_json::Value| rs(json!({"rules": [{"field": field, "op": op, "value": value}]}));
    let valid = |r: &RuleSet| r.check(&cat).is_empty();
    for unit in [json!("Days"), json!("DAY"), json!("days"), json!(null)] {
        let r = rule("captureDate", "inLast", json!({"n": 30, "unit": unit}));
        assert!(valid(&r) && r.matches(&p, &cat), "{unit}");
    }
    let r = rule("captureDate", "inLast", json!({"n": 30}));
    assert!(valid(&r) && r.matches(&p, &cat), "no unit is days");
    let week = rule("captureDate", "inLast", json!({"n": 1, "unit": "week"}));
    assert!(valid(&week) && !week.matches(&p, &cat), "a week, not a day: 18 days ago is outside it");
    assert!(rule("captureDate", "inLast", json!({"n": 3, "unit": "Weeks"})).matches(&p, &cat));
    let odd = rule("captureDate", "inLast", json!({"n": 30, "unit": "fortnights"}));
    assert!(!valid(&odd) && !odd.matches(&p, &cat), "an unknown unit is refused and matches nothing");
    // rating: only a rule no rating could meet
    for (op, v) in [("lt", json!(6)), ("gt", json!(-1)), ("isNot", json!(9)), ("between", json!([3, 8]))] {
        assert!(valid(&rule("rating", op, v.clone())), "{op} {v}");
    }
    for (op, v) in [("is", json!(9)), ("gte", json!(7)), ("gt", json!(5)), ("lt", json!(0)), ("between", json!([6, 9]))] {
        assert!(!valid(&rule("rating", op, v.clone())), "{op} {v}");
    }
    // a custom label name, any case, accents too
    let r = rule("label", "is", json!("été"));
    assert!(valid(&r) && r.matches(&p, &cat));
    // "isn't" with nothing points at "isn't empty", the others at "is empty"
    let hint = |op: &str| rule("title", op, json!("")).check(&cat).first().map(|p| p.message.clone()).unwrap_or_default();
    assert!(hint("isNot").contains("isn't empty"), "{}", hint("isNot"));
    assert!(hint("contains").contains("“is empty”"), "{}", hint("contains"));
    set_now(None);
}

/// An album is in it or not: Album offers "is" and "isn't", never "≥" or "between".
#[test]
fn album_rules_are_is_or_isnt() {
    assert_eq!(field_kind("album"), Some(Kind::Album));
    let ops: Vec<&str> = ops_for(Kind::Album).iter().map(|o| o.0).collect();
    assert_eq!(ops, ["is", "isNot"]);
    let r = rs(json!({"rules": [{"field": "album", "op": "gte", "value": 1}]}));
    assert_eq!(r.check(&Catalog::new()).first().map(|p| p.message.as_str()), Some("`album` has no operator `gte`"));
}

/// An Album rule names its album by id, written as a number or a numeric string, the same way
/// for the check, the matcher and the editor. A folder holds albums, not photos, so it can't be
/// tested.
#[test]
fn album_rule_ids() {
    use crate::{Album, AlbumId, Op};
    assert_eq!(album_rule_id(&json!(3)), Some(AlbumId(3)));
    assert_eq!(album_rule_id(&json!("3")), Some(AlbumId(3)));
    assert_eq!(album_rule_id(&json!(3.0)), Some(AlbumId(3)));
    for v in [json!(1.5), json!(-1), json!("x"), json!(null)] {
        assert_eq!(album_rule_id(&v), None, "{v}");
    }
    let mut cat = Catalog::new();
    cat.apply(Op::AddAlbum { album: Album { folder: true, ..Album::new(AlbumId(4), "Trips") } }).unwrap();
    let p = rs(json!({"rules": [{"field": "album", "op": "is", "value": 4}]})).check(&cat);
    assert!(p.first().is_some_and(|p| p.message.contains("folder")), "{p:?}");
}

/// Album rules saved before Album offered only "is" / "isn't" (≥, between… from when it was a
/// number field) always meant "isn't" to the matcher; upgrading says so, so they check again.
#[test]
fn old_album_operators_upgrade_to_isnt() {
    let mut r = rs(json!({"rules": [
        {"field": "album", "op": "gte", "value": 1},
        {"field": "album", "op": "is", "value": 1},
        {"field": "rating", "op": "gte", "value": 3},
        {"group": {"rules": [{"field": "album", "op": "between", "value": [1, 2]}]}}
    ]}));
    r.upgrade();
    let ops = serde_json::to_value(&r).unwrap().to_string();
    assert!(ops.contains(r#""field":"album","op":"isNot","value":1"#), "{ops}");
    assert!(ops.contains(r#""field":"album","op":"is","value":1"#), "{ops}");
    assert!(ops.contains(r#""field":"rating","op":"gte""#), "other fields keep theirs: {ops}");
    assert!(ops.contains(r#""field":"album","op":"isNot","value":[1,2]"#), "inside groups too: {ops}");
}

/// Besides its English message (for agents), a problem names its field and the kind of issue,
/// so the editor can show "Title: needs something to look for" in any language. Every issue
/// has its own short text.
#[test]
fn problems_name_their_field_and_issue() {
    let cat = Catalog::new();
    let check = |rules: serde_json::Value| rs(json!({"rules": rules})).check(&cat);
    let p = check(json!([{"field": "title", "op": "contains", "value": ""}]));
    assert_eq!((p[0].field.as_deref(), p[0].issue), (Some("title"), Issue::NeedsTextOrEmpty));
    let p = check(json!([{"field": "title", "op": "isNot", "value": ""}]));
    assert_eq!(p[0].issue, Issue::NeedsTextOrNotEmpty);
    // keywords and people say "are empty", and so does their hint
    let p = check(json!([{"field": "keywords", "op": "contains", "value": ""}, {"field": "person", "op": "notContains", "value": " "}]));
    assert_eq!((p[0].issue, p[1].issue), (Issue::NeedsNamesOrEmpty, Issue::NeedsNamesOrNotEmpty));
    assert!(p[0].issue.text().contains("“are empty”") && p[0].message.contains("“are empty”"));
    let p = check(json!([{"field": "rating", "op": "is", "value": 9}, {"group": {"rules": []}}]));
    assert_eq!((p[0].field.as_deref(), p[0].issue), (Some("rating"), Issue::NoRatingMatches));
    assert_eq!((p[1].field.as_deref(), p[1].issue), (None, Issue::EmptyGroup));
    let p = check(json!([{"field": "captureDate", "op": "between", "value": ["2026-12", "2026-01"]}]));
    assert_eq!(p[0].issue, Issue::DatesReversed);
    let p = check(json!([{"field": "album", "op": "is", "value": null}]));
    assert_eq!(p[0].issue, Issue::ChooseAlbum);
    let mut texts: Vec<&str> = Issue::ALL.iter().map(|i| i.text()).collect();
    assert!(texts.iter().all(|t| !t.is_empty()));
    texts.sort_unstable();
    texts.dedup();
    assert_eq!(texts.len(), Issue::ALL.len(), "each issue reads differently");
}

/// A rule date reads one way for the check and the matcher: a space works as the "T"
/// ("2026-10-01 10:00" matches a photo taken then). Imported capture times can carry fractions
/// of a second and a time zone (EXIF SubSecTime / OffsetTime: "…T10:00:00.250+02:00"), so a
/// rule may too, after the seconds; anywhere else they are no date.
#[test]
fn rule_dates_have_one_reading() {
    let cat = Catalog::new();
    let mut p = photo(1);
    p.captured = Some("2026-10-01T10:00:00".into());
    let rule = |op: &str, v: serde_json::Value| rs(json!({"rules": [{"field": "captureDate", "op": op, "value": v}]}));
    for (op, v) in [
        ("is", json!("2026-10-01 10:00")),
        ("is", json!(" 2026-10-01T10 ")),
        ("between", json!(["2026-10-01 09:00", "2026-10-01 11:00"])),
        ("after", json!("2026-10-01 09:59")),
    ] {
        let r = rule(op, v.clone());
        assert!(r.check(&cat).is_empty() && r.matches(&p, &cat), "{op} {v}");
    }
    // a capture time with a fraction and a zone, as imports write it
    let mut zoned = photo(2);
    zoned.captured = Some("2026-08-14T10:00:00.250+02:00".into());
    for v in ["2026-08-14T10:00:00.250", "2026-08-14T10:00:00.250+02:00", "2026-08-14T10:00:00"] {
        let r = rule("is", json!(v));
        assert!(r.check(&cat).is_empty() && r.matches(&zoned, &cat), "{v}");
    }
    assert!(rule("is", json!("2026-08-14T10:00:00Z")).check(&cat).is_empty());
    for v in [
        "2026-8-14",
        "2026-08-14T1",
        "2026-08-14X10",
        "26-08-14",
        "2026-08-14T25:00",
        "2026-08-14Z",
        "2026-08-14 10:00-05:00",
        "2026-08-14T10:00:00.",
        "2026-08-14T10:00:00+2",
        "2026-08-14T10:00:00+99:99",
        "2026-08-14T10:00:00+18:01",
    ] {
        let p = rule("is", json!(v)).check(&cat);
        assert_eq!(p.first().map(|p| p.issue), Some(Issue::NeedsDate), "{v}");
    }
    assert_eq!(rule_date("2026-10-01 10:00"), Ok("2026-10-01T10:00".to_string()));
    assert_eq!(rule_date("2026"), Ok("2026".to_string()));
    assert!(rule("is", json!("2026-08-14T10:00:00+18:00")).check(&cat).is_empty());
}

/// The field menu shows every rule field once: at the top level or in exactly one group,
/// and [`FIELDS`] lists them in menu order.
#[test]
fn every_field_is_in_the_menu_exactly_once() {
    let menu: Vec<&str> = TOP_LEVEL_FIELDS.iter().chain(FIELD_GROUPS.iter().flat_map(|g| g.1.iter())).copied().collect();
    let fields: Vec<&str> = FIELDS.iter().map(|f| f.0).collect();
    assert_eq!(menu, fields, "the menu and FIELDS list the same fields in the same order");
    for (label, group) in FIELD_GROUPS {
        assert!(!group.is_empty(), "group {label} is empty");
    }
    let mut labels: Vec<&str> = FIELD_GROUPS.iter().map(|g| g.0).collect();
    labels.dedup();
    assert_eq!(labels.len(), FIELD_GROUPS.len(), "group labels are unique");
}

/// Related fields sit together: all the file fields, all the camera (EXIF) fields, all the
/// dates, all the keyword fields.
#[test]
fn related_fields_share_a_group() {
    let same = |fields: &[&str], group: &str| {
        for f in fields {
            assert_eq!(field_group(f), Some(group), "{f}");
        }
    };
    same(&["fileName", "extension", "filePath", "kind", "format", "duration"], "File");
    same(&["album", "virtualCopy", "copyName", "stacked"], "Source");
    same(&["longEdge", "shortEdge", "aspect", "megapixels"], "Size");
    same(&["edited", "cropped", "treatment"], "Develop");
    same(&["camera", "lens", "focalLength", "aperture", "shutterSpeed", "iso"], "Camera Info");
    same(&["captureDate", "importDate", "editDate"], "Date");
    same(&["keywords", "keywordCount", "person", "personCount"], "Keywords & People");
    same(&["title", "caption", "altText", "creator", "copyright", "copyrightStatus"], "Description");
    same(&["location", "city", "state", "country", "hasGps"], "Location");
    for f in ["rating", "flag", "label", "text"] {
        assert_eq!(field_group(f), None, "{f} stays at the top level");
    }
    assert_eq!(field_group("nope"), None);
    assert_eq!(field_label("filePath"), Some("File Path"));
    assert_eq!(field_label("nope"), None);
}

/// The `filePath` rule exactly as it was before the in-place ASCII path: new strings per photo.
fn file_path_old(op: &str, source: &Source, value: &serde_json::Value) -> bool {
    let want = lower(value);
    let path = match source {
        Source::File { path } => path.replace('\\', "/"),
        Source::Demo { .. } => String::new(),
    };
    let want = want.replace('\\', "/");
    match op {
        "contains" => !want.trim().is_empty() && path.to_lowercase().contains(want.trim()),
        "notContains" => want.trim().is_empty() || !path.to_lowercase().contains(want.trim()),
        _ => text_op(op, &path, &want),
    }
}

#[test]
fn file_path_rules_match_the_old_strings_for_any_path() {
    let paths = [
        "",
        " ",
        "  /a/b.jpg \t",
        "/Volumes/share/photos/Trip/IMG_0001.jpg",
        "/Volumes/share/PHOTOS/trip/img_0001.JPG",
        r"D:\Photos\Lake Como\IMG_0001.JPG",
        r"d:/photos/lake como/img_0001.jpg",
        r"\\nas\share\x.jpg",
        "/photos/Straße/a.jpg",
        "/photos/STRASSE/a.jpg",
        "/photos/İstanbul/a.jpg",
        "/photos/i̇stanbul/a.jpg",
        "/photos/ΟΔΟΣ/a.jpg",
        "/photos/οδος/a.jpg",
        "/photos/ΟΔΌΣ/a.jpg",
        "/photos/日本/東京.jpg",
        "/p/\u{0B}x\u{0B}",
        "/p/\u{85}x\u{85}",
        "a",
    ];
    let wants = [
        "",
        " ",
        "a",
        "A",
        "/",
        r"\",
        r"\photos\",
        "/photos/",
        "photos",
        "PHOTOS",
        "Lake Como",
        " lake como ",
        "lake  como",
        r"d:\photos\lake",
        "IMG_0001.JPG",
        ".jpg",
        "straße",
        "STRASSE",
        "strasse",
        "İstanbul",
        "i̇stanbul",
        "istanbul",
        "ΟΔΟΣ",
        "οδος",
        "οδοσ",
        "ΟΔΌΣ",
        "東京",
        "日本",
        "x",
        "\u{0B}x",
        "/a/b.jpg",
        "/a/b.jpg \t",
        "\u{85}x",
    ];
    let ops = ["contains", "notContains", "is", "isNot", "startsWith", "endsWith", "isEmpty", "isNotEmpty", "bogus"];
    let mut values: Vec<serde_json::Value> = wants.iter().map(|w| json!(w)).collect();
    values.extend([json!(null), json!(5), json!(true), json!(["a", "b"])]);
    let mut sources: Vec<Source> = paths.iter().map(|p| Source::File { path: (*p).into() }).collect();
    sources.push(Source::Demo { scene: 0 });
    for source in &sources {
        for op in ops {
            for value in &values {
                assert_eq!(file_path_matches(op, source, value), file_path_old(op, source, value), "{source:?} {op} {value}");
            }
        }
    }
}

#[test]
fn file_path_rule_through_a_rule_set_ignores_case_and_slash_direction() {
    let mut p = photo(1);
    p.source = Source::File { path: r"D:\Photos\Lake Como\IMG_0001.JPG".into() };
    let r = rs(json!({"rules": [{"field": "filePath", "op": "contains", "value": "/LAKE como/"}]}));
    assert!(r.matches(&p, &Catalog::default()));
}

/// Every piece of a long path, in both cases and both slash directions, wherever it starts
/// (the search skips eight bytes at a time, so starts and ends at every offset matter).
#[test]
fn file_path_contains_finds_every_piece_of_a_long_path() {
    let path = r"D:\Photos\Lake Como\Wedding-Day_2026\IMG_0001 (copy).JPG";
    let source = Source::File { path: path.into() };
    let shout = |s: &str| s.to_uppercase();
    let swap = |s: &str| s.replace('\\', "/");
    for from in 0..path.len() {
        for to in from + 1..=path.len() {
            let piece = &path[from..to];
            for want in [piece.to_string(), shout(piece), swap(piece), piece.replace('/', r"\"), format!("{piece}x"), format!("x{piece}")] {
                let value = json!(want);
                for op in ["contains", "notContains"] {
                    assert_eq!(file_path_matches(op, &source, &value), file_path_old(op, &source, &value), "{op} {want:?}");
                }
            }
        }
    }
}
