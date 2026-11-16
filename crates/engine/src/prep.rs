//! Preparing photos in the background: while the user culls, the photos ahead are made ready
//! before they are reached, and with the machine otherwise idle the previews of every photo in
//! view are built, so stepping never waits for a render.
//!
//! What is prepared is set per mode, because each mode needs something else:
//! - **Culling** (a folder browsed before import): previews of every photo in the folder, the next
//!   photos' loupe renders and their eye focus strips.
//! - **Library**: the same for the photos in the library's view.
//! - **Develop** (editing): the next photos decoded and kept ready to edit.
//!
//! Each mode has its own switch for the whole-view build, how many photos ahead to prepare (0 =
//! none) and which photos ([`Scope`]). The settings are app-wide (the app store's `shared.json`)
//! and a library can keep its own (its `prefs.json`): a timelapse library prepares nothing while a
//! wedding library prepares everything.
//!
//! The whole-view build is the preview build ([`crate::cmd::previews`]) in its background form: two
//! lanes, waiting whenever the photo on screen is being made ([`foreground`]) and while the user's
//! own work runs (an export, an import: [`crate::activity::Activity::user_work`]), nearest photos to
//! the user first and re-aimed as they move ([`crate::cmd::previews::PreviewBuild::focus_on`]),
//! photos with previews skipped, one row in the activity stack that can be cancelled. It starts
//! once the user is culling (a few photos stepped through in the loupe), never just because a
//! library was opened (opening an old library to look at two photos must not start rebuilding).
//!
//! The pictures on screen come first: while a grid or filmstrip cell, or the loupe, still lacks
//! its first picture (not even the cheapest, a raw's embedded JPEG or a cached thumbnail), the
//! frontend says so every frame ([`pictures_missing`]) and background work that reads raws waits
//! ([`wait_for_pictures`]): eye finding, culling scans, this preparation. The signals hold for a
//! moment after the last call, so a frontend that stops saying it (the pictures arrived, or it
//! closed) never holds anything back for long.
//!
//! A build that was started and did not run to its end (the view changed under it, another build
//! took its place, previews were switched off, the app quit) is owed to the library: it is taken
//! up again for the view then shown, from the grid as well and after a restart, once that view has
//! stayed the same for a moment ([`PrepState::unfinished`], [`SETTLE_AFTER`]). What is left to
//! build is never stored: each build asks, photo by photo, whether its previews are on disk.
//!
//! Sources: own design (OneSlips: PERF-CULL-TIERS, "lowest and fastest resolution first, then the
//! next cheap one, then anything bigger"; PERF-PREVIEW-PREP, A1-PREP-RESYNC, PREP-NEIGHBOURS).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use lcc_catalog::PhotoId;
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
    /// Photos ahead whose eye focus strip is worked out before they are reached (0 = only the one on screen).
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
        DevelopPrep { ahead: 5, scope: Scope::All }
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
        merge(&mut v, patch, 0)?;
        let p: PrepSettings = serde_json::from_value(v).map_err(|e| format!("background preparation settings: {e}"))?;
        Ok(p.sanitized())
    }
}

/// `patch` merged into `into`, objects field by field (two levels deep: mode → field).
fn merge(into: &mut Value, patch: &Value, depth: usize) -> Result<(), String> {
    match (into, patch) {
        (Value::Object(a), Value::Object(b)) if depth < 3 => {
            for (k, v) in b {
                match a.get_mut(k) {
                    Some(old) => merge(old, v, depth + 1)?,
                    None => return Err(format!("unknown background preparation setting `{k}`")),
                }
            }
        }
        (a, b) => *a = b.clone(),
    }
    Ok(())
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

/// How long the view must have stayed the same before an unfinished build is taken up again for
/// it ([`PrepState::unfinished`]): photos arriving in batches (an import) or a few sorts tried one
/// after the other start one build, not one each.
pub const SETTLE_AFTER: std::time::Duration = std::time::Duration::from_secs(3);

/// In a library (not a folder being culled) the background build prepares the photos this far
/// before and after the one on screen, not the whole view (note 30 of 2026-10-24: "for a gallery
/// that's already been edited, I don't know why it's preparing"); stepping out of that stretch
/// prepares the next one.
pub const AROUND: usize = 40;

/// How long the photos' settings must have stayed as they are before the photos whose settings
/// changed off screen (Auto Sync, Sync, paste, presets) are rendered again (ms): a drag synced to a
/// selection changes them every frame.
const RESYNC_QUIET_MS: u64 = 600;

/// The background preparation's state in a session.
#[derive(Default)]
pub struct PrepState {
    /// App-wide settings.
    pub app: PrepSettings,
    /// This library's own (`None`: it uses the app-wide ones).
    pub library: Option<PrepSettings>,
    /// A build of this library's previews was started and has not run to its end: it is owed, and
    /// [`Session::prep_tick`] takes it up again for the view shown, without the user stepping
    /// through photos first ([`STEPS_BEFORE_BUILD`] guards only a library with nothing owed) and
    /// with no photo in the loupe. Kept in the library's `prefs.json` (`prepUnfinished`), so it
    /// holds across a restart.
    ///
    /// - Set, and saved, when a build for the view starts.
    /// - Cleared, and saved, when such a build ran to its end (its queue ran empty; a photo that
    ///   failed counts as built), and when the user stopped it from its row: they said no.
    /// - Left set by everything else that ends a build: the view changing, a build that was asked
    ///   for taking its place, previews switched off, the app quitting.
    ///
    /// It says "ask again", not what is left: the build that follows skips every photo whose
    /// previews are on disk. A view with nothing to build (no marked photo) leaves it as it is.
    pub unfinished: bool,
    /// Before when an unfinished build is not taken up again: [`SETTLE_AFTER`] after the view shown
    /// last changed (`None`: no change seen yet). The first build of a library with nothing owed
    /// doesn't wait for it (the steps in the loupe are its guard).
    pub not_before: Option<web_time::Instant>,
    /// Another wait than [`SETTLE_AFTER`] (tests: none, or one that never ends by itself).
    pub settle: Option<std::time::Duration>,
    /// The view (its signature) last seen while a build could start: a different one starts the wait.
    view_seen: Option<u64>,
    /// The background build this preparation started, and the library it was started in
    /// ([`Session::library_generation`]): a build left over from another library says nothing
    /// about this one's [`PrepState::unfinished`].
    build: Option<Arc<PreviewBuild>>,
    build_library: u64,
    /// The view (its signature, [`signature`]) the build was started for: in the grid, where no
    /// photo says which view the user is in, another view stops the build.
    build_view: u64,
    /// The running build still has photos whose render jobs aren't made ([`crate::cmd::previews::feed`]):
    /// the frontend ticks again soon ([`Session::prep_feeding`]).
    feeding: bool,
    /// The view (its signature) whose build was started, or that the user stopped.
    started_for: Option<u64>,
    declined_for: Option<u64>,
    /// The view the steps were counted in, the photo last seen, and the steps.
    steps_in: Option<u64>,
    last_at: Option<PhotoId>,
    steps: usize,
    /// Each photo's settings as last seen (their `Arc`'s address), the catalog revision and the
    /// library they were seen in: what changed off screen is rendered again ([`Session::prep_tick`]).
    seen: std::collections::HashMap<PhotoId, usize>,
    seen_rev: Option<(u64, Option<std::path::PathBuf>)>,
    /// Photos whose settings changed while they weren't on screen and whose previews are not made
    /// for them yet, and when the last such change was seen.
    changed: std::collections::BTreeSet<PhotoId>,
    changed_at: u64,
    /// The running build is the one for [`Self::changed`] photos.
    resync: bool,
    /// The running build was stopped to make way for the changed photos (not by the user).
    superseded: bool,
}

impl std::fmt::Debug for PrepState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrepState").field("app", &self.app).field("library", &self.library).field("steps", &self.steps).finish()
    }
}

impl PrepState {
    /// Forget the view state a library that was open before left behind, when a library opens: the
    /// view guards (`started_for`, `declined_for`, the view last seen), the settle wait and the
    /// step count were earned in that library's views, and a view signature is only its photos'
    /// ids and their count, so another library's view can carry the same one. What a library opens
    /// with is kept: the settings, what it is owed (`unfinished`, which the caller has just read),
    /// the settle wait a test asks for, and the build being followed (told apart by the library it
    /// was started in, `build_library`).
    pub(crate) fn opened_library(&mut self) {
        self.view_seen = None;
        self.started_for = None;
        self.declined_for = None;
        self.not_before = None;
        self.steps_in = None;
        self.last_at = None;
        self.steps = 0;
    }

    /// The wait after a view change ([`SETTLE_AFTER`]; this crate's tests: none unless they set one).
    fn settle(&self) -> std::time::Duration {
        self.settle.unwrap_or(if cfg!(test) { std::time::Duration::ZERO } else { SETTLE_AFTER })
    }

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
    let start = START.get_or_init(web_time::Instant::now);
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX).saturating_add(1)
}

/// The frontend says the photo on screen is being made (a render it waits for is on its way):
/// background preparation waits until it has been quiet for a moment.
pub fn foreground() {
    FOREGROUND_UNTIL.store(clock_ms().saturating_add(FOREGROUND_HOLD_MS), Ordering::Relaxed);
}

/// How long one [`pictures_missing`] holds background work back (ms): a few frames.
const PICTURES_HOLD_MS: u64 = 150;

/// Until when (ms on [`clock_ms`]) something on screen has no picture at all yet.
static PICTURES_UNTIL: AtomicU64 = AtomicU64::new(0);

/// The frontend says a cell on screen (grid, filmstrip) or the loupe has no picture yet, not even
/// the cheapest (a raw's embedded JPEG, a cached thumbnail): every background job waits until each
/// has one (the user's rule, notes 1 and 4 of 2026-10-22: "lowest and fastest resolution preloaded
/// first, then the next cheap one, then we worry about anything bigger"). Say it every frame it is
/// so; it holds for a moment.
pub fn pictures_missing() {
    PICTURES_UNTIL.store(clock_ms().saturating_add(PICTURES_HOLD_MS), Ordering::Relaxed);
}

/// Whether something on screen still lacks its first picture ([`pictures_missing`]).
pub fn pictures_busy() -> bool {
    clock_ms() < PICTURES_UNTIL.load(Ordering::Relaxed)
}

/// Wait while something on screen lacks its first picture (background work that doesn't render,
/// e.g. eye finding: it waits for the pictures, not for the full render). `true`: `stop()` said so
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

/// How many tests hold background builds back ([`TestHold`]): a count, so tests running side by
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
        TEST_HOLD.fetch_sub(1, Ordering::Relaxed);
    }
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

/// Wait while the photo on screen is being made. `true`: `cancel` was set meanwhile.
pub fn wait_for_foreground(cancel: &AtomicBool) -> bool {
    wait_for_foreground_while(cancel, &|| true)
}

/// [`wait_for_foreground`], given up as soon as `still` says the wait is no longer needed (a build
/// lane whose photos the other lane took). `true`: `cancel` was set meanwhile.
pub fn wait_for_foreground_while(cancel: &AtomicBool, still: &dyn Fn() -> bool) -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    while foreground_busy() && still() {
        if cancel.load(Ordering::Relaxed) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
    #[cfg(target_arch = "wasm32")]
    let _ = still;
    cancel.load(Ordering::Relaxed)
}

/// The longest the background build's first lane waits for the photo on screen and its
/// neighbours before it builds its next photo anyway (ms). Stepping through a folder keeps the
/// photo on screen being made all the time: a build that always waited never got ahead of the user
/// (2026-11-09: 16 photos in 70 s of culling a wedding). The other lanes wait as long as it takes,
/// and every lane waits for the pictures missing on screen ([`pictures_missing`]).
pub const STEADY_WAIT_MS: u64 = 1500;

/// Wait while the photo on screen is being made, at most [`STEADY_WAIT_MS`], and while something on
/// screen lacks its first picture (no limit). `true`: `cancel` was set meanwhile.
pub fn wait_for_foreground_steady(cancel: &AtomicBool) -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let t0 = clock_ms();
        while foreground_busy() {
            if cancel.load(Ordering::Relaxed) {
                return true;
            }
            if clock_ms().saturating_sub(t0) >= STEADY_WAIT_MS && !pictures_busy() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
    }
    cancel.load(Ordering::Relaxed)
}

/// Photos the background build renders side by side: each render spreads over the cores already,
/// so two keep the machine busy while leaving room for the photo on screen; one under a small memory budget (a phone),
/// where two photos decoding at once took memory the photo on screen needed.
pub fn background_lanes() -> usize {
    let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
    lanes_for(cores, crate::memory::budget())
}

/// The memory budget under which the background build renders one photo at a time.
const TWO_LANE_BUDGET: usize = 4 << 30;

/// [`background_lanes`] on a machine with `cores` threads and a memory budget of `budget` bytes.
fn lanes_for(cores: usize, budget: usize) -> usize {
    if cores >= 8 && budget >= TWO_LANE_BUDGET { 2 } else { 1 }
}

/// How many photos the background preparation renders on pools of its own at once
/// ([`on_background_pool`]); a build with more lanes renders the others on the lane's thread.
pub const BACKGROUND_POOLS: usize = 4;

/// The background preparation's thread pool `i` (< [`BACKGROUND_POOLS`]): its threads run at the
/// lowest priority ([`lcc_sysmem::lowest_thread_priority`]), below the render workers that make
/// the photo on screen, so the preparation takes only the processor time the photo on screen
/// leaves. `None` when it couldn't be made.
pub fn background_pool(i: usize) -> Option<&'static rayon::ThreadPool> {
    static POOLS: [std::sync::OnceLock<Option<rayon::ThreadPool>>; BACKGROUND_POOLS] = [const { std::sync::OnceLock::new() }; BACKGROUND_POOLS];
    POOLS
        .get(i)?
        .get_or_init(|| {
            let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
            rayon::ThreadPoolBuilder::new()
                .num_threads(cores.saturating_sub(2).max(1))
                .thread_name(move |k| format!("lc-prep-{i}-{k}"))
                .start_handler(|_| {
                    lcc_sysmem::lowest_thread_priority();
                })
                .build()
                .map_err(|e| log::warn!("background preparation: no thread pool of its own ({e})"))
                .ok()
        })
        .as_ref()
}

/// Run `f` (one photo of the background preparation) on a [`background_pool`] no other photo is
/// using, else here. One photo a pool: two photos installed on one pool let a worker waiting
/// inside one photo's parallel work pick up the other photo's whole render on its own stack,
/// which mixes their per-thread state (the GPU context's failure slot, the background flag) and
/// can deadlock at the memory gate (the inner photo waits for memory the outer one holds).
pub fn on_background_pool<R: Send>(f: impl FnOnce() -> R + Send) -> R {
    static IN_USE: [std::sync::Mutex<()>; BACKGROUND_POOLS] = [const { std::sync::Mutex::new(()) }; BACKGROUND_POOLS];
    for (i, slot) in IN_USE.iter().enumerate() {
        // (a poisoned slot only means a photo panicked there: the pool is still fine)
        let held = match slot.try_lock() {
            Ok(g) => Some(g),
            Err(std::sync::TryLockError::Poisoned(p)) => Some(p.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => None,
        };
        if let Some(_held) = held
            && let Some(pool) = background_pool(i)
        {
            return pool.install(f);
        }
    }
    f()
}

/// A cheap identity of the photos in view: a build is started once per view.
fn signature(ids: &[PhotoId], s: &Session) -> u64 {
    let mut h = lcc_preview::Hasher128::new();
    h.u64(ids.len() as u64).u64(s.catalog.len() as u64);
    for id in ids.iter().take(64).chain(ids.iter().rev().take(64)) {
        h.u64(id.0);
    }
    h.finish().0 as u64
}

/// Whether `id` is a pick, or has stars or a colour label (the [`Scope::Marked`] photos).
pub fn marked(s: &Session, id: PhotoId) -> bool {
    s.catalog.photo(id).is_some_and(|p| p.flag == lcc_catalog::Flag::Pick || p.rating > 0 || p.label.is_some())
}

impl Session {
    /// Whether the background preparation (or another preview build) is making `id`'s preview right
    /// now. A step onto the photo the build is halfway through started a second render of the same raw
    /// beside it (two decodes of a 45 MP CR3 instead of one), and that render on screen held the build
    /// back while it ran: the loupe waits for the build's preview instead (2026-11-10 culling sweep).
    pub fn being_prepared(&self, id: PhotoId) -> bool {
        crate::cmd::previews::being_built(self, id)
    }

    /// The background preparation settings in use (this library's own, else the app-wide ones).
    pub fn prep_settings(&self) -> PrepSettings {
        self.prep.settings()
    }

    /// Advance the background preparation (cheap; call every frame). `mode`: what the user is doing;
    /// `at`: the photo in the loupe (`None` in the grid), `backward`: stepping back.
    ///
    /// Photos whose settings changed while they were not on screen (Auto Sync, Sync, paste, a preset
    /// on the selection) are rendered again first, nearest the photo on screen first, once the edits
    /// have settled (note 11 of 2026-10-24: "none of the selected images had re-rendered with the
    /// updated look, even after 15 min"). In a library the rest is prepared around the photo on screen
    /// ([`AROUND`]), not the whole view; a folder being culled is prepared whole.
    pub fn prep_tick(&mut self, mode: Mode, at: Option<PhotoId>, backward: bool) {
        self.prep_tick_inner(mode, at, backward);
        // the build's render jobs, a few photos a tick, nearest the user first (making all of them
        // when it started froze the UI for 1.3 s in a 2,597-photo folder, 2026-11-10)
        self.prep.feeding = crate::cmd::previews::feed(self);
    }

    /// Whether the background build is still making its photos' render jobs: tick again within a
    /// frame or so, not only every half second.
    pub fn prep_feeding(&self) -> bool {
        self.prep.feeding
    }

    fn prep_tick_inner(&mut self, mode: Mode, at: Option<PhotoId>, backward: bool) {
        self.note_changes(at);
        let settings = self.prep.settings();
        let want = settings.mode(mode);
        let wanted = want.previews && settings.scope(mode) != Scope::OnScreen;
        // a folder being culled is prepared from the photo the user is on, in the grid too
        let at = if mode == Mode::Culling { at.or_else(|| self.active()) } else { at };
        if self.prep_follow_build(mode, wanted, at, backward) || !wanted {
            return;
        }
        let busy = self.preview_build.as_ref().is_some_and(|b| b.running());
        if !busy && self.resync_due() {
            self.start_resync(at, backward);
            return;
        }
        // an owed build needs no steps and no photo in the loupe; with nothing owed the grid starts
        // nothing (opening a library is not a reason to build, note 36)
        let owed = self.prep.unfinished;
        if at.is_none() && !owed {
            return;
        }
        let view = self.visible_cloned();
        let view_sig = signature(&view, self);
        let now = web_time::Instant::now();
        if self.prep.view_seen != Some(view_sig) {
            // another view than the one last seen: an owed build waits until it has been still
            self.prep.view_seen = Some(view_sig);
            self.prep.not_before = now.checked_add(self.prep.settle());
        }
        if let Some(at) = at {
            self.prep_count_step(view_sig, at);
        }
        // the photo the build goes from: the one in the loupe; in the grid the active photo, else
        // the view's first
        let Some(anchor) = at.or_else(|| self.active()).or_else(|| view.first().copied()) else { return };
        let (sig, ids) = self.prep_view(mode, anchor);
        // a folder browsed to be culled is prepared as soon as a photo of it is looked at (the
        // user waited on photo 1 for minutes and nothing was prepared); a library after a few steps
        let steps_needed = if mode == Mode::Culling { 1 } else { STEPS_BEFORE_BUILD };
        let ready = if owed { self.prep.not_before.is_none_or(|at| now >= at) } else { self.prep.steps >= steps_needed };
        if busy
            || !ready
            || self.prep.started_for == Some(sig)
            || self.prep.declined_for == Some(sig)
            || self.library.as_ref().is_none_or(|l| !l.on_disk)
        {
            return;
        }
        let ids: Vec<u64> = match settings.scope(mode) {
            Scope::Marked => ids.into_iter().filter(|id| marked(self, *id)).map(|id| id.0).collect(),
            _ => ids.into_iter().map(|id| id.0).collect(),
        };
        self.prep.started_for = Some(sig);
        // (nothing to build in this view: nothing starts, and what is owed stays owed)
        if ids.is_empty() {
            return;
        }
        // (`feed`: its jobs are made a few photos a frame, `feed` in the frontend's tick)
        let p = json!({"ids": ids, "background": true, "from": anchor.0, "backward": backward, "feed": true});
        match self.execute("library.buildPreviews", &p) {
            Ok(_) => {
                self.prep.build = self.preview_build.clone();
                self.prep.build_library = self.library_generation;
                self.prep.build_view = view_sig;
                // owed to the library until it has run to its end
                self.set_prep_unfinished(true);
            }
            Err(e) => log::info!("background preparation: {e}"),
        }
    }

    /// The build this preparation started, followed: re-aimed at the user while it runs, stopped
    /// when it is no longer wanted, when the user moved on, or when its library is no longer the
    /// one open, and, once it is over, what its end means for the view and for
    /// [`PrepState::unfinished`]. `true`: it is still running (nothing else starts).
    fn prep_follow_build(&mut self, mode: Mode, wanted: bool, at: Option<PhotoId>, backward: bool) -> bool {
        let Some(b) = self.prep.build.clone() else { return false };
        if b.running() {
            if self.prep.build_library != self.library_generation {
                // a build left over from the library before: its photos are another library's,
                // and `opened_library` forgot `started_for`; stopped, and the library now open is
                // prepared in its place (not counted as stopped by the user)
                b.cancel.store(true, Ordering::Relaxed);
                self.prep.superseded = true;
                return true;
            }
            if !wanted {
                // switched off: stopped by the preparation itself, so `started_for` goes too. Left
                // set, the stop would read as the user's once the build is over and previews are on
                // again, and this view would not be prepared again in this session (#3947)
                b.cancel.store(true, Ordering::Relaxed);
                self.prep.started_for = None;
            } else if let Some(id) = at {
                b.focus_on(id, backward);
            }
            // the user moved on to a photo the build doesn't hold (another folder, or another
            // stretch of the library): the build of the photos left behind makes way for the
            // ones in view (a wedding browsed after another folder kept building the other
            // folder's previews, 2026-11-09). A folder still being read keeps its build: the
            // photos read since join the next one, which skips what this one built. A photo the
            // build leaves out of the view it was started for (an unmarked photo with "Picks, rated
            // and labelled", a photo the filter hides) is no reason: the build that replaced it was
            // the same one, stopped again a frame later, over and over ("Stopping…" for good and
            // nothing rendered, 26.46.1)
            if wanted
                && !self.prep.resync
                && !b.cancel.load(Ordering::Relaxed)
                && let Some(id) = at
                && self.prep.started_for.is_some()
                && !b.order.contains(&id.0)
                && Some(self.prep_view(mode, id).0) != self.prep.started_for
            {
                b.cancel.store(true, Ordering::Relaxed);
                self.prep.superseded = true;
            }
            // in the grid: another view than the one the build was started for (a folder, a filter,
            // a sort, photos imported) is not prepared by it; stopped, and the view now shown is
            // prepared in its place (not counted as stopped by the user)
            if wanted
                && at.is_none()
                && !self.prep.resync
                && !b.cancel.load(Ordering::Relaxed)
                && self.prep.started_for.is_some()
                && self.prep.build_view != signature(&self.visible_cloned(), self)
            {
                b.cancel.store(true, Ordering::Relaxed);
                self.prep.superseded = true;
            }
            // photos changed off screen go before the rest of the view: its build makes way
            if wanted && !self.prep.resync && self.resync_due() && !b.cancel.load(Ordering::Relaxed) {
                b.cancel.store(true, Ordering::Relaxed);
                self.prep.superseded = true;
            }
            return true;
        }
        self.prep.build = None;
        let mine = self.preview_build.as_ref().is_some_and(|p| Arc::ptr_eq(p, &b));
        // (a build left over from a library closed since says nothing about the one open now)
        let here = self.prep.build_library == self.library_generation;
        let resync = std::mem::take(&mut self.prep.resync);
        if std::mem::take(&mut self.prep.superseded) {
            // made way for the changed photos, or the library left: may start again afterwards
            // (built photos are skipped), and what is owed stays owed
            self.prep.started_for = None;
        } else if b.cancel.load(Ordering::Relaxed) {
            if resync {
                // stopped from its row: these photos are left as they are
                self.prep.changed.clear();
            } else if mine && wanted {
                // stopped from its row: not again for this view, and nothing is owed any more (a
                // stop the preparation made itself cleared `started_for`, and leaves the build owed)
                if here && self.prep.started_for.is_some() {
                    self.set_prep_unfinished(false);
                }
                self.prep.declined_for = self.prep.started_for;
            } else {
                // replaced by a build that was asked for, or switched off: may start again later
                self.prep.started_for = None;
            }
        } else if here && !resync {
            // ran to its end: nothing is owed
            self.set_prep_unfinished(false);
        }
        false
    }

    /// One more photo seen in the loupe in the view `sig` (the count starts again in another view).
    fn prep_count_step(&mut self, sig: u64, at: PhotoId) {
        if self.prep.steps_in != Some(sig) {
            self.prep.steps_in = Some(sig);
            self.prep.steps = 0;
            self.prep.last_at = None;
        }
        if self.prep.last_at != Some(at) {
            self.prep.last_at = Some(at);
            self.prep.steps = self.prep.steps.saturating_add(1);
        }
    }

    /// Note that a build is owed to this library, or no longer ([`PrepState::unfinished`]), and
    /// save it with the library's preferences when it changed. A save that fails is not an error
    /// here: the session goes on by what it noted, and the file is at worst one build behind (a
    /// restart takes up a build that finds everything built, or takes none up).
    fn set_prep_unfinished(&mut self, unfinished: bool) {
        if self.prep.unfinished != unfinished {
            self.prep.unfinished = unfinished;
            let _ = self.save_prefs();
        }
    }

    /// How long until the background preparation has something to do with nothing else happening:
    /// an unfinished build taken up again once the view has been still ([`SETTLE_AFTER`]). The app
    /// asks for a frame then, so it doesn't wait for the next input. `None` when nothing waits or
    /// the time has come.
    pub fn prep_wake_in(&self) -> Option<std::time::Duration> {
        let at = self.prep.not_before.filter(|_| self.prep.unfinished)?;
        let wait = at.saturating_duration_since(web_time::Instant::now());
        (!wait.is_zero()).then_some(wait)
    }

    /// What the background build prepares with the user on photo `at` in `mode`, and its identity:
    /// a folder being culled whole; a library the stretch around `at` ([`AROUND`]), a build per
    /// stretch.
    fn prep_view(&mut self, mode: Mode, at: PhotoId) -> (u64, Vec<PhotoId>) {
        let ids = self.visible_cloned();
        let sig = signature(&ids, self);
        match (mode, ids.iter().position(|id| *id == at)) {
            (Mode::Culling, _) | (_, None) => (sig, ids),
            (_, Some(i)) => {
                let stretch = i / AROUND;
                let sig = sig ^ (stretch as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15);
                let from = (stretch * AROUND).saturating_sub(AROUND);
                (sig, ids.into_iter().skip(from).take(AROUND * 3).collect())
            }
        }
    }

    /// Note which photos' settings changed since the last look (cheap: only when the catalog
    /// changed, one pointer compare a photo). The photo on screen renders itself.
    fn note_changes(&mut self, at: Option<PhotoId>) {
        let rev = self.catalog.revision;
        let lib = self.library.as_ref().map(|l| l.dir.as_path());
        match &self.prep.seen_rev {
            Some((r, l)) if *r == rev && l.as_deref() == lib => return,
            Some((_, l)) if l.as_deref() == lib => {}
            // another library (photo ids mean other photos): start over
            _ => {
                self.prep.seen.clear();
                self.prep.changed.clear();
            }
        }
        self.prep.seen_rev = Some((rev, lib.map(std::path::Path::to_path_buf)));
        let first = self.prep.seen.is_empty();
        let mut any = false;
        for p in self.catalog.photos() {
            let ptr = Arc::as_ptr(&p.develop) as usize;
            if let Some(old) = self.prep.seen.insert(p.id, ptr)
                && old != ptr
                && !first
                && Some(p.id) != at
            {
                self.prep.changed.insert(p.id);
                any = true;
            }
        }
        if self.prep.seen.len() > self.catalog.len() {
            let catalog = &self.catalog;
            self.prep.seen.retain(|id, _| catalog.photo(*id).is_some());
        }
        if any {
            self.prep.changed_at = clock_ms();
        }
    }

    /// Whether the photos changed off screen are to be rendered again now: there are some, no edit
    /// is in progress and nothing changed for a moment.
    fn resync_due(&self) -> bool {
        !self.prep.changed.is_empty() && self.interaction.is_none() && clock_ms().saturating_sub(self.prep.changed_at) >= RESYNC_QUIET_MS
    }

    /// Render the previews of the photos changed off screen again, nearest the photo on screen
    /// first (in the direction of travel), the ones out of view last.
    fn start_resync(&mut self, at: Option<PhotoId>, backward: bool) {
        let view = self.visible_cloned();
        let here = at.and_then(|a| view.iter().position(|id| *id == a)).unwrap_or(0);
        let pos: std::collections::HashMap<PhotoId, usize> = view.iter().enumerate().map(|(i, id)| (*id, i)).collect();
        let changed = std::mem::take(&mut self.prep.changed);
        let mut ids: Vec<PhotoId> = changed.into_iter().filter(|id| self.catalog.photo(*id).is_some()).collect();
        let distance = |id: &PhotoId| match pos.get(id) {
            // ahead in the direction of travel first, then the one as far behind
            Some(&i) => {
                let (ahead, d) = if i >= here { (!backward, i - here) } else { (backward, here - i) };
                (0u8, d.saturating_mul(2).saturating_sub(usize::from(ahead)))
            }
            None => (1, 0),
        };
        ids.sort_by_key(distance);
        if ids.is_empty() {
            return;
        }
        let p = json!({"ids": ids.iter().map(|id| id.0).collect::<Vec<_>>(), "background": true});
        match self.execute("library.buildPreviews", &p) {
            Ok(_) => {
                self.prep.build = self.preview_build.clone();
                self.prep.build_library = self.library_generation;
                self.prep.resync = true;
            }
            Err(e) => log::info!("background preparation: {e}"),
        }
    }

    /// Change the background preparation settings: the app-wide ones, or (`library`) this library's
    /// own. Saved at once.
    pub fn set_prep_settings(&mut self, settings: PrepSettings, library: bool) -> crate::Result<()> {
        let settings = settings.sanitized();
        if library {
            self.prep.library = Some(settings);
        } else {
            self.prep.app = settings;
        }
        self.save_prefs()
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
        assert!(PrepSettings::default().patched(&json!({"library": {"previews": false, "unknown": true}})).is_err());
    }

    #[test]
    fn a_patch_with_an_unknown_field_is_rejected_at_every_level() {
        let original = PrepSettings::default().patched(&json!({"culling": {"ahead": 3}, "develop": {"ahead": 1}})).unwrap();
        let before = original;
        for patch in [
            json!({"unknown": true}),
            json!({"culling": {"ahead": 4}, "unknown": true}),
            json!({"culling": {"previews": false, "unknown": true}}),
            json!({"develop": {"ahead": 0, "unknown": true}}),
        ] {
            assert!(original.patched(&patch).is_err(), "{patch}");
            assert_eq!(original, before, "{patch} leaves the settings as they were");
        }
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
        assert!(!wait_for_pictures(&|| false, std::time::Duration::from_secs(5)), "eye finding waits for it");
        assert!(t.elapsed().as_millis() >= 100);
        assert!(!pictures_busy());
    }

    /// A picture missing on screen never holds background work back longer than the caller's
    /// bound; a stop request ends the wait.
    #[test]
    fn a_wait_for_pictures_is_bounded_and_can_be_stopped() {
        pictures_missing();
        let t = web_time::Instant::now();
        assert!(!wait_for_pictures(&|| false, std::time::Duration::from_millis(30)));
        assert!(t.elapsed().as_millis() < 140, "bounded by the caller");
        pictures_missing();
        assert!(wait_for_pictures(&|| true, std::time::Duration::from_secs(5)), "a stop ends the wait");
    }
}

#[cfg(test)]
mod lane_tests {
    use super::lanes_for;

    /// Two photos side by side only where the cores and the memory are there for them.
    #[test]
    fn a_small_memory_budget_builds_one_photo_at_a_time() {
        let gb = 1usize << 30;
        assert_eq!(lanes_for(32, 16 * gb), 2);
        assert_eq!(lanes_for(8, 3 * gb / 2), 1, "a phone: eight cores, a 1.5 GiB budget");
        assert_eq!(lanes_for(4, 16 * gb), 1);
    }
}
