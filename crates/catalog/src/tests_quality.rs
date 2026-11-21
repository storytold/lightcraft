//! The Image Quality Scanner's catalog side (issue #1313): the `QualityScan` a photo carries, the
//! verdict from the two cutoffs, the filter (`quality`, `qualityMin`, `qualityMax`), the sort key
//! and the filter bar's tally. The scanner itself (measuring pixels) is the engine's.

use crate::rules;
use crate::*;

/// A photo with `quality` as its scan (or none).
fn photo(i: u64, quality: Option<QualityScan>) -> Photo {
    let mut p = Photo::new(
        PhotoId(i),
        Source::File { path: format!("/pics/IMG_{i:04}.jpg") },
        &format!("IMG_{i:04}.jpg"),
        "JPEG",
        100,
        100,
        "2026-01-01",
    );
    p.analysis = quality.map(|q| Analysis { quality: Some(q), ..Default::default() });
    p
}

fn scanned(score: f32, accepted: f32, rejected: f32) -> QualityScan {
    QualityScan { score, verdict: QualityVerdict::of(score, accepted, rejected), ..Default::default() }
}

/// The cutoffs are inclusive, the rejected one winning where they touch, and a score that is not
/// a number is Pending — never a flag on a photo nobody can compare.
#[test]
fn a_verdict_reads_the_two_cutoffs_inclusively() {
    assert_eq!(QualityVerdict::of(70.0, 70.0, 25.0), QualityVerdict::Accepted);
    assert_eq!(QualityVerdict::of(69.9, 70.0, 25.0), QualityVerdict::Pending);
    assert_eq!(QualityVerdict::of(25.0, 70.0, 25.0), QualityVerdict::Rejected);
    assert_eq!(QualityVerdict::of(25.1, 70.0, 25.0), QualityVerdict::Pending);
    assert_eq!(QualityVerdict::of(f32::NAN, 70.0, 25.0), QualityVerdict::Pending);
    // the rejected cutoff wins a cross, so a scan that stores one can't pick what it meant
    assert_eq!(QualityVerdict::of(50.0, 50.0, 50.0), QualityVerdict::Rejected);
    // and each verdict asks for the flag it stands for (Pending: the photo's own)
    assert_eq!(QualityVerdict::Accepted.flag(), Some(Flag::Pick));
    assert_eq!(QualityVerdict::Rejected.flag(), Some(Flag::Reject));
    assert_eq!(QualityVerdict::Pending.flag(), None);
    assert_eq!(QualityVerdict::of(1.0, 2.0, 3.0).as_str(), "rejected");
}

#[test]
fn the_quality_filter_reads_the_verdict_and_the_score() {
    let mut c = Catalog::new();
    let accepted = scanned(80.0, 70.0, 25.0);
    let pending = scanned(50.0, 70.0, 25.0);
    let rejected = scanned(10.0, 70.0, 25.0);
    for (i, q) in [Some(accepted), Some(pending), Some(rejected), None].into_iter().enumerate() {
        let p = photo(i as u64 + 1, q);
        c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    let shown = |f: Filter| -> Vec<u64> { c.query(&f, &Sort::default()).iter().map(|id| id.0).collect() };
    // the verdict
    for (want, ids) in [
        ("accepted", vec![1]),
        ("pending", vec![2]),
        ("rejected", vec![3]),
        ("unscanned", vec![4]),
    ] {
        let f = Filter { quality: Some(want.to_string()), ..Default::default() };
        assert_eq!(shown(f), ids, "{want}");
    }
    // the bounds are inclusive, and an unscored photo matches neither: it has no number to compare
    let floor = Filter { quality_min: Some(50.0), ..Default::default() };
    assert_eq!(shown(floor), [1, 2]);
    let ceiling = Filter { quality_max: Some(50.0), ..Default::default() };
    assert_eq!(shown(ceiling), [2, 3]);
    let band = Filter { quality_min: Some(20.0), quality_max: Some(60.0), ..Default::default() };
    assert_eq!(shown(band), [2]);
    // both together: the verdict and the number must hold
    let both = Filter { quality: Some("accepted".into()), quality_min: Some(90.0), ..Default::default() };
    assert!(shown(both).is_empty(), "a photo accepted at 80 is below a floor of 90");
    // the description names each constraint (smart albums show it)
    let described = Filter { quality: Some("unscanned".into()), ..Default::default() }.describe();
    assert!(described.contains("not quality scanned"), "{described}");
    let described = Filter { quality_min: Some(40.0), quality_max: Some(60.0), ..Default::default() }.describe();
    assert!(described.contains("quality score"), "{described}");
}

#[test]
fn sorting_by_quality_puts_unscanned_photos_first_ascending() {
    let mut c = Catalog::new();
    let shots = [Some(scanned(80.0, 70.0, 25.0)), None, Some(scanned(10.0, 70.0, 25.0)), Some(scanned(0.0, 70.0, 25.0))];
    for (i, q) in shots.into_iter().enumerate() {
        let p = photo(i as u64 + 1, q);
        c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    // ascending: no score, then the measured scores (a measured 0 is still a score)
    let asc = c.query(&Filter::default(), &Sort { key: SortKey::Quality, ascending: true, ..Default::default() });
    let asc: Vec<u64> = asc.iter().map(|id| id.0).collect();
    assert_eq!(asc, [2, 4, 3, 1]);
    // descending: the best first, the unscanned last
    let desc = c.query(&Filter::default(), &Sort { key: SortKey::Quality, ascending: false, ..Default::default() });
    let desc: Vec<u64> = desc.iter().map(|id| id.0).collect();
    assert_eq!(desc, [1, 3, 4, 2]);
}

#[test]
fn the_tally_counts_each_verdict_and_the_scanned_photos() {
    let mut c = Catalog::new();
    let shots = [Some(scanned(80.0, 70.0, 25.0)), Some(scanned(50.0, 70.0, 25.0)), Some(scanned(10.0, 70.0, 25.0)), None];
    for (i, q) in shots.into_iter().enumerate() {
        let p = photo(i as u64 + 1, q);
        c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    let t = c.tally(&Filter::default());
    assert_eq!((t.quality_accepted, t.quality_pending, t.quality_rejected, t.quality_unscanned), (1, 1, 1, 1), "{t:?}");
    assert_eq!(c.quality_scanned(), 3);
    assert!(c.any_quality_scanned(), "the filter bar shows its quality picker once one photo is scored");
    // the tally is the view's, so a filter changes it as it changes the grid
    let t = c.tally(&Filter { quality: Some("rejected".into()), ..Default::default() });
    assert_eq!((t.total, t.quality_rejected), (1, 1), "{t:?}");
    // how many the catalog has scored does not change with the view
    assert_eq!(c.quality_scanned(), 3);
    assert!(c.any_quality_scanned());
}

/// The score survives the journal and the snapshot: it is the photo's own data (catalog format 20).
#[test]
fn a_scan_is_written_and_read_back_on_the_photo() {
    let mut c = Catalog::new();
    let p = photo(1, None);
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    let scan = QualityScan { sharpness: 81.5, noise: 63.0, exposure: 90.0, compression: Some(45.25), ..scanned(72.5, 70.0, 25.0) };
    c.apply(Op::SetAnalysis { id: PhotoId(1), analysis: Some(Analysis { quality: Some(scan), ..Default::default() }) }).unwrap();
    let back = c.photo(PhotoId(1)).and_then(|p| p.analysis).and_then(|a| a.quality).expect("the score is on the photo");
    assert_eq!(back, scan);
    assert_eq!(back.verdict, QualityVerdict::Accepted);
    // the JSON the journal and the snapshot write names the fields the manual does
    let v = serde_json::to_value(back).unwrap();
    for key in ["sharpness", "noise", "exposure", "compression", "score", "verdict"] {
        assert!(v.get(key).is_some(), "{key} is not written: {v}");
    }
    assert_eq!(v["verdict"], "accepted");
    // a raw photo has no compression measured: absent, not 0
    let scan = QualityScan { compression: None, ..scanned(50.0, 70.0, 25.0) };
    let v = serde_json::to_value(scan).unwrap();
    assert!(v.get("compression").is_none(), "an unmeasured factor is absent: {v}");
    assert!(v.get("flagWritten").is_none(), "no flag written: absent, and a scan of an older build reads with none");
    // the flag a scan wrote is kept with the score, so the next scan may change it (and only it)
    let scan = QualityScan { flag_written: Some(Flag::Pick), ..scanned(50.0, 70.0, 25.0) };
    let v = serde_json::to_value(scan).unwrap();
    assert_eq!(v["flagWritten"], "pick", "{v}");
    assert_eq!(serde_json::from_value::<QualityScan>(v).unwrap(), scan);
}

/// The rule fields read the same numbers the filter and the sort do, so a smart collection and the
/// filter bar can never disagree about where a photo stands.
#[test]
fn the_rule_fields_read_the_stored_scan() {
    let mut c = Catalog::new();
    let p = photo(1, Some(QualityScan { sharpness: 81.0, noise: 63.0, exposure: 90.0, compression: Some(45.0), ..scanned(72.5, 70.0, 25.0) }));
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    let p = photo(2, None);
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    let rules = |rule: serde_json::Value| -> Vec<u64> {
        let f: Filter = serde_json::from_value(serde_json::json!({"ruleSet": {"match": "all", "rules": [rule]}})).unwrap();
        c.query(&f, &Sort::default()).iter().map(|id| id.0).collect()
    };
    assert_eq!(rules(serde_json::json!({"field": "qualityScore", "op": "gte", "value": 70})), [1]);
    assert_eq!(rules(serde_json::json!({"field": "qualitySharpness", "op": "between", "value": [80, 90]})), [1]);
    assert_eq!(rules(serde_json::json!({"field": "qualityVerdict", "op": "is", "value": "accepted"})), [1]);
    assert_eq!(rules(serde_json::json!({"field": "qualityVerdict", "op": "isNot", "value": "accepted"})), [2]);
    // — but an unscored photo has no number: a comparison it can't meet keeps it out
    assert_eq!(rules(serde_json::json!({"field": "qualityScore", "op": "lt", "value": 100})), Vec::<u64>::new());
    // the fields are grouped for the rule editor, under their own heading
    for field in ["qualityScore", "qualitySharpness", "qualityNoise", "qualityExposure", "qualityCompression", "qualityVerdict"] {
        assert_eq!(rules::field_group(field), Some("Image Quality"), "{field}");
    }
}
