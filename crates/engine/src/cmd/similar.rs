//! Find Similar (`library.findSimilar`), split so the app can read the photos on a worker thread: the
//! first search reads a thumbnail-level source of every photo in the library, and in a large
//! library that froze the window for as long as it took (issue #3468). [`plan`] lists what is to be
//! read (no file-system calls), [`measure`] reads it without the session (cancellable, with
//! progress), [`keep`] adds what it found to the session's signatures and [`apply`] answers from
//! those. The command itself (the CLI, MCP without an app) does all of them at once.
//!
//! A search keeps no decoded source and learns nothing from one: it only looks, and in a large library
//! it would push out of memory what the grid and the loupe are showing.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

use lightcraft_catalog::PhotoId;
use serde_json::{Value, json};

use super::bad;
use crate::media::{SourceLevel, SourceRef, content_key};
use crate::{Result, Session};

const C: &str = "library.findSimilar";

/// The kind of the row in the activity stack that a search on a worker thread holds.
pub const KIND: &str = "similar";

/// A content key and that content's look-alike signature.
pub type Signature = (String, [f32; 64]);

/// Whether a search can start (the command's `enabled`): one at a time, for what a second one would
/// read the first is reading.
pub fn no_search_running(s: &Session) -> std::result::Result<(), String> {
    if s.activity.list().iter().any(|r| r.kind == KIND) { Err("a search for similar photos is already running".to_string()) } else { Ok(()) }
}

/// What a search asks.
#[derive(Debug)]
pub struct Search {
    id: PhotoId,
    min: f32,
    filter: bool,
}

/// A content whose signature isn't known yet, and how to read it: from the first of its photos that
/// can be read (a copy that is offline gives way to the next).
struct Content {
    key: String,
    sources: Vec<SourceRef>,
}

/// What a search has to read: the searched photo first, as nothing can be compared without it.
pub struct Reads {
    me: Option<Content>,
    rest: Vec<Content>,
}

impl Reads {
    /// How many contents are to be read.
    pub fn len(&self) -> usize {
        self.rest.len().saturating_add(usize::from(self.me.is_some()))
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The search `p` asks for and what it has to read, once for each content not known yet. No
/// file-system calls.
pub fn plan(s: &mut Session, p: &Value) -> Result<(Search, Reads)> {
    let id = super::photo_id(p, C)?.or(s.active()).ok_or_else(|| bad(C, "no photo"))?;
    let min = p.get("similarity").and_then(Value::as_f64).unwrap_or(0.8) as f32;
    let filter = p.get("filter").and_then(Value::as_bool).unwrap_or(true);
    let photo = s.catalog.photo(id).ok_or_else(|| bad(C, "can't read the photo"))?;
    let my_key = content_key(photo);
    let me =
        (!s.signatures.contains_key(&my_key)).then(|| Content { sources: vec![s.media.source_ref(photo, SourceLevel::Thumb)], key: my_key.clone() });
    let mut rest: Vec<Content> = Vec::new();
    let mut at = std::collections::HashMap::new();
    for q in s.catalog.photos().filter(|q| q.in_library()) {
        let key = content_key(q);
        // (the searched photo's content is known by the time these are read, or the search has ended)
        if key == my_key || s.signatures.contains_key(&key) {
            continue;
        }
        let source = s.media.source_ref(q, SourceLevel::Thumb);
        match at.get(&key).and_then(|i: &usize| rest.get_mut(*i)) {
            Some(content) => content.sources.push(source),
            None => {
                at.insert(key.clone(), rest.len());
                rest.push(Content { key, sources: vec![source] });
            }
        }
    }
    Ok((Search { id, min, filter }, Reads { me, rest }))
}

/// `content`'s signature, or `None` when none of its photos can be read or `cancel` is set first. A
/// decoder's panic is that photo's failure, not the search's.
fn signature_of(content: &Content, cancel: &AtomicBool) -> Option<[f32; 64]> {
    content.sources.iter().find_map(|source| {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        crate::guard::catch("Find Similar", || {
            crate::memory::in_background(|| source.load_source()).ok().map(|decoded| lightcraft_pipeline::cull::signature(&decoded.image))
        })
        .ok()
        .flatten()
    })
}

/// Read what [`plan`] listed, several photos at once. Doesn't touch the session. `progress(done)` is
/// called from the threads doing the reading; once `cancel` is set no further file is started, and
/// what was read until then comes back. A searched photo that can't be read ends it there.
pub fn measure(reads: Reads, cancel: &AtomicBool, progress: &(dyn Fn(usize) + Sync)) -> Vec<Signature> {
    let found = Mutex::new(Vec::with_capacity(reads.len().min(4096)));
    let done = AtomicUsize::new(0);
    let keep = |key: String, v: [f32; 64]| found.lock().unwrap_or_else(PoisonError::into_inner).push((key, v));
    if let Some(me) = reads.me {
        let Some(v) = signature_of(&me, cancel) else { return Vec::new() };
        keep(me.key, v);
        done.store(1, Ordering::Relaxed);
        progress(1);
    }
    let lanes = crate::export::export_parallelism(reads.rest.len());
    let queue = Mutex::new(reads.rest.into_iter());
    let work = || {
        while !cancel.load(Ordering::Relaxed) {
            let next = queue.lock().unwrap_or_else(PoisonError::into_inner).next();
            let Some(content) = next else { break };
            if let Some(v) = signature_of(&content, cancel) {
                keep(content.key, v);
            }
            progress(done.fetch_add(1, Ordering::Relaxed).saturating_add(1));
        }
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::scope(|sc| {
        for k in 1..lanes.max(1) {
            // a lane that can't start leaves its photos to the others (and to this thread)
            if std::thread::Builder::new().name(format!("similar-{k}")).spawn_scoped(sc, work).is_err() {
                break;
            }
        }
        work();
    });
    #[cfg(target_arch = "wasm32")]
    {
        let _ = lanes;
        work();
    }
    found.into_inner().unwrap_or_else(PoisonError::into_inner)
}

/// Add what [`measure`] found to the signatures, also when the search was stopped: they hold for the
/// contents they were read from, and the next search doesn't read those again.
pub fn keep(s: &mut Session, found: Vec<Signature>) {
    s.signatures.extend(found);
}

/// Answer `search` from the signatures known now, most similar first, and filter the view to the
/// photos found. A photo with none (it can't be read, or came after the [`plan`]) isn't compared.
pub fn apply(s: &mut Session, search: &Search) -> Result<Value> {
    let known = s.catalog.photo(search.id).and_then(|photo| s.signatures.get(&content_key(photo)));
    let me = *known.ok_or_else(|| bad(C, "can't read the photo"))?;
    let mut hits: Vec<(PhotoId, f32)> = Vec::new();
    for q in s.catalog.photos().filter(|q| q.in_library()) {
        if let Some(v) = s.signatures.get(&content_key(q)) {
            let sim = lightcraft_pipeline::cull::similarity(&me, v);
            if sim >= search.min {
                hits.push((q.id, sim.clamp(0.0, 1.0)));
            }
        }
    }
    hits.sort_by(|a, b| b.1.total_cmp(&a.1));
    if search.filter {
        s.shelved_filter = None;
        s.filter.only = hits.iter().map(|h| h.0).collect();
        // an empty `only` means "no constraint": no hits is its own state that shows no photos
        s.filter.only_none = s.filter.only.is_empty();
    }
    Ok(json!({"photos": hits.iter().map(|(i, v)| json!({"id": i.0, "similarity": (v * 1000.0).round() / 1000.0})).collect::<Vec<_>>()}))
}

/// Find Similar: photos that look like `id` (signatures from thumbnail-level sources, cached by
/// content), most similar first; the view is filtered to them.
pub(super) fn run(s: &mut Session, p: &Value) -> Result<Value> {
    let (search, reads) = plan(s, p)?;
    let found = measure(reads, &AtomicBool::new(false), &|_| {});
    keep(s, found);
    apply(s, &search)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use lightcraft_catalog::{Op, Photo, Source};
    use lightcraft_raster::Rgb32f;

    use super::*;

    /// Reads per file, the files that can't be read and the file whose decoder panics.
    #[derive(Default)]
    struct Files {
        reads: Mutex<HashMap<String, usize>>,
        unreadable: Mutex<Vec<String>>,
        panics: Mutex<Option<String>>,
        /// Set once this many reads have started.
        cancel_after: Option<(usize, Arc<AtomicBool>)>,
    }

    impl Files {
        fn reads_of(&self, path: &str) -> usize {
            self.reads.lock().unwrap().get(path).copied().unwrap_or(0)
        }
        fn reads(&self) -> usize {
            self.reads.lock().unwrap().values().sum()
        }
    }

    /// A session of photos `/p/<name>.jpg`, each with its content hash when it has one. Odd numbers
    /// in a name mean a picture that gets brighter to the right, others one that does downwards.
    fn session(photos: &[(&str, Option<&str>)], files: Files) -> (Session, Arc<Files>) {
        let files = Arc::new(files);
        let mut s = Session::new();
        for (i, (name, hash)) in photos.iter().enumerate() {
            let source = Source::File { path: format!("/p/{name}.jpg") };
            let mut p = Photo::new(PhotoId(i as u64 + 1), source, &format!("{name}.jpg"), "JPEG", 64, 64, "2026-01-01T00:00:00");
            p.content_hash = hash.map(str::to_string);
            s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        }
        let seen = files.clone();
        s.media.file_loader = Some(Arc::new(move |path: &str, _| {
            let started = {
                let mut reads = seen.reads.lock().unwrap();
                *reads.entry(path.to_string()).or_default() += 1;
                reads.values().sum::<usize>()
            };
            if let Some((after, cancel)) = &seen.cancel_after
                && started >= *after
            {
                cancel.store(true, Ordering::SeqCst);
            }
            if seen.panics.lock().unwrap().as_deref() == Some(path) {
                panic!("a decoder fails on {path}");
            }
            if seen.unreadable.lock().unwrap().iter().any(|f| f == path) {
                return Err(format!("{path}: gone"));
            }
            let across = path.bytes().filter(u8::is_ascii_digit).any(|b| b % 2 == 1);
            let image = Rgb32f::from_fn(64, 64, |x, y| {
                let v = if across { x } else { y };
                [v as f32 / 64.0; 3]
            });
            Ok((image, Default::default()))
        }));
        (s, files)
    }

    fn ids(r: &Value) -> Vec<u64> {
        r["photos"].as_array().unwrap().iter().map(|h| h["id"].as_u64().unwrap()).collect()
    }

    /// A search on a worker thread holds a row of its kind: the command is off until the row goes.
    #[test]
    fn a_running_search_turns_the_command_off() {
        let (mut s, files) = session(&[("a1", None), ("b2", None)], Files::default());
        let row = s.activity.start(KIND, "Find Similar Photos", crate::activity::Cancel::No);
        let off = s.commands().into_iter().find(|c| c.id == C).is_some_and(|c| !c.enabled);
        assert!(off, "the menu shows it off");
        assert!(matches!(s.execute(C, &json!({"id": 1})), Err(crate::EngineError::Disabled(..))));
        assert_eq!(files.reads(), 0);
        drop(row);
        assert!(s.execute(C, &json!({"id": 1})).is_ok());
    }

    #[test]
    fn making_the_plan_reads_no_file() {
        let (mut s, files) = session(&[("a1", None), ("b2", None), ("c1", None)], Files::default());
        let (_, reads) = plan(&mut s, &json!({"id": 1})).unwrap();
        assert_eq!(reads.len(), 3);
        assert_eq!(files.reads(), 0);
    }

    #[test]
    fn the_command_reads_each_photo_and_finds_the_ones_alike() {
        let names: Vec<String> = (0..40).map(|i| format!("p{i}")).collect();
        let photos: Vec<(&str, Option<&str>)> = names.iter().map(|n| (n.as_str(), None)).collect();
        let (mut s, files) = session(&photos, Files::default());
        let r = s.execute("library.findSimilar", &json!({"id": 1})).unwrap();
        assert_eq!(files.reads(), 40, "each once");
        assert!(ids(&r).contains(&1) && ids(&r).len() < 40, "{r}");
        // known now: no file is read again
        let again = s.execute("library.findSimilar", &json!({"id": 1})).unwrap();
        assert_eq!(files.reads(), 40);
        assert_eq!(ids(&again), ids(&r));
    }

    #[test]
    fn a_content_is_read_once_and_an_unreadable_copy_gives_way_to_the_next() {
        let files = Files::default();
        files.unreadable.lock().unwrap().push("/p/copy2.jpg".into());
        let photos = [("me1", None), ("copy2", Some("h")), ("copy4", Some("h")), ("copy6", Some("h"))];
        let (mut s, files) = session(&photos, files);
        let r = s.execute("library.findSimilar", &json!({"id": 1, "similarity": -1.0})).unwrap();
        assert_eq!(files.reads_of("/p/copy2.jpg"), 1);
        assert_eq!(files.reads_of("/p/copy4.jpg"), 1, "the next copy stands in");
        assert_eq!(files.reads_of("/p/copy6.jpg"), 0, "its content is known by then");
        assert_eq!(ids(&r).len(), 4, "every copy is compared: {r}");
    }

    #[test]
    fn a_searched_photo_that_cannot_be_read_ends_the_search_before_any_other_is_read() {
        let files = Files::default();
        files.unreadable.lock().unwrap().push("/p/me1.jpg".into());
        let (mut s, files) = session(&[("me1", None), ("b2", None), ("c1", None)], files);
        let e = s.execute("library.findSimilar", &json!({"id": 1})).unwrap_err();
        assert!(e.to_string().contains("can't read the photo"), "{e}");
        assert_eq!(files.reads(), 1, "only the searched photo");
        assert!(s.signatures.is_empty());
        assert_eq!(s.filter, Default::default(), "the view stays as it was");
    }

    #[test]
    fn a_photo_nobody_has_is_refused_at_once() {
        let (mut s, files) = session(&[("a1", None)], Files::default());
        assert!(matches!(plan(&mut s, &json!({"id": 99})), Err(crate::EngineError::BadParams { .. })));
        assert_eq!(files.reads(), 0);
    }

    #[test]
    fn a_photo_whose_decoder_panics_is_left_out_and_the_search_goes_on() {
        let files = Files::default();
        *files.panics.lock().unwrap() = Some("/p/bad3.jpg".into());
        let (mut s, _) = session(&[("me1", None), ("bad3", None), ("ok1", None), ("ok5", None)], files);
        let r = s.execute("library.findSimilar", &json!({"id": 1, "similarity": -1.0})).unwrap();
        assert_eq!(ids(&r).len(), 3, "{r}");
        assert!(!ids(&r).contains(&2), "{r}");
    }

    /// Stopped after some reads: what was read is kept, the view is left alone, and the next search
    /// reads only the rest.
    #[test]
    fn a_stopped_search_keeps_what_it_read_and_the_next_reads_the_rest() {
        let cancel = Arc::new(AtomicBool::new(false));
        let files = Files { cancel_after: Some((5, cancel.clone())), ..Default::default() };
        let names: Vec<String> = (0..64).map(|i| format!("p{i}")).collect();
        let photos: Vec<(&str, Option<&str>)> = names.iter().map(|n| (n.as_str(), None)).collect();
        let (mut s, _) = session(&photos, files);
        let (_, reads) = plan(&mut s, &json!({"id": 1})).unwrap();
        let found = measure(reads, &cancel, &|_| {});
        let read = found.len();
        assert!((1..64).contains(&read), "stopped along the way: {read}");
        keep(&mut s, found);
        assert_eq!(s.filter, Default::default());
        let (_, rest) = plan(&mut s, &json!({"id": 1})).unwrap();
        assert_eq!(rest.len(), 64 - read);
    }

    #[test]
    fn a_stopped_search_reads_nothing_more() {
        let (mut s, files) = session(&[("a1", None), ("b2", None), ("c1", None)], Files::default());
        let (_, reads) = plan(&mut s, &json!({"id": 1})).unwrap();
        assert!(measure(reads, &AtomicBool::new(true), &|_| {}).is_empty());
        assert_eq!(files.reads(), 0);
    }

    /// What changed while the files were read: a photo deleted since isn't an answer, and one added
    /// since has no signature to compare.
    #[test]
    fn photos_that_changed_during_the_search_are_answered_as_they_are_now() {
        let (mut s, _) = session(&[("a1", None), ("b1", None), ("c1", None)], Files::default());
        let (search, reads) = plan(&mut s, &json!({"id": 1, "similarity": -1.0})).unwrap();
        let found = measure(reads, &AtomicBool::new(false), &|_| {});
        s.catalog.apply(Op::RemovePhoto { id: PhotoId(2) }).unwrap();
        let added = Photo::new(PhotoId(9), Source::File { path: "/p/new1.jpg".into() }, "new1.jpg", "JPEG", 64, 64, "2026-01-01T00:00:00");
        s.catalog.apply(Op::AddPhoto { photo: Box::new(added) }).unwrap();
        keep(&mut s, found);
        let mut answer = ids(&apply(&mut s, &search).unwrap());
        answer.sort_unstable();
        assert_eq!(answer, [1, 3]);
    }

    #[test]
    fn progress_counts_every_content_read_or_not() {
        let files = Files::default();
        files.unreadable.lock().unwrap().push("/p/b2.jpg".into());
        let (mut s, _) = session(&[("a1", None), ("b2", None), ("c1", None), ("d2", None)], files);
        let (_, reads) = plan(&mut s, &json!({"id": 1})).unwrap();
        let top = AtomicUsize::new(0);
        let found = measure(reads, &AtomicBool::new(false), &|done| {
            top.fetch_max(done, Ordering::SeqCst);
        });
        assert_eq!(top.load(Ordering::SeqCst), 4);
        assert_eq!(found.len(), 3);
    }
}
