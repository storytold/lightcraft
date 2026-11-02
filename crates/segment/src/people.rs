//! People masks from Google's MediaPipe models (Apache-2.0), run by the pure-Rust TFLite
//! interpreter ([`crate::tflite`]) on the files Google publishes, unchanged:
//!
//! - [`PeopleModel`]: Selfie Multiclass — every pixel's probability of background, hair, body skin,
//!   face skin, clothes and other (accessories);
//! - [`FaceModel`]: Face Landmarker — BlazeFace finds the faces (on overlapping windows, as photos
//!   hold smaller faces than the selfies it was made for), FaceMesh-V2 places 478 landmarks on each,
//!   and [`crate::mediapipe`] turns them into eyes, irises, brows, lips and teeth.
//!
//! [`people_mask`] combines them into one mask for a People selection ("Hair" + "Lips" of the
//! second person from the left, …). Original LightCraft code.

use std::path::Path;

use crate::matte;
use crate::mediapipe::{self, Detection, FacePart, Grid};
use crate::tflite;
use crate::{Error, Result};

/// The Selfie Multiclass file in its model folder.
pub const PEOPLE_FILE: &str = "selfie_multiclass_256x256.tflite";
/// The Face Landmarker bundle in its model folder.
pub const FACE_FILE: &str = "face_landmarker.task";
/// Faces scoring below this are ignored (BlazeFace's false positives on hands, tattoos and foliage
/// score 0.5–0.6 on wedding photos; real faces, even in profile, above 0.65).
pub const FACE_THRESHOLD: f32 = 0.65;

fn read(dir: &Path, name: &str, max: u64) -> Result<Vec<u8>> {
    let p = dir.join(name);
    let len = std::fs::metadata(&p).map_err(|e| Error::Message(format!("{}: {e}", p.display())))?.len();
    if len > max {
        return Err(Error::Message(format!("{}: unexpectedly large ({len} bytes)", p.display())));
    }
    std::fs::read(&p).map_err(|e| Error::Message(format!("{}: {e}", p.display())))
}

/// Selfie Multiclass.
pub struct PeopleModel {
    net: tflite::Model,
}

/// Per-pixel class probabilities over the image stretched to `side × side` (softmax over
/// [`mediapipe::SELFIE_CLASSES`], interleaved: `[y][x][class]`).
pub struct PeopleMap {
    pub side: usize,
    pub probs: Vec<f32>,
}

impl PeopleMap {
    /// The summed probability of `classes` at normalized point `(u, v)` (bilinear).
    pub fn at(&self, classes: &[usize], u: f32, v: f32) -> f32 {
        let n = mediapipe::SELFIE_CLASSES.len();
        let s = self.side;
        if s == 0 {
            return 0.0;
        }
        let fx = (u * s as f32 - 0.5).clamp(0.0, (s - 1) as f32);
        let fy = (v * s as f32 - 0.5).clamp(0.0, (s - 1) as f32);
        let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
        let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
        let px = |x: usize, y: usize| -> f32 {
            let base = (y.min(s - 1) * s + x.min(s - 1)) * n;
            classes.iter().filter_map(|c| self.probs.get(base + c)).sum()
        };
        let top = px(x0, y0) * (1.0 - tx) + px(x0 + 1, y0) * tx;
        let bot = px(x0, y0 + 1) * (1.0 - tx) + px(x0 + 1, y0 + 1) * tx;
        (top * (1.0 - ty) + bot * ty).clamp(0.0, 1.0)
    }

    /// The share of the image that is a person (any class but background), 0..1.
    pub fn person_share(&self) -> f32 {
        let n = mediapipe::SELFIE_CLASSES.len();
        let cells = self.probs.len() / n.max(1);
        if cells == 0 {
            return 0.0;
        }
        self.probs.chunks(n).filter(|p| p.first().is_some_and(|bg| *bg < 0.5)).count() as f32 / cells as f32
    }
}

impl PeopleModel {
    pub fn load(dir: &Path) -> Result<Self> {
        let bytes = read(dir, PEOPLE_FILE, 64 << 20)?;
        let net = tflite::Model::from_bytes(&bytes)?;
        let side = mediapipe::SELFIE_INPUT;
        if net.input_shapes() != vec![vec![1, side, side, 3]]
            || net.output_shapes().first() != Some(&vec![1, side, side, mediapipe::SELFIE_CLASSES.len()])
        {
            return Err(Error::Message(format!("{}: not the Selfie Multiclass model", dir.join(PEOPLE_FILE).display())));
        }
        Ok(Self { net })
    }

    /// Class probabilities for an 8-bit RGB image (`w × h`), stretched to the model's input.
    pub fn segment(&self, rgb: &[u8], w: usize, h: usize) -> Result<PeopleMap> {
        let side = mediapipe::SELFIE_INPUT;
        let (sx, sy) = (side as f32 / w.max(1) as f32, side as f32 / h.max(1) as f32);
        let input = mediapipe::warp(rgb, w, h, &[sx, 0.0, 0.0, 0.0, sy, 0.0], side, -1.0, 1.0);
        let out = self.net.run(&[&input])?;
        let logits = out.into_iter().next().ok_or_else(|| Error::Message("Selfie Multiclass gave no output".into()))?;
        if logits.len() != side * side * mediapipe::SELFIE_CLASSES.len() {
            return Err(Error::Message("Selfie Multiclass: unexpected output size".into()));
        }
        Ok(PeopleMap { side, probs: mediapipe::selfie_softmax(&logits) })
    }
}

/// Face Landmarker: BlazeFace + FaceMesh-V2.
pub struct FaceModel {
    detector: tflite::Model,
    mesh: tflite::Model,
}

/// A face with its landmarks (photo pixels).
#[derive(Clone, Debug)]
pub struct Face {
    pub detection: Detection,
    /// FaceMesh's 478 landmarks, when it accepted the face (it doesn't handle strong profiles).
    pub landmarks: Option<Vec<[f32; 2]>>,
}

impl FaceModel {
    pub fn load(dir: &Path) -> Result<Self> {
        let bundle = read(dir, FACE_FILE, 64 << 20)?;
        let entry = |name: &str| {
            tflite::zip::entry(&bundle, name).ok_or_else(|| Error::Message(format!("{}: no {name} inside", dir.join(FACE_FILE).display())))
        };
        let detector = tflite::Model::from_bytes(entry("face_detector.tflite")?)?;
        let mesh = tflite::Model::from_bytes(entry("face_landmarks_detector.tflite")?)?;
        let (b, m) = (mediapipe::BLAZEFACE_INPUT, mediapipe::MESH_INPUT);
        if detector.input_shapes() != vec![vec![1, b, b, 3]] || mesh.input_shapes() != vec![vec![1, m, m, 3]] {
            return Err(Error::Message(format!("{}: not the Face Landmarker bundle", dir.join(FACE_FILE).display())));
        }
        Ok(Self { detector, mesh })
    }

    /// The faces in an 8-bit RGB image (`w × h`), left to right, scoring at least [`FACE_THRESHOLD`].
    pub fn detect(&self, rgb: &[u8], w: usize, h: usize) -> Result<Vec<Detection>> {
        let b = mediapipe::BLAZEFACE_INPUT;
        let mut all = Vec::new();
        for win in mediapipe::face_windows(w, h) {
            let input = mediapipe::window(rgb, w, h, win, b);
            let out = self.detector.run(&[&input])?;
            let (Some(reg), Some(cls)) = (out.first(), out.get(1)) else { return Err(Error::Message("BlazeFace gave no output".into())) };
            for d in mediapipe::decode_blazeface(reg, cls, FACE_THRESHOLD) {
                all.push(d.from_window(win.0 as f32, win.1 as f32, win.2 as f32));
            }
        }
        let mut faces = mediapipe::merge_windows(all, 0.3);
        faces.sort_by(|a, b| a.center()[0].total_cmp(&b.center()[0]));
        Ok(faces)
    }

    /// FaceMesh's landmarks for `det` (photo pixels); `None` when it doesn't see a face there.
    pub fn landmarks(&self, rgb: &[u8], w: usize, h: usize, det: &Detection) -> Result<Option<Vec<[f32; 2]>>> {
        let m = mediapipe::mesh_crop(det);
        let input = mediapipe::warp(rgb, w, h, &m, mediapipe::MESH_INPUT, 0.0, 1.0);
        let out = self.mesh.run(&[&input])?;
        let (Some(raw), Some(presence)) = (out.first(), out.get(1).and_then(|p| p.first())) else {
            return Err(Error::Message("FaceMesh gave no output".into()));
        };
        let p = 1.0 / (1.0 + (-presence.clamp(-80.0, 80.0)).exp());
        if p < 0.5 {
            return Ok(None);
        }
        Ok(mediapipe::landmarks_to_photo(raw, &m))
    }

    /// Faces with landmarks, left to right.
    pub fn faces(&self, rgb: &[u8], w: usize, h: usize) -> Result<Vec<Face>> {
        self.detect(rgb, w, h)?.into_iter().map(|d| Ok(Face { landmarks: self.landmarks(rgb, w, h, &d)?, detection: d })).collect()
    }
}

/// What a People selection asks for.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PeopleRequest {
    /// 0: everyone; n: the n-th person from the left (by their face).
    pub person: u32,
    /// Selfie Multiclass classes (indices into [`mediapipe::SELFIE_CLASSES`]).
    pub classes: Vec<usize>,
    pub face_parts: Vec<FacePart>,
}

impl PeopleRequest {
    /// From Lightroom's part names (`"Entire Person"`, `"Face Skin"`, `"Body Skin"`, `"Hair"`,
    /// `"Clothes"`, `"Eyebrows"`, `"Eye Sclera"`, `"Iris and Pupil"`, `"Lips"`, `"Teeth"`); none,
    /// or "Entire Person", is the whole person. `None` for a name it doesn't know.
    pub fn parse(person: u32, parts: &[String]) -> Option<PeopleRequest> {
        let mut r = PeopleRequest { person, ..Default::default() };
        let whole = [1, 2, 3, 4, 5];
        if parts.is_empty() {
            r.classes = whole.to_vec();
        }
        for p in parts {
            let n = p.to_ascii_lowercase();
            let class = match n.as_str() {
                "person" | "entire person" | "entireperson" => {
                    r.classes.extend(whole);
                    continue;
                }
                "hair" => 1,
                "body skin" | "bodyskin" => 2,
                "face skin" | "faceskin" => 3,
                "skin" => {
                    r.classes.extend([2, 3]);
                    continue;
                }
                "clothes" | "clothing" => 4,
                _ => {
                    r.face_parts.push(FacePart::parse(&n)?);
                    continue;
                }
            };
            r.classes.push(class);
        }
        r.classes.sort_unstable();
        r.classes.dedup();
        r.face_parts.dedup();
        Some(r)
    }

    /// The entire person (every class, or the request with no parts), face parts aside.
    pub fn is_whole_person(&self) -> bool {
        self.classes == [1, 2, 3, 4, 5]
    }

    pub fn needs_people(&self) -> bool {
        !self.classes.is_empty()
    }

    /// Face parts, or one person among several (told apart by their faces).
    pub fn needs_faces(&self) -> bool {
        !self.face_parts.is_empty() || self.person > 0
    }
}

/// A computed mask: `side²` logits over `rect` of the image (normalized; `None` = all of it).
pub struct MaskGrid {
    pub side: usize,
    pub logits: Vec<f32>,
    pub rect: Option<[f64; 4]>,
    /// Faces whose features couldn't be located (no landmarks: profiles), so face skin there
    /// still includes eyes and mouth (only hair is left out).
    pub faces_without_features: usize,
}

/// A class matte made from the 256-pixel class map, refined on the photo: specks the coarse map
/// leaves inside a person (lace, sequins: `fill`, for clothes and the entire person) filled, then
/// a guided filter on the photo's luminance at the grid's resolution, so the edge follows the
/// garment's or the body's own edge instead of the class map's blur.
fn refine(grid: &Grid, cov: &mut Vec<f32>, rgb: &[u8], fill: bool) {
    let side = grid.side;
    if fill {
        // at most 1/2000 of the grid: a 23-cell square at 1024
        matte::fill_small_holes(cov, side, (side * side / 2000).max(4));
    }
    if rgb.len() < grid.w.saturating_mul(grid.h).saturating_mul(3) || grid.w == 0 || grid.h == 0 {
        return;
    }
    let guide: Vec<f32> = (0..side * side)
        .map(|c| {
            let [x, y] = grid.centre(c % side, c / side);
            let (x, y) = ((x.max(0.0) as usize).min(grid.w - 1), (y.max(0.0) as usize).min(grid.h - 1));
            let p = rgb.get((y * grid.w + x) * 3..(y * grid.w + x) * 3 + 3).unwrap_or(&[0, 0, 0]);
            (0.299 * f32::from(p[0]) + 0.587 * f32::from(p[1]) + 0.114 * f32::from(p[2])) / 255.0
        })
        .collect();
    // a radius of about 1/170 of the grid (6 cells at 1024): wide enough to pull a 256-map's
    // blur onto the edge, narrow enough not to cross to the next person
    let r = (side / 170).max(2);
    *cov = matte::guided_filter(cov, &guide, side, side, r, 2e-3);
}

/// Selfie Multiclass's skin classes (body skin, face skin).
const SKIN: [usize; 2] = [2, 3];
/// What a garment never includes: hair, body skin, face skin.
const NOT_GARMENT: [usize; 3] = [1, 2, 3];

/// The photo's colours at the grid's cell centres (0..1, interleaved RGB), the guide of
/// [`matte::guided_filter_rgb`]; `None` when `rgb` isn't the grid's photo.
fn colour_guide(grid: &Grid, rgb: &[u8]) -> Option<Vec<f32>> {
    if grid.w == 0 || grid.h == 0 || rgb.len() < grid.w.saturating_mul(grid.h).saturating_mul(3) {
        return None;
    }
    let side = grid.side;
    Some(
        (0..side * side)
            .flat_map(|c| {
                let [x, y] = grid.centre(c % side, c / side);
                let (x, y) = ((x.max(0.0) as usize).min(grid.w - 1), (y.max(0.0) as usize).min(grid.h - 1));
                let p = rgb.get((y * grid.w + x) * 3..(y * grid.w + x) * 3 + 3).unwrap_or(&[0, 0, 0]);
                [f32::from(p[0]) / 255.0, f32::from(p[1]) / 255.0, f32::from(p[2]) / 255.0]
            })
            .collect(),
    )
}

fn smoothstep(lo: f32, hi: f32, v: f32) -> f32 {
    let t = ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// A garment's coverage (`cov`, the grid's cells) cleaned up on the photo: the skin and hair of
/// the person wearing it taken out (the class map's, its edges pulled onto the photo's by a
/// colour guided filter — a neckline, bare arms and hands holding a bouquet aren't the dress),
/// then the garment's own edge pulled onto the photo's and steepened, so the outline is crisp
/// instead of a halo from a coarse model output stretched over the photo. `on_person`: the
/// garment is worn (skin and hair are cut only then).
fn clean_garment(grid: &Grid, cov: &mut Vec<f32>, rgb: &[u8], map: Option<&PeopleMap>, on_person: bool) {
    let side = grid.side;
    let Some(guide) = colour_guide(grid, rgb) else { return };
    // radius about 1/170 of the grid (6 cells at 1024), as for the class mattes
    let r = (side / 170).max(2);
    if let Some(map) = map.filter(|_| on_person) {
        let rect = grid.rect;
        let excl: Vec<f32> = (0..side * side)
            .map(|c| {
                let u = (rect[0] + ((c % side) as f64 + 0.5) / side as f64 * (rect[2] - rect[0])) as f32;
                let v = (rect[1] + ((c / side) as f64 + 0.5) / side as f64 * (rect[3] - rect[1])) as f32;
                map.at(&NOT_GARMENT, u, v)
            })
            .collect();
        let excl = matte::guided_filter_rgb(&excl, &guide, side, side, r, 1e-3);
        for (c, e) in cov.iter_mut().zip(&excl) {
            *c *= 1.0 - smoothstep(0.3, 0.6, *e);
        }
    }
    // (a wider window for the garment's own edge, a 64th of the grid — 16 cells, four of SAM's
    // 256 — so it reaches past a halo of that width to the photo's edge)
    let refined = matte::guided_filter_rgb(cov, &guide, side, side, (side / 64).max(r), 1e-3);
    // steeper: the refined edge's soft tail (the halo) goes, the inside stays whole
    *cov = refined.into_iter().map(|v| smoothstep(0.25, 0.75, v)).collect();
}

/// Whether a selection (`cov` over `grid`) is mostly on a person: at least half of what it selects
/// is a person in the class map (a garment worn, a bouquet held), not a dress on a hanger.
fn worn(grid: &Grid, cov: &[f32], map: &PeopleMap) -> bool {
    let side = grid.side;
    let rect = grid.rect;
    let (mut on, mut all) = (0usize, 0usize);
    // every 4th cell is plenty
    for c in (0..side * side).step_by(4) {
        if cov.get(c).is_some_and(|v| *v > 0.5) {
            let u = (rect[0] + ((c % side) as f64 + 0.5) / side as f64 * (rect[2] - rect[0])) as f32;
            let v = (rect[1] + ((c / side) as f64 + 0.5) / side as f64 * (rect[3] - rect[1])) as f32;
            all += 1;
            if map.at(&[0], u, v) < 0.5 {
                on += 1;
            }
        }
    }
    all > 0 && on * 2 >= all
}

/// Side of the grid a garment found by a coarse model (SAM 2.1's 256-cell masks) is refined at.
pub const GARMENT_SIDE: usize = 1024;

/// A remembered garment (or any remembered object) found by SAM 2.1 — `logits`, `lside²` over the
/// stretched photo — refined on the photo (`rgb`, `w × h`, the image SAM saw) at
/// [`GARMENT_SIDE`] cells: specks in lace filled, the wearer's skin and hair cut out when it is
/// worn (`map`: the photo's class map, when the people model is installed), and the edge pulled
/// onto the photo's and made crisp. `None` for inputs of the wrong size or when nothing is left.
pub fn garment_mask(logits: &[f32], lside: usize, rgb: &[u8], w: usize, h: usize, map: Option<&PeopleMap>) -> Option<MaskGrid> {
    if lside == 0 || logits.len() != lside * lside || w == 0 || h == 0 {
        return None;
    }
    let side = GARMENT_SIDE;
    let grid = Grid { side, rect: [0.0, 0.0, 1.0, 1.0], w, h };
    let at = |x: usize, y: usize| logits.get(y.min(lside - 1) * lside + x.min(lside - 1)).copied().unwrap_or(-16.0);
    let mut cov: Vec<f32> = (0..side * side)
        .map(|c| {
            let fx = (((c % side) as f32 + 0.5) / side as f32 * lside as f32 - 0.5).clamp(0.0, (lside - 1) as f32);
            let fy = (((c / side) as f32 + 0.5) / side as f32 * lside as f32 - 0.5).clamp(0.0, (lside - 1) as f32);
            let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
            let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
            let l = (at(x0, y0) * (1.0 - tx) + at(x0 + 1, y0) * tx) * (1.0 - ty) + (at(x0, y0 + 1) * (1.0 - tx) + at(x0 + 1, y0 + 1) * tx) * ty;
            1.0 / (1.0 + (-l.clamp(-80.0, 80.0)).exp())
        })
        .collect();
    matte::fill_small_holes(&mut cov, side, (side * side / 2000).max(4));
    let on_person = map.is_some_and(|m| worn(&grid, &cov, m));
    clean_garment(&grid, &mut cov, rgb, map, on_person);
    if cov.iter().all(|c| *c < 0.5) {
        return None;
    }
    Some(MaskGrid { side, logits: mediapipe::coverage_to_logits(&cov), rect: None, faces_without_features: 0 })
}

/// The part of a `w × h` image a mask of face parts covers: the chosen faces' boxes with a margin, in image
/// proportions. `None`: no such face, or nothing of it inside the image.
fn face_parts_rect(chosen: &[usize], faces: &[Face], w: usize, h: usize) -> Option<[f64; 4]> {
    let boxes: Vec<&Detection> = chosen.iter().filter_map(|i| faces.get(*i)).map(|f| &f.detection).collect();
    if boxes.is_empty() {
        return None;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for b in boxes {
        let m = 0.3 * b.width().max(b.height());
        (x0, y0, x1, y1) = (x0.min(b.x0 - m), y0.min(b.y0 - m), x1.max(b.x1 + m), y1.max(b.y1 + m));
    }
    let r = [
        f64::from((x0 / w as f32).clamp(0.0, 1.0)),
        f64::from((y0 / h as f32).clamp(0.0, 1.0)),
        f64::from((x1 / w as f32).clamp(0.0, 1.0)),
        f64::from((y1 / h as f32).clamp(0.0, 1.0)),
    ];
    (r[2] > r[0] && r[3] > r[1]).then_some(r)
}

/// The features of every face (eyes, brows, mouth, facial hair) on `grid`, softened, to cut out of skin, and how many
/// faces had no landmarks to find them by.
fn feature_holes(grid: &Grid, faces: &[Face], rgb: &[u8]) -> (Vec<f32>, usize) {
    let mut holes = vec![0f32; grid.side * grid.side];
    let mut without = 0;
    for f in faces {
        match &f.landmarks {
            Some(lm) => mediapipe::feature_holes(grid, &mut holes, lm, rgb),
            None => without += 1,
        }
    }
    (mediapipe::soften(&holes, grid.side), without)
}

/// The People selection `req` on an 8-bit RGB image (`w × h`), from the Selfie Multiclass map and
/// the faces (left to right) its parts need. `None`: nothing of it is there (no such person, no
/// face for face parts).
pub fn people_mask(req: &PeopleRequest, rgb: &[u8], w: usize, h: usize, map: Option<&PeopleMap>, faces: &[Face]) -> Option<MaskGrid> {
    if w == 0 || h == 0 {
        return None;
    }
    // the faces it is about: everyone's, or the n-th from the left
    let chosen: Vec<usize> = match req.person {
        0 => (0..faces.len()).collect(),
        n => vec![(n as usize).checked_sub(1).filter(|i| *i < faces.len())?],
    };
    let aspect = w as f32 / h as f32;
    // a pixel belongs to the person whose face is nearest (distances in photo proportions)
    let owner = |u: f32, v: f32| -> Option<usize> {
        faces
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let [cx, cy] = f.detection.center();
                let (dx, dy) = ((u - cx / w as f32) * aspect, (v - cy / h as f32) * 0.6);
                (i, dx * dx + dy * dy)
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    };
    let mut rect = None;
    let side;
    // the entire person: everything that isn't background (eyes, mouth, hair, clothes, what they
    // hold), with nothing cut out of the face
    let whole = req.is_whole_person();
    let skin = !whole && req.classes.iter().any(|c| SKIN.contains(c));
    if req.needs_people() {
        // the whole image: classes cover bodies; finer when face features are drawn or cut out
        side = 1024;
    } else {
        // only face parts: a grid over the chosen faces
        rect = Some(face_parts_rect(&chosen, faces, w, h)?);
        side = 768;
    }
    let grid = Grid { side, rect: rect.unwrap_or([0.0, 0.0, 1.0, 1.0]), w, h };
    let mut cov = vec![0f32; side * side];
    // skin never includes the face's features: eyes, brows, mouth, facial hair (of every face,
    // whoever the mask is for)
    let (holes, faces_without_features) = if skin { feature_holes(&grid, faces, rgb) } else { (Vec::new(), 0) };
    let (skin_classes, other_classes): (Vec<usize>, Vec<usize>) = req.classes.iter().partition(|c| SKIN.contains(c));
    // skin and the other classes apart: the skin is refined on its own, then the features are cut
    // out of it (refining after the cut filled the eye and brow holes back in)
    let mut skin_cov = vec![0f32; if skin { side * side } else { 0 }];
    let mut mine = vec![true; if skin { side * side } else { 0 }];
    if let Some(map) = map.filter(|_| req.needs_people()) {
        let r = grid.rect;
        for j in 0..side {
            for i in 0..side {
                let u = (r[0] + (i as f64 + 0.5) / side as f64 * (r[2] - r[0])) as f32;
                let v = (r[1] + (j as f64 + 0.5) / side as f64 * (r[3] - r[1])) as f32;
                if req.person > 0 && owner(u, v).is_none_or(|o| !chosen.contains(&o)) {
                    if let Some(m) = mine.get_mut(j * side + i) {
                        *m = false;
                    }
                    continue;
                }
                let value = if whole {
                    1.0 - map.at(&[0], u, v)
                } else {
                    if let Some(s) = skin_cov.get_mut(j * side + i) {
                        *s = map.at(&skin_classes, u, v);
                    }
                    if other_classes.is_empty() { 0.0 } else { map.at(&other_classes, u, v) }
                };
                if let Some(c) = cov.get_mut(j * side + i) {
                    *c = value.clamp(0.0, 1.0);
                }
            }
        }
    }
    if map.is_some() && req.needs_people() {
        if whole || !other_classes.is_empty() {
            refine(&grid, &mut cov, rgb, whole || req.classes.contains(&4));
        }
        if !whole && req.classes == [4] {
            // Clothes alone: the class map's blur leaks onto a neckline, arms and hands
            clean_garment(&grid, &mut cov, rgb, map, true);
        }
        if skin {
            // the face skin up to the hairline (the class map fades out on the upper forehead)
            for (k, f) in faces.iter().enumerate() {
                if req.person > 0 && !chosen.contains(&k) {
                    continue;
                }
                if let (Some(lm), Some(map)) = (&f.landmarks, map) {
                    mediapipe::forehead(&grid, &mut skin_cov, lm, rgb, |u, v| map.at(&[1], u, v));
                }
            }
            refine(&grid, &mut skin_cov, rgb, false);
            for (k, c) in cov.iter_mut().enumerate() {
                let own = mine.get(k).copied().unwrap_or(true);
                let s = skin_cov.get(k).copied().unwrap_or(0.0) * (1.0 - holes.get(k).copied().unwrap_or(0.0));
                if own {
                    *c = (*c + s).clamp(0.0, 1.0);
                }
            }
        }
    }
    for i in &chosen {
        let Some(lm) = faces.get(*i).and_then(|f| f.landmarks.as_ref()) else { continue };
        for part in &req.face_parts {
            mediapipe::rasterize(&grid, &mut cov, lm, *part, rgb);
        }
    }
    if cov.iter().all(|c| *c < 0.5) {
        return None;
    }
    Some(MaskGrid { side, logits: mediapipe::coverage_to_logits(&cov), rect, faces_without_features })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_parse_lightrooms_part_names() {
        let r = PeopleRequest::parse(0, &[]).unwrap();
        assert_eq!(r.classes, vec![1, 2, 3, 4, 5]);
        assert!(!r.needs_faces());
        let r = PeopleRequest::parse(2, &["Hair".into(), "Lips".into(), "Iris and Pupil".into()]).unwrap();
        assert_eq!(r.classes, vec![1]);
        assert_eq!(r.face_parts, vec![FacePart::Lips, FacePart::Iris]);
        assert!(r.needs_faces() && r.needs_people());
        assert!(PeopleRequest::parse(0, &["Elbows".into()]).is_none());
    }

    fn map_left_half_hair() -> PeopleMap {
        let side = 16;
        let mut probs = vec![0f32; side * side * 6];
        for y in 0..side {
            for x in 0..side {
                let c = if x < side / 2 { 1 } else { 0 };
                probs[(y * side + x) * 6 + c] = 1.0;
            }
        }
        PeopleMap { side, probs }
    }

    #[test]
    fn masks_follow_the_classes_and_the_chosen_person() {
        let map = map_left_half_hair();
        assert!((map.person_share() - 0.5).abs() < 1e-6);
        let req = PeopleRequest::parse(0, &["Hair".into()]).unwrap();
        let m = people_mask(&req, &[], 100, 100, Some(&map), &[]).unwrap();
        assert_eq!(m.side, 1024);
        assert!(m.logits[512 * 1024 + 100] > 4.0 && m.logits[512 * 1024 + 900] < -4.0);
        // two faces: person 2 (the right one) owns the right half, where there is no hair
        let face = |x: f32| Face {
            detection: Detection { x0: x - 5.0, y0: 10.0, x1: x + 5.0, y1: 20.0, score: 1.0, keypoints: [[0.0; 2]; 6] },
            landmarks: None,
        };
        let faces = [face(25.0), face(75.0)];
        assert!(people_mask(&PeopleRequest::parse(2, &["Hair".into()]).unwrap(), &[], 100, 100, Some(&map), &faces).is_none());
        let m = people_mask(&PeopleRequest::parse(1, &["Hair".into()]).unwrap(), &[], 100, 100, Some(&map), &faces).unwrap();
        assert!(m.logits[512 * 1024 + 100] > 4.0);
        // no such person, or face parts without landmarks: nothing
        assert!(people_mask(&PeopleRequest::parse(3, &["Hair".into()]).unwrap(), &[], 100, 100, Some(&map), &faces).is_none());
        assert!(people_mask(&PeopleRequest::parse(0, &["Lips".into()]).unwrap(), &[], 100, 100, None, &faces).is_none());
        assert!(people_mask(&req, &[], 0, 0, Some(&map), &[]).is_none());
    }

    /// A synthetic face filling a 1000 × 1000 photo: oval, eyes, brows and lips as rings of landmarks.
    fn synthetic_face() -> Vec<[f32; 2]> {
        let mut lm = vec![[500.0f32, 500.0]; mediapipe::MESH_POINTS];
        let ring = |idx: &[usize], cx: f32, cy: f32, rx: f32, ry: f32, lm: &mut Vec<[f32; 2]>| {
            for (k, i) in idx.iter().enumerate() {
                let a = k as f32 / idx.len() as f32 * std::f32::consts::TAU;
                lm[*i] = [cx + rx * a.cos(), cy + ry * a.sin()];
            }
        };
        ring(&mediapipe::FACE_OVAL, 500.0, 500.0, 300.0, 400.0, &mut lm);
        ring(&mediapipe::EYES[0], 380.0, 400.0, 50.0, 18.0, &mut lm);
        ring(&mediapipe::EYES[1], 620.0, 400.0, 50.0, 18.0, &mut lm);
        for (e, (c, rim)) in mediapipe::IRISES.iter().enumerate() {
            let cx = if e == 0 { 380.0 } else { 620.0 };
            lm[*c] = [cx, 400.0];
            ring(rim, cx, 400.0, 12.0, 12.0, &mut lm);
        }
        for (b, (upper, lower)) in mediapipe::BROWS.iter().enumerate() {
            let cx = if b == 0 { 380.0 } else { 620.0 };
            for (k, i) in upper.iter().enumerate() {
                lm[*i] = [cx - 60.0 + 30.0 * k as f32, 335.0];
            }
            for (k, i) in lower.iter().enumerate() {
                lm[*i] = [cx - 60.0 + 30.0 * k as f32, 350.0];
            }
        }
        ring(&mediapipe::LIPS_OUTER, 500.0, 680.0, 90.0, 35.0, &mut lm);
        ring(&mediapipe::LIPS_INNER, 500.0, 680.0, 60.0, 12.0, &mut lm);
        lm[2] = [500.0, 590.0];
        lm[205] = [360.0, 560.0];
        lm[425] = [640.0, 560.0];
        lm
    }

    fn face_skin_everywhere(side: usize) -> PeopleMap {
        let mut probs = vec![0f32; side * side * 6];
        for px in probs.chunks_mut(6) {
            px[3] = 1.0;
        }
        PeopleMap { side, probs }
    }

    /// Regression (the user's bug): "Entire person" took face skin's cut-outs and lost the eyes,
    /// brows and mouth; and the 256-cell class map's specks of background in a lace dress showed
    /// as square holes. The entire person includes every feature of the face and has no specks.
    #[test]
    fn entire_person_keeps_the_eyes_and_mouth_and_has_no_square_holes() {
        let lm = synthetic_face();
        let rgb = vec![150u8; 1000 * 1000 * 3];
        let det = Detection { x0: 200.0, y0: 100.0, x1: 800.0, y1: 900.0, score: 1.0, keypoints: [[0.0; 2]; 6] };
        let faces = [Face { detection: det, landmarks: Some(lm) }];
        // face skin above, clothes below with a few one-cell specks of background (lace)
        let side = 64;
        let mut probs = vec![0f32; side * side * 6];
        for y in 0..side {
            for x in 0..side {
                let c = if [(20, 50), (40, 55), (33, 60)].contains(&(x, y)) {
                    0
                } else if y < 45 {
                    3
                } else {
                    4
                };
                probs[(y * side + x) * 6 + c] = 1.0;
            }
        }
        let map = PeopleMap { side, probs };
        for parts in [vec![], vec!["Entire Person".to_string()]] {
            let req = PeopleRequest::parse(1, &parts).unwrap();
            assert!(req.is_whole_person());
            let m = people_mask(&req, &rgb, 1000, 1000, Some(&map), &faces).unwrap();
            let at = |x: usize, y: usize| m.logits[(y * m.side / 1000) * m.side + x * m.side / 1000];
            for (what, x, y) in
                [("eye", 380, 400), ("iris", 620, 400), ("brow", 380, 342), ("lips", 500, 650), ("mouth", 500, 680), ("cheek", 300, 560)]
            {
                assert!(at(x, y) > 2.0, "{what} is part of the entire person: {}", at(x, y));
            }
            // the specks (cells of 1000/64 pixels) are filled
            for (x, y) in [(20, 50), (40, 55), (33, 60)] {
                let (px, py) = ((x * 1000 + 500) / side, (y * 1000 + 500) / side);
                assert!(at(px, py) > 0.0, "no hole at {px},{py}: {}", at(px, py));
            }
        }
        // face skin still leaves the eyes out
        let m = people_mask(&PeopleRequest::parse(1, &["Face Skin".into()]).unwrap(), &rgb, 1000, 1000, Some(&map), &faces).unwrap();
        assert!(m.logits[(400 * m.side / 1000) * m.side + 380 * m.side / 1000] < -2.0);
    }

    /// Face skin has holes where the eyes, brows and mouth are, and keeps the cheeks and forehead;
    /// hair inside the face (the model's hair class) is not skin; body skin loses the features too.
    #[test]
    fn face_skin_leaves_the_features_out() {
        let lm = synthetic_face();
        let rgb = vec![150u8; 1000 * 1000 * 3];
        let det = Detection { x0: 200.0, y0: 100.0, x1: 800.0, y1: 900.0, score: 1.0, keypoints: [[0.0; 2]; 6] };
        let faces = [Face { detection: det, landmarks: Some(lm) }];
        let map = face_skin_everywhere(64);
        let req = PeopleRequest::parse(0, &["Face Skin".into()]).unwrap();
        let m = people_mask(&req, &rgb, 1000, 1000, Some(&map), &faces).unwrap();
        assert_eq!((m.side, m.faces_without_features), (1024, 0));
        let at = |x: usize, y: usize| m.logits[(y * 1024 / 1000) * 1024 + x * 1024 / 1000];
        for (what, x, y) in
            [("eye", 380, 400), ("iris", 620, 400), ("eye corner (lashes)", 425, 405), ("brow", 380, 342), ("lips", 500, 650), ("mouth", 500, 680)]
        {
            assert!(at(x, y) < -2.0, "{what} is not skin: {}", at(x, y));
        }
        for (what, x, y) in [("cheek", 300, 560), ("forehead", 500, 220), ("chin", 500, 800)] {
            assert!(at(x, y) > 2.0, "{what} is skin: {}", at(x, y));
        }
        // soft edges: something between in and out next to the eye
        let edge = (300..400).map(|y| at(380, y)).any(|l| l.abs() < 2.0);
        assert!(edge, "feathered edge");
        // the same holes in body skin
        let body = PeopleMap { side: 64, probs: map.probs.iter().enumerate().map(|(i, _)| if i % 6 == 2 { 1.0 } else { 0.0 }).collect() };
        let req = PeopleRequest::parse(0, &["Body Skin".into()]).unwrap();
        let m2 = people_mask(&req, &rgb, 1000, 1000, Some(&body), &faces).unwrap();
        assert!(m2.logits[(400 * 1024 / 1000) * 1024 + 380 * 1024 / 1000] < -2.0);
        // hair over the face (here its left half) is not face skin
        let mut probs = map.probs.clone();
        for y in 0..64 {
            for x in 0..32 {
                let i = (y * 64 + x) * 6;
                probs[i + 3] = 0.0;
                probs[i + 1] = 1.0;
            }
        }
        let hair = PeopleMap { side: 64, probs };
        let m3 = people_mask(&PeopleRequest::parse(0, &["Face Skin".into()]).unwrap(), &rgb, 1000, 1000, Some(&hair), &faces).unwrap();
        assert!(m3.logits[(560 * 1024 / 1000) * 1024 + 300 * 1024 / 1000] < -2.0, "hair over the cheek is not skin");
        // a face without landmarks (profile): nothing cut, and the result says so
        let profile = [Face { detection: faces[0].detection.clone(), landmarks: None }];
        let m4 = people_mask(&PeopleRequest::parse(0, &["Face Skin".into()]).unwrap(), &rgb, 1000, 1000, Some(&map), &profile).unwrap();
        assert_eq!(m4.faces_without_features, 1);
        // asking for skin and lips: lips come back on top of the skin's hole
        let m5 = people_mask(&PeopleRequest::parse(0, &["Face Skin".into(), "Lips".into()]).unwrap(), &rgb, 1000, 1000, Some(&map), &faces).unwrap();
        assert!(m5.logits[(650 * 1024 / 1000) * 1024 + 500 * 1024 / 1000] > 2.0);
    }

    /// Regression (the user's bug, a man and a woman at a dinner table): on real faces the eyes,
    /// brows and lips were tinted — the edge refinement ran after the features were cut and
    /// filled the small holes back in, and a smiling eye's opening is too thin a sliver to cut
    /// anyway — and the man's upper forehead was left out where the class map fades before the
    /// hairline. A small face (eyes 20 pixels wide, squinting): the eyes, brows and lips are 0,
    /// the forehead up to the hair is skin, the hair isn't.
    #[test]
    fn small_smiling_faces_lose_eyes_and_lips_and_keep_the_forehead() {
        let mut lm = synthetic_face();
        // the oval starting at the top (landmark 10), the chin (152) at the bottom
        for (k, i) in mediapipe::FACE_OVAL.iter().enumerate() {
            let a = k as f32 / mediapipe::FACE_OVAL.len() as f32 * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2;
            lm[*i] = [500.0 + 300.0 * a.cos(), 500.0 + 400.0 * a.sin()];
        }
        // squinting: the lids nearly meet
        for (e, cx) in [(0, 380.0f32), (1, 620.0)] {
            for (k, i) in mediapipe::EYES[e].iter().enumerate() {
                let a = k as f32 / 16.0 * std::f32::consts::TAU;
                lm[*i] = [cx + 50.0 * a.cos(), 400.0 + 4.0 * a.sin()];
            }
        }
        // the face 0.2× in a 400-pixel photo, centred at (200, 200): 120 wide, the oval's top at 120
        let lm: Vec<[f32; 2]> = lm.iter().map(|p| [200.0 + (p[0] - 500.0) * 0.2, 200.0 + (p[1] - 500.0) * 0.2]).collect();
        let (w, h) = (400usize, 400usize);
        let in_head = |x: f32, y: f32| ((x - 200.0) / 66.0).powi(2) + ((y - 205.0) / 92.0).powi(2) <= 1.0;
        let mut rgb = vec![0u8; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let c = if y < 118 && in_head(x as f32, y as f32) {
                    [45, 32, 25] // hair
                } else if in_head(x as f32, y as f32) {
                    // skin, with a little texture so the refinement has something to follow
                    let n = ((x * 7 + y * 13) % 5) as u8;
                    [205 - n, 150, 125]
                } else {
                    [70, 90, 120]
                };
                rgb[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&c);
            }
        }
        // the class map: hair on top, face skin only from the brows down (it fades on the forehead)
        let side = 64;
        let mut probs = vec![0f32; side * side * 6];
        for j in 0..side {
            for i in 0..side {
                let (x, y) = ((i as f32 + 0.5) * w as f32 / side as f32, (j as f32 + 0.5) * h as f32 / side as f32);
                let c = if !in_head(x, y) {
                    0
                } else if y < 118.0 {
                    1
                } else if y < 160.0 {
                    0
                } else {
                    3
                };
                probs[(j * side + i) * 6 + c] = 1.0;
            }
        }
        let map = PeopleMap { side, probs };
        let det = Detection { x0: 140.0, y0: 120.0, x1: 260.0, y1: 280.0, score: 0.9, keypoints: [[0.0; 2]; 6] };
        let faces = [Face { detection: det, landmarks: Some(lm) }];
        let m = people_mask(&PeopleRequest::parse(0, &["Face Skin".into()]).unwrap(), &rgb, w, h, Some(&map), &faces).unwrap();
        let at = |x: f32, y: f32| m.logits[((y / h as f32 * m.side as f32) as usize) * m.side + (x / w as f32 * m.side as f32) as usize];
        for (what, x, y) in
            [("eye", 176.0, 180.0), ("eye", 224.0, 180.0), ("lid above the eye", 176.0, 177.0), ("brow", 176.0, 168.5), ("lips", 200.0, 230.0)]
        {
            assert!(at(x, y) < -1.0, "{what} is not skin: {}", at(x, y));
        }
        for (what, x, y) in [("cheek", 160.0, 212.0), ("forehead", 200.0, 145.0), ("upper forehead", 200.0, 128.0)] {
            assert!(at(x, y) > 1.0, "{what} is skin: {}", at(x, y));
        }
        assert!(at(200.0, 105.0) < -1.0, "the hair is not skin: {}", at(200.0, 105.0));
        assert!(at(30.0, 30.0) < -1.0, "the background is not skin");
    }

    /// Regression (the user's dress mask): a remembered dress came back as SAM 2.1's 256-cell mask
    /// stretched over the photo — a soft halo around the dress — with the plunging neckline's skin
    /// and the hands in it. Refined: the skin is out, the edge sits on the dress's own edge, and
    /// nothing a few pixels outside it is selected; a dress on its own (not worn) keeps its insides.
    #[test]
    fn a_found_dress_loses_the_wearers_skin_and_its_halo() {
        let (w, h) = (1024usize, 1024usize);
        // the dress: a white trapezoid from y 200 down; inside it, a skin-coloured neckline wedge
        let dress = |x: f32, y: f32| y >= 200.0 && (x - 256.0).abs() <= 80.0 + (y - 200.0) * 0.3;
        let neckline = |x: f32, y: f32| (200.0..280.0).contains(&y) && (x - 256.0).abs() <= (280.0 - y) * 0.4;
        let mut rgb = vec![0u8; w * h * 3];
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                let c = if neckline(fx, fy) {
                    [222, 170, 140]
                } else if dress(fx, fy) {
                    [240, 236, 228]
                } else {
                    [60, 62, 70]
                };
                rgb[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&c);
            }
        }
        // SAM's coarse logits (256 cells, 4 pixels each): the dress and the neckline, with a soft
        // edge 8 pixels wide around it (the halo: still half selected 4 pixels out)
        let ls = 256;
        let edge = |x: f32, y: f32| -> f32 {
            // the distance outside the dress, along x (its sides are near vertical)
            if y < 200.0 {
                return 200.0 - y;
            }
            ((x - 256.0).abs() - (80.0 + (y - 200.0) * 0.3)).max(0.0)
        };
        let logits: Vec<f32> = (0..ls * ls)
            .map(|k| {
                let (x, y) = (((k % ls) as f32 + 0.5) * 4.0, ((k / ls) as f32 + 0.5) * 4.0);
                if dress(x, y) { 6.0 } else { 6.0 - 1.5 * edge(x, y) }
            })
            .collect();
        // the class map: the neckline is body skin, the dress clothes, the rest background
        let side = 64;
        let mut probs = vec![0f32; side * side * 6];
        for j in 0..side {
            for i in 0..side {
                let (x, y) = ((i as f32 + 0.5) * 16.0, (j as f32 + 0.5) * 16.0);
                let c = if neckline(x, y) {
                    2
                } else if dress(x, y) {
                    4
                } else {
                    0
                };
                probs[(j * side + i) * 6 + c] = 1.0;
            }
        }
        let map = PeopleMap { side, probs };
        let m = garment_mask(&logits, ls, &rgb, w, h, Some(&map)).unwrap();
        let at = |x: f32, y: f32| m.logits[((y / h as f32 * m.side as f32) as usize) * m.side + (x / w as f32 * m.side as f32) as usize];
        assert!(at(256.0, 225.0) < -1.0, "the neckline's skin is out: {}", at(256.0, 225.0));
        assert!(at(256.0, 400.0) > 1.0 && at(200.0, 300.0) > 1.0, "the dress is in");
        // its edge at y = 350 is x = 256 − 125 = 131: 3 pixels outside, nothing (the coarse mask
        // had a logit of 1.5 there, a visible tint), 3 inside, all
        assert!(at(128.0, 350.0) < -2.0, "no halo: {}", at(128.0, 350.0));
        assert!(at(134.0, 350.0) > 2.0, "the edge itself stays: {}", at(134.0, 350.0));
        // not worn (no person in the class map): nothing taken out of it
        let empty = PeopleMap { side, probs: (0..side * side * 6).map(|i| if i % 6 == 0 { 1.0 } else { 0.0 }).collect() };
        let m2 = garment_mask(&logits, ls, &rgb, w, h, Some(&empty)).unwrap();
        assert!(m2.logits[((225 * m2.side) / h) * m2.side + 256 * m2.side / w] > 0.0, "a dress on a hanger keeps what SAM drew");
        // hostile sizes
        assert!(garment_mask(&logits[..10], ls, &rgb, w, h, None).is_none());
        assert!(garment_mask(&logits, ls, &rgb, 0, 0, None).is_none());
    }

    /// Facial hair: dark, textured pixels in the lower face are cut out of skin; dark but smooth
    /// skin (a shadow) and the cheeks stay.
    #[test]
    fn beards_are_not_skin_but_shadows_are() {
        let lm = synthetic_face();
        let mut rgb = vec![150u8; 1000 * 1000 * 3];
        for y in 0..1000 {
            for x in 0..1000 {
                let i = (y * 1000 + x) * 3;
                let v = if (700..880).contains(&y) && (380..620).contains(&x) {
                    // stubble: dark and noisy
                    if (x * 7 + y * 13) % 3 == 0 { 20 } else { 75 }
                } else if (600..700).contains(&y) && (250..330).contains(&x) {
                    // a smooth shadow on the lower cheek
                    70
                } else {
                    150
                };
                rgb[i..i + 3].copy_from_slice(&[v, v, v]);
            }
        }
        let grid = mediapipe::Grid { side: 200, rect: [0.0, 0.0, 1.0, 1.0], w: 1000, h: 1000 };
        let mut holes = vec![0f32; 200 * 200];
        mediapipe::feature_holes(&grid, &mut holes, &lm, &rgb);
        let at = |x: usize, y: usize| holes[(y / 5) * 200 + x / 5];
        assert!(at(500, 800) > 0.9, "stubble is cut");
        assert!(at(290, 650) < 0.1, "a smooth shadow stays skin");
        assert!(at(300, 560) < 0.1 && at(500, 220) < 0.1, "cheek and forehead stay");
    }

    #[test]
    fn missing_or_wrong_model_files_are_errors() {
        let dir = std::env::temp_dir().join(format!("lc-people-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(PeopleModel::load(&dir).is_err());
        assert!(FaceModel::load(&dir).is_err());
        std::fs::write(dir.join(PEOPLE_FILE), b"not a tflite file").unwrap();
        std::fs::write(dir.join(FACE_FILE), b"PK not a zip").unwrap();
        assert!(PeopleModel::load(&dir).is_err());
        assert!(FaceModel::load(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
