//! Preparing photos in the background (PERF-PREVIEW-PREP, notes 47 and 48): while the user culls,
//! the photos ahead are made ready before they are reached, and with the machine otherwise idle
//! the previews of every photo in view are built, so stepping never waits for a render.
//!
//! What is prepared is set per mode, because each mode needs something else:
//! - **Culling** (a folder browsed before import, the Import tab): previews of every photo in the
//!   folder, the next photos' loupe renders and their eye focus strips.
//! - **Library**: the same for the photos in the library's view.
//! - **Develop** (editing): the next photos decoded and kept ready to edit.
//!
//! Each mode has its own switch for the whole-view build, how many photos ahead to prepare (0 =
//! none) and which photos ([`Scope`]). The settings are app-wide (the desktop app keeps them in `ui.json`)
//! and a library can keep its own (its `prefs.json`): a timelapse library prepares nothing while a
//! wedding library prepares everything.
//!
//! The whole-view build is the preview build ([`crate::cmd::previews`]) in its background form: two
//! lanes, waiting whenever the photo on screen is being made ([`foreground`]), nearest photos to the
//! user first and re-aimed as they move ([`crate::cmd::previews::PreviewBuild::focus_on`]), photos
//! with previews skipped, one row in the activity stack that can be cancelled. It starts once the
//! user is culling (a few photos stepped through in the loupe), never just because a library was
//! opened (note 36: opening an old library to look at two photos must not start rebuilding).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use lightcraft_catalog::PhotoId;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::Session;
use crate::cmd::previews::PreviewBuild;

/// Which photos a mode prepares.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Scope {
    /// Every photo in view (the folder, the collection, the filter).
    #[default]
    All,
    /// Only picks, and photos with stars or a colour label.
    Marked,
    /// Only the photo on screen: nothing ahead, nothing in the background.
    OnScreen,
}

impl Scope {
    pub fn parse(s: &str) -> Option<Scope> {
        match s {
            "all" => Some(Scope::All),
            "marked" => Some(Scope::Marked),
            "onScreen" => Some(Scope::OnScreen),
            _ => None,
        }
    }
}

/// The most photos prepared ahead of the one on screen.
pub const MAX_AHEAD: u8 = 20;

/// What one mode prepares.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ModePrep {
    /// Build the previews of every photo in scope in the background.
    pub previews: bool,
    /// Photos ahead (in the direction of travel) whose loupe render is made before they are reached (0 = none).
    pub ahead: u8,
    /// Photos ahead whose eye focus strip is worked out before they are reached (0 = only the one on
    /// screen), for a frontend that shows one (the desktop app doesn't yet).
    pub eye_ahead: u8,
    pub scope: Scope,
}

impl Default for ModePrep {
    fn default() -> Self {
        ModePrep { previews: true, ahead: 5, eye_ahead: 8, scope: Scope::All }
    }
}

/// Editing: what is kept ready for the next photos.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DevelopPrep {
    /// Photos ahead decoded and kept ready to edit (0 = none).
    pub ahead: u8,
    pub scope: Scope,
}

impl Default for DevelopPrep {
    fn default() -> Self {
        DevelopPrep { ahead: 2, scope: Scope::All }
    }
}

/// The background preparation settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PrepSettings {
    pub culling: ModePrep,
    pub library: ModePrep,
    pub develop: DevelopPrep,
}

impl PrepSettings {
    /// Values from files or agents kept in range.
    pub fn sanitized(mut self) -> Self {
        for m in [&mut self.culling, &mut self.library] {
            m.ahead = m.ahead.min(MAX_AHEAD);
            m.eye_ahead = m.eye_ahead.min(MAX_AHEAD);
        }
        self.develop.ahead = self.develop.ahead.min(MAX_AHEAD);
        self
    }

    /// The settings of a browsing mode ([`Mode::Culling`] or [`Mode::Library`]; Develop prepares
    /// views the way the Library does).
    pub fn mode(&self, mode: Mode) -> ModePrep {
        match mode {
            Mode::Culling => self.culling,
            Mode::Library | Mode::Develop => self.library,
        }
    }

    /// Photos ahead to render for the loupe in `mode` (0 when its scope is the photo on screen).
    pub fn ahead(&self, mode: Mode) -> usize {
        match mode {
            Mode::Develop if self.develop.scope == Scope::OnScreen => 0,
            Mode::Develop => usize::from(self.develop.ahead),
            m => {
                let p = self.mode(m);
                if p.scope == Scope::OnScreen { 0 } else { usize::from(p.ahead) }
            }
        }
    }

    /// Photos ahead whose eye focus strip is worked out in `mode`.
    pub fn eye_ahead(&self, mode: Mode) -> usize {
        let p = self.mode(mode);
        if p.scope == Scope::OnScreen { 0 } else { usize::from(p.eye_ahead) }
    }

    /// The scope that applies in `mode`.
    pub fn scope(&self, mode: Mode) -> Scope {
        match mode {
            Mode::Develop => self.develop.scope,
            m => self.mode(m).scope,
        }
    }

    /// Apply the fields of `patch` (a partial object, as `prefs.setBackgroundPrep` takes it).
    pub fn patched(&self, patch: &Value) -> Result<PrepSettings, String> {
        let mut v = serde_json::to_value(self).map_err(|e| e.to_string())?;
        merge(&mut v, patch, 0);
        let p: PrepSettings = serde_json::from_value(v).map_err(|e| format!("background preparation settings: {e}"))?;
        Ok(p.sanitized())
    }
}

/// `patch` merged into `into`, objects field by field (two levels deep: mode → field).
fn merge(into: &mut Value, patch: &Value, depth: usize) {
    match (into, patch) {
        (Value::Object(a), Value::Object(b)) if depth < 3 => {
            for (k, v) in b {
                match a.get_mut(k) {
                    Some(old) => merge(old, v, depth + 1),
                    None => {
                        a.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (a, b) => *a = b.clone(),
    }
}

/// What the user is doing, as far as preparation goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Mode {
    Culling,
    Library,
    Develop,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "culling" => Some(Mode::Culling),
            "library" => Some(Mode::Library),
            "develop" => Some(Mode::Develop),
            _ => None,
        }
    }

    /// The mode of what `s` shows when the frontend doesn't say: a folder browsed before import is
    /// culling, anything else the library.
    pub fn of(s: &Session) -> Mode {
        if s.source == crate::LibrarySource::Folder { Mode::Culling } else { Mode::Library }
    }
}

/// Photos the user steps through in the loupe before the whole view is prepared: a library opened
/// to look at one photo starts nothing.
pub const STEPS_BEFORE_BUILD: usize = 3;
/// How long a view whose build couldn't start waits before trying again.
pub const RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(30);

/// The background preparation's state in a session.
#[derive(Default)]
pub struct PrepState {
    /// App-wide settings.
    pub app: PrepSettings,
    /// This library's own (`None`: it uses the app-wide ones).
    pub library: Option<PrepSettings>,
    /// The background build this preparation started.
    build: Option<Arc<PreviewBuild>>,
    /// The view (its signature) whose build was started, or that the user stopped.
    started_for: Option<u64>,
    declined_for: Option<u64>,
    /// A build that couldn't start: the view (its signature), and when it may try again (not on
    /// every tick while the cause lasts; CodeRabbit on #3080).
    pub retry_at: Option<(u64, web_time::Instant)>,
    /// Tests: the next starts to fail, as a build thread that can't be spawned does.
    #[cfg(test)]
    pub(crate) fail_starts: usize,
    /// The view the steps were counted in, the photo last seen, and the steps.
    steps_in: Option<u64>,
    last_at: Option<PhotoId>,
    steps: usize,
    /// The library this state is of ([`Session::library_generation`]): another one open (opened, or opened again)
    /// starts it afresh ([`Session::follow_library`]), though its view may look the same (the same photo ids, as
    /// many photos).
    library_gen: Option<u64>,
    /// The signature of the visible list of a generation ([`Session::view_signature`]).
    signed: Option<(u64, Scope, u64)>,
    /// The build whose photos are being planned a slice at a time, and whether the last slice left
    /// photos to plan (the app draws another frame for the next).
    plan: Option<Planning>,
    advancing: bool,
    /// Tests: signatures worked out, photos planned, and the photos a slice plans.
    #[cfg(test)]
    pub(crate) signatures: usize,
    #[cfg(test)]
    pub(crate) planned: usize,
    #[cfg(test)]
    pub(crate) slice: Option<usize>,
}

/// A background build whose photos are being planned ([`crate::cmd::previews::BuildPlan`]): the
/// view it is for (its signature), the plan, and what the jobs planned so far were made against: the
/// library's preview cache (as it was: its `generation`) and the catalog at `revision` (as of the last tick that found
/// the plan current).
/// A plan that no longer matches them ([`Session::plan_outdated`]) is planned again.
struct Planning {
    sig: u64,
    plan: crate::cmd::previews::BuildPlan,
    cache: Arc<lightcraft_preview::PreviewCache>,
    generation: u64,
    revision: u64,
}

impl std::fmt::Debug for PrepState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrepState").field("app", &self.app).field("library", &self.library).field("steps", &self.steps).finish()
    }
}

impl PrepState {
    /// The settings in use.
    pub fn settings(&self) -> PrepSettings {
        self.library.unwrap_or(self.app)
    }
}

/// Until when (ms on [`clock_ms`]) the photo on screen is being made: background work waits.
static FOREGROUND_UNTIL: AtomicU64 = AtomicU64::new(0);

/// How long a foreground signal holds background work back (ms): a little more than a frame
/// apart, so a frontend saying it every frame holds it back the whole time.
const FOREGROUND_HOLD_MS: u64 = 250;

fn clock_ms() -> u64 {
    static START: std::sync::OnceLock<web_time::Instant> = std::sync::OnceLock::new();
    u64::try_from(START.get_or_init(web_time::Instant::now).elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// The frontend says the photo on screen is being made (a render it waits for is on its way):
/// background preparation waits until it has been quiet for a moment.
pub fn foreground() {
    FOREGROUND_UNTIL.store(clock_ms().saturating_add(FOREGROUND_HOLD_MS), Ordering::Relaxed);
}

/// Whether the photo on screen is being made now.
pub fn foreground_busy() -> bool {
    #[cfg(test)]
    if TEST_HOLD.load(Ordering::Relaxed) > 0 {
        return true;
    }
    clock_ms() < FOREGROUND_UNTIL.load(Ordering::Relaxed)
}

/// Tests: hold every background build at its foreground wait until released (a synchronization
/// point that doesn't depend on the hold's 250 ms). A count, not a switch: tests running side by
/// side each hold until their own release, and one test's release doesn't free another's build.
#[cfg(test)]
static TEST_HOLD: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Holds background builds while it lives (also when the test fails), whatever other holds do.
#[cfg(test)]
pub(crate) struct TestHold;

#[cfg(test)]
impl TestHold {
    pub(crate) fn new() -> Self {
        TEST_HOLD.fetch_add(1, Ordering::Relaxed);
        TestHold
    }
}

#[cfg(test)]
impl Drop for TestHold {
    fn drop(&mut self) {
        // (one release per hold made: never below 0)
        TEST_HOLD.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Wait while the photo on screen is being made. `true`: `cancel` was set meanwhile.
pub fn wait_for_foreground(cancel: &AtomicBool) -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    while foreground_busy() {
        if cancel.load(Ordering::Relaxed) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
    cancel.load(Ordering::Relaxed)
}

/// Photos the background build renders side by side: each render spreads over the cores already,
/// so two keep the machine busy while leaving room for the photo on screen.
pub fn background_lanes() -> usize {
    let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
    if cores >= 8 { 2 } else { 1 }
}

/// A cheap identity of the photos a build prepares: a build is started once per view. `scope`
/// [`Scope::Marked`]: which of them are marked counts too (marking a photo changes the set).
fn signature(ids: &[PhotoId], s: &Session, scope: Scope) -> u64 {
    let mut h = lightcraft_preview::Hasher128::new();
    h.u64(ids.len() as u64).u64(s.catalog.len() as u64);
    // every id: a sort that moves photos in the middle is another view
    for id in ids {
        h.u64(id.0);
        if scope == Scope::Marked {
            h.u64(u64::from(marked(s, *id)));
        }
    }
    h.finish().0 as u64
}

/// Whether `id` is a pick, or has stars or a colour label (the [`Scope::Marked`] photos).
pub fn marked(s: &Session, id: PhotoId) -> bool {
    s.catalog.photo(id).is_some_and(|p| p.flag == lightcraft_catalog::Flag::Pick || p.rating > 0 || p.label.is_some())
}

impl Session {
    /// How long until a view whose build couldn't start tries again ([`RETRY_AFTER`]), while one
    /// waits: the app asks for a frame then, so the retry doesn't wait for the next input
    /// (CodeRabbit on #3077/#3080). `None` once the time has come.
    pub fn prep_retry_in(&self) -> Option<std::time::Duration> {
        // (a deadline that has passed asks for nothing: the pass at it tried again, or the view
        // can't be tried now and a frame at once on every pass would keep an idle window drawing:
        // CodeRabbit on #3077 / #3080)
        let wait = self.prep.retry_at.map(|(_, at)| at.saturating_duration_since(web_time::Instant::now()))?;
        (!wait.is_zero()).then_some(wait)
    }

    /// The background preparation settings in use (this library's own, else the app-wide ones).
    pub fn prep_settings(&self) -> PrepSettings {
        self.prep.settings()
    }

    /// Advance the background preparation (cheap; call every frame). `mode`: what the user is doing;
    /// `at`: the photo in the loupe (`None` in the grid), `backward`: stepping back.
    pub fn prep_tick(&mut self, mode: Mode, at: Option<PhotoId>, backward: bool) {
        self.follow_library();
        // the ✕ of the row the plan holds: its photos are not planned any further, nor that view again
        if self.prep.plan.as_ref().is_some_and(|p| p.plan.cancelled()) {
            self.decline_plan();
        }
        let settings = self.prep.settings();
        let want = settings.mode(mode);
        // the whole view's build follows its own row (Culling, or Library also while editing): its
        // switch and its "which photos"; the Editing row's scope is for the photos kept ready ahead
        let wanted = want.previews && want.scope != Scope::OnScreen;
        // the running build: re-aimed at the user, stopped when switched off
        if let Some(b) = self.prep.build.clone() {
            if b.running() {
                if !wanted {
                    b.cancel.store(true, Ordering::Relaxed);
                } else if self.prep.started_for.is_some_and(|sig| sig != self.view_signature(want.scope).0) {
                    // another view (a folder, a filter, another library): this build's photos are
                    // not the ones shown; stopped, and the view now shown is prepared in its place
                    // (not counted as stopped by the user)
                    b.cancel.store(true, Ordering::Relaxed);
                    self.prep.started_for = None;
                } else if let Some(id) = at {
                    b.focus_on(id, backward);
                }
                return;
            }
            self.prep.build = None;
            let mine = self.preview_build.as_ref().is_some_and(|p| Arc::ptr_eq(p, &b));
            if b.cancel.load(Ordering::Relaxed) {
                if mine && wanted {
                    // stopped from its row: not again for this view
                    self.prep.declined_for = self.prep.started_for;
                } else {
                    // replaced by a build that was asked for, or switched off: may start again later
                    self.prep.started_for = None;
                }
            }
        }
        if !wanted {
            self.drop_plan();
            return;
        }
        let Some(at) = at else {
            // (the grid: no photo to build from, and no frame wanted for it)
            self.prep.advancing = false;
            return;
        };
        let (sig, ids) = self.view_signature(want.scope);
        if self.prep.plan.as_ref().is_some_and(|p| p.sig != sig || self.plan_outdated(p)) {
            // the view changed while its photos were being planned (or the library, its preview cache or what a
            // photo is rendered from did): the plan is of another view, or its jobs are out of date
            self.drop_plan();
        }
        // (a plan that is still current is current as of this catalog: the next tick looks only at what changes from
        // here, so many unrelated changes over a long plan never outrun what the catalog remembers photo by photo)
        let revision = self.catalog.revision;
        if let Some(p) = self.prep.plan.as_mut() {
            p.revision = revision;
        }
        if self.prep.steps_in != Some(sig) {
            self.prep.steps_in = Some(sig);
            self.prep.steps = 0;
            self.prep.last_at = None;
        }
        if self.prep.last_at != Some(at) {
            self.prep.last_at = Some(at);
            self.prep.steps = self.prep.steps.saturating_add(1);
        }
        let busy = self.preview_build.as_ref().is_some_and(|b| b.running());
        if self.prep.plan.is_some() {
            if busy {
                // a build that was asked for began: this one starts over once it is done
                self.drop_plan();
            } else {
                self.advance_plan(at, backward);
            }
            return;
        }
        if busy
            || self.prep.steps < STEPS_BEFORE_BUILD
            || self.prep.started_for == Some(sig)
            || self.prep.declined_for == Some(sig)
            || self.prep.retry_at.is_some_and(|(view, at)| view == sig && web_time::Instant::now() < at)
            || self.library.as_ref().is_none_or(|l| !l.on_disk)
        {
            return;
        }
        self.prep.started_for = Some(sig);
        let edge = Some(crate::cmd::previews::standard_edge(self));
        let plan = crate::cmd::previews::BuildPlan::new(self, ids, want.scope == Scope::Marked, edge);
        let (cache, revision) = (self.media.rendered.clone(), self.catalog.revision);
        let generation = cache.generation();
        self.prep.plan = Some(Planning { sig, plan, cache, generation, revision });
        self.advance_plan(at, backward);
    }

    /// Whether the build's photos are still being planned: the app asks for another frame to go on.
    pub fn prep_planning(&self) -> bool {
        self.prep.advancing
    }

    /// The view's identity ([`signature`]) with its photos. Worked out once per generation of the
    /// visible list, not on every call: a frame asks every time, a whole view hashed each time
    /// is what a big library can't afford.
    fn view_signature(&mut self, scope: Scope) -> (u64, Arc<[PhotoId]>) {
        let (generation, ids) = self.visible_shared();
        if let Some((g, sc, sig)) = self.prep.signed
            && g == generation
            && sc == scope
        {
            return (sig, ids);
        }
        let sig = signature(&ids, self, scope);
        self.prep.signed = Some((generation, scope, sig));
        #[cfg(test)]
        {
            self.prep.signatures += 1;
        }
        (sig, ids)
    }

    /// Whether the jobs `p` has planned are out of date: another library's preview cache is in use (the plan is of
    /// the library before, though its view looks the same), the cache was cleared (the jobs would save into the
    /// cache as it was and fail), or a photo's settings, file or presence changed since the plan was last found
    /// current (its jobs would render, and be keyed, as it was). A rating, a keyword or a photo's analysis (which no
    /// job reads) changes none of them. (A catalog the library has just loaded doesn't know what changed since its
    /// revision, but nothing has when the revision is the same.)
    fn plan_outdated(&self, p: &Planning) -> bool {
        const RENDERED_FROM: lightcraft_catalog::Aspects =
            lightcraft_catalog::Aspects::DEVELOP.with(lightcraft_catalog::Aspects::FILE).with(lightcraft_catalog::Aspects::PRESENCE);
        !Arc::ptr_eq(&p.cache, &self.media.rendered)
            || p.cache.generation() != p.generation
            || (self.catalog.revision != p.revision && self.catalog.aspects_since(p.revision).is_none_or(|changed| changed.any_of(RENDERED_FROM)))
    }

    /// Another library than the one the preparation state is of is open: what it holds is of the library before and
    /// its view may look the same as this one's (the same photo ids and as many photos), so it must not stand for it.
    /// The build that runs is stopped (it renders for a preview cache that is gone), the plan is dropped, and the
    /// views that were started, stopped by the user or that failed to start are tried afresh, with the steps counted
    /// again.
    fn follow_library(&mut self) {
        if self.prep.library_gen == Some(self.library_generation()) {
            return;
        }
        self.prep.library_gen = Some(self.library_generation());
        if let Some(b) = self.prep.build.as_ref().filter(|b| b.running()) {
            b.cancel.store(true, Ordering::Relaxed);
        }
        self.drop_plan();
        let p = &mut self.prep;
        (p.started_for, p.declined_for, p.retry_at) = (None, None, None);
        (p.steps_in, p.last_at, p.steps) = (None, None, 0);
    }

    /// Give up the plan under way; its view is prepared afresh when it is next due.
    fn drop_plan(&mut self) {
        if self.prep.plan.take().is_some() {
            self.prep.started_for = None;
        }
        self.prep.advancing = false;
    }

    /// `library.cancelPreviews` while the photos of a build are being planned (nothing runs yet to cancel): the plan
    /// is ended like its row's ✕ ends it ([`Self::decline_plan`]). A build that is running is the one cancelled instead.
    pub(crate) fn cancel_prep_plan(&mut self) {
        if self.preview_build.as_ref().is_some_and(|b| b.running()) {
            return;
        }
        self.decline_plan();
    }

    /// The plan under way is dropped, and its view not prepared again, as if its build had been stopped from its row.
    fn decline_plan(&mut self) {
        if let Some(p) = &self.prep.plan {
            self.prep.declined_for = Some(p.sig);
        }
        self.drop_plan();
    }

    /// Plan one more slice of the build's photos; once all are planned, start the build.
    fn advance_plan(&mut self, at: PhotoId, backward: bool) {
        let Some(Planning { sig, mut plan, cache, generation, revision }) = self.prep.plan.take() else { return };
        #[cfg(test)]
        let slice = self.prep.slice.unwrap_or(crate::cmd::previews::PLAN_SLICE);
        #[cfg(not(test))]
        let slice = crate::cmd::previews::PLAN_SLICE;
        let done = crate::guard::catch("preparing photos", || plan.advance(self, slice));
        self.prep.advancing = matches!(done, Ok(false));
        let started = match done {
            Ok(false) => {
                self.prep.plan = Some(Planning { sig, plan, cache, generation, revision });
                return;
            }
            Ok(true) if plan.selected == 0 => return,
            Ok(true) => {
                let launch = crate::cmd::previews::Launch {
                    edge: plan.edge(),
                    background: true,
                    force: false,
                    row: true,
                    wait: cfg!(target_arch = "wasm32"),
                    from: Some((at, backward)),
                    #[cfg(test)]
                    gate: crate::cmd::previews::test_gate::take(),
                };
                // (tests: a start made to fail, as a build thread that can't be spawned fails)
                #[cfg(test)]
                let fail = self.prep.fail_starts.checked_sub(1).map(|left| self.prep.fail_starts = left).is_some();
                #[cfg(not(test))]
                let fail = false;
                if fail {
                    Err(crate::EngineError::Disabled("library.buildPreviews".into(), "test: the build couldn't start".into()))
                } else {
                    self.execute_fn("library.buildPreviews", |s| crate::cmd::previews::start_planned(s, plan, launch))
                }
            }
            Err(e) => Err(crate::EngineError::Other(e)),
        };
        match started {
            Ok(_) => {
                self.prep.build = self.preview_build.clone();
                self.prep.retry_at = None;
            }
            Err(e) => {
                // not started: this view tries again after a while (not on every tick)
                self.prep.started_for = None;
                self.prep.retry_at = web_time::Instant::now().checked_add(RETRY_AFTER).map(|at| (sig, at));
                log::info!("background preparation: {e}");
            }
        }
    }

    /// Change the background preparation settings: the app-wide ones, or (`library`) this library's
    /// own. Saved at once.
    pub fn set_prep_settings(&mut self, settings: PrepSettings, library: bool) -> crate::Result<()> {
        let settings = settings.sanitized();
        if library {
            let previous = self.prep.library;
            self.prep.library = Some(settings);
            self.save_library_prep(previous)
        } else {
            let previous = self.prep.app;
            self.prep.app = settings;
            let saved = self.save_prefs();
            if saved.is_err() {
                self.prep.app = previous;
            }
            saved
        }
    }

    /// Back to the app-wide settings for this library.
    pub fn clear_library_prep(&mut self) -> crate::Result<()> {
        let previous = self.prep.library;
        self.prep.library = None;
        self.save_library_prep(previous)
    }

    /// Save this library's preparation settings; when that fails, `previous` is put back, so the
    /// session doesn't go on with settings the command reported as not changed (CodeRabbit on #3544).
    fn save_library_prep(&mut self, previous: Option<PrepSettings>) -> crate::Result<()> {
        let saved = self.save_prefs();
        if saved.is_err() {
            self.prep.library = previous;
        }
        saved
    }

    pub(crate) fn prep_json(&self) -> Value {
        json!({
            "app": self.prep.app,
            "library": self.prep.library,
            "settings": self.prep.settings(),
            "own": self.prep.library.is_some(),
            "building": self.prep.build.as_ref().filter(|b| b.running()).map(|b| b.json()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_patch_changes_only_what_it_names_and_is_kept_in_range() {
        let p = PrepSettings::default().patched(&json!({"culling": {"ahead": 99}, "develop": {"scope": "onScreen"}})).unwrap();
        assert_eq!(p.culling.ahead, MAX_AHEAD);
        assert_eq!(p.culling.eye_ahead, 8, "untouched");
        assert!(p.library.previews);
        assert_eq!(p.develop.scope, Scope::OnScreen);
        assert_eq!(p.ahead(Mode::Develop), 0);
        assert!(PrepSettings::default().patched(&json!({"culling": {"scope": "sometimes"}})).is_err());
    }

    /// Tests side by side: one test's release doesn't free the build another test holds (CodeRabbit on #3077).
    #[test]
    fn one_tests_release_does_not_free_another_tests_hold() {
        let a = TestHold::new();
        let b = TestHold::new();
        drop(a);
        assert!(TEST_HOLD.load(Ordering::Relaxed) > 0 && foreground_busy(), "the other hold still holds");
        drop(b);
    }

    #[test]
    fn the_photo_on_screen_holds_background_work_back() {
        let cancel = AtomicBool::new(false);
        foreground();
        assert!(foreground_busy());
        let t = web_time::Instant::now();
        assert!(!wait_for_foreground(&cancel));
        assert!(t.elapsed().as_millis() >= 100, "waited for the foreground to go quiet");
        cancel.store(true, Ordering::Relaxed);
        foreground();
        assert!(wait_for_foreground(&cancel), "a cancel ends the wait");
    }
}
