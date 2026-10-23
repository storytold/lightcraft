//! Measured camera looks: a camera's own rendering of one of its picture styles (a Fujifilm film simulation),
//! measured by having the camera convert charts of known colour, as a 3D lookup from white-balanced camera RGB to
//! the colour the camera writes. A look replaces the photo's starting tone and colour: it is the whole rendering
//! from scene to display, so a profile matrix and tone curve could not hold it (lightness depends on colour, and
//! some looks tint their greys). Method and measurements: `docs/camera-preview-colour.md`, "Measured camera looks".
//!
//! Looks are registered here by profile id (`look:…`) and found by the finishing stage. A raw only renders one when
//! its decoder recorded how its pixels relate to the camera's colour ([`CameraRgb`] in [`crate::SourceInfo`]): the
//! finishing stage undoes the starting colour, so exposure, white balance and the tonal edits made before the tone
//! map reach the look as they would reach the camera. A photo whose profile is a look is decoded with the starting
//! colour's matrix alone, which undoes exactly; the hue/saturation table a camera profile adds can map two camera
//! colours to one (the bundled X-T4 profile does, in yellow greens), so it is left to this stage ([`deferred`]).
//! A look rendered from a photo decoded with its table (the profile browser's previews, a photo with an AI-denoised
//! picture, and every photo in a build without a separate decode for looks, such as the web build) undoes the table
//! approximately.
//! Looks render on the CPU.
//!
//! [`deferred`]: CameraRgb::deferred

use std::sync::{Arc, OnceLock, RwLock};

use lightcraft_color::{ADOBE_RGB, Mat3, REC2020, luminance_2020};
use lightcraft_develop::DevelopSettings;
use lightcraft_raw::profile::HsvTable;

use crate::hue_sat::HueSat;
use crate::tone::ToneMap;

/// The profile id prefix of measured looks.
pub const PREFIX: &str = "look:";

/// Whether `id` is a measured look's profile id (one that renders does so on the CPU, [`needs_cpu`]).
pub fn is_look_profile(id: &str) -> bool {
    id.starts_with(PREFIX)
}

/// The Adobe RGB (1998) encoding exponent (563/256) of the camera output a look predicts.
const ADOBE_GAMMA: f32 = 563.0 / 256.0;

/// One grid axis: `n` nodes evenly from `min` to `max`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Axis {
    pub min: f32,
    pub max: f32,
    pub n: usize,
}

impl Axis {
    /// Finite, increasing, and between 2 and 256 nodes.
    fn valid(&self) -> bool {
        self.min.is_finite() && self.max.is_finite() && self.max > self.min && (2..=256).contains(&self.n)
    }

    /// The lower node and the fraction towards the next one, with `x` clamped to the axis.
    #[inline]
    fn locate(&self, x: f32) -> (usize, f32) {
        let step = (self.max - self.min) / (self.n - 1) as f32;
        let f = if x.is_finite() { (x.clamp(self.min, self.max) - self.min) / step } else { 0.0 };
        let i = (f as usize).min(self.n - 2);
        (i, f - i as f32)
    }
}

/// A measured look: white-balanced camera RGB → CIELAB through the camera's forward matrix → a grid of the camera's
/// Adobe RGB output, interpolated trilinearly.
#[derive(Clone, Debug, PartialEq)]
pub struct Look {
    /// The profile id (`look:…`).
    pub id: String,
    /// The camera model as its files report it (Exif `Model`); the look renders only that camera's raws.
    pub model: String,
    pub name: String,
    pub group: String,
    pub description: String,
    /// White-balanced camera RGB → CIE XYZ, rows.
    forward: [[f32; 3]; 3],
    /// The XYZ white CIELAB is taken against.
    white: [f32; 3],
    lightness: Axis,
    ab: Axis,
    /// Adobe RGB (1998) encoded output per node, index `(l · n_ab + a) · n_ab + b`.
    nodes: Vec<[f32; 3]>,
    /// Linear Adobe RGB → linear Rec.2020 (both D65).
    to_working: [[f32; 3]; 3],
}

impl Look {
    /// A look from its parts, checked: finite and bounded numbers, an invertible forward matrix, a positive white,
    /// and one output per grid node.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: &str,
        model: &str,
        name: &str,
        group: &str,
        description: &str,
        forward: [[f64; 3]; 3],
        white: [f64; 3],
        lightness: Axis,
        ab: Axis,
        nodes: Vec<[f32; 3]>,
    ) -> Result<Look, String> {
        if !is_look_profile(id) || id.len() <= PREFIX.len() {
            return Err(format!("look id `{id}` must start with `{PREFIX}`"));
        }
        if model.trim().is_empty() || name.trim().is_empty() {
            return Err(format!("{id}: no camera model or name"));
        }
        let m = Mat3(forward);
        if !forward.iter().flatten().all(|v| v.is_finite() && v.abs() < 8.0) || m.inverse().is_none() {
            return Err(format!("{id}: the forward matrix is not usable"));
        }
        if !white.iter().all(|v| v.is_finite() && *v > 0.0) {
            return Err(format!("{id}: the white is not usable"));
        }
        if !lightness.valid() || !ab.valid() {
            return Err(format!("{id}: bad grid axes"));
        }
        let want = lightness.n.checked_mul(ab.n).and_then(|v| v.checked_mul(ab.n));
        if want != Some(nodes.len()) {
            return Err(format!("{id}: expected {} grid nodes, found {}", want.unwrap_or(0), nodes.len()));
        }
        if !nodes.iter().flatten().all(|v| v.is_finite()) {
            return Err(format!("{id}: a grid node is not a number"));
        }
        Ok(Look {
            id: id.to_owned(),
            model: model.trim().to_owned(),
            name: name.to_owned(),
            group: group.to_owned(),
            description: description.to_owned(),
            forward: m.to_f32(),
            white: white.map(|v| v as f32),
            lightness,
            ab,
            nodes,
            to_working: ADOBE_RGB.to_space(&REC2020).to_f32(),
        })
    }

    /// Whether the look is for raws of camera `model` (as files report it, without case).
    pub fn fits(&self, model: &str) -> bool {
        self.model.eq_ignore_ascii_case(model.trim())
    }

    /// The look's display colour (linear Rec.2020, white 1) for white-balanced camera RGB `cam`.
    pub fn apply(&self, cam: [f32; 3]) -> [f32; 3] {
        let cam = cam.map(|v| if v.is_finite() { v.max(0.0) } else { 0.0 });
        let m = &self.forward;
        let xyz: [f32; 3] = std::array::from_fn(|i| m[i][0] * cam[0] + m[i][1] * cam[1] + m[i][2] * cam[2]);
        let lab = lab(xyz, self.white);
        let enc = self.lookup(lab).map(|v| v.clamp(0.0, 1.0));
        let lin = enc.map(|v| v.powf(ADOBE_GAMMA));
        let t = &self.to_working;
        std::array::from_fn(|i| t[i][0] * lin[0] + t[i][1] * lin[1] + t[i][2] * lin[2])
    }

    /// The grid's Adobe RGB encoded output at CIELAB `lab`, trilinear, the input clamped to the grid.
    fn lookup(&self, lab: [f32; 3]) -> [f32; 3] {
        let (il, tl) = self.lightness.locate(lab[0]);
        let (ia, ta) = self.ab.locate(lab[1]);
        let (ib, tb) = self.ab.locate(lab[2]);
        let n = self.ab.n;
        let node = |l: usize, a: usize, b: usize| self.nodes.get((l * n + a) * n + b).copied().unwrap_or([0.0; 3]);
        let lerp = |p: [f32; 3], q: [f32; 3], t: f32| -> [f32; 3] { std::array::from_fn(|k| p[k] + (q[k] - p[k]) * t) };
        let plane = |l: usize| {
            let lo = lerp(node(l, ia, ib), node(l, ia, ib + 1), tb);
            let hi = lerp(node(l, ia + 1, ib), node(l, ia + 1, ib + 1), tb);
            lerp(lo, hi, ta)
        };
        lerp(plane(il), plane(il + 1), tl)
    }
}

/// CIELAB of `xyz` against `white`.
#[inline]
fn lab(xyz: [f32; 3], white: [f32; 3]) -> [f32; 3] {
    const D: f32 = 6.0 / 29.0;
    let f = |t: f32| if t > D * D * D { t.cbrt() } else { t / (3.0 * D * D) + 4.0 / 29.0 };
    let [fx, fy, fz] = std::array::from_fn(|i| f(xyz[i] / white[i]));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// The registered looks, in registration order.
fn registry() -> &'static RwLock<Vec<Arc<Look>>> {
    static R: OnceLock<RwLock<Vec<Arc<Look>>>> = OnceLock::new();
    R.get_or_init(Default::default)
}

/// Make `look` available to renders under its id (replacing a look of the same id).
pub fn register(look: Look) {
    let mut r = registry().write().unwrap_or_else(|e| e.into_inner());
    r.retain(|l| l.id != look.id);
    r.push(Arc::new(look));
}

/// Forget a look.
pub fn unregister(id: &str) {
    registry().write().unwrap_or_else(|e| e.into_inner()).retain(|l| l.id != id);
}

/// The look registered for profile `id`.
pub fn get(id: &str) -> Option<Arc<Look>> {
    if !is_look_profile(id) {
        return None;
    }
    registry().read().unwrap_or_else(|e| e.into_inner()).iter().find(|l| l.id == id).cloned()
}

/// The looks registered for camera `model`, in registration order.
pub fn for_model(model: &str) -> Vec<Arc<Look>> {
    registry().read().unwrap_or_else(|e| e.into_inner()).iter().filter(|l| l.fits(model)).cloned().collect()
}

/// How a raw's decoded pixels relate to its white-balanced camera RGB, recorded by the decoder for a camera that
/// has measured looks: decoded = `hue_sat`(`matrix` · camera RGB), or `matrix` · camera RGB when the table is
/// [`deferred`](Self::deferred). Camera RGB here is white-balanced with the as-shot multipliers (green 1) and scaled
/// so 1 is the sensor's full range, as the looks were measured.
#[derive(Clone, Debug, PartialEq)]
pub struct CameraRgb {
    /// The camera model as the file reports it.
    pub model: String,
    /// Camera RGB → the decoded linear Rec.2020, before `hue_sat`, rows.
    pub matrix: [[f32; 3]; 3],
    /// The hue/saturation table of the starting colour, applied after `matrix` ([`HueSat`]).
    pub hue_sat: Option<Arc<HsvTable>>,
    /// The decode left `hue_sat` out (the photo was decoded for a look): the finishing stage applies it to a render
    /// without one, and a look undoes `matrix` alone.
    pub deferred: bool,
    /// The gain the camera applies to this raw before its look: 2 for a DR200 raw, which it exposed a stop darker
    /// and brightens back, 1 for DR100.
    pub exposure: f32,
}

/// Fixed-point steps that undo a starting colour's hue/saturation table ([`Inverse::camera_rgb`]).
const INVERSE_STEPS: usize = 8;

/// [`CameraRgb`] ready to undo, per pixel.
pub struct Inverse<'a> {
    /// Decoded linear Rec.2020 (before the table) → camera RGB with the look's exposure.
    from: [[f32; 3]; 3],
    hue_sat: Option<HueSat<'a>>,
}

impl CameraRgb {
    /// The per-pixel inverse; `None` when the matrix can't be inverted.
    pub fn inverse(&self) -> Option<Inverse<'_>> {
        let m = Mat3(self.matrix.map(|r| r.map(f64::from)));
        let exposure = f64::from(if self.exposure.is_finite() && self.exposure > 0.0 { self.exposure } else { 1.0 });
        let inv = m.inverse()?;
        if !inv.0.iter().flatten().all(|v| v.is_finite()) {
            return None;
        }
        let hue_sat = match &self.hue_sat {
            Some(t) if !self.deferred => Some(HueSat::new(t)?),
            _ => None,
        };
        Some(Inverse { from: Mat3(inv.0.map(|r| r.map(|v| v * exposure))).to_f32(), hue_sat })
    }
}

impl Inverse<'_> {
    /// Camera RGB (times the look's exposure) of decoded pixel `c`. A table applied at decode only turns hues and
    /// scales saturation, so `y ← y + (c − table(y))` finds a colour it maps to `c` in a few steps; where it maps two
    /// colours to one, that may not be the camera's (an approximation, for previews only).
    #[inline]
    pub fn camera_rgb(&self, c: [f32; 3]) -> [f32; 3] {
        let mut y = c;
        if let Some(h) = &self.hue_sat {
            for _ in 0..INVERSE_STEPS {
                let r = h.apply(y);
                y = std::array::from_fn(|k| y[k] + (c[k] - r[k]));
            }
        }
        let m = &self.from;
        std::array::from_fn(|i| m[i][0] * y[0] + m[i][1] * y[1] + m[i][2] * y[2])
    }
}

/// The starting colour's table a render without a look applies itself, when the decode deferred it: every render of
/// such a source but one that renders its look ([`LookParams::new`]).
pub fn deferred_table(s: &DevelopSettings, info: &crate::SourceInfo) -> Option<Arc<HsvTable>> {
    let c = info.camera_rgb.as_ref().filter(|c| c.deferred)?;
    if LookParams::new(s, info).is_some() {
        return None;
    }
    c.hue_sat.clone()
}

/// Whether rendering `s` against `info` needs the CPU: a measured look, or a source decoded for one, whose deferred
/// table only the CPU's finishing stage applies. The GPU path is not tried for these.
pub fn needs_cpu(s: &DevelopSettings, info: &crate::SourceInfo) -> bool {
    info.camera_rgb.as_ref().is_some_and(|c| c.deferred) || LookParams::new(s, info).is_some()
}

/// A look as the finishing stage renders it for one photo.
pub struct LookParams {
    pub look: Arc<Look>,
    pub camera: Arc<CameraRgb>,
    /// The profile amount (0..2): 0 renders the photo's starting look, 1 the measured look.
    pub amount: f32,
    /// Contrast, Whites and Blacks on the look's display colour, as on a rendered photo; `None` at 0.
    pub tone: Option<ToneMap>,
}

impl LookParams {
    /// The look `s` asks for, when the source can render it: a registered look for the source's camera.
    pub fn new(s: &DevelopSettings, info: &crate::SourceInfo) -> Option<LookParams> {
        let look = get(&s.profile.id)?;
        let camera = info.camera_rgb.clone().filter(|c| info.raw && look.fits(&c.model))?;
        let l = &s.light;
        let tone = (l.contrast != 0.0 || l.whites != 0.0 || l.blacks != 0.0).then(|| ToneMap::display(l.contrast, l.whites, l.blacks));
        Some(LookParams { look, camera, amount: (s.profile.amount / 100.0).clamp(0.0, 2.0) as f32, tone })
    }
}

/// [`LookParams`] with its inverse, for one render.
pub struct LookRun<'a> {
    params: &'a LookParams,
    inverse: Inverse<'a>,
    /// The deferred starting-colour table, which the photo's starting look (an amount below 100 %) needs.
    table: Option<HueSat<'a>>,
    /// The render's exposure gain: the deferred table applies to the decoded colour, before exposure.
    gain: f32,
}

impl<'a> LookRun<'a> {
    /// `params` with the camera colour's inverse built, for a render at exposure gain `gain`; `None` when it can't be
    /// inverted (the render keeps its starting look).
    pub fn new(params: &'a LookParams, gain: f32) -> Option<LookRun<'a>> {
        let c = &params.camera;
        let table = match &c.hue_sat {
            Some(t) if c.deferred => Some(HueSat::new(t)?),
            _ => None,
        };
        let gain = if gain.is_finite() && gain > 0.0 { gain } else { 1.0 };
        Some(LookRun { params, inverse: c.inverse()?, table, gain })
    }

    /// The display colour (linear Rec.2020) of scene colour `c`; `base` renders the photo's starting look, for an
    /// amount other than 100 %.
    #[inline]
    pub fn apply(&self, c: [f32; 3], base: impl Fn([f32; 3]) -> [f32; 3]) -> [f32; 3] {
        let p = self.params;
        let mut m = p.look.apply(self.inverse.camera_rgb(c));
        if let Some(t) = &p.tone {
            let y = luminance_2020(m);
            let o = t.apply(y);
            m = if y > 1e-9 { m.map(|v| v * o / y) } else { [0.0; 3] };
        }
        if p.amount == 1.0 {
            return m;
        }
        // the starting look's colour: a deferred table applied to the colour before exposure, as the decode would have
        let g = self.gain;
        let b = base(match &self.table {
            Some(t) => t.apply(c.map(|v| v / g)).map(|v| v.max(0.0) * g),
            None => c,
        });
        std::array::from_fn(|k| b[k] + (m[k] - b[k]) * p.amount)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// X-T4's D65 forward matrix (white-balanced camera RGB → XYZ D50), as the bundled looks use it.
    const FORWARD: [[f64; 3]; 3] = [[0.675162, 0.317733, -0.028599], [0.248581, 1.05006, -0.298641], [0.04702, -0.219918, 0.998002]];
    const WHITE: [f64; 3] = [0.96422, 1.0, 0.82521];
    const L: Axis = Axis { min: 0.0, max: 100.0, n: 17 };
    const AB: Axis = Axis { min: -168.0, max: 168.0, n: 29 };

    /// A look whose every node holds `f` of the node's CIELAB colour.
    fn look_of(id: &str, f: impl Fn([f32; 3]) -> [f32; 3]) -> Look {
        let mut nodes = Vec::new();
        for l in 0..L.n {
            for a in 0..AB.n {
                for b in 0..AB.n {
                    let at = |axis: Axis, i: usize| axis.min + (axis.max - axis.min) * i as f32 / (axis.n - 1) as f32;
                    nodes.push(f([at(L, l), at(AB, a), at(AB, b)]));
                }
            }
        }
        Look::new(id, "X-T4", "Test", "X-T4 looks", "", FORWARD, WHITE, L, AB, nodes).unwrap()
    }

    /// CIELAB → Adobe RGB encoded (the camera output a colorimetric look would write).
    fn adobe_of_lab(lab: [f32; 3]) -> [f32; 3] {
        let d = 6.0f32 / 29.0;
        let finv = |t: f32| if t > d { t * t * t } else { 3.0 * d * d * (t - 4.0 / 29.0) };
        let fy = (lab[0] + 16.0) / 116.0;
        let xyz = [finv(fy + lab[1] / 500.0) * WHITE[0] as f32, finv(fy), finv(fy - lab[2] / 200.0) * WHITE[2] as f32];
        // XYZ D50 → linear Adobe RGB D65 (Bradford), encoded
        let to = ADOBE_RGB.from_xyz().mul(&lightcraft_color::bradford(lightcraft_color::D50, lightcraft_color::D65)).to_f32();
        std::array::from_fn(|i| (to[i][0] * xyz[0] + to[i][1] * xyz[1] + to[i][2] * xyz[2]).max(0.0).powf(1.0 / ADOBE_GAMMA))
    }

    /// A look whose nodes hold each node's own colour renders camera colour through its forward matrix.
    #[test]
    fn a_colorimetric_look_renders_camera_colour_through_its_forward_matrix() {
        let look = look_of("look:test/colorimetric", adobe_of_lab);
        // white-balanced camera white is the look's white: D65 white in Rec.2020 is (1, 1, 1)
        let w = look.apply([1.0, 1.0, 1.0]);
        assert!(w.iter().all(|v| (v - 1.0).abs() < 0.01), "{w:?}");
        // a mid colour: the grid's interpolation error stays small
        let to_2020 = REC2020.from_xyz().mul(&lightcraft_color::bradford(lightcraft_color::D50, lightcraft_color::D65)).to_f32();
        for cam in [[0.18f32, 0.18, 0.18], [0.30, 0.18, 0.08], [0.06, 0.12, 0.20]] {
            let xyz: [f32; 3] = std::array::from_fn(|i| (0..3).map(|j| FORWARD[i][j] as f32 * cam[j]).sum());
            let want: [f32; 3] = std::array::from_fn(|i| (0..3).map(|j| to_2020[i][j] * xyz[j]).sum());
            let got = look.apply(cam);
            for k in 0..3 {
                assert!((got[k] - want[k]).abs() < 0.01, "{cam:?}: {got:?} vs {want:?}");
            }
        }
        // negative and non-finite camera values read as black, the output stays finite and in range
        for cam in [[-1.0, 0.0, 0.0], [f32::NAN, 0.2, 0.1], [f32::INFINITY, 1.0, 1.0], [40.0, 40.0, 40.0]] {
            assert!(look.apply(cam).iter().all(|v| v.is_finite() && (-0.01..=1.01).contains(v)), "{cam:?}");
        }
    }

    /// Wrong node counts, non-finite numbers, a bad prefix, model, matrix, white or axis are refused.
    #[test]
    fn damaged_looks_are_refused() {
        let ok = |nodes: Vec<[f32; 3]>| Look::new("look:t/x", "X-T4", "T", "G", "", FORWARD, WHITE, L, AB, nodes);
        let n = L.n * AB.n * AB.n;
        assert!(ok(vec![[0.5; 3]; n]).is_ok());
        assert!(ok(vec![[0.5; 3]; n - 1]).is_err(), "node count");
        let mut nan = vec![[0.5; 3]; n];
        nan[7][1] = f32::NAN;
        assert!(ok(nan).is_err(), "non-finite node");
        let nodes = vec![[0.5; 3]; n];
        assert!(Look::new("lut:t/x", "X-T4", "T", "G", "", FORWARD, WHITE, L, AB, nodes.clone()).is_err(), "prefix");
        assert!(Look::new("look:t/x", "", "T", "G", "", FORWARD, WHITE, L, AB, nodes.clone()).is_err(), "model");
        assert!(Look::new("look:t/x", "X-T4", "T", "G", "", [[0.0; 3]; 3], WHITE, L, AB, nodes.clone()).is_err(), "singular");
        assert!(Look::new("look:t/x", "X-T4", "T", "G", "", FORWARD, [1.0, 0.0, 1.0], L, AB, nodes.clone()).is_err(), "white");
        let bad = Axis { min: 1.0, max: 1.0, n: 17 };
        assert!(Look::new("look:t/x", "X-T4", "T", "G", "", FORWARD, WHITE, bad, AB, nodes).is_err(), "axis");
    }

    /// Looks are found by profile id and by camera model, and unregister.
    #[test]
    fn looks_register_by_id_and_camera() {
        register(look_of("look:test/registry", |_| [0.5; 3]));
        assert!(get("look:test/registry").is_some());
        assert!(get("lut:test/registry").is_none());
        assert!(for_model("x-t4 ").iter().any(|l| l.id == "look:test/registry"), "the model matches without case or spaces");
        assert!(for_model("X-T3").iter().all(|l| l.id != "look:test/registry"));
        unregister("look:test/registry");
        assert!(get("look:test/registry").is_none());
    }

    /// A photo decoded for a look (its table deferred) gives back its camera colour exactly; one decoded with the
    /// table gives back a colour the table maps to the same pixel, for previews.
    #[test]
    fn the_inverse_recovers_camera_rgb() {
        let mut seed = 0x51f1_5e7a_9c3d_2b11u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f32 / (1u64 << 53) as f32
        };
        // a smooth table with hue turns of up to ±8° and saturation ×0.9–1.1, the size the X-T4 profile fits
        let mut data = Vec::new();
        for v in 0..5 {
            for h in 0..72 {
                for s in 0..5 {
                    let (hue, k) = (((h * 5) as f32).to_radians(), s as f32 / 4.0);
                    data.push([8.0 * k * (hue + v as f32 * 0.3).sin(), 1.0 + 0.1 * k * (2.0 * hue).cos(), 1.0]);
                }
            }
        }
        let table = HsvTable { hue_divisions: 72, sat_divisions: 5, val_divisions: 5, data, srgb_value: true };
        let matrix = [[1.19, -0.20, 0.0], [-0.08, 1.15, -0.07], [0.01, -0.85, 1.83]];
        let mul = |m: &[[f32; 3]; 3], v: [f32; 3]| -> [f32; 3] { std::array::from_fn(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2]) };
        let hue_sat = HueSat::new(&table).unwrap();
        let deferred = CameraRgb { model: "X-T4".into(), matrix, hue_sat: Some(Arc::new(table.clone())), deferred: true, exposure: 2.0 };
        let applied = CameraRgb { deferred: false, ..deferred.clone() };
        let (exact, approximate) = (deferred.inverse().unwrap(), applied.inverse().unwrap());
        let (mut worst_exact, mut residuals) = (0.0f32, Vec::new());
        for _ in 0..20_000 {
            let cam = [next() * 0.9 + 0.01, next() * 0.9 + 0.01, next() * 0.9 + 0.01];
            let lin = mul(&matrix, cam);
            let top = cam.iter().fold(0.0f32, |m, v| m.max(*v));
            let back = exact.camera_rgb(lin);
            for k in 0..3 {
                worst_exact = worst_exact.max((back[k] / 2.0 - cam[k]).abs() / top);
            }
            let decoded = hue_sat.apply(lin);
            let peak = decoded.iter().fold(0.0f32, |m, v| m.max(*v));
            if lin.iter().chain(&decoded).any(|v| *v < 0.02 * peak) {
                continue; // at the edge of the gamut, where the decode clips a channel at 0
            }
            let again = hue_sat.apply(mul(&matrix, approximate.camera_rgb(decoded).map(|v| v / 2.0)));
            residuals.push((0..3).map(|k| (again[k] - decoded[k]).abs()).fold(0.0f32, f32::max) / peak);
        }
        assert!(worst_exact < 1e-5, "deferred table: worst relative error {worst_exact}");
        residuals.sort_by(f32::total_cmp);
        let at = |q: f64| residuals[((residuals.len() - 1) as f64 * q) as usize];
        assert!(at(0.5) < 1e-4 && at(0.95) < 1e-2, "applied table: residual median {} p95 {}", at(0.5), at(0.95));
        // a singular matrix gives no inverse instead of a broken render
        assert!(CameraRgb { matrix: [[0.0; 3]; 3], ..deferred }.inverse().is_none());
    }

    /// A small decoded picture: greys and colours, linear Rec.2020.
    fn picture() -> lightcraft_raster::Rgb32f {
        lightcraft_raster::Rgb32f::from_fn(24, 16, |x, y| {
            let t = (x as f32 + 0.5) / 24.0;
            match y / 4 {
                0 => [t, t, t],
                1 => [0.6 * t, 0.25 * t, 0.08 * t],
                2 => [0.10 * t, 0.45 * t, 0.12 * t],
                _ => [0.08 * t, 0.15 * t, 0.55 * t],
            }
        })
    }

    /// Default settings with profile `id` at `amount`.
    fn settings(id: &str, amount: f64) -> DevelopSettings {
        let mut s = DevelopSettings::default();
        s.profile.id = id.into();
        s.profile.amount = amount;
        s
    }

    /// A raw source with `camera` as its camera colour.
    fn raw_info(camera: Option<CameraRgb>) -> crate::SourceInfo {
        crate::SourceInfo { raw: true, camera_rgb: camera.map(Arc::new), ..Default::default() }
    }

    /// Camera colour of `model` decoded through the identity matrix, no table.
    fn camera(model: &str) -> CameraRgb {
        CameraRgb { model: model.into(), matrix: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], hue_sat: None, deferred: false, exposure: 1.0 }
    }

    /// The 8-bit render of `src` at its own size.
    fn rgba(src: &lightcraft_raster::Rgb32f, info: &crate::SourceInfo, s: &DevelopSettings) -> Vec<u8> {
        crate::render(src, info, s, &crate::RenderRequest::fit(src.width, src.height)).image.data.iter().flatten().copied().collect()
    }

    /// A look renders its own colour, only on raws of its camera whose decoder recorded their camera colour; at amount
    /// 0 it is the photo's starting look.
    #[test]
    fn a_look_replaces_the_starting_look_for_its_camera_only() {
        register(look_of("look:test/grey", |_| [0.5; 3]));
        let src = picture();
        let base = rgba(&src, &raw_info(Some(camera("X-T4"))), &settings("lc.color", 100.0));
        let looked = rgba(&src, &raw_info(Some(camera("X-T4"))), &settings("look:test/grey", 100.0));
        // Adobe RGB 0.5 everywhere: the same mid grey in every pixel
        assert!(looked.chunks(4).all(|p| p[..3] == looked[..3]), "one colour");
        assert!((i32::from(looked[0]) - i32::from(looked[2])).abs() <= 1 && (100..=140).contains(&looked[0]), "{:?}", &looked[..4]);
        assert_ne!(looked, base);
        // not this camera, no camera colour, or a rendered (non-raw) source: the starting look
        assert_eq!(rgba(&src, &raw_info(Some(camera("X-T3"))), &settings("look:test/grey", 100.0)), base);
        assert_eq!(rgba(&src, &raw_info(None), &settings("look:test/grey", 100.0)), base);
        let jpeg = crate::SourceInfo { raw: false, ..raw_info(Some(camera("X-T4"))) };
        assert_eq!(rgba(&src, &jpeg, &settings("look:test/grey", 100.0)), rgba(&src, &jpeg, &settings("lc.color", 100.0)));
        // amount 0 is the starting look; 50 % lies between
        assert_eq!(rgba(&src, &raw_info(Some(camera("X-T4"))), &settings("look:test/grey", 0.0)), base);
        let half = rgba(&src, &raw_info(Some(camera("X-T4"))), &settings("look:test/grey", 50.0));
        assert!(half.iter().zip(&base).zip(&looked).all(|((h, b), l)| h.saturating_add(1) >= *b.min(l) && *h <= b.max(l).saturating_add(1)));
        unregister("look:test/grey");
        assert_eq!(rgba(&src, &raw_info(Some(camera("X-T4"))), &settings("look:test/grey", 100.0)), base, "an unknown look renders nothing");
    }

    /// A photo decoded for a look (its table deferred) renders without one as the photo decoded with its table does.
    #[test]
    fn a_deferred_table_is_applied_to_renders_without_a_look() {
        // a table that turns hues one way for dark colours and the other way for bright ones, so it is not
        // scale-invariant and exposure changes what it does
        let data = (0..2 * 4 * 3).map(|i| [if i < 12 { 12.0 } else { -12.0 }, if i < 12 { 1.2 } else { 0.85 }, 1.0]).collect();
        let table = Arc::new(HsvTable { hue_divisions: 4, sat_divisions: 3, val_divisions: 2, data, srgb_value: true });
        let hue_sat = HueSat::new(&table).unwrap();
        let plain = picture();
        let mut with_table = plain.clone();
        for p in &mut with_table.data {
            *p = hue_sat.apply(*p).map(|v| v.max(0.0));
        }
        let decoded = |deferred| raw_info(Some(CameraRgb { hue_sat: Some(table.clone()), deferred, ..camera("X-T4") }));
        let mut s = settings("lc.color", 100.0);
        let want = rgba(&with_table, &decoded(false), &s);
        let got = rgba(&plain, &decoded(true), &s);
        assert!(got.iter().zip(&want).all(|(a, b)| a.abs_diff(*b) <= 1), "the deferred table renders as the applied one");
        assert_ne!(rgba(&plain, &decoded(false), &s), want, "the table changes the picture");
        // a look at amount 0 is the starting look, at any exposure
        register(look_of("look:test/deferred", |_| [0.5; 3]));
        s.light.exposure = 1.0;
        let starting = rgba(&with_table, &decoded(false), &s);
        let at_zero = rgba(&plain, &decoded(true), &settings_with("look:test/deferred", 0.0, 1.0));
        assert!(at_zero.iter().zip(&starting).all(|(a, b)| a.abs_diff(*b) <= 1), "amount 0 at +1 EV is the starting look");
        unregister("look:test/deferred");
    }

    /// Default settings with profile `id` at `amount` and exposure `ev`.
    fn settings_with(id: &str, amount: f64, ev: f64) -> DevelopSettings {
        let mut s = settings(id, amount);
        s.light.exposure = ev;
        s
    }

    /// A look and a source decoded for one render on the CPU; other renders, and a look the source can't render, don't
    /// need it.
    #[test]
    fn looks_and_deferred_sources_need_the_cpu() {
        register(look_of("look:test/cpu", |_| [0.5; 3]));
        let table = Arc::new(HsvTable { hue_divisions: 1, sat_divisions: 2, val_divisions: 1, data: vec![[0.0, 1.0, 1.0]; 2], srgb_value: false });
        let plain = raw_info(Some(camera("X-T4")));
        let deferred = raw_info(Some(CameraRgb { hue_sat: Some(table), deferred: true, ..camera("X-T4") }));
        assert!(needs_cpu(&settings("look:test/cpu", 100.0), &plain));
        assert!(!needs_cpu(&settings("lc.color", 100.0), &plain));
        assert!(needs_cpu(&settings("lc.color", 100.0), &deferred), "the deferred table");
        assert!(!needs_cpu(&settings("look:test/cpu", 100.0), &raw_info(Some(camera("X-T3")))), "another camera's look");
        assert!(!needs_cpu(&settings("look:test/cpu", 100.0), &raw_info(None)));
        assert!(deferred_table(&settings("look:test/cpu", 100.0), &deferred).is_none(), "the look undoes the matrix alone");
        assert!(deferred_table(&settings("lc.color", 100.0), &deferred).is_some());
        unregister("look:test/cpu");
    }
}
