//! An Upright correction (`Geometry::upright_transform`) is one photo's own analysis result: the
//! correction found for that image (by LightCraft, or Lightroom's saved one for it), stored so
//! previews and exports agree, in the frame the photo is shown in. When settings move or change
//! (issue #205):
//!
//! - between entries of the same file (a photo and its virtual copies) the correction moves too,
//!   turned by the difference in their Rotate Left / Right;
//! - between different files the rest of the Geometry group moves (the Upright mode, the Transform
//!   sliders, Guided guides, Constrain Crop), not the correction: the target keeps its own while
//!   its mode stays the same, else solves its own (a correction found for one scene is wrong for
//!   another);
//! - presets never carry a correction (they are for other photos);
//! - a change that turns a photo turns the correction it keeps, and a change of mode drops it to
//!   be solved again;
//! - settings JSON (`develop.merge`) that states the photo's own correction, in whatever frame it
//!   was read and with its numbers rounded, names that correction, which is kept as above; any
//!   other correction it states is taken as given.

use lightcraft_catalog::{Photo, PhotoId, Source};
use lightcraft_develop::DevelopSettings;
use lightcraft_geom::{Homography, Orientation};
use serde_json::Value;

/// The photo settings were taken from, as far as its Upright correction is concerned.
#[derive(Clone, Debug)]
pub struct UprightSource {
    source: Source,
    master: PhotoId,
    /// Its Rotate Left / Right when the settings were taken.
    user: Orientation,
    /// The library it was in (photo ids start again in each library).
    library: std::sync::Weak<()>,
}

impl UprightSource {
    /// Whether it was taken in the library `identity` is the identity of
    /// (`Session::library_identity`). Photo ids belong to one library, so a source from another
    /// one can't be matched to a photo by id.
    pub fn taken_in(&self, identity: &std::sync::Arc<()>) -> bool {
        std::sync::Weak::ptr_eq(&self.library, &std::sync::Arc::downgrade(identity))
    }
}

/// Photo `p` with settings `s`, in the library whose identity is `library`, as the source of a
/// transfer.
pub fn source_of(p: &Photo, s: &DevelopSettings, library: &std::sync::Arc<()>) -> UprightSource {
    UprightSource { source: p.source.clone(), master: p.copy_of.unwrap_or(p.id), user: s.orientation, library: std::sync::Arc::downgrade(library) }
}

/// Whether photo `p` (in the library `from` was taken in) shows the same file as `from`: the photo
/// itself, its original or another of its virtual copies, or another entry of the file.
fn same_file(from: &UprightSource, p: &Photo) -> bool {
    from.master == p.copy_of.unwrap_or(p.id) || from.source == p.source
}

/// The turn that takes a photo shown with `from` to `to` (both after the file's own orientation).
fn turn(from: Orientation, to: Orientation) -> Orientation {
    from.inverse().then(to)
}

/// Settings `after` of photo `p` (which had `before`) once `patch` was applied, with its Upright
/// correction settled as the module describes. `source`: the photo the patch was taken from, in
/// this photo's library (`None`: none, as for a preset).
pub fn settle(p: &Photo, before: &DevelopSettings, after: &mut DevelopSettings, patch: &Value, source: Option<&UprightSource>) {
    if patch.pointer("/geometry/upright_transform").is_none() {
        return settle_edit(before, after);
    }
    let g = &mut after.geometry;
    g.upright_transform = match source.filter(|from| same_file(from, p)) {
        // the same file's correction, in this entry's rotation
        Some(from) => g.upright_transform.map(|h| lightcraft_pipeline::upright::turned(&h, turn(from.user, after.orientation))),
        // another file's (or a preset's): not this photo's; it keeps its own while the mode stays
        None if g.upright == before.geometry.upright => {
            before.geometry.upright_transform.map(|h| lightcraft_pipeline::upright::turned(&h, turn(before.orientation, after.orientation)))
        }
        None => None,
    };
}

/// A change of a photo's own settings (`before` to `after`) that doesn't state a correction: one
/// that turns the photo turns the correction it keeps; one that changes the Upright mode drops it
/// to be solved again.
pub fn settle_edit(before: &DevelopSettings, after: &mut DevelopSettings) {
    let g = &mut after.geometry;
    if g.upright != before.geometry.upright {
        g.upright_transform = None;
    } else if after.orientation != before.orientation {
        g.upright_transform = g.upright_transform.map(|h| lightcraft_pipeline::upright::turned(&h, turn(before.orientation, after.orientation)));
    }
}

/// Settings `after` of a photo (which had `before`) once settings JSON `patch` was merged into them
/// (`develop.merge`), with its Upright correction settled. A correction the JSON states that is the
/// photo's own ([`names_own`]) counts as not stated: the photo keeps its correction, turned to the
/// stated orientation, or drops it for a new mode ([`settle_edit`]). Settings sent back whole carry
/// the correction as they were read: before a turn, or before an earlier send turned it, and maybe
/// with the numbers rewritten. Any other stated correction is taken as given.
pub fn settle_json(before: &DevelopSettings, after: &mut DevelopSettings, patch: &Value) {
    if let Some(v) = patch.pointer("/geometry/upright_transform") {
        let stated = serde_json::from_value::<Option<Homography>>(v.clone()).ok();
        if !stated.is_some_and(|h| names_own(h, before.geometry.upright_transform)) {
            return;
        }
        after.geometry.upright_transform = before.geometry.upright_transform;
    }
    settle_edit(before, after);
}

/// The eight frames a photo's correction can be read in (any Rotate Left / Right and flip).
const FRAMES: [Orientation; 8] = [
    Orientation::Normal,
    Orientation::FlipH,
    Orientation::Rotate180,
    Orientation::FlipV,
    Orientation::Transpose,
    Orientation::Rotate90,
    Orientation::Transverse,
    Orientation::Rotate270,
];

/// Whether correction `stated` is the photo's own correction `own`, read in any of its frames
/// (`upright::turned` only swaps and negates elements, so each frame's copy is exact), up to
/// [`SAME_CORRECTION`]. Whichever frames match, keeping `own` turned to the stated orientation gives
/// the same result.
fn names_own(stated: Option<Homography>, own: Option<Homography>) -> bool {
    match (stated, own) {
        (None, None) => true,
        (Some(s), Some(o)) => FRAMES.iter().any(|f| same_correction(&s, &lightcraft_pipeline::upright::turned(&o, *f))),
        _ => false,
    }
}

/// How far apart two corrections may be, per element with each scaled so its last element is 1,
/// and still be the same correction.
///
/// - A program that writes the numbers its own way may round them. At 6 decimals that moves an
///   element by at most 5e-7, about twice that after the scaling, and at 9 or 12 by far less: the
///   limit is ten times the 6-decimal error.
/// - Corrections this close make the same picture. Moving each element of a typical correction by
///   1e-5 moves no corner of the photo by more than about 6e-5 of half its long edge: under 0.2 px at
///   6000 px. So a correction stated on purpose and found this close to the photo's own (two
///   photos' Level corrections can be) loses at most that much by being taken as the photo's own,
///   and anything further apart is taken as given.
const SAME_CORRECTION: f64 = 1e-5;

/// Whether corrections `a` and `b` are the same up to [`SAME_CORRECTION`] (never for a matrix that
/// can't be scaled: a last element of 0 or not finite).
fn same_correction(a: &Homography, b: &Homography) -> bool {
    let ([.., sa], [.., sb]) = (a.0, b.0);
    sa.is_finite() && sb.is_finite() && sa != 0.0 && sb != 0.0 && a.0.iter().zip(b.0.iter()).all(|(x, y)| (x / sa - y / sb).abs() <= SAME_CORRECTION)
}

/// `patch` applied to settings `before` of photo `p`, taken from `source` ([`settle`]).
pub fn transferred(p: &Photo, before: &DevelopSettings, patch: &Value, source: Option<&UprightSource>) -> DevelopSettings {
    let mut after = lightcraft_develop::apply_partial(before, patch, 1.0);
    settle(p, before, &mut after, patch, source);
    after
}

/// `preset` at `amount` applied to settings `before` of photo `p`, as Apply Preset stores it: a
/// preset carries no Upright correction ([`settle`] with no source photo). Previews use it too.
pub fn apply_preset(p: &Photo, before: &DevelopSettings, preset: &lightcraft_develop::Preset, amount: f64) -> DevelopSettings {
    let mut after = preset.apply(before, amount);
    settle(p, before, &mut after, &preset.settings, None);
    after
}

/// Preset settings without a saved Upright correction (the mode and the sliders stay).
pub fn without_correction(mut settings: Value) -> Value {
    if let Some(g) = settings.get_mut("geometry").and_then(Value::as_object_mut) {
        g.remove("upright_transform");
    }
    settings
}
