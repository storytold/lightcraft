use super::{Caches, album_picker, i18n, panels};

/// The left panel's counts.
#[derive(Clone, Copy, Debug, Default)]
pub struct LibraryCounts {
    pub total: usize,
    pub picks: usize,
    pub deleted: usize,
}

/// The values the filter bar's pickers offer.
#[derive(Clone, Debug, Default)]
pub struct FilterValues {
    pub cameras: Vec<String>,
    pub lenses: Vec<String>,
    pub keywords: Vec<String>,
}

/// What the smart-album rule dialog shows besides the rules themselves: their problems (each row
/// marks its own) and the albums an Album rule picks from. (How many photos the rules match is a [`Count`].)
pub struct RulesView {
    pub problems: Vec<lightcraft_catalog::rules::Problem>,
    pub albums: Vec<album_picker::AlbumEntry>,
}

/// How many photos a dialog's filter matches ([`LiveCount::of`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Count {
    /// The count of this filter, if one has been made (none while the first is still running, or after one failed).
    pub n: Option<usize>,
    /// The catalog has changed since `n` was counted, and a new count is waiting or running: the dialog marks it.
    pub updating: bool,
}

/// How many of a filter's counts may still be running after its filter changed (a count can't be stopped).
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
const MAX_LET_GO: usize = 2;

/// How long a frame waits for a count it has just started: a small library is counted in well under this, and shows
/// the count at once instead of a frame of "counting…" for each change.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
const QUICK: std::time::Duration = std::time::Duration::from_millis(4);

/// The longest a count of the same rules rests after one that took long: counts of a catalog that changes under them
/// follow each other no closer than the last one took (at most this), so one core isn't counting for a whole import.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
const MAX_REST: std::time::Duration = std::time::Duration::from_secs(2);

/// What a count was made of: the catalog revision, and the minute "in the last…" rules counted back from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    revision: u64,
    minute: u64,
}

/// A count of the photos a filter matches, made on a worker thread: counting every photo takes seconds in a large
/// library, and an import changes the catalog on every frame, so a count per catalog revision would hold the UI
/// thread (or the worker) for the whole import. The count is of what is counted (`what`: the filter and the folder
/// it carries), not of the catalog revision or the minute: while either changes the last count stays shown, marked
/// as updating, and the next one starts when the worker is free (and has rested, for a library so large that
/// counting it takes long). Where no thread can be had (wasm32) the count is made on the spot.
#[derive(Default)]
pub struct LiveCount {
    /// The last count to finish, with what it counted and when.
    done: Option<(u64, Stamp, usize)>,
    /// The count running now.
    job: Option<CountJob>,
    /// Counts of a filter that has since changed, still running: they end on their own, and new counts wait for
    /// them only when several are.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    let_go: Vec<std::thread::JoinHandle<()>>,
    /// The what and stamp of a count that failed (already logged): not tried again for the same catalog.
    failed: Option<(u64, Stamp)>,
    /// No count of the same rules starts before this: the last one's own duration after it ended.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    rest_until: Option<std::time::Instant>,
    /// How many counts ran on the worker, and how many on the UI thread (no thread to spare, or wasm).
    pub jobs: usize,
    pub ui_thread_scans: usize,
    /// Tests: how long each worker count waits before it counts (a large library's seconds, made small).
    #[cfg(test)]
    pub(crate) slow: Option<std::time::Duration>,
}

/// What a count answers: how many, and how long it took.
type Counted = Result<(usize, std::time::Duration), String>;

/// A count of every photo on a worker thread ([`LiveCount`]).
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
struct CountJob {
    what: u64,
    stamp: Stamp,
    /// The count, sent before the repaint is asked for, so the frame it wakes finds it.
    done: std::sync::mpsc::Receiver<Counted>,
    worker: std::thread::JoinHandle<()>,
}

impl LiveCount {
    /// The last count to finish, and the catalog revision it counted (tests).
    #[cfg(test)]
    pub(crate) fn last(&self) -> Option<(usize, u64)> {
        self.done.map(|(_, stamp, n)| (n, stamp.revision))
    }

    /// Whether a count is running on the worker (it has not finished, taken or not).
    pub(crate) fn running(&self) -> bool {
        self.job.as_ref().is_some_and(|job| !job.worker.is_finished())
    }

    /// The count of `what` (the filter `filter` makes) in `cat`: the one made, or the one before while a new one
    /// runs. "In the last…" rules count back from `now`, of which `minute` is the part that tells a new count from
    /// the last. `ctx` repaints when the worker is done.
    pub(crate) fn of(
        &mut self,
        cat: &lightcraft_catalog::Catalog,
        what: u64,
        minute: &str,
        filter: impl FnOnce() -> lightcraft_catalog::Filter,
        now: &str,
        ctx: &egui::Context,
    ) -> Count {
        self.collect();
        let stamp = Stamp { revision: cat.revision, minute: key_of(minute) };
        // a count of other rules is no use now: let it go (it ends on its own)
        if let Some(job) = self.job.take_if(|j| j.what != what) {
            self.let_go.push(job.worker);
        }
        let shown = self.done.filter(|(of, _, _)| *of == what);
        if let Some((_, at, n)) = shown
            && at == stamp
        {
            return Count { n: Some(n), updating: false };
        }
        self.let_go.retain(|w| !w.is_finished());
        if self.failed == Some((what, stamp)) {
            return Count::default();
        }
        let before = shown.map(|(_, _, n)| n);
        if self.job.is_none() && self.let_go.len() < MAX_LET_GO {
            // (new rules count at once; the same rules again, once the catalog has changed, after a rest)
            if before.is_some()
                && let Some(rest) = self.rest()
            {
                ctx.request_repaint_after(rest);
            } else if let Some(n) = self.start(cat, what, stamp, filter(), now, ctx) {
                return Count { n: Some(n), updating: false };
            }
        }
        Count { n: before, updating: true }
    }

    /// How much longer a count of the same rules waits, if it does.
    fn rest(&self) -> Option<std::time::Duration> {
        // (none on wasm32, where `Instant::now` can't be asked)
        self.rest_until?.checked_duration_since(std::time::Instant::now())
    }

    /// Start a count of every photo on a worker thread, and say how many if it ends at once; or, where there is no
    /// thread to be had, count them here.
    fn start(
        &mut self,
        cat: &lightcraft_catalog::Catalog,
        what: u64,
        stamp: Stamp,
        filter: lightcraft_catalog::Filter,
        now: &str,
        ctx: &egui::Context,
    ) -> Option<usize> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            // (the copy is of the photo pointers, not the photos: some milliseconds in a library of millions)
            let (copy, ctx, now) = (cat.clone(), ctx.clone(), now.to_string());
            #[cfg(test)]
            let slow = self.slow;
            let (tx, done) = std::sync::mpsc::channel();
            let spawned = std::thread::Builder::new().name("lc-rules-count".into()).spawn({
                let filter = filter.clone();
                move || {
                    let began = std::time::Instant::now();
                    #[cfg(test)]
                    if let Some(wait) = slow {
                        std::thread::sleep(wait);
                    }
                    let counted = lightcraft_engine::guard::catch("Counting the photos a filter matches", || {
                        lightcraft_catalog::rules::set_now(Some(now));
                        copy.count(&filter)
                    });
                    let _ = tx.send(counted.map(|n| (n, began.elapsed())));
                    ctx.request_repaint();
                }
            });
            match spawned {
                Ok(worker) => {
                    self.jobs += 1;
                    return match done.recv_timeout(QUICK) {
                        Ok(counted) => self.finish(what, stamp, counted),
                        Err(_) => {
                            self.job = Some(CountJob { what, stamp, done, worker });
                            None
                        }
                    };
                }
                Err(e) => log::warn!("photo count: can't start a thread ({e}); counting here"),
            }
        }
        #[cfg(target_arch = "wasm32")]
        let _ = ctx;
        self.ui_thread_scans += 1;
        lightcraft_catalog::rules::set_now(Some(now.to_string()));
        let counted = lightcraft_engine::guard::catch("Counting the photos a filter matches", || cat.count(&filter));
        self.finish(what, stamp, counted.map(|n| (n, std::time::Duration::ZERO)))
    }

    /// Take the worker's count once it is done.
    fn collect(&mut self) {
        let Some(job) = &self.job else { return };
        let counted = match job.done.try_recv() {
            Ok(counted) => counted,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                log::error!("photo count: the count stopped without an answer");
                Err("stopped without an answer".to_string())
            }
        };
        let Some(job) = self.job.take() else { return };
        self.finish(job.what, job.stamp, counted);
    }

    /// A count of `what` ended: shown from now on, or (when it failed) not tried again for this catalog. Says how many.
    fn finish(&mut self, what: u64, stamp: Stamp, counted: Counted) -> Option<usize> {
        match counted {
            Ok((n, took)) => {
                self.failed = None;
                self.done = Some((what, stamp, n));
                // (only a count that ran on a thread took any time; on wasm32 there is none to ask the time of)
                if !took.is_zero() {
                    self.rest_until = Some(std::time::Instant::now() + took.min(MAX_REST));
                }
                Some(n)
            }
            Err(_) => {
                self.failed = Some((what, stamp));
                None
            }
        }
    }
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
        let kept = self.keyword_tree.take().filter(|(r, _)| still_right(cat, *r, READS_META)).map(|(_, t)| t);
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
    /// rules, the language or (for "in the last…" rules) the minute changes, not every frame. How many photos the
    /// rules match is counted on a worker thread ([`LiveCount`]): the [`Count`] is the last one made while the
    /// catalog changes under it (an import commits a batch per frame), and none while a rule edit's count is
    /// still running.
    pub fn rules_view(
        &mut self,
        cat: &lightcraft_catalog::Catalog,
        rules: &lightcraft_catalog::RuleSet,
        editing: Option<u64>,
        folder: Option<String>,
        now: &str,
        ctx: &egui::Context,
    ) -> (std::sync::Arc<RulesView>, Count) {
        let relative = rules.depends_on_now();
        let minute = if relative { now.get(..16).unwrap_or(now) } else { "" };
        let written = serde_json::to_string(rules).unwrap_or_default();
        let k = key_of((cat.revision, &written, editing, &folder, minute, i18n::language().code()));
        let view = match &self.rules_view {
            Some((key, v)) if *key == k => v.clone(),
            _ => {
                // (problems and the albums to pick from look at the rules and the albums, not at every photo)
                let problems = rules.check_for(cat, editing.map(lightcraft_catalog::AlbumId));
                let v = std::sync::Arc::new(RulesView { problems, albums: panels::dialogs::album_entries(cat, editing) });
                self.rules_view = Some((k, v.clone()));
                v
            }
        };
        if !view.problems.is_empty() {
            return (view, Count::default());
        }
        let what = key_of((&written, &folder));
        let filter = || lightcraft_catalog::Filter { rule_set: Some(rules.clone()), library_folder: folder.clone(), ..Default::default() };
        let count = self.rules_count.of(cat, what, minute, filter, now, ctx);
        (view, count)
    }

    /// How many photos `filter` matches, for Create Smart Album's live count: counted on a worker thread like
    /// [`Caches::rules_view`]'s.
    pub(crate) fn smart_album_count(
        &mut self,
        cat: &lightcraft_catalog::Catalog,
        filter: &lightcraft_catalog::Filter,
        now: &str,
        ctx: &egui::Context,
    ) -> Count {
        let minute = if filter.depends_on_now() { now.get(..16).unwrap_or(now) } else { "" };
        let what = key_of(format!("{filter:?}"));
        self.new_smart_album_count.of(cat, what, minute, || filter.clone(), now, ctx)
    }

    /// Whether a dialog's count is running on a worker (a frame may still have to take it).
    pub fn count_running(&self) -> bool {
        self.rules_count.running() || self.new_smart_album_count.running()
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

    pub fn album_counts(
        &mut self,
        cat: &lightcraft_catalog::Catalog,
        now: &str,
    ) -> std::sync::Arc<std::collections::HashMap<lightcraft_catalog::AlbumId, usize>> {
        let relative = cat.albums().any(|a| a.smart.as_ref().is_some_and(|f| f.depends_on_now()));
        let minute = if relative { now.get(..16).unwrap_or(now) } else { "" };
        // the counts of this minute, if there are any: an edit to a photo or a few is followed from them, a smart
        // album asking only those photos again; anything more, or a new minute, is counted from every photo
        let kept = self.album_counts.take().filter(|(of, _, _)| of == minute);
        if let Some((_, tally, shown)) = &kept
            && tally.revision() == cat.revision
        {
            let shown = shown.clone();
            self.album_counts = kept;
            return shown;
        }
        if relative {
            // "in the last N days" counts back from the session clock, as in the grid. (Within a minute an edit asks
            // only the photos it touched, at this second; the others were asked earlier in the minute. That is as
            // far out as the counts already were between edits, and the next minute counts them all again.)
            lightcraft_catalog::rules::set_now(Some(now.to_string()));
        }
        let tally = match kept {
            Some((_, mut tally, _)) => {
                if !tally.update(cat) {
                    self.album_count_scans += 1;
                }
                tally
            }
            None => {
                self.album_count_scans += 1;
                lightcraft_catalog::tally::AlbumTally::of(cat)
            }
        };
        let shown = std::sync::Arc::new(tally.counts().clone());
        self.album_counts = Some((minute.to_string(), tally, shown.clone()));
        shown
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
        let n = c.album_counts(&cat, now);
        assert_eq!((n[&AlbumId(10)], n[&AlbumId(11)]), (1, 1));
        assert_eq!(c.album_count_scans, 1);
        for _ in 0..10 {
            c.album_counts(&cat, "2026-09-30T12:00:40");
        }
        assert_eq!(c.album_count_scans, 1, "unchanged frames in the same minute don't rescan");
        // a photo rated: followed from that photo, not counted from every one
        cat.apply(Op::SetRating { id: PhotoId(2), rating: 4 }).unwrap();
        assert_eq!(c.album_counts(&cat, now)[&AlbumId(10)], 2);
        assert_eq!(c.album_count_scans, 1);
        // rule change
        let rs: RuleSet = serde_json::from_value(serde_json::json!({"rules": [{"field": "rating", "op": "gte", "value": 5}]})).unwrap();
        cat.apply(Op::SetAlbumRules { id: AlbumId(10), rules: Box::new(Filter { rule_set: Some(rs), ..Default::default() }) }).unwrap();
        assert_eq!(c.album_counts(&cat, now)[&AlbumId(10)], 1);
        // an hour later the 11:59:30 photo has left "in the last hour", with no catalog change
        assert_eq!(c.album_counts(&cat, "2026-09-30T13:00:10")[&AlbumId(11)], 0);
        assert_eq!(c.album_count_scans, 3, "new rules and a new minute are counted from every photo; the rating was not");
        lightcraft_catalog::rules::set_now(None);
    }

    fn rule_set(v: u64) -> RuleSet {
        serde_json::from_value(serde_json::json!({"rules": [{"field": "rating", "op": "gte", "value": v}]})).unwrap()
    }

    type Shown = (std::sync::Arc<super::RulesView>, super::Count);

    /// [`super::Caches::rules_view`] of `rules` once the count it started is done and taken.
    fn rules_counted(c: &mut super::Caches, cat: &Catalog, rules: &RuleSet, now: &str) -> Shown {
        let ctx = egui::Context::default();
        let t0 = std::time::Instant::now();
        loop {
            let shown = c.rules_view(cat, rules, None, None, now, &ctx);
            if !shown.1.updating {
                return shown;
            }
            assert!(t0.elapsed() < std::time::Duration::from_secs(30), "the count is done");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    fn rated_library() -> Catalog {
        let mut cat = Catalog::default();
        for i in 0..3u64 {
            cat.apply(photo(i + 1, "2026-09-30T11:00:00", if i == 0 { 5 } else { 1 })).unwrap();
        }
        cat
    }

    /// The rule dialog's problems and album list are worked out once per change to the catalog or the rules, not
    /// every frame; so are the sidebar's smart-album problems. Its count is made once per rules (and catalog).
    #[test]
    fn rule_dialog_and_problems_are_cached_until_something_changes() {
        let mut cat = rated_library();
        let mut c = super::Caches::default();
        let now = "2026-09-30T12:00:00";
        let (view, count) = rules_counted(&mut c, &cat, &rule_set(3), now);
        assert_eq!((count.n, view.problems.len()), (Some(1), 0));
        let jobs = c.rules_count.jobs;
        for _ in 0..10 {
            rules_counted(&mut c, &cat, &rule_set(3), now);
        }
        assert_eq!(c.rules_count.jobs, jobs, "unchanged frames reuse it");
        assert_eq!(rules_counted(&mut c, &cat, &rule_set(1), now).1.n, Some(3), "a rule change");
        let (view, count) = rules_counted(&mut c, &cat, &rule_set(9), now);
        assert_eq!((view.problems.len(), count), (1, super::Count::default()), "rules with a problem count nothing");
        cat.apply(Op::SetRating { id: PhotoId(2), rating: 4 }).unwrap();
        assert_eq!(rules_counted(&mut c, &cat, &rule_set(3), now).1.n, Some(2), "a catalog change");
        assert_eq!(c.rules_count.ui_thread_scans, 0);
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

    const SLOW: std::time::Duration = std::time::Duration::from_millis(120);

    /// Counting every photo takes seconds in a large library: it runs on a worker, never on the UI thread, and
    /// while the catalog changes (an import commits a batch per frame) the count made before stays, marked as
    /// updating, until the worker is free to make the next.
    #[test]
    fn the_rules_count_is_made_on_a_worker_and_follows_a_catalog_that_changes_every_frame() {
        let mut cat = rated_library();
        let mut c = super::Caches::default();
        c.rules_count.slow = Some(SLOW);
        let ctx = egui::Context::default();
        let now = "2026-09-30T12:00:00";
        let rules = rule_set(3);
        let first = c.rules_view(&cat, &rules, None, None, now, &ctx).1;
        assert_eq!(first, super::Count { n: None, updating: true }, "a count that takes a while has none to show yet");
        assert_eq!((c.rules_count.jobs, c.rules_count.ui_thread_scans), (1, 0));
        assert_eq!(rules_counted(&mut c, &cat, &rules, now).1, super::Count { n: Some(1), updating: false });
        // a revision a frame: the last count stays, and nothing is counted on this thread
        let (jobs, began) = (c.rules_count.jobs, std::time::Instant::now());
        let mut marked = 0;
        for i in 0..40u64 {
            cat.apply(Op::SetRating { id: PhotoId(1 + i % 3), rating: if i % 2 == 0 { 5 } else { 4 } }).unwrap();
            let count = c.rules_view(&cat, &rules, None, None, now, &ctx).1;
            assert!(count.n.is_some(), "the count made before is shown while the next is made");
            marked += usize::from(count.updating);
            std::thread::sleep(SLOW / 10);
        }
        assert_eq!(marked, 40, "every frame says the count is out of date");
        assert_eq!(c.rules_count.ui_thread_scans, 0, "no frame counted the library");
        let most = began.elapsed().as_millis() / SLOW.as_millis() + 1;
        assert!((c.rules_count.jobs - jobs) as u128 <= most, "one count at a time: {} in {most} counts' time", c.rules_count.jobs - jobs);
        // the changes stop: the count reaches the right value
        let right = cat.count(&Filter { rule_set: Some(rules.clone()), ..Default::default() });
        assert_eq!(rules_counted(&mut c, &cat, &rules, now).1, super::Count { n: Some(right), updating: false });
    }

    /// An edit to the rules shows the new rules' count once it is made, never the old rules' meanwhile; going back
    /// to the rules before keeps their count.
    #[test]
    fn a_rule_edit_shows_its_own_count_not_the_one_before() {
        let cat = rated_library();
        let mut c = super::Caches::default();
        c.rules_count.slow = Some(SLOW);
        let ctx = egui::Context::default();
        let now = "2026-09-30T12:00:00";
        assert_eq!(rules_counted(&mut c, &cat, &rule_set(3), now).1.n, Some(1));
        let edited = c.rules_view(&cat, &rule_set(1), None, None, now, &ctx).1;
        assert_eq!(edited, super::Count { n: None, updating: true }, "not the 1 of the rules before");
        // back to the first rules while the edit is being counted: their count is as it was, and stays so when the
        // edit's count ends
        let back = c.rules_view(&cat, &rule_set(3), None, None, now, &ctx).1;
        assert_eq!(back, super::Count { n: Some(1), updating: false });
        std::thread::sleep(SLOW * 3);
        assert_eq!(c.rules_view(&cat, &rule_set(3), None, None, now, &ctx).1, super::Count { n: Some(1), updating: false });
        assert_eq!(c.rules_count.jobs, 2, "no count of them again");
        // edits typed faster than counts end: the last rules' count is the one that shows
        for v in [2, 3, 4, 5, 1] {
            c.rules_view(&cat, &rule_set(v), None, None, now, &ctx);
        }
        assert_eq!(rules_counted(&mut c, &cat, &rule_set(1), now).1, super::Count { n: Some(3), updating: false });
        assert_eq!(c.rules_count.ui_thread_scans, 0);
    }

    /// "In the last…" rules count back from the session clock on the worker too; a new minute is a count out of date
    /// like a change to the catalog (the count before stays, marked), not new rules.
    #[test]
    fn a_new_minute_marks_the_count_and_the_worker_counts_back_from_the_session_clock() {
        let mut cat = Catalog::default();
        for i in 0..3u64 {
            cat.apply(photo(i + 1, "2026-09-30T11:30:00", 1)).unwrap();
        }
        let mut c = super::Caches::default();
        c.rules_count.slow = Some(SLOW);
        let ctx = egui::Context::default();
        let last_hour: RuleSet =
            serde_json::from_value(serde_json::json!({"rules": [{"field": "captureDate", "op": "inLast", "value": {"n": 1, "unit": "hours"}}]}))
                .unwrap();
        // (the session's clock, not the machine's, which is long past these photos)
        let t0 = std::time::Instant::now();
        let mut shown = c.rules_view(&cat, &last_hour, None, None, "2026-09-30T12:00:00", &ctx).1;
        while shown.updating {
            assert!(t0.elapsed() < std::time::Duration::from_secs(30), "the count is done");
            std::thread::sleep(std::time::Duration::from_millis(1));
            shown = c.rules_view(&cat, &last_hour, None, None, "2026-09-30T12:00:00", &ctx).1;
        }
        assert_eq!(shown.n, Some(3), "counted back from the session clock");
        let later = c.rules_view(&cat, &last_hour, None, None, "2026-09-30T12:40:00", &ctx).1;
        assert_eq!(later, super::Count { n: Some(3), updating: true }, "the minute changed: the count before, marked");
        assert_eq!(rules_counted(&mut c, &cat, &last_hour, "2026-09-30T12:40:00").1.n, Some(0), "an hour and ten minutes on");
        lightcraft_catalog::rules::set_now(None);
    }

    /// A count of the same rules waits a while after one that took long, so a catalog that changes every frame
    /// doesn't keep a core counting.
    #[test]
    fn counts_of_the_same_rules_rest_between_them() {
        let mut cat = rated_library();
        let mut c = super::Caches::default();
        c.rules_count.slow = Some(SLOW);
        let ctx = egui::Context::default();
        let now = "2026-09-30T12:00:00";
        let rules = rule_set(3);
        assert_eq!(rules_counted(&mut c, &cat, &rules, now).1.n, Some(1));
        let jobs = c.rules_count.jobs;
        // (the count took SLOW: the next waits as long)
        cat.apply(Op::SetRating { id: PhotoId(2), rating: 4 }).unwrap();
        assert_eq!(c.rules_view(&cat, &rules, None, None, now, &ctx).1, super::Count { n: Some(1), updating: true });
        assert_eq!(c.rules_count.jobs, jobs, "resting");
        let t0 = std::time::Instant::now();
        while c.rules_count.jobs == jobs {
            assert!(t0.elapsed() < std::time::Duration::from_secs(30), "the rest ends");
            std::thread::sleep(std::time::Duration::from_millis(5));
            assert_eq!(c.rules_view(&cat, &rules, None, None, now, &ctx).1, super::Count { n: Some(1), updating: true });
        }
        assert_eq!(c.rules_count.jobs, jobs + 1, "rested, counting");
        assert_eq!(rules_counted(&mut c, &cat, &rules, now).1.n, Some(2));
    }

    /// Create Smart Album's count is made the same way.
    #[test]
    fn the_new_smart_album_count_is_made_on_a_worker_too() {
        let mut cat = rated_library();
        let mut c = super::Caches::default();
        c.new_smart_album_count.slow = Some(SLOW);
        let ctx = egui::Context::default();
        let now = "2026-09-30T12:00:00";
        let filter = Filter { rule_set: Some(rule_set(3)), ..Default::default() };
        let t0 = std::time::Instant::now();
        let mut count = c.smart_album_count(&cat, &filter, now, &ctx);
        assert_eq!(count.n, None);
        while count.updating {
            assert!(t0.elapsed() < std::time::Duration::from_secs(30), "the count is done");
            std::thread::sleep(std::time::Duration::from_millis(1));
            count = c.smart_album_count(&cat, &filter, now, &ctx);
        }
        assert_eq!(count.n, Some(1));
        cat.apply(Op::SetRating { id: PhotoId(2), rating: 4 }).unwrap();
        std::thread::sleep(SLOW * 2);
        assert_eq!(c.smart_album_count(&cat, &filter, now, &ctx), super::Count { n: Some(1), updating: true }, "the count before, marked");
        assert_eq!(c.new_smart_album_count.ui_thread_scans, 0);
    }

    /// Without "in the last…" rules the clock never causes a rescan.
    #[test]
    fn absolute_rules_ignore_the_clock() {
        let mut cat = Catalog::default();
        let mut c = super::Caches::default();
        cat.apply(photo(1, "2026-09-30T11:59:30", 5)).unwrap();
        cat.apply(smart(10, serde_json::json!({"rules": [{"field": "rating", "op": "gte", "value": 3}]}))).unwrap();
        c.album_counts(&cat, "2026-09-30T12:00:00");
        c.album_counts(&cat, "2027-01-01T00:00:00");
        assert_eq!(c.album_count_scans, 1);
    }
}
