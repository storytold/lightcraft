//! Issue #1131: the People view counts the faces of what the view shows. Given two named people in
//! different photos, when a collection holding only one of those photos is the source, then only
//! that person is offered; back on All Photos, both are.

use lcc_catalog::Op;
use serde_json::json;

use crate::Session;

#[test]
fn people_follow_the_views_source() {
    let mut s = Session::with_demo();
    let ids: Vec<_> = s.catalog.photos().map(|p| p.id).take(2).collect();
    for (id, name) in [(ids[0], "Jane Doe"), (ids[1], "John Roe")] {
        let mut meta = s.catalog.photo(id).unwrap().meta.clone();
        meta.regions = vec![lcc_meta::Region {
            rect: lcc_geom::Rect { x0: 0.3, y0: 0.2, x1: 0.55, y1: 0.6 },
            kind: lcc_meta::RegionKind::Face,
            name: Some(name.to_string()),
            description: None,
        }];
        s.commit("setup", Op::SetMeta { id, meta: Box::new(meta) }).unwrap();
    }
    let names = |s: &Session| -> Vec<String> { s.catalog.people_in(&s.people_filter()).into_iter().map(|p| p.name).collect() };
    let mut both = names(&s);
    both.sort();
    assert_eq!(both, ["Jane Doe", "John Roe"]);
    let a = s.execute("album.create", &json!({"name": "Jane"})).unwrap()["id"].as_u64().unwrap();
    s.execute("album.addPhotos", &json!({"id": a, "ids": [ids[0].0]})).unwrap();
    s.execute("library.source", &json!({"kind": "album", "id": a})).unwrap();
    assert_eq!(names(&s), ["Jane Doe"], "only the collection's faces");
    // a person picked in the filter bar doesn't narrow the cards themselves
    s.filter.person = Some("John Roe".into());
    assert_eq!(names(&s), ["Jane Doe"]);
    s.filter.person = None;
    s.execute("library.source", &json!({"kind": "all"})).unwrap();
    assert_eq!(names(&s).len(), 2, "everyone again");
}
