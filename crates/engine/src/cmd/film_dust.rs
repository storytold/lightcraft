//! Find Film Dust and Hair (`spot.findFilmDust`): on a scanned negative or slide, the dust
//! specks, hairs and scratches on the film, healed with Remove spots. A scanner's infrared pass
//! (a VueScan or SilverFast RGBI TIFF, [`lcc_codecs::Decoded::infrared`]) says exactly where they
//! are; without one the converted picture is searched for the sharp bright marks they leave
//! ([`lcc_pipeline::film_dust`]).

use lcc_develop::{AutoSpot, AutoSpotKind, Spot, SpotMode};
use lcc_geom::{Affine, Point};
use lcc_pipeline::RenderRequest;
use serde_json::{Value, json};

use super::masks::{SPOT_SIZE, pick_source};
use super::{CommandSpec, bad, cmd, f64_or, has_active, str_param};
use crate::media::SourceLevel;
use crate::{Result, Session};

/// The long edge the photo is searched at: film dust and hairs are only a few pixels wide.
const SEARCH_EDGE: usize = 2400;

/// Where the defects come from.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    /// The infrared pass when the file has one, else the picture.
    Auto,
    /// The infrared pass only: an error without one.
    Infrared,
    /// The picture only.
    Picture,
}

/// The mask Film Dust makes of the infrared pass is named this, and a new one replaces it.
pub const MASK_NAME: &str = "Film dust (infrared)";
/// The grid a film-dust mask is stored on (the stored segmentation's side): dust is small, so the finest the
/// mask format keeps.
const MASK_SIDE: usize = lcc_develop::segmask::MAX_SIDE;

/// How many pixels, at the search edge, the marks are grown (shrunk when negative): the slider's range.
const EXPAND_RANGE: (f64, f64) = (-8.0, 40.0);

/// What a run reads: the photo's render at the search edge, and the infrared pass through the same
/// geometry when the photo has one and `source` allows it.
struct Read {
    img: lcc_raster::Rgba8,
    infrared: Option<lcc_raster::Plane>,
    settings: lcc_develop::DevelopSettings,
}

fn source_param(p: &Value, c: &str) -> Result<Source> {
    Ok(match str_param(p, "infrared").unwrap_or("auto") {
        "auto" => Source::Auto,
        "only" | "true" => Source::Infrared,
        "never" | "false" => Source::Picture,
        other => return Err(bad(c, format!("`infrared` must be auto, only or never, not {other:?}"))),
    })
}

fn expand_param(p: &Value) -> i32 {
    f64_or(p, "expand", 2.0).clamp(EXPAND_RANGE.0, EXPAND_RANGE.1).round() as i32
}

/// `expand` is given in pixels at the search edge; a smaller photo is grown by the same fraction.
fn expand_px(expand: i32, long: usize) -> i32 {
    (f64::from(expand) * long as f64 / SEARCH_EDGE as f64).round() as i32
}

fn read(s: &mut Session, c: &str, id: crate::PhotoId, source: Source) -> Result<Read> {
    // the uncropped photo, so positions are the spots' own coordinates
    // (never larger than the photo: a small photo blown up would make every hair a wide patch)
    let edge = s.catalog.photo(id).map_or(SEARCH_EDGE, |p| (p.width.max(p.height) as usize).clamp(64, SEARCH_EDGE));
    let job = s.render_job(id, edge, edge, false, false).ok_or_else(|| bad(c, "no photo"))?;
    let img = job.run().rendered.map_err(|e| bad(c, e))?.image;
    let settings = (*s.develop_of(id).unwrap_or_default()).clone();
    // the infrared pass through the same geometry as the picture (orientation, lens, perspective;
    // not the crop), at the picture's size
    let infrared = if source == Source::Picture {
        None
    } else {
        // the preview-level decode and the facts of that same decode (a plane the size of another
        // level's pixels would not fit)
        let photo = s.catalog.photo(id).cloned().ok_or_else(|| bad(c, "no photo"))?;
        let decoded = s.media.source_ref(&photo, SourceLevel::Preview).load_source().map_err(|e| bad(c, e))?;
        let src = decoded.image.clone();
        let info = decoded.info_or(crate::media::source_info(&photo));
        info.infrared.as_deref().and_then(|ir| {
            let mut uncropped = settings.clone();
            uncropped.crop.geometry = lcc_geom::CropGeometry::default();
            let req = RenderRequest { apply_crop: false, ..RenderRequest::fit(edge, edge) };
            let plane = lcc_pipeline::render_plane(&src, ir, &info, &uncropped, &req)?;
            Some(if (plane.width, plane.height) == (img.width, img.height) {
                plane
            } else {
                lcc_raster::resample::resize(&plane, img.width, img.height, lcc_raster::resample::Filter::Mitchell)
            })
        })
    };
    if source == Source::Infrared && infrared.is_none() {
        return Err(bad(c, "this photo has no infrared pass: a film scanner's RGBI file (VueScan 64-bit RGBI, SilverFast HDRi) carries one"));
    }
    Ok(Read { img, infrared, settings })
}

fn luma_of(img: &lcc_raster::Rgba8) -> lcc_raster::Plane {
    lcc_raster::Plane::from_fn(img.width, img.height, |x, y| {
        let q = img.get(x, y);
        (0.2126 * f32::from(q[0]) + 0.7152 * f32::from(q[1]) + 0.0722 * f32::from(q[2])) / 255.0
    })
}

fn find_film_dust(s: &mut Session, p: &Value) -> Result<Value> {
    let c = "spot.findFilmDust";
    let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
    let source = source_param(p, c)?;
    let sensitivity = (f64_or(p, "sensitivity", 50.0) as f32).clamp(0.0, 100.0);
    let expand = expand_param(p);
    let replace = p.get("replace").and_then(Value::as_bool).unwrap_or(true);
    let mode = match str_param(p, "mode").unwrap_or("fill") {
        "fill" => SpotMode::Fill,
        "heal" => SpotMode::Heal,
        other => return Err(bad(c, format!("`mode` must be fill or heal, not {other:?}"))),
    };
    let Read { img, infrared, settings } = read(s, c, id, source)?;
    let mut dd = settings;
    let found_all = match &infrared {
        Some(ir) => lcc_pipeline::film_dust::detect_infrared(ir, &luma_of(&img), sensitivity),
        None => lcc_pipeline::film_dust::detect(&img, sensitivity),
    };
    // only what's inside the crop: a scan's rebate (edge print, sprocket holes) is usually
    // cropped away, and its lettering isn't dust
    let (w, h) = (img.width as f64, img.height as f64);
    let geometry = dd.crop.geometry;
    let (crop, to_crop) = (geometry.rect_px(w, h), Affine::rotate_about(geometry.angle.to_radians(), Point::new(w / 2.0, h / 2.0)));
    let inside = |q: &(f64, f64)| crop.contains(to_crop.apply(Point::new(q.0 * w, q.1 * h)));
    let found: Vec<_> = found_all.into_iter().filter(|d| d.points.iter().filter(|q| inside(q)).count() * 2 > d.points.len()).collect();
    let out: Vec<Value> =
        found.iter().map(|d| json!({"points": d.points.iter().map(|q| [q.0, q.1]).collect::<Vec<_>>(), "size": d.radius})).collect();
    let used_infrared = infrared.is_some();
    let reply = |added: usize, replaced: usize| json!({"spots": out, "added": added, "replaced": replaced, "infrared": used_infrared, "sensitivity": sensitivity, "expand": expand});
    if !p.get("add").and_then(Value::as_bool).unwrap_or(true) {
        return Ok(reply(0, 0));
    }
    // the spots an earlier run added go (the user's own stay), so a run at another sensitivity
    // replaces them instead of piling up
    let before = dd.spots.len();
    if replace {
        dd.spots.retain(|sp| !sp.auto.is_some_and(|a| a.kind == AutoSpotKind::FilmDust));
    }
    let replaced = before - dd.spots.len();
    if found.is_empty() && replaced == 0 {
        return Ok(reply(0, 0));
    }
    // each mark's spot is grown (or shrunk) by the expand: a fraction of the long edge, as spot sizes are
    let grow = f64::from(expand) / SEARCH_EDGE as f64;
    let base = Spot::default();
    for d in &found {
        let mut spot = Spot {
            mode,
            points: d.points.iter().map(|q| Point::new(q.0, q.1)).collect(),
            size: (d.radius + grow).clamp(SPOT_SIZE.0, SPOT_SIZE.1),
            feather: base.feather,
            opacity: 100.0,
            // a Fill is rebuilt from its surroundings: no source; a Heal copies one
            source_offset: if mode == SpotMode::Heal { d.source.map(|o| Point::new(o.0, o.1)) } else { None },
            auto: Some(AutoSpot { kind: AutoSpotKind::FilmDust, confirmed: used_infrared, confidence: d.strength.clamp(0.0, 1.0) }),
            ..Spot::default()
        };
        if mode == SpotMode::Heal && spot.source_offset.is_none() {
            spot.source_offset = pick_source(s, id, &dd, &spot, None);
        }
        dd.spots.push(spot);
    }
    s.set_develop(id, dd, "Find Film Dust and Hair")?;
    Ok(reply(found.len(), replaced))
}

/// `spot.filmDustMask`: the infrared pass's marks as a mask of the photo (a stored segmentation
/// over the uncropped frame), named [`MASK_NAME`], replacing an earlier one, and selected: the
/// Remove panel shows it as its overlay, the brush adds to or takes from it, and Fill Selected
/// Mask / Generate into Selected Mask remove what it covers.
fn film_dust_mask(s: &mut Session, p: &Value) -> Result<Value> {
    let c = "spot.filmDustMask";
    let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
    let sensitivity = (f64_or(p, "sensitivity", 50.0) as f32).clamp(0.0, 100.0);
    let expand = expand_param(p);
    let Read { img, infrared, settings } = read(s, c, id, Source::Infrared)?;
    let ir = infrared.ok_or_else(|| bad(c, "no infrared pass"))?;
    let map = lcc_pipeline::film_dust::infrared_map(&ir, sensitivity, expand_px(expand, ir.width.max(ir.height)))
        .ok_or_else(|| bad(c, "the infrared pass is too small to read"))?;
    // the stored segmentation covers the uncropped frame stretched to a square, finest grid
    let side = MASK_SIDE.min(map.width.max(map.height)).max(16);
    let square = lcc_raster::resample::resize(&map, side, side, lcc_raster::resample::Filter::Mitchell);
    // a mark is a logit well above 0, clear film well below: the sigmoid of the bilinear sample
    // between them is the mask's soft edge
    let logits: Vec<f32> = square.data.iter().map(|v| if v.is_finite() && *v > 0.0 { 8.0 } else { -8.0 }).collect();
    let coverage = logits.iter().filter(|l| **l > 0.0).count() as f64 / logits.len().max(1) as f64;
    let seg = lcc_develop::SegMask::from_logits(side, &logits);
    // an earlier film-dust mask goes (the user's own masks stay)
    let old: Vec<u32> = settings.masks.iter().filter(|m| m.name == MASK_NAME).map(|m| m.id).collect();
    for mid in old {
        s.execute("mask.delete", &json!({"id": mid}))?;
    }
    let r = s.execute("mask.add", &json!({"kind": "object", "points": [], "seg": seg, "name": MASK_NAME}))?;
    let mid = r.get("activeMask").and_then(Value::as_u64).or_else(|| s.active_mask.map(u64::from));
    let _ = img;
    Ok(json!({"mask": mid, "coverage": coverage, "sensitivity": sensitivity, "expand": expand}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "spot.filmDustMask",
            "Film Dust Mask",
            ["Photo", "Develop Settings"],
            None,
            "{sensitivity?: 0..100 (50), expand?: px at the 2400 px search size (a smaller photo by the same fraction), -8..40 (2)} — the infrared pass's marks (dust, hairs, scratches) as a mask of the photo, named \"Film dust (infrared)\", replacing an earlier one and selected: the Remove panel shows it, the brush adds to or takes from it, and Fill Selected Mask / Generate into Selected Mask remove what it covers. An error without the pass → {mask, coverage: 0..1, sensitivity, expand}",
            has_active,
            film_dust_mask
        ),
        cmd!(
            "spot.findFilmDust",
            "Find Film Dust and Hair",
            ["Photo", "Develop Settings"],
            None,
            "{sensitivity?: 0..100 (50), expand?: px at the 2400 px search size, -8..40 (2): how far each mark's spot is grown (shrunk when negative), mode?: fill|heal (fill: each spot is rebuilt from its surroundings; heal: copied from one source), add?: bool (true), replace?: bool (true: the spots an earlier run added go first, so a run at another sensitivity replaces them), infrared?: auto|only|never (auto)} — on a scanned negative or slide, find dust specks, hairs and scratches on the film inside the crop and add a heal spot on each speck or patch and a heal stroke along each line (one undo step). A film scanner's infrared pass (a VueScan or SilverFast RGBI TIFF) says where they are, whatever the picture shows; without one the converted picture is searched for the sharp bright marks they leave on smooth areas. `only`: an error without the pass; `never`: the picture alone. The spots are tagged auto film dust (`confirmed` when they came from the pass) → {spots: [{points: [[x, y], …], size}], added, replaced, infrared: bool, sensitivity, expand}",
            has_active,
            find_film_dust
        ),
    ]
}
