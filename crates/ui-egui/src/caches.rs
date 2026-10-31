use super::{Caches, album_picker, i18n, panels};

/// The left panel's counts.
#[derive(Clone, Copy, Debug, Default)]
pub struct LibraryCounts {
    pub total: usize,
    pub picks: usize,
    pub deleted: usize,
}

/// The albums' counts as the sidebar shows them ([`Caches::album_counts`]).
pub(crate) struct AlbumCounts {
    /// The library they are of ([`lightcraft_engine::Session::library_generation`]).
    library: u64,
    /// The minute they are of while a smart album asks about "the last N days" (else empty).
    minute: String,
    /// What follows edits to a photo or a few.
    tally: lightcraft_catalog::tally::AlbumTally,
    /// The counts as the panels take them.
    shown: std::sync::Arc<std::collections::HashMap<lightcraft_catalog::AlbumId, usize>>,
}

/// A count of every photo on a worker thread ([`Caches::album_counts`]).
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub(crate) struct AlbumRecount {
    /// The library, minute and catalog revision it counts.
    library: u64,
    minute: String,
    revision: u64,
    /// The count, sent before the repaint is asked for, so the frame it wakes finds it.
    done: std::sync::mpsc::Receiver<Result<lightcraft_catalog::tally::AlbumTally, String>>,
    /// The thread, until it has let go of its copy of the catalog.
    worker: std::thread::JoinHandle<()>,
}

/// The values the filter bar's pickers offer.
#[derive(Clone, Debug, Default)]
pub struct FilterValues {
    pub cameras: Vec<String>,
    pub lenses: Vec<String>,
    pub keywords: Vec<String>,
}

/// What the smart-album rule dialog shows besides the rules themselves: their problems (each row
/// marks its own), how many photos they match and the albums an Album rule picks from.
pub struct RulesView {
    pub problems: Vec<lightcraft_catalog::rules::Problem>,
    pub count: usize,
    pub albums: Vec<album_picker::AlbumEntry>,
}

pub(crate) fn key_of(parts: impl std::hash::Hash) -> u64 {
    use std::hash::{BuildHasher, BuildHasherDefault, DefaultHasher};
    BuildHasherDefault::<DefaultHasher>::default().hash_one(parts)
}

/// Whether a tally of the library made when the catalog was at `revision` is still right, given that it reads only
/// `reads` of a photo: nothing changed since, or only other things of some photos did (a rating changes no folder's
/// count, a slider changes none at all). Each of these tallies asks every photo, so one kept is a pass over the
/// library saved on every edit.
fn still_right(cat: &lightcraft_catalog::Catalog, revision: u64, reads: lightcraft_catalog::Aspects) -> bool {
    revision == cat.revision || cat.aspects_since(revision).is_some_and(|changed| !changed.any_of(reads))
}

/// What the tallies below read of a photo, besides whether it is in the library.
const READS_FILE: lightcraft_catalog::Aspects = lightcraft_catalog::Aspects::PRESENCE.with(lightcraft_catalog::Aspects::FILE);

const READS_META: lightcraft_catalog::Aspects = lightcraft_catalog::Aspects::PRESENCE.with(lightcraft_catalog::Aspects::META);

/// The keyword tree: the keywords photos carry, and the keyword list's own.
const READS_KEYWORDS: lightcraft_catalog::Aspects = READS_META.with(lightcraft_catalog::Aspects::KEYWORD_LIST);

const READS_PEOPLE: lightcraft_catalog::Aspects = READS_META.with(lightcraft_catalog::Aspects::ANALYSIS);

const READS_CAPTURED: lightcraft_catalog::Aspects = lightcraft_catalog::Aspects::PRESENCE.with(lightcraft_catalog::Aspects::CAPTURED);

const READS_FLAG: lightcraft_catalog::Aspects = lightcraft_catalog::Aspects::PRESENCE.with(lightcraft_catalog::Aspects::FLAG);

impl Caches {
    /// The folders the library's photos were imported from.
    pub fn folder_tree(&mut self, cat: &lightcraft_catalog::Catalog) -> std::sync::Arc<Vec<lightcraft_catalog::FolderNode>> {
        let kept = self.folder_tree.take().filter(|(r, _)| still_right(cat, *r, READS_FILE)).map(|(_, t)| t);
        let t = kept.unwrap_or_else(|| std::sync::Arc::new(cat.folder_tree()));
        self.folder_tree = Some((cat.revision, t.clone()));
        t
    }
    /// The folder tree as last built, when it still holds (a read-only check cannot rebuild it).
    pub(crate) fn folder_tree_if_fresh(&self, cat: &lightcraft_catalog::Catalog) -> Option<std::sync::Arc<Vec<lightcraft_catalog::FolderNode>>> {
        self.folder_tree.as_ref().filter(|(r, _)| still_right(cat, *r, READS_FILE)).map(|(_, t)| t.clone())
    }
    /// The library's keyword tree.
    pub fn keyword_tree(&mut self, cat: &lightcraft_catalog::Catalog) -> std::sync::Arc<Vec<lightcraft_catalog::KeywordNode>> {
        let kept = self.keyword_tree.take().filter(|(r, _)| still_right(cat, *r, READS_KEYWORDS)).map(|(_, t)| t);
        let t = kept.unwrap_or_else(|| std::sync::Arc::new(cat.keyword_tree()));
        self.keyword_tree = Some((cat.revision, t.clone()));
        t
    }
    /// The Keywording box's read-only names for the selection: what exported files carry
    /// (`export`), or the keywords with those containing them.
    pub(crate) fn keyword_names(
        &mut self,
        cat: &lightcraft_catalog::Catalog,
        selection: &[lightcraft_catalog::PhotoId],
        export: bool,
    ) -> std::sync::Arc<Vec<panels::keywording::Chip>> {
        let key = key_of((selection, export));
        match &self.keyword_names {
            Some((r, k, c)) if *r == cat.revision && *k == key => c.clone(),
            _ => {
                let c = std::sync::Arc::new(if export {
                    panels::keywording::will_export(cat, selection)
                } else {
                    panels::keywording::with_containing(cat, selection)
                });
                self.keyword_names = Some((cat.revision, key, c.clone()));
                c
            }
        }
    }
    /// The selection's keywords (the Keywording box's chips).
    pub(crate) fn keyword_chips(
        &mut self,
        cat: &lightcraft_catalog::Catalog,
        selection: &[lightcraft_catalog::PhotoId],
    ) -> std::sync::Arc<Vec<panels::keywording::Chip>> {
        let key = key_of(selection);
        match &self.keyword_chips {
            Some((r, k, c)) if *r == cat.revision && *k == key => c.clone(),
            _ => {
                let c = std::sync::Arc::new(panels::keywording::chips(cat, selection));
                self.keyword_chips = Some((cat.revision, key, c.clone()));
                c
            }
        }
    }
    /// How many photos the selection is: each once, those the library has (what the Keywording
    /// box's "N of M" counts).
    pub(crate) fn selected_photos(&mut self, cat: &lightcraft_catalog::Catalog, selection: &[lightcraft_catalog::PhotoId]) -> usize {
        let key = key_of(selection);
        match self.selected_photos {
            Some((r, k, n)) if r == cat.revision && k == key => n,
            _ => {
                let n = panels::keywording::distinct(cat, selection).len();
                self.selected_photos = Some((cat.revision, key, n));
                n
            }
        }
    }
    /// The selection's keywords grouped by the keyword they are inside (the grouped Keywording box).
    pub(crate) fn keyword_groups(
        &mut self,
        cat: &lightcraft_catalog::Catalog,
        selection: &[lightcraft_catalog::PhotoId],
    ) -> std::sync::Arc<Vec<panels::keywording::Group>> {
        let key = key_of(selection);
        match &self.keyword_groups {
            Some((r, k, g)) if *r == cat.revision && *k == key => g.clone(),
            _ => {
                let chips = self.keyword_chips(cat, selection);
                let g = std::sync::Arc::new(panels::keywording::grouped(&chips));
                self.keyword_groups = Some((cat.revision, key, g.clone()));
                g
            }
        }
    }
    /// How many selected photos have each keyword (the Keyword List's tick boxes).
    pub(crate) fn keyword_ticks(
        &mut self,
        cat: &lightcraft_catalog::Catalog,
        selection: &[lightcraft_catalog::PhotoId],
    ) -> std::sync::Arc<panels::keyword_list::Ticks> {
        let key = key_of(selection);
        match &self.keyword_ticks {
            Some((r, k, t)) if *r == cat.revision && *k == key => t.clone(),
            _ => {
                let t = std::sync::Arc::new(panels::keyword_list::Ticks::of(cat, selection));
                self.keyword_ticks = Some((cat.revision, key, t.clone()));
                t
            }
        }
    }
    /// The people named on faces among the photos the filter lets through (its own `person` aside),
    /// with photo counts.
    pub fn people(
        &mut self,
        cat: &lightcraft_catalog::Catalog,
        filter: &lightcraft_catalog::Filter,
    ) -> std::sync::Arc<Vec<lightcraft_catalog::Person>> {
        let key = lightcraft_catalog::Filter { person: None, ..filter.clone() };
        match &self.people {
            Some((r, f, t)) if *r == cat.revision && *f == key => t.clone(),
            _ => {
                let t = std::sync::Arc::new(cat.people_in(&key));
                self.people = Some((cat.revision, key, t.clone()));
                t
            }
        }
    }
    /// All Photos / Picks / Recently Deleted counts.
    pub fn counts(&mut self, cat: &lightcraft_catalog::Catalog) -> LibraryCounts {
        if let Some((r, c)) = self.counts
            && still_right(cat, r, READS_FLAG)
        {
            self.counts = Some((cat.revision, c));
            return c;
        }
        let mut c = LibraryCounts::default();
        for p in cat.photos() {
            if p.in_library() {
                c.total += 1;
                if p.flag == lightcraft_catalog::Flag::Pick {
                    c.picks += 1;
                }
            } else if p.deleted && !p.local {
                c.deleted += 1;
            }
        }
        self.counts = Some((cat.revision, c));
        c
    }
    /// Everyone named on a face in the library (for completing a name as it is typed).
    pub fn person_names(&mut self, cat: &lightcraft_catalog::Catalog) -> std::sync::Arc<Vec<String>> {
        let kept = self.person_names.take().filter(|(r, _)| still_right(cat, *r, READS_PEOPLE)).map(|(_, v)| v);
        let v = kept.unwrap_or_else(|| std::sync::Arc::new(cat.people().into_iter().map(|p| p.name).collect::<Vec<_>>()));
        self.person_names = Some((cat.revision, v.clone()));
        v
    }
    /// The By Date tree.
    pub fn date_groups(&mut self, cat: &lightcraft_catalog::Catalog) -> std::sync::Arc<Vec<lightcraft_catalog::DateGroup>> {
        let kept = self.date_groups.take().filter(|(r, _)| still_right(cat, *r, READS_CAPTURED)).map(|(_, g)| g);
        let g = kept.unwrap_or_else(|| std::sync::Arc::new(cat.date_groups()));
        self.date_groups = Some((cat.revision, g.clone()));
        g
    }
    /// Cameras, lenses and keywords in the library (filter bar pickers).
    pub fn filter_values(&mut self, cat: &lightcraft_catalog::Catalog) -> std::sync::Arc<FilterValues> {
        match &self.filter_values {
            Some((r, v)) if *r == cat.revision => v.clone(),
            _ => {
                let distinct = |mut v: Vec<String>| {
                    v.retain(|s| !s.trim().is_empty());
                    v.sort_by_key(|s| s.to_lowercase());
                    v.dedup();
                    v
                };
                let lib = || cat.photos().filter(|p| p.in_library());
                let v = std::sync::Arc::new(FilterValues {
                    cameras: distinct(lib().map(|p| p.meta.camera.clone()).collect()),
                    lenses: distinct(lib().map(|p| p.meta.lens.clone()).collect()),
                    keywords: cat.keywords().into_iter().map(|(k, _)| k).collect(),
                });
                self.filter_values = Some((cat.revision, v.clone()));
                v
            }
        }
    }
    /// What the Add keyword box offers for `typed` (see [`panels::keywording::offers`]). Kept apart
    /// from [`Self::suggestions`]: both are asked for on every frame of typing, and each looks
    /// through the library's keywords.
    pub(crate) fn completions(
        &mut self,
        cat: &lightcraft_catalog::Catalog,
        current: &[String],
        typed: &str,
    ) -> std::sync::Arc<panels::keywording::Offers> {
        let k = key_of((cat.revision, current, typed));
        match &self.completions {
            Some((h, v)) if *h == k => v.clone(),
            _ => {
                let v = std::sync::Arc::new(panels::keywording::offers(cat, current, typed));
                self.completions = Some((k, v.clone()));
                v
            }
        }
    }
    /// Keyword suggestions for a photo with `current` keywords and the typed `prefix`.
    pub fn suggestions(&mut self, cat: &lightcraft_catalog::Catalog, current: &[String], prefix: &str, n: usize) -> std::sync::Arc<Vec<String>> {
        let k = key_of((cat.revision, current, prefix, n));
        match &self.suggestions {
            Some((h, v)) if *h == k => v.clone(),
            _ => {
                let v = std::sync::Arc::new(cat.keyword_suggestions(current, prefix, n));
                self.suggestions = Some((k, v.clone()));
                v
            }
        }
    }
    /// Every album's photo count for the sidebar (a smart album evaluates its rules, which
    /// scans the catalog). Recomputed when the catalog changes (photos, metadata, album rules
    /// all bump its revision) and — only while some smart album has an "in the last…" rule —
    /// when `now` (the session clock, ISO) enters a new minute, so such counts follow the clock
    /// within a minute without rescanning every frame.
    /// [`RulesView`] for `rules` edited in smart album `editing` (`None`: a new album), within
    /// `folder` when it was made from a folder view: worked out again only when the catalog, the
    /// rules, the language or (for "in the last…" rules) the minute changes, not every frame.
    pub fn rules_view(
        &mut self,
        cat: &lightcraft_catalog::Catalog,
        rules: &lightcraft_catalog::RuleSet,
        editing: Option<u64>,
        folder: Option<String>,
        now: &str,
    ) -> std::sync::Arc<RulesView> {
        let relative = rules.depends_on_now();
        let minute = if relative { now.get(..16).unwrap_or(now) } else { "" };
        let k = key_of((cat.revision, serde_json::to_string(rules).unwrap_or_default(), editing, &folder, minute, i18n::language().code()));
        if let Some((key, v)) = &self.rules_view
            && *key == k
        {
            return v.clone();
        }
        if relative {
            lightcraft_catalog::rules::set_now(Some(now.to_string()));
        }
        let problems = rules.check_for(cat, editing.map(lightcraft_catalog::AlbumId));
        let filter = lightcraft_catalog::Filter { rule_set: Some(rules.clone()), library_folder: folder, ..Default::default() };
        let count = if problems.is_empty() { cat.query(&filter, &Default::default()).len() } else { 0 };
        let v = std::sync::Arc::new(RulesView { problems, count, albums: panels::dialogs::album_entries(cat, editing) });
        self.rules_view_scans += 1;
        self.rules_view = Some((k, v.clone()));
        v
    }

    /// A count an open dialog shows (photos a filter matches, photos with a keyword…): `count` runs
    /// again only when the catalog or `what` (what is counted) changes, not every frame.
    pub(crate) fn dialog_count(&mut self, cat: &lightcraft_catalog::Catalog, what: impl std::hash::Hash, count: impl FnOnce() -> usize) -> usize {
        let k = key_of((cat.revision, what));
        if let Some((key, n)) = self.dialog_count
            && key == k
        {
            return n;
        }
        let n = count();
        self.dialog_count_scans += 1;
        self.dialog_count = Some((k, n));
        n
    }

    /// The problems of every saved smart album that has some (`Catalog::smart_album_problems`),
    /// for the sidebar's ⚠ marks: worked out again only when the catalog changes.
    pub fn smart_album_problems(
        &mut self,
        cat: &lightcraft_catalog::Catalog,
    ) -> std::sync::Arc<std::collections::HashMap<lightcraft_catalog::AlbumId, Vec<lightcraft_catalog::rules::Problem>>> {
        if let Some((rev, v)) = &self.smart_problems
            && *rev == cat.revision
        {
            return v.clone();
        }
        let v: std::sync::Arc<std::collections::HashMap<_, _>> = std::sync::Arc::new(
            cat.albums().filter(|a| a.is_smart()).map(|a| (a.id, cat.smart_album_problems(a.id))).filter(|(_, p)| !p.is_empty()).collect(),
        );
        self.smart_problem_scans += 1;
        self.smart_problems = Some((cat.revision, v.clone()));
        v
    }

    /// How many photos each album of the library `library` ([`lightcraft_engine::Session::library_generation`])
    /// shows, `now` being the session clock ("in the last…" rules count back from it).
    ///
    /// An edit to a photo or a few is followed at once: a smart album asks only those photos again (a keyword
    /// created and given to them too: the keyword list itself counts in no album). Anything more (an album's rules
    /// or photos changed, a stack, an edit to many photos such as a keyword renamed across them, a new minute while
    /// a smart album asks about "the last N days", another library) counts every photo again, seconds in a large
    /// library. That count runs on a worker thread (natively) while the counts shown before stay (none the first
    /// time, and none of another library); `ctx` repaints when it is done, and edits made meanwhile are followed
    /// from it.
    pub fn album_counts(
        &mut self,
        cat: &lightcraft_catalog::Catalog,
        library: u64,
        now: &str,
        ctx: &egui::Context,
    ) -> std::sync::Arc<std::collections::HashMap<lightcraft_catalog::AlbumId, usize>> {
        let relative = cat.albums().any(|a| a.smart.as_ref().is_some_and(|f| f.depends_on_now()));
        let minute = if relative { now.get(..16).unwrap_or(now) } else { "" };
        self.collect_album_recount(library);
        // (album ids start again in every library: another one's counts would be shown on this one's albums)
        if self.album_counts.as_ref().is_some_and(|c| c.library != library) {
            self.album_counts = None;
        }
        if let Some(counts) = self.album_counts.as_ref().filter(|c| c.minute == minute && c.tally.revision() == cat.revision) {
            return counts.shown.clone();
        }
        if relative {
            // "in the last N days" counts back from the session clock, as in the grid. (Within a minute an edit asks
            // only the photos it touched, at this second; the others were asked earlier in the minute. That is as
            // far out as the counts already were between edits, and the next minute counts them all again.)
            lightcraft_catalog::rules::set_now(Some(now.to_string()));
        }
        if let Some(counts) = self.album_counts.as_mut().filter(|c| c.minute == minute)
            && counts.tally.update(cat)
        {
            counts.shown = std::sync::Arc::new(counts.tally.counts().clone());
            return counts.shown.clone();
        }
        self.recount_albums(cat, library, minute, now, relative, ctx);
        self.album_counts.as_ref().map(|c| c.shown.clone()).unwrap_or_default()
    }

    /// A count of every photo is running ([`Caches::album_counts`]).
    pub fn album_recount_running(&self) -> bool {
        self.album_recount.as_ref().is_some_and(|job| !job.worker.is_finished())
    }

    /// Count every photo for [`Caches::album_counts`]: on a worker thread when one can be started, else here.
    fn recount_albums(&mut self, cat: &lightcraft_catalog::Catalog, library: u64, minute: &str, now: &str, relative: bool, ctx: &egui::Context) {
        // one at a time: what it finds is followed up to the catalog as it is by then, or counted again
        let failed = self.album_recount_failed.as_ref().is_some_and(|(l, r, m)| *l == library && *r == cat.revision && m == minute);
        if self.album_recount.is_some() || failed {
            return;
        }
        self.album_count_scans += 1;
        #[cfg(not(target_arch = "wasm32"))]
        {
            // (cheap: the copy shares the photos)
            let (copy, ctx, now) = (cat.clone(), ctx.clone(), relative.then(|| now.to_string()));
            let (tx, done) = std::sync::mpsc::channel();
            let spawned = std::thread::Builder::new().name("lc-album-counts".into()).spawn(move || {
                let counted = lightcraft_engine::guard::catch("Counting the albums' photos", || {
                    lightcraft_catalog::rules::set_now(now);
                    lightcraft_catalog::tally::AlbumTally::of(&copy)
                });
                let _ = tx.send(counted);
                ctx.request_repaint();
            });
            match spawned {
                Ok(worker) => {
                    self.album_recount = Some(AlbumRecount { library, minute: minute.to_string(), revision: cat.revision, done, worker });
                    return;
                }
                Err(e) => log::warn!("album counts: can't start a thread ({e}); counting them here"),
            }
        }
        #[cfg(target_arch = "wasm32")]
        let _ = (now, relative, ctx);
        // (`album_counts` has set the clock on this thread)
        let counted = lightcraft_engine::guard::catch("Counting the albums' photos", || lightcraft_catalog::tally::AlbumTally::of(cat));
        self.finish_album_recount(library, cat.revision, minute.to_string(), counted);
    }

    /// Take the worker's count once it is done; a count of another library is let go (its thread ends on its own).
    fn collect_album_recount(&mut self, library: u64) {
        let Some(job) = &self.album_recount else { return };
        if job.library != library {
            self.album_recount = None;
            return;
        }
        let counted = match job.done.try_recv() {
            Ok(counted) => counted,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                log::error!("album counts: the count stopped without an answer");
                Err("stopped without an answer".to_string())
            }
        };
        let Some(job) = self.album_recount.take() else { return };
        self.finish_album_recount(job.library, job.revision, job.minute, counted);
    }

    /// The counts a count of every photo made are the ones shown. One that failed (already logged) leaves those
    /// shown before, and isn't tried again for the same library, catalog and minute.
    fn finish_album_recount(&mut self, library: u64, revision: u64, minute: String, counted: Result<lightcraft_catalog::tally::AlbumTally, String>) {
        match counted {
            Ok(tally) => {
                self.album_recount_failed = None;
                let shown = std::sync::Arc::new(tally.counts().clone());
                self.album_counts = Some(AlbumCounts { library, minute, tally, shown });
            }
            Err(_) => self.album_recount_failed = Some((library, revision, minute)),
        }
    }
}

#[cfg(test)]
mod cache_tests {
    use lightcraft_catalog::{Album, AlbumId, Catalog, Filter, Op, Photo, PhotoId, RuleSet, Source};

    fn photo(id: u64, captured: &str, rating: u8) -> Op {
        let mut p = Photo::new(PhotoId(id), Source::Demo { scene: 0 }, &format!("p{id}.jpg"), "JPEG", 60, 40, "2026-01-01T00:00:00");
        p.captured = Some(captured.into());
        p.rating = rating;
        Op::AddPhoto { photo: Box::new(p) }
    }

    fn smart(id: u64, rules: serde_json::Value) -> Op {
        let mut a = Album::new(AlbumId(id), format!("smart {id}"));
        let rules: RuleSet = serde_json::from_value(rules).unwrap();
        a.smart = Some(Box::new(Filter { rule_set: Some(rules), ..Default::default() }));
        Op::AddAlbum { album: a }
    }

    type Counts = std::sync::Arc<std::collections::HashMap<AlbumId, usize>>;

    /// [`super::Caches::album_counts`] of library `library` once a count of every photo it started is done and
    /// taken.
    fn counted_in(c: &mut super::Caches, cat: &Catalog, library: u64, now: &str) -> Counts {
        let ctx = egui::Context::default();
        let t0 = std::time::Instant::now();
        loop {
            let shown = c.album_counts(cat, library, now, &ctx);
            if c.album_recount.is_none() {
                return shown;
            }
            assert!(t0.elapsed() < std::time::Duration::from_secs(30), "the count is done");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    fn counted(c: &mut super::Caches, cat: &Catalog, now: &str) -> Counts {
        counted_in(c, cat, 0, now)
    }

    /// Smart-album counts: unchanged frames reuse them; metadata and rule changes and (for
    /// "in the last…" rules) the clock crossing into a new minute recompute them.
    #[test]
    fn smart_album_counts_are_cached_until_something_changes() {
        let mut cat = Catalog::default();
        let mut c = super::Caches::default();
        for (i, d) in ["2026-09-30T11:59:30", "2026-09-29T08:00:00", "2026-01-01T08:00:00"].iter().enumerate() {
            cat.apply(photo(i as u64 + 1, d, if i == 0 { 5 } else { 1 })).unwrap();
        }
        cat.apply(smart(10, serde_json::json!({"rules": [{"field": "rating", "op": "gte", "value": 3}]}))).unwrap();
        cat.apply(smart(11, serde_json::json!({"rules": [{"field": "captureDate", "op": "inLast", "value": {"n": 1, "unit": "hours"}}]}))).unwrap();
        let now = "2026-09-30T12:00:00";
        let n = counted(&mut c, &cat, now);
        assert_eq!((n[&AlbumId(10)], n[&AlbumId(11)]), (1, 1));
        assert_eq!(c.album_count_scans, 1);
        for _ in 0..10 {
            counted(&mut c, &cat, "2026-09-30T12:00:40");
        }
        assert_eq!(c.album_count_scans, 1, "unchanged frames in the same minute don't rescan");
        // a photo rated: followed from that photo, not counted from every one
        cat.apply(Op::SetRating { id: PhotoId(2), rating: 4 }).unwrap();
        assert_eq!(counted(&mut c, &cat, now)[&AlbumId(10)], 2);
        assert_eq!(c.album_count_scans, 1);
        // rule change
        let rs: RuleSet = serde_json::from_value(serde_json::json!({"rules": [{"field": "rating", "op": "gte", "value": 5}]})).unwrap();
        cat.apply(Op::SetAlbumRules { id: AlbumId(10), rules: Box::new(Filter { rule_set: Some(rs), ..Default::default() }) }).unwrap();
        assert_eq!(counted(&mut c, &cat, now)[&AlbumId(10)], 1);
        // an hour later the 11:59:30 photo has left "in the last hour", with no catalog change
        assert_eq!(counted(&mut c, &cat, "2026-09-30T13:00:10")[&AlbumId(11)], 0);
        assert_eq!(c.album_count_scans, 3, "new rules and a new minute are counted from every photo; the rating was not");
        lightcraft_catalog::rules::set_now(None);
    }

    /// The rule dialog's problems, live count and album list are worked out once per change to the
    /// catalog or the rules, not every frame; so are the sidebar's smart-album problems.
    #[test]
    fn rule_dialog_and_problems_are_cached_until_something_changes() {
        let mut cat = Catalog::default();
        let mut c = super::Caches::default();
        for i in 0..3u64 {
            cat.apply(photo(i + 1, "2026-09-30T11:00:00", if i == 0 { 5 } else { 1 })).unwrap();
        }
        let rules =
            |v: u64| -> RuleSet { serde_json::from_value(serde_json::json!({"rules": [{"field": "rating", "op": "gte", "value": v}]})).unwrap() };
        let now = "2026-09-30T12:00:00";
        let view = c.rules_view(&cat, &rules(3), None, None, now);
        assert_eq!((view.count, view.problems.len()), (1, 0));
        for _ in 0..10 {
            c.rules_view(&cat, &rules(3), None, None, now);
        }
        assert_eq!(c.rules_view_scans, 1, "unchanged frames reuse it");
        assert_eq!(c.rules_view(&cat, &rules(1), None, None, now).count, 3, "a rule change");
        assert_eq!(c.rules_view(&cat, &rules(9), None, None, now).problems.len(), 1);
        cat.apply(Op::SetRating { id: PhotoId(2), rating: 4 }).unwrap();
        assert_eq!(c.rules_view(&cat, &rules(3), None, None, now).count, 2, "a catalog change");
        assert_eq!(c.rules_view_scans, 4);
        // the sidebar's problems
        cat.apply(Op::AddAlbum { album: lightcraft_catalog::Album::new(AlbumId(20), "Trip") }).unwrap();
        cat.apply(smart(21, serde_json::json!({"rules": [{"field": "album", "op": "is", "value": 20}]}))).unwrap();
        assert!(c.smart_album_problems(&cat).get(&AlbumId(21)).is_none_or(Vec::is_empty));
        for _ in 0..10 {
            c.smart_album_problems(&cat);
        }
        assert_eq!(c.smart_problem_scans, 1);
        cat.apply(Op::RemoveAlbum { id: AlbumId(20) }).unwrap();
        assert!(c.smart_album_problems(&cat).get(&AlbumId(21)).is_some_and(|p| !p.is_empty()), "its album is gone");
        assert_eq!(c.smart_problem_scans, 2);
    }

    /// Without "in the last…" rules the clock never causes a rescan.
    #[test]
    fn absolute_rules_ignore_the_clock() {
        let mut cat = Catalog::default();
        let mut c = super::Caches::default();
        cat.apply(photo(1, "2026-09-30T11:59:30", 5)).unwrap();
        cat.apply(smart(10, serde_json::json!({"rules": [{"field": "rating", "op": "gte", "value": 3}]}))).unwrap();
        counted(&mut c, &cat, "2026-09-30T12:00:00");
        counted(&mut c, &cat, "2027-01-01T00:00:00");
        assert_eq!(c.album_count_scans, 1);
    }

    /// Counting every photo again (an album's rules changed, an edit to many photos) takes seconds in a large
    /// library: it runs off the UI thread, and the sidebar keeps the counts it showed until the new ones are ready.
    #[test]
    fn a_count_of_every_photo_runs_in_the_background_and_the_counts_shown_stay_meanwhile() {
        let mut cat = Catalog::default();
        let mut c = super::Caches::default();
        for i in 0..3u64 {
            cat.apply(photo(i + 1, "2026-09-30T11:00:00", if i == 0 { 5 } else { 1 })).unwrap();
        }
        cat.apply(smart(10, serde_json::json!({"rules": [{"field": "rating", "op": "gte", "value": 3}]}))).unwrap();
        let now = "2026-09-30T12:00:00";
        assert_eq!(counted(&mut c, &cat, now)[&AlbumId(10)], 1);
        // new rules: every photo is asked again
        let rs: RuleSet = serde_json::from_value(serde_json::json!({"rules": [{"field": "rating", "op": "gte", "value": 1}]})).unwrap();
        cat.apply(Op::SetAlbumRules { id: AlbumId(10), rules: Box::new(Filter { rule_set: Some(rs), ..Default::default() }) }).unwrap();
        let ctx = egui::Context::default();
        assert_eq!(c.album_counts(&cat, 0, now, &ctx)[&AlbumId(10)], 1, "the frame after the change shows the counts it had");
        assert!(c.album_recount.is_some(), "and every photo is counted elsewhere");
        // done counting is said whether or not a frame has taken the count yet (a folded Albums section takes none)
        let t0 = std::time::Instant::now();
        while c.album_recount_running() {
            assert!(t0.elapsed() < std::time::Duration::from_secs(30), "the count ends");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        // a photo rated meanwhile: followed from what the count finds
        cat.apply(Op::SetRating { id: PhotoId(3), rating: 0 }).unwrap();
        assert_eq!(counted(&mut c, &cat, now)[&AlbumId(10)], 2, "three photos rated at least 1, then one not");
        assert_eq!(c.album_count_scans, 2, "counted whole once for the new rules; the rating was followed");
        // the first time there is nothing to show yet: no count, rather than a wrong one
        let mut fresh = super::Caches::default();
        assert!(fresh.album_counts(&cat, 0, now, &ctx).get(&AlbumId(10)).is_none());
        assert_eq!(counted(&mut fresh, &cat, now)[&AlbumId(10)], 2);
    }

    /// Album ids start again in every library: once another library is open, the albums of the one before never
    /// lend it their counts, nor does a count of that library still running when it closed.
    #[test]
    fn another_library_never_shows_the_counts_of_the_one_before() {
        let library = |rated: &[u8]| {
            let mut cat = Catalog::default();
            for (i, r) in rated.iter().enumerate() {
                cat.apply(photo(i as u64 + 1, "2026-09-30T11:00:00", *r)).unwrap();
            }
            cat.apply(smart(10, serde_json::json!({"rules": [{"field": "rating", "op": "gte", "value": 3}]}))).unwrap();
            cat
        };
        let (mut first, second) = (library(&[5, 1, 1]), library(&[5, 5, 1]));
        let (mut c, ctx, now) = (super::Caches::default(), egui::Context::default(), "2026-09-30T12:00:00");
        assert_eq!(counted_in(&mut c, &first, 1, now)[&AlbumId(10)], 1);
        assert!(c.album_counts(&second, 2, now, &ctx).get(&AlbumId(10)).is_none(), "nothing until its own are counted");
        assert_eq!(counted_in(&mut c, &second, 2, now)[&AlbumId(10)], 2);
        // a count of the first library under way when the second opens is let go
        let mut c = super::Caches::default();
        assert_eq!(counted_in(&mut c, &first, 1, now)[&AlbumId(10)], 1);
        let rs: RuleSet = serde_json::from_value(serde_json::json!({"rules": [{"field": "rating", "op": "gte", "value": 1}]})).unwrap();
        first.apply(Op::SetAlbumRules { id: AlbumId(10), rules: Box::new(Filter { rule_set: Some(rs), ..Default::default() }) }).unwrap();
        assert_eq!(c.album_counts(&first, 1, now, &ctx)[&AlbumId(10)], 1);
        assert!(c.album_recount.is_some(), "the first library is being counted");
        assert_eq!(counted_in(&mut c, &second, 2, now)[&AlbumId(10)], 2, "the second library's own count");
    }
}
