//! The loupe's look-ahead: stand-ins and thumbnails for the photos it is likely to be stepped to
//! next (PERF-CULL-2).

use super::{LightcraftApp, PhotoId};

/// How many photos ahead (in the direction of travel) the loupe makes its stand-in for, and how
/// many behind (a step back is common when culling).
const AHEAD: usize = 5;
pub(super) const BEHIND: usize = 1;
/// The look-ahead's priority: below the loupe's own render (100), above everything else.
const AHEAD_PRIORITY: u32 = 96;
/// Thumbnails the loupe keeps while stepping (as many as the grid keeps), and how many photos on
/// either side of the open one stay when it trims them.
const MAX_LOUPE_THUMBS: usize = 600;
const KEEP_AROUND: usize = 64;
/// The thumbnail size the look-ahead asks for (the filmstrip's at 2x).
const AHEAD_THUMB_EDGE: usize = 256;

/// What [`look_ahead`] last worked out (see `LightcraftApp::look_ahead`).
#[derive(Debug, Default)]
pub struct LookAhead {
    /// The last photo the loupe showed, for the direction of travel.
    last: Option<PhotoId>,
    /// +1 forward (the default), -1 backward.
    dir: i8,
    /// (photo, direction, catalog revision, visible list generation, edge, crop, which photos) the
    /// requests were made for: a new source, filter or sort changes the neighbours without an edit,
    /// and a change of the preparation settings what may be made.
    made_for: Option<(PhotoId, i8, u64, u64, usize, bool, Prepare)>,
}

/// Which photos the look-ahead may prepare, from Settings ▸ Performance for what the user is doing
/// ([`lightcraft_engine::prep::Mode`]): none when its "Photos ahead" is 0 or it prepares only the photo
/// on screen, else the photos of its "which photos".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Prepare {
    Nothing,
    Photos(lightcraft_engine::prep::Scope),
}

impl Prepare {
    /// For `ahead` photos ahead (the mode's count, 0 for the photo on screen only) of `scope`.
    pub(crate) fn of(ahead: usize, scope: lightcraft_engine::prep::Scope) -> Prepare {
        if ahead == 0 || scope == lightcraft_engine::prep::Scope::OnScreen { Prepare::Nothing } else { Prepare::Photos(scope) }
    }
}

impl LookAhead {
    /// The photos to prepare around `id` at position `i` of `ids`: [`AHEAD`] in the direction of
    /// travel, nearest first, then [`BEHIND`] the other way, of those `prepare` allows. (The
    /// stand-ins are cheap: as many as [`AHEAD`] whatever the mode's count, which is for the full
    /// renders made ahead.)
    fn targets(&self, app: &LightcraftApp, ids: &[PhotoId], i: usize, prepare: Prepare) -> Vec<PhotoId> {
        let Prepare::Photos(scope) = prepare else { return Vec::new() };
        let mut targets = self.targets_n(ids, i, AHEAD, BEHIND);
        if scope == lightcraft_engine::prep::Scope::Marked {
            targets.retain(|t| lightcraft_engine::prep::marked(&app.session, *t));
        }
        targets
    }

    /// `ahead` photos in the direction of travel from position `i` of `ids`, nearest first, then
    /// `behind` the other way.
    pub fn targets_n(&self, ids: &[PhotoId], i: usize, ahead: usize, behind: usize) -> Vec<PhotoId> {
        let dir: isize = if self.dir < 0 { -1 } else { 1 };
        let at = |k: isize| i.checked_add_signed(k).and_then(|j| ids.get(j)).copied();
        let cap = |n: usize| n.min(ids.len()) as isize;
        let ahead = (1..=cap(ahead)).filter_map(|k| at(dir * k));
        let behind = (1..=cap(behind)).filter_map(|k| at(-dir * k));
        ahead.chain(behind).collect()
    }

    /// Whether the user is stepping backwards.
    pub fn backward(&self) -> bool {
        self.dir < 0
    }
}

/// Culling: make the loupe's stand-ins (the camera JPEG of an unedited raw, or a cached render) of
/// the next photos in the direction of travel, so that a step shows the next one at once instead
/// of after reading and decoding it (see [`Slot::QuickAhead`]).
/// Only the photos `prepare` allows (Settings ▸ Performance): none at all asks for nothing, and drops the
/// stand-ins still queued.
pub(crate) fn look_ahead(app: &mut LightcraftApp, id: PhotoId, edge: usize, crop: bool, prepare: Prepare) {
    let rev = app.session.catalog.revision;
    if app.look_ahead.last != Some(id) {
        let ids = app.session.visible_cloned();
        let pos = |p: PhotoId| ids.iter().position(|q| *q == p);
        if let (Some(prev), Some(i)) = (app.look_ahead.last, pos(id))
            && let Some(j) = pos(prev)
        {
            app.look_ahead.dir = if j > i { -1 } else { 1 };
        }
        app.look_ahead.last = Some(id);
    }
    let (visible_gen, ids) = app.session.visible_shared();
    let want = (id, app.look_ahead.dir, rev, visible_gen, edge, crop, prepare);
    if app.look_ahead.made_for == Some(want) {
        return;
    }
    app.look_ahead.made_for = Some(want);
    let Some(i) = ids.iter().position(|p| *p == id) else { return };
    let targets = app.look_ahead.targets(app, &ids, i, prepare);
    // stepping through a whole wedding in the loupe would keep every thumbnail it ever drew: keep
    // the ones around the photo (what the filmstrip can show) once there are many
    if app.renderer.thumb_textures() > MAX_LOUPE_THUMBS {
        let keep = ids.iter().skip(i.saturating_sub(KEEP_AROUND)).take(2 * KEEP_AROUND + 1).copied().collect();
        app.renderer.evict_thumbs(&keep, MAX_LOUPE_THUMBS);
    }
    // their thumbnails first (a small embedded JPEG each, a few ms): something to show at once
    // even when a step outruns the full-size previews, and in full screen, where no filmstrip
    // asks for them
    for &nid in &targets {
        crate::panels::grid::request_thumb(app, nid, AHEAD_THUMB_EDGE, AHEAD_PRIORITY + 2);
    }
    let jobs = targets.into_iter().filter_map(|nid| app.session.quick_view_job(nid, edge, crop)).collect();
    app.renderer.look_ahead(jobs, AHEAD_PRIORITY);
}
