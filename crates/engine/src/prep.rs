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
    /// The view the steps were counted in, the photo last seen, and the steps.
    steps_in: Option<u64>,
    last_at: Option<PhotoId>,
    steps: usize,
    /// The view's signature, kept under what it is made of (the visible list's generation, the
    /// catalog's revision, the scope): `prep_tick` runs every frame and needn't hash the list each time.
    sig_cache: Option<((u64, u64, Scope), u64)>,
}

impl PrepState {
    /// Another library opened: stop the build this preparation started and forget the views it
    /// counted or prepared (two libraries can show views with the same signature). The settings stay.
    pub(crate) fn library_opened(&mut self) {
        if let Some(b) = self.build.take() {
            b.cancel.store(true, Ordering::Relaxed);
        }
        *self = PrepState { app: self.app, library: self.library, ..Default::default() };
    }
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

/// Until when (ms on [`clock_ms`]) something on screen has no picture at all yet.
static PICTURES_UNTIL: AtomicU64 = AtomicU64::new(0);

/// The frontend says a cell on screen (grid, filmstrip) or the loupe has no picture yet, not even
/// the cheapest (a raw's embedded JPEG, a cached thumbnail): every background job waits until each
/// has one (the cheapest picture first for the whole view, then the bigger ones). Say it every frame it is
/// so; it holds for a moment.
pub fn pictures_missing() {
    PICTURES_UNTIL.store(clock_ms().saturating_add(FOREGROUND_HOLD_MS), Ordering::Relaxed);
}

/// Whether something on screen still lacks its first picture ([`pictures_missing`]).
pub fn pictures_busy() -> bool {
    clock_ms() < PICTURES_UNTIL.load(Ordering::Relaxed)
}

/// Wait while something on screen lacks its first picture (for background work that doesn't render:
/// it waits for the pictures, not for the full render). `true`: `stop()` said so
/// meanwhile. Waits at most `max`.
pub fn wait_for_pictures(stop: &dyn Fn() -> bool, max: std::time::Duration) -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let t0 = web_time::Instant::now();
        while pictures_busy() && t0.elapsed() < max {
            if stop() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
    }
    #[cfg(target_arch = "wasm32")]
    let _ = max;
    stop()
}

/// Whether the photo on screen is being made now, or something on screen lacks its first picture.
pub fn foreground_busy() -> bool {
    #[cfg(test)]
    if TEST_HOLD.load(Ordering::Relaxed) > 0 {
        return true;
    }
    let now = clock_ms();
    now < FOREGROUND_UNTIL.load(Ordering::Relaxed) || now < PICTURES_UNTIL.load(Ordering::Relaxed)
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
    /// The background preparation settings in use (this library's own, else the app-wide ones).
    pub fn prep_settings(&self) -> PrepSettings {
        self.prep.settings()
    }

    /// Advance the background preparation (cheap; call every frame). `mode`: what the user is doing;
    /// `at`: the photo in the loupe (`None` in the grid), `backward`: stepping back.
    pub fn prep_tick(&mut self, mode: Mode, at: Option<PhotoId>, backward: bool) {
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
                } else if self.prep.started_for.is_some_and(|sig| sig != self.view_signature(want.scope)) {
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
            return;
        }
        let Some(at) = at else { return };
        let sig = self.view_signature(want.scope);
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
        if busy
            || self.prep.steps < STEPS_BEFORE_BUILD
            || self.prep.started_for == Some(sig)
            || self.prep.declined_for == Some(sig)
            || self.library.as_ref().is_none_or(|l| !l.on_disk)
        {
            return;
        }
        let (_, visible) = self.visible_shared();
        let ids: Vec<u64> = match want.scope {
            Scope::Marked => visible.iter().copied().filter(|id| marked(self, *id)).map(|id| id.0).collect(),
            _ => visible.iter().map(|id| id.0).collect(),
        };
        self.prep.started_for = Some(sig);
        if ids.is_empty() {
            return;
        }
        let p = json!({"ids": ids, "background": true, "from": at.0, "backward": backward});
        match self.execute("library.buildPreviews", &p) {
            Ok(_) => self.prep.build = self.preview_build.clone(),
            Err(e) => log::info!("background preparation: {e}"),
        }
    }

    /// The visible list's [`signature`] for `scope`, worked out again only when the list, the catalog
    /// or the scope changed.
    fn view_signature(&mut self, scope: Scope) -> u64 {
        let (generation, ids) = self.visible_shared();
        let key = (generation, self.catalog.revision, scope);
        if let Some((k, sig)) = self.prep.sig_cache
            && k == key
        {
            return sig;
        }
        let sig = signature(&ids, self, scope);
        self.prep.sig_cache = Some((key, sig));
        sig
    }

    /// Change the background preparation settings: the app-wide ones, or (`library`) this library's
    /// own. This library's are saved at once (`prefs.json`); the app-wide ones are the frontend's to
    /// keep (the desktop app's `ui.json`), and `prefs.json` doesn't hold them.
    pub fn set_prep_settings(&mut self, settings: PrepSettings, library: bool) -> crate::Result<()> {
        let settings = settings.sanitized();
        if library {
            self.prep.library = Some(settings);
            self.save_prefs()
        } else {
            self.prep.app = settings;
            Ok(())
        }
    }

    /// Back to the app-wide settings for this library.
    pub fn clear_library_prep(&mut self) -> crate::Result<()> {
        self.prep.library = None;
        self.save_prefs()
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

    #[test]
    fn a_picture_missing_on_screen_holds_every_background_job_back() {
        let cancel = AtomicBool::new(false);
        pictures_missing();
        assert!(pictures_busy() && foreground_busy());
        let t = web_time::Instant::now();
        assert!(!wait_for_foreground(&cancel), "the background build waits for it");
        assert!(t.elapsed().as_millis() >= 100);
        pictures_missing();
        let t = web_time::Instant::now();
        assert!(!wait_for_pictures(&|| false, std::time::Duration::from_secs(5)), "work that doesn't render waits for it");
        assert!(t.elapsed().as_millis() >= 100);
        assert!(!pictures_busy());
    }

    /// The view's signature is kept while nothing it is made of changes (CodeRabbit on #3080: it was
    /// worked out every frame), and worked out again when a marked photo changes or the view does.
    #[test]
    fn the_view_signature_is_kept_until_the_view_or_the_catalog_changes() {
        let mut s = Session::with_demo();
        let ids = s.visible_cloned();
        let first = s.view_signature(Scope::Marked);
        let cached = s.prep.sig_cache.expect("kept");
        assert_eq!(s.view_signature(Scope::Marked), first);
        assert_eq!(s.prep.sig_cache, Some(cached), "the same entry: not worked out again");
        assert_eq!(first, signature(&ids, &s, Scope::Marked));
        let one = ids.first().copied().expect("a demo photo");
        s.execute("photo.rate", &json!({"ids": [one.0], "rating": if s.catalog.photo(one).is_some_and(|p| p.rating > 0) { 0 } else { 3 }})).unwrap();
        let after = s.view_signature(Scope::Marked);
        assert_ne!(after, first, "a photo marked or unmarked is another set of marked photos");
        assert_eq!(after, signature(&s.visible_cloned(), &s, Scope::Marked));
        assert_ne!(s.view_signature(Scope::All), after, "another scope");
    }
}
