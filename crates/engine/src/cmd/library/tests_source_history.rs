//! Go Back / Go Forward through the sources `library.source` showed.
use lcc_catalog::{AlbumId, Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::{LibrarySource, Session};

fn session() -> (Session, u64, u64) {
    let mut s = Session::new();
    for id in 1..=3u64 {
        let photo = Photo::new(PhotoId(id), Source::File { path: format!("/hist/{id}.jpg") }, &format!("{id}.jpg"), "JPEG", 40, 30, "2026-11-10");
        s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    }
    let mut make = |name: &str| s.execute("album.create", &json!({"name": name})).unwrap()["id"].as_u64().unwrap();
    let (a, b) = (make("A"), make("B"));
    (s, a, b)
}

fn can(s: &Session, id: &str) -> bool {
    s.commands().into_iter().find(|c| c.id == id).map(|c| c.enabled).unwrap_or(false)
}

fn show_album(s: &mut Session, id: u64) {
    s.execute("library.source", &json!({"kind": "album", "id": id})).unwrap();
}

#[test]
fn back_and_forward_walk_the_source_history() {
    // back and forward walk the sources and are off at the ends
    {
        let (mut s, a, b) = session();
        assert!(!can(&s, "library.sourceBack") && !can(&s, "library.sourceForward"));
        show_album(&mut s, a);
        show_album(&mut s, b);
        assert!(can(&s, "library.sourceBack") && !can(&s, "library.sourceForward"));
        s.execute("library.sourceBack", &json!({})).unwrap();
        assert_eq!(s.source, LibrarySource::Album(AlbumId(a)));
        s.execute("library.sourceBack", &json!({})).unwrap();
        assert_eq!(s.source, LibrarySource::All, "the source shown before the first choice starts the history");
        assert!(!can(&s, "library.sourceBack") && can(&s, "library.sourceForward"));
        assert!(s.execute("library.sourceBack", &json!({})).is_err());
        s.execute("library.sourceForward", &json!({})).unwrap();
        s.execute("library.sourceForward", &json!({})).unwrap();
        assert_eq!(s.source, LibrarySource::Album(AlbumId(b)));
        assert!(!can(&s, "library.sourceForward"));
    }
    // choosing a new source clears the forward half
    {
        let (mut s, a, b) = session();
        show_album(&mut s, a);
        show_album(&mut s, b);
        s.execute("library.sourceBack", &json!({})).unwrap();
        s.execute("library.source", &json!({"kind": "picks"})).unwrap();
        assert!(!can(&s, "library.sourceForward"));
        s.execute("library.sourceBack", &json!({})).unwrap();
        assert_eq!(s.source, LibrarySource::Album(AlbumId(a)), "B is gone from the history");
    }
    // sources that no longer exist are skipped
    {
        let (mut s, a, b) = session();
        show_album(&mut s, a);
        show_album(&mut s, b);
        s.execute("library.source", &json!({"kind": "picks"})).unwrap();
        s.execute("album.delete", &json!({"id": b})).unwrap();
        s.execute("library.sourceBack", &json!({})).unwrap();
        assert_eq!(s.source, LibrarySource::Album(AlbumId(a)), "the deleted collection is stepped over");
        s.execute("library.sourceForward", &json!({})).unwrap();
        assert_eq!(s.source, LibrarySource::Picks);
    }
    // the history is bounded
    {
        let (mut s, _, _) = session();
        for i in 0..120 {
            let kind = if i % 2 == 0 { "picks" } else { "all" };
            s.execute("library.source", &json!({"kind": kind})).unwrap();
        }
        let mut steps = 0;
        while s.execute("library.sourceBack", &json!({})).is_ok() {
            steps += 1;
        }
        assert!(steps < crate::view::SourceHistory::MAX && steps > 10, "got {steps}");
    }
}
