//! Fujifilm film-recipe lookups for the Info panel.
//!
//! Import stores the recipe on the photo. Older catalogs did not, so the panel reads the file on a
//! worker thread and remembers the answer. The memory key includes the library generation: photo
//! ids start at 1 in every catalog, and egui would otherwise keep the previous library's text.

use std::collections::VecDeque;

use lightcraft_catalog::{Photo, Source};

/// How many photos to remember. Alternating two, or stepping back through a filmstrip, must not
/// open the files again. Entries from another library are dropped first.
const CACHE_MAX: usize = 64;
/// A missing or unreadable file is tried again after this long, not on every frame.
const RETRY_AFTER: f64 = 5.0;

pub(crate) const TASK: &str = "Film recipe";

/// One file in one library. Photo ids are not part of it: a virtual copy shares the file, and
/// photo ids start over in every catalog (the library generation keeps those apart).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RecipeKey {
    pub library: u64,
    pub path: String,
    pub hash: Option<String>,
    pub file_size: u64,
}

impl RecipeKey {
    pub(crate) fn from_photo(library: u64, photo: &Photo) -> Option<Self> {
        let Source::File { path } = &photo.source else { return None };
        if path.is_empty() {
            return None;
        }
        Some(Self { library, path: path.clone(), hash: photo.content_hash.clone(), file_size: photo.file_size })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RecipeView {
    pub text: String,
    pub lines: Vec<(String, String)>,
}

impl RecipeView {
    fn from_recipe(recipe: &lightcraft_meta::FujiRecipe) -> Option<Self> {
        if recipe.rows.is_empty() {
            return None;
        }
        Some(Self { text: recipe.text(), lines: recipe.rows.iter().map(|row| (row.label.clone(), row.value.clone())).collect() })
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Outcome {
    Absent,
    Ready(RecipeView),
    /// `at` is egui time, seconds. A recent failure stays blank instead of starting another read.
    Failed(f64),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Plan {
    Show(RecipeView),
    Hidden,
    Wait,
    Start(RecipeKey),
}

#[derive(Default)]
pub(crate) struct Recipes {
    entries: VecDeque<(RecipeKey, Outcome)>,
    inflight: Option<RecipeKey>,
    discard_inflight: bool,
    wanted: Option<RecipeKey>,
}

impl Recipes {
    /// A relink may replace a file without changing its key. Its old worker must not refill the cache.
    pub(crate) fn forget(&mut self, key: &RecipeKey) {
        self.entries.retain(|(old, _)| old != key);
        if self.inflight.as_ref() == Some(key) {
            self.discard_inflight = true;
        }
    }

    /// What the Info panel should do for `photo` in `library` at egui time `now`.
    pub(crate) fn plan(&mut self, library: u64, photo: &Photo, now: f64) -> Plan {
        if let Some(view) = photo.meta.fuji_recipe.as_ref().and_then(RecipeView::from_recipe) {
            self.wanted = None;
            return Plan::Show(view);
        }
        if photo.meta.fuji_recipe_known {
            self.wanted = None;
            return Plan::Hidden;
        }
        let Some(key) = RecipeKey::from_photo(library, photo) else {
            self.wanted = None;
            return Plan::Hidden;
        };
        if !lightcraft_meta::might_hold_fuji_recipe(&photo.meta.camera, &photo.format, &photo.file_name) {
            self.wanted = None;
            return Plan::Hidden;
        }
        if let Some(hit) = self.cached(&key, now) {
            self.wanted = None;
            return hit;
        }
        if self.inflight.as_ref() == Some(&key) {
            // the user came back to this file: don't start the one they left when this read ends
            self.wanted = None;
            return Plan::Wait;
        }
        if self.inflight.is_some() {
            self.wanted = Some(key);
            return Plan::Wait;
        }
        self.wanted = None;
        Plan::Start(key)
    }

    /// Claim the one background read. A second photo waits until this one finishes.
    pub(crate) fn reserve(&mut self, key: &RecipeKey) -> bool {
        if self.inflight.is_some() {
            self.wanted = Some(key.clone());
            return false;
        }
        if self.wanted.as_ref() == Some(key) {
            self.wanted = None;
        }
        self.inflight = Some(key.clone());
        self.discard_inflight = false;
        true
    }

    /// The worker never started. Remember the failure so the next frame does not try again at once.
    pub(crate) fn fail_reserve(&mut self, key: &RecipeKey, now: f64) {
        if self.inflight.as_ref() == Some(key) {
            self.inflight = None;
        }
        self.remember(key.clone(), Outcome::Failed(now));
    }

    /// The worker died without a result (the task row is already gone).
    pub(crate) fn abandon_idle(&mut self, running: bool, now: f64) {
        if running {
            return;
        }
        let Some(key) = self.inflight.take() else { return };
        if !std::mem::take(&mut self.discard_inflight) {
            self.remember(key, Outcome::Failed(now));
        }
    }

    /// Store `result` when it still belongs to the open library. The photo the user moved on to,
    /// if it still needs a read, is returned so the caller can start it.
    pub(crate) fn finish(
        &mut self,
        key: &RecipeKey,
        result: Result<Option<lightcraft_meta::FujiRecipe>, String>,
        library_now: u64,
        now: f64,
    ) -> Option<RecipeKey> {
        if self.inflight.as_ref() != Some(key) {
            return None;
        }
        self.inflight = None;
        if !std::mem::take(&mut self.discard_inflight) && key.library == library_now {
            let outcome = match result {
                Ok(recipe) => match recipe.as_ref().and_then(RecipeView::from_recipe) {
                    Some(view) => Outcome::Ready(view),
                    None => Outcome::Absent,
                },
                Err(_) => Outcome::Failed(now),
            };
            self.remember(key.clone(), outcome);
        }
        self.wanted.take().filter(|next| next.library == library_now && self.inflight.is_none() && !self.fresh(next, now))
    }

    fn cached(&mut self, key: &RecipeKey, now: f64) -> Option<Plan> {
        let outcome = self.touch(key)?.clone();
        Some(match outcome {
            Outcome::Absent => Plan::Hidden,
            Outcome::Ready(view) => Plan::Show(view),
            Outcome::Failed(at) if now < at || now - at < RETRY_AFTER => Plan::Wait,
            Outcome::Failed(_) => return None,
        })
    }

    fn fresh(&self, key: &RecipeKey, now: f64) -> bool {
        match self.entries.iter().find(|(k, _)| k == key).map(|(_, outcome)| outcome) {
            Some(Outcome::Absent | Outcome::Ready(_)) => true,
            Some(Outcome::Failed(at)) => now < *at || now - at < RETRY_AFTER,
            None => false,
        }
    }

    fn touch(&mut self, key: &RecipeKey) -> Option<&Outcome> {
        let pos = self.entries.iter().position(|(k, _)| k == key)?;
        let entry = self.entries.remove(pos)?;
        self.entries.push_back(entry);
        self.entries.back().map(|(_, outcome)| outcome)
    }

    fn remember(&mut self, key: RecipeKey, outcome: Outcome) {
        let library = key.library;
        self.entries.retain(|(old, _)| old != &key);
        self.entries.push_back((key, outcome));
        while self.entries.len() > CACHE_MAX {
            if let Some(pos) = self.entries.iter().position(|(old, _)| old.library != library) {
                self.entries.remove(pos);
            } else {
                self.entries.pop_front();
            }
        }
    }
}

/// Read a recipe off the UI thread. `reader`, when the session has one, is the in-memory file
/// hook (the browser build). Otherwise only the EXIF window or the RAF's embedded JPEG is read.
pub(crate) fn read_path(reader: Option<lightcraft_engine::merge::ByteReader>, path: &str) -> Result<Option<lightcraft_meta::FujiRecipe>, String> {
    if let Some(read) = reader {
        let bytes = read(path)?;
        return Ok(lightcraft_meta::recipe_from_bytes(&bytes));
    }
    lightcraft_meta::read_recipe_file(std::path::Path::new(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_catalog::{PhotoId, Source};

    fn photo(id: u64, path: &str, format: &str) -> Photo {
        Photo::new(PhotoId(id), Source::File { path: path.into() }, path, format, 100, 80, "2026-10-12")
    }

    fn recipe(value: &str) -> lightcraft_meta::FujiRecipe {
        lightcraft_meta::FujiRecipe { rows: vec![lightcraft_meta::FujiRow { tag: 0x1401, label: "Film simulation".into(), value: value.into() }] }
    }

    fn key(library: u64, photo: &Photo) -> RecipeKey {
        RecipeKey::from_photo(library, photo).expect("file")
    }

    #[test]
    fn a_stored_recipe_is_shown_and_another_library_is_not() {
        let mut cache = Recipes::default();
        let mut first = photo(1, "C:/one/DSCF0001.JPG", "JPEG");
        first.meta.fuji_recipe = Some(recipe("Classic Chrome"));
        first.meta.fuji_recipe_known = true;
        assert!(matches!(cache.plan(1, &first, 0.0), Plan::Show(view) if view.lines[0].1 == "Classic Chrome"));

        let mut second = photo(1, "D:/two/DSCF0001.JPG", "JPEG");
        second.meta.camera = "FUJIFILM X-T5".into();
        assert!(matches!(cache.plan(2, &second, 0.0), Plan::Start(k) if k.library == 2 && k.path.ends_with("DSCF0001.JPG")));
    }

    #[test]
    fn the_same_photo_id_in_another_library_does_not_reuse_the_cache() {
        let mut cache = Recipes::default();
        let shot = photo(1, "C:/roll/DSCF0001.JPG", "JPEG");
        let first = key(1, &shot);
        assert!(cache.reserve(&first));
        assert!(cache.finish(&first, Ok(Some(recipe("Classic Chrome"))), 1, 0.0).is_none());
        assert!(matches!(cache.plan(1, &shot, 1.0), Plan::Show(view) if view.text.contains("Classic Chrome")));

        let mut other = shot.clone();
        other.meta.camera = "FUJIFILM X-T5".into();
        match cache.plan(2, &other, 1.0) {
            Plan::Start(k) => assert_eq!(k.library, 2),
            other => panic!("library 2 must not show library 1's recipe: {other:?}"),
        }
        // a read started in library 1 that finishes after the switch must not replace that
        // library's answer, and must not become library 2's answer
        let late = key(1, &shot);
        assert!(cache.finish(&late, Ok(Some(recipe("Eterna"))), 2, 1.0).is_none());
        assert!(matches!(cache.plan(2, &other, 1.0), Plan::Start(_)));
        assert!(matches!(cache.plan(1, &shot, 1.0), Plan::Show(view) if view.text.contains("Classic Chrome")));
    }

    #[test]
    fn alternating_photos_and_a_replaced_file_use_the_cache_key() {
        let mut cache = Recipes::default();
        let a = photo(1, "C:/a.jpg", "JPEG");
        let b = photo(2, "C:/b.jpg", "JPEG");
        let ka = key(4, &a);
        let kb = key(4, &b);
        cache.reserve(&ka);
        assert!(cache.finish(&ka, Ok(Some(recipe("Classic Chrome"))), 4, 0.0).is_none());
        cache.reserve(&kb);
        cache.finish(&kb, Ok(None), 4, 0.0);
        assert!(matches!(cache.plan(4, &a, 1.0), Plan::Show(_)));
        assert!(matches!(cache.plan(4, &b, 1.0), Plan::Hidden));

        let mut replaced = a.clone();
        replaced.content_hash = Some("abc".into());
        assert!(matches!(cache.plan(4, &replaced, 1.0), Plan::Start(k) if k.hash.as_deref() == Some("abc")));
        let mut moved = a.clone();
        if let Source::File { path } = &mut moved.source {
            path.push_str(".new");
        }
        assert!(matches!(cache.plan(4, &moved, 1.0), Plan::Start(_)));
    }

    #[test]
    fn a_nikon_raw_is_not_queued_and_a_failure_waits_before_retrying() {
        let mut cache = Recipes::default();
        let mut nikon = photo(3, "C:/DSC_0001.NEF", "NEF");
        nikon.meta.camera = "NIKON Z 9".into();
        assert!(matches!(cache.plan(1, &nikon, 0.0), Plan::Hidden));
        assert!(matches!(cache.plan(1, &photo(4, "C:/demo.jpg", "JPEG").tap_demo(), 0.0), Plan::Hidden));

        let jpeg = photo(5, "C:/unknown.jpg", "JPEG");
        let k = key(1, &jpeg);
        cache.reserve(&k);
        assert!(cache.finish(&k, Err("offline".into()), 1, 0.0).is_none());
        assert!(matches!(cache.plan(1, &jpeg, 1.0), Plan::Wait));
        assert!(matches!(cache.plan(1, &jpeg, 6.0), Plan::Start(_)));
    }

    #[test]
    fn only_one_read_runs_and_the_photo_the_user_left_is_not_prefetched() {
        let mut cache = Recipes::default();
        let a = photo(1, "C:/a.jpg", "JPEG");
        let b = photo(2, "C:/b.jpg", "JPEG");
        let ka = key(1, &a);
        assert!(cache.reserve(&ka));
        assert!(matches!(cache.plan(1, &b, 0.0), Plan::Wait));
        assert!(!cache.reserve(&key(1, &b)));
        let mut nikon = photo(9, "C:/c.NEF", "NEF");
        nikon.meta.camera = "NIKON Z 6".into();
        assert!(matches!(cache.plan(1, &nikon, 0.0), Plan::Hidden));
        assert!(cache.finish(&ka, Ok(Some(recipe("Acros"))), 1, 1.0).is_none(), "leaving for a Nikon cancels the queued JPEG");
    }

    #[test]
    fn going_back_to_the_file_being_read_drops_the_one_that_was_waiting() {
        let mut cache = Recipes::default();
        let a = photo(1, "C:/a.jpg", "JPEG");
        let b = photo(2, "C:/b.jpg", "JPEG");
        let ka = key(1, &a);
        assert!(cache.reserve(&ka));
        assert!(matches!(cache.plan(1, &b, 0.0), Plan::Wait));
        assert!(matches!(cache.plan(1, &a, 0.5), Plan::Wait));
        assert!(cache.finish(&ka, Ok(Some(recipe("Acros"))), 1, 1.0).is_none());
        assert!(matches!(cache.plan(1, &b, 1.0), Plan::Start(_)));
    }

    #[test]
    fn a_failed_read_still_starts_the_photo_the_user_moved_to() {
        let mut cache = Recipes::default();
        let a = photo(1, "C:/a.jpg", "JPEG");
        let b = photo(2, "C:/b.jpg", "JPEG");
        let ka = key(1, &a);
        assert!(cache.reserve(&ka));
        assert!(matches!(cache.plan(1, &b, 0.0), Plan::Wait));
        let next = cache.finish(&ka, Err("offline".into()), 1, 1.0).expect("the photo on screen is still waiting");
        assert_eq!(next, key(1, &b));
    }

    #[test]
    fn a_virtual_copy_reuses_the_read_of_the_same_file() {
        let mut cache = Recipes::default();
        let original = photo(1, "C:/roll/DSCF0001.JPG", "JPEG");
        let mut copy = photo(2, "C:/roll/DSCF0001.JPG", "JPEG");
        copy.meta.camera = "FUJIFILM X-T5".into();
        let k = key(1, &original);
        assert_eq!(key(1, &copy), k);
        assert!(cache.reserve(&k));
        assert!(matches!(cache.plan(1, &copy, 0.0), Plan::Wait));
        assert!(cache.finish(&k, Ok(Some(recipe("Classic Chrome"))), 1, 1.0).is_none());
        assert!(matches!(cache.plan(1, &copy, 1.0), Plan::Show(view) if view.lines[0].1 == "Classic Chrome"));
    }

    #[test]
    fn the_cache_forgets_entries_past_its_cap_and_other_libraries_first() {
        let mut cache = Recipes::default();
        let foreign = key(1, &photo(1, "C:/old.jpg", "JPEG"));
        cache.reserve(&foreign);
        cache.finish(&foreign, Ok(None), 1, 0.0);
        for i in 0..CACHE_MAX {
            let shot = photo(i as u64 + 10, &format!("C:/{i}.jpg"), "JPEG");
            let k = key(2, &shot);
            cache.reserve(&k);
            cache.finish(&k, Ok(None), 2, 0.0);
        }
        assert!(matches!(cache.plan(1, &photo(1, "C:/old.jpg", "JPEG"), 1.0), Plan::Start(_)));
        assert!(matches!(cache.plan(2, &photo(10, "C:/0.jpg", "JPEG"), 1.0), Plan::Hidden));
    }

    #[test]
    fn same_path_relink_forgets_only_that_file_including_a_late_read() {
        let path = std::env::temp_dir().join(format!("lc-fuji-relink-{}.jpg", std::process::id()));
        std::fs::write(&path, b"replacement").unwrap();
        for command in ["photo.relink", "photo.locate"] {
            for (pending, moved) in [(false, false), (true, false), (false, true), (true, true)] {
                let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::new(), crate::Services::default());
                let original = if moved { path.with_extension("old.jpg") } else { path.clone() };
                let shot = photo(1, original.to_str().unwrap(), "JPEG");
                let other = photo(2, "C:/other.jpg", "JPEG");
                for ph in [&shot, &other] {
                    app.session.catalog.apply(lightcraft_catalog::Op::AddPhoto { photo: Box::new(ph.clone()) }).unwrap();
                }
                app.session.selection.active = Some(shot.id);
                let generation = app.session.library_generation();
                let k = key(generation, &shot);
                let other_key = key(generation, &other);
                app.recipes.reserve(&other_key);
                app.recipes.finish(&other_key, Ok(Some(recipe("Eterna"))), generation, 0.0);
                if moved {
                    let destination = key(generation, &photo(3, path.to_str().unwrap(), "JPEG"));
                    app.recipes.reserve(&destination);
                    app.recipes.finish(&destination, Ok(Some(recipe("Acros"))), generation, 0.0);
                }
                app.recipes.reserve(&k);
                if !pending {
                    app.recipes.finish(&k, Ok(Some(recipe("Classic Chrome"))), generation, 0.0);
                }
                assert!(app.run(command, serde_json::json!({"id": 1, "path": path.with_extension("missing")})).is_err());
                if !pending {
                    assert!(matches!(app.recipes.plan(generation, &shot, 1.0), Plan::Show(_)), "failed relink keeps the cache");
                }
                app.run(command, serde_json::json!({"id": 1, "path": path})).unwrap();
                let relinked = app.session.catalog.photo(shot.id).unwrap().clone();
                assert_eq!(key(generation, &relinked) == k, !moved, "no probe changed the hash or size");
                if pending {
                    assert!(matches!(app.recipes.plan(generation, &relinked, 1.0), Plan::Wait));
                    app.recipes.finish(&k, Ok(Some(recipe("Classic Chrome"))), generation, 1.0);
                }
                assert!(matches!(app.recipes.plan(generation, &relinked, 2.0), Plan::Start(_)), "{command}, pending={pending}, moved={moved}");
                assert!(matches!(app.recipes.plan(generation, &other, 2.0), Plan::Show(view) if view.text.contains("Eterna")));
            }
        }
        std::fs::remove_file(path).unwrap();
    }

    trait Tap {
        fn tap_demo(self) -> Self;
    }
    impl Tap for Photo {
        fn tap_demo(mut self) -> Self {
            self.source = Source::Demo { scene: 1 };
            self
        }
    }
}
