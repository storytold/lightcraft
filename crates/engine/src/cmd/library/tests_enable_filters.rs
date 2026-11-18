//! `library.toggleFilters` (Library > Enable Filters, Cmd+L) through the real engine.
use lcc_catalog::{Filter, Flag, Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::{LibrarySource, Result, Session};

fn session() -> Result<Session> {
    let mut s = Session::new();
    for (id, rating, flag) in [(1, 5, Flag::Pick), (2, 5, Flag::None), (3, 2, Flag::Pick), (4, 1, Flag::None)] {
        let mut photo = Photo::new(
            PhotoId(id),
            Source::File { path: format!("/app-enable-filters/{id}.jpg") },
            &format!("{id}.jpg"),
            "JPEG",
            40,
            30,
            "2026-10-23",
        );
        photo.rating = rating;
        photo.flag = flag;
        s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) })?;
    }
    Ok(s)
}

fn stars_and_pick(s: &mut Session) {
    s.execute("library.filter", &json!({"rating": 4, "flag": "pick"})).unwrap();
}

#[test]
fn off_shows_everything_and_on_brings_back_the_same_filter() {
    // off shows everything and on brings back the same filter
    {
        let mut s = session().unwrap();
        stars_and_pick(&mut s);
        let set = s.filter.clone();
        assert_eq!(s.visible().len(), 1, "the filter hides photos, or off and on would look alike");
        let r = s.execute("library.toggleFilters", &json!({})).unwrap();
        assert_eq!((r["enabled"].clone(), r["count"].clone()), (json!(false), json!(4)));
        assert_eq!(s.filter, Filter::default());
        assert_eq!(s.visible().len(), 4);
        let r = s.execute("library.toggleFilters", &json!({})).unwrap();
        assert_eq!((r["enabled"].clone(), r["count"].clone()), (json!(true), json!(1)));
        assert_eq!(s.filter, set, "stars and flag come back together");
        assert_eq!(s.visible().len(), 1);
    }
    // with no filter it does nothing and leaves nothing to bring back
    {
        let mut s = session().unwrap();
        let r = s.execute("library.toggleFilters", &json!({})).unwrap();
        assert_eq!((r["enabled"].clone(), r["count"].clone()), (json!(true), json!(4)));
        let r = s.execute("library.toggleFilters", &json!({})).unwrap();
        assert_eq!(r["enabled"], json!(true));
        assert_eq!(s.filter, Filter::default());
        assert!(s.shelved_filter.is_none());
    }
}

#[test]
fn a_filter_set_while_off_replaces_the_one_put_aside() {
    // setting a filter while off turns filtering on and forgets the old one
    {
        let mut s = session().unwrap();
        stars_and_pick(&mut s);
        s.execute("library.toggleFilters", &json!({})).unwrap();
        s.execute("library.filter", &json!({"rating": 2})).unwrap();
        assert_eq!(s.visible().len(), 3);
        // off again, then on: the filter that was set last, not the first one
        s.execute("library.toggleFilters", &json!({})).unwrap();
        s.execute("library.toggleFilters", &json!({})).unwrap();
        assert_eq!(s.filter.rating, 2);
        assert_eq!(s.filter.flag, None);
    }
    // emptying the filter by hand while off leaves nothing to bring back
    {
        let mut s = session().unwrap();
        stars_and_pick(&mut s);
        s.execute("library.toggleFilters", &json!({})).unwrap();
        s.execute("library.filter", &json!({"rating": 2})).unwrap();
        s.execute("library.filter", &json!({"rating": 0})).unwrap();
        assert_eq!(s.filter, Filter::default());
        s.execute("library.toggleFilters", &json!({})).unwrap();
        assert_eq!(s.filter, Filter::default(), "the filter from before it was switched off is not back");
        assert_eq!(s.visible().len(), 4);
    }
    // clearing the filter while off leaves nothing to bring back
    {
        let mut s = session().unwrap();
        stars_and_pick(&mut s);
        s.execute("library.toggleFilters", &json!({})).unwrap();
        s.execute("library.clearFilter", &json!({})).unwrap();
        s.execute("library.toggleFilters", &json!({})).unwrap();
        assert_eq!(s.filter, Filter::default());
        assert_eq!(s.visible().len(), 4);
    }
    // applying a preset while off turns filtering on with the preset
    {
        let mut s = session().unwrap();
        s.filter_presets.push(super::super::filters::FilterPreset { name: "Fives".into(), filter: Filter { rating: 5, ..Default::default() } });
        stars_and_pick(&mut s);
        s.execute("library.toggleFilters", &json!({})).unwrap();
        s.execute("filter.applyPreset", &json!({"name": "Fives"})).unwrap();
        s.execute("library.toggleFilters", &json!({})).unwrap();
        s.execute("library.toggleFilters", &json!({})).unwrap();
        assert_eq!(s.filter.rating, 5);
        assert_eq!(s.filter.flag, None);
    }
    // show only replaces an empty result and forgets the filter put aside
    // Showing a fixed set of photos (Immich smart search) after a Find Similar with no hits shows
    // that set, not an empty grid, and forgets a filter Enable Filters put aside.
    {
        let mut s = session().unwrap();
        s.execute("library.filter", &json!({"rating": 4})).unwrap();
        s.execute("library.toggleFilters", &json!({})).unwrap();
        assert!(s.filters_off());
        s.show_only(Vec::new());
        assert!(s.visible_cloned().is_empty(), "an empty result shows nothing");
        assert!(!s.filters_off(), "the filter put aside is forgotten");
        s.show_only(vec![PhotoId(3)]);
        assert_eq!(s.visible_cloned(), vec![PhotoId(3)]);
    }
}

#[test]
fn the_filter_put_aside_follows_folder_and_keyword_changes() {
    // a folder filter does not come back over the folder now shown
    {
        let mut s = session().unwrap();
        s.execute("library.filter", &json!({"rating": 4, "libraryFolder": "/app-enable-filters"})).unwrap();
        s.execute("library.toggleFilters", &json!({})).unwrap();
        s.source = LibrarySource::LibraryFolder;
        s.library_folder = Some("/app-enable-filters".into());
        s.execute("library.toggleFilters", &json!({})).unwrap();
        assert_eq!(s.filter.rating, 4);
        assert_eq!(s.filter.library_folder, None);
    }
    // removing a folder while off drops it from the filter put aside
    {
        let mut s = session().unwrap();
        s.execute("library.filter", &json!({"rating": 4, "libraryFolder": "/app-enable-filters"})).unwrap();
        s.execute("library.toggleFilters", &json!({})).unwrap();
        s.execute("library.removeFolder", &json!({"path": "/app-enable-filters"})).unwrap();
        s.execute("library.toggleFilters", &json!({})).unwrap();
        assert_eq!(s.filter.library_folder, None, "the removed folder is not brought back");
        assert_eq!(s.filter.rating, 4, "the rest of the filter is");
    }
    // removing the only criterion put aside leaves filtering on
    {
        let mut s = session().unwrap();
        s.execute("library.filter", &json!({"libraryFolder": "/app-enable-filters"})).unwrap();
        s.execute("library.toggleFilters", &json!({})).unwrap();
        s.execute("library.removeFolder", &json!({"path": "/app-enable-filters"})).unwrap();
        assert!(s.shelved_filter.is_none(), "an empty filter is not left put aside");
    }
    // a keyword renamed while off is followed by the filter put aside
    {
        let mut s = session().unwrap();
        let mut photo = Photo::new(PhotoId(5), Source::File { path: "/app-enable-filters/5.jpg".into() }, "5.jpg", "JPEG", 40, 30, "2026-10-23");
        photo.meta.keywords = vec!["cat".into()];
        s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        s.filter.keyword = Some("cat".into());
        assert_eq!(s.visible().len(), 1);
        s.execute("library.toggleFilters", &json!({})).unwrap();
        s.execute("keyword.rename", &json!({"from": "cat", "to": "dog"})).unwrap();
        let r = s.execute("library.toggleFilters", &json!({})).unwrap();
        assert_eq!(r["enabled"], json!(true));
        assert_eq!(s.filter.keyword.as_deref(), Some("dog"), "the put-aside filter follows the rename");
        assert_eq!(s.visible().len(), 1);
    }
}
