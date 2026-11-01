//! The loupe's look-ahead: stand-ins and thumbnails for the photos it is likely to be stepped to
//! next (PERF-CULL-2).

use super::{LightcraftApp, PhotoId};

/// How many photos ahead (in the direction of travel) the loupe makes its stand-in for, and how
/// many behind (a step back is common when culling).
const AHEAD: usize = 5;
const BEHIND: usize = 1;
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
    /// (photo, direction, catalog revision, visible list generation, edge, crop) the requests
    /// were made for: a new source, filter or sort changes the neighbours without an edit.
    made_for: Option<(PhotoId, i8, u64, u64, usize, bool)>,
}

impl LookAhead {
    /// The photos to prepare around `id` at position `i` of `ids`: [`AHEAD`] in the direction of
    /// travel, nearest first, then [`BEHIND`] the other way.
    fn targets(&self, ids: &[PhotoId], i: usize) -> Vec<PhotoId> {
        let dir: isize = if self.dir < 0 { -1 } else { 1 };
        let at = |k: isize| i.checked_add_signed(k).and_then(|j| ids.get(j)).copied();
        let ahead = (1..=AHEAD as isize).filter_map(|k| at(dir * k));
        let behind = (1..=BEHIND as isize).filter_map(|k| at(-dir * k));
        ahead.chain(behind).collect()
    }
}

/// Culling: make the loupe's stand-ins (the camera JPEG of an unedited raw, or a cached render) of
/// the next photos in the direction of travel, so that a step shows the next one at once instead
/// of after reading and decoding it (see [`Slot::QuickAhead`]).
pub(crate) fn look_ahead(app: &mut LightcraftApp, id: PhotoId, edge: usize, crop: bool) {
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
    let want = (id, app.look_ahead.dir, rev, visible_gen, edge, crop);
    if app.look_ahead.made_for == Some(want) {
        return;
    }
    app.look_ahead.made_for = Some(want);
    let Some(i) = ids.iter().position(|p| *p == id) else { return };
    let targets = app.look_ahead.targets(&ids, i);
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
