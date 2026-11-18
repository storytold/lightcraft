//! Culling suggestions (issue #489): `photo.analyze` suggests 1–5 stars and a Pick / Reject flag
//! from the measured scores, and applies them only where the user has not rated or flagged.

use serde_json::json;

use lcc_catalog::Flag;

use crate::Session;

/// One test: analysing the demo library is the cost.
#[test]
fn analyze_suggests_and_applies_stars_and_flags() {
    let mut s = Session::with_demo();
    let vis = s.visible_cloned();
    assert!(vis.len() >= 2);

    // suggestions come back even without applying them: the answer alone is useful for bulk culling
    let before: Vec<(u8, Flag)> = vis
        .iter()
        .map(|id| {
            let p = s.catalog.photo(*id).unwrap();
            (p.rating, p.flag)
        })
        .collect();
    let r = s.execute("photo.analyze", &json!({"ids": vis.iter().map(|i| i.0).collect::<Vec<_>>()})).unwrap();
    for row in r["photos"].as_array().cloned().unwrap_or_default() {
        assert!((1..=5).contains(&row["suggestedStars"].as_u64().unwrap_or(0)));
    }
    for (id, (rating, flag)) in vis.iter().zip(before) {
        let p = s.catalog.photo(*id).unwrap();
        assert_eq!(p.rating, rating, "no rating applied");
        assert_eq!(p.flag, flag, "no flag applied");
    }

    // a photo the user rated and one they flagged: suggestions must not touch them
    s.execute("photo.rate", &json!({"ids": [vis[0].0], "rating": 3})).unwrap();
    s.execute("photo.flag", &json!({"ids": [vis[1].0], "flag": "pick"})).unwrap();

    let r =
        s.execute("photo.analyze", &json!({"ids": vis.iter().map(|i| i.0).collect::<Vec<_>>(), "applyRatings": true, "applyFlags": true})).unwrap();

    // every photo gets a suggestion in its answer and on the record
    let photos = r["photos"].as_array().cloned().unwrap_or_default();
    assert_eq!(photos.len(), vis.len(), "one row per photo");
    for row in &photos {
        let stars = row["suggestedStars"].as_u64().unwrap_or(0);
        assert!((1..=5).contains(&stars), "stars {stars} out of range");
        assert!(matches!(row["suggestedFlag"].as_str(), Some("pick") | Some("reject") | Some("")));
        let id = row["id"].as_u64().unwrap_or(0);
        let p = s.catalog.photo(lcc_catalog::PhotoId(id)).unwrap_or_else(|| panic!("photo {id} gone"));
        let a = p.analysis.expect("analysis stored");
        assert_eq!(a.suggested, Some(stars as u8), "stored suggestion matches the answer");
        assert!(a.cull_score.is_some());
    }
    // the user's choices survived
    let p0 = s.catalog.photo(vis[0]).unwrap();
    assert_eq!(p0.rating, 3, "a user rating is never overwritten");
    let p1 = s.catalog.photo(vis[1]).unwrap();
    assert_eq!(p1.flag, Flag::Pick, "a user flag is never overwritten");
    // somewhere in the library, unrated photos did get ratings and unflagged ones flags
    assert!(r["rated"].as_u64().unwrap_or(0) + r["flagged"].as_u64().unwrap_or(0) > 0, "nothing applied");
}
