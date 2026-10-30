//! Bake per-film-simulation camera profiles into a look file (`assets/camera-looks/<model>.json`,
//! `lightcraft_pipeline::looks`), as the starting point of `tools/fit_look.py`. A profile is JSON with the `matrix` and
//! `hue_sat` of a `lightcraft-cli calibrate` profile, fitted to raws shot in one film simulation, and a `tone`
//! (`CameraTone`: tone curve knots and chroma curve) pooled over them; the X-T5 file started from such profiles.
//!
//! A look is the whole rendering from white-balanced camera RGB: each grid node (CIELAB through the forward matrix)
//! is rendered the way the finishing stage renders a starting look (matrix, hue/saturation table, tone map on
//! luminance, chroma curve, highlights desaturated to white) and stored as Adobe RGB. A look takes camera RGB with
//! only the dynamic range gain, so the tone curve is placed for raw exposure bias `BIAS` (the curves are pooled on
//! the bias-corrected scale): -1.72 for an X-T5 at DR100 from ISO 250.
//!
//! usage: cargo run --release -p lightcraft-engine --example bake_look -- OUT.json MODEL GROUP BIAS ID=NAME=PROFILE.json…
//! Prints, per look, how far the baked grid is from rendering the profile directly (CIE76 dE over camera colours).
use lightcraft_color::{ADOBE_RGB, D50, D65, Mat3, REC2020, bradford, luminance_2020};
use lightcraft_pipeline::hue_sat::HueSat;
use lightcraft_pipeline::looks::{Axis, Look};
use lightcraft_pipeline::tone::{CameraTone, ToneMap};
use lightcraft_raw::profile::HsvTable;

const ADOBE_GAMMA: f32 = 563.0 / 256.0;
/// The grid and node encoding of the X-T4 file, so both read alike.
const LIGHTNESS: (f64, f64, usize) = (0.0, 100.0, 17);
const AB: (f64, f64, usize) = (-168.0, 168.0, 29);
const NODE_OFFSET: f64 = -4.0;
const NODE_STEP: f64 = 1.0 / 8192.0;
const WHITE: [f64; 3] = [0.96422, 1.0, 0.82521];

#[derive(serde::Deserialize)]
struct Profile {
    matrix: [[f64; 3]; 3],
    hue_sat: Option<HsvTable>,
    tone: CameraTone,
}

/// One profile as the finishing stage renders it from white-balanced camera RGB.
struct Render<'a> {
    matrix: [[f32; 3]; 3],
    hue_sat: Option<HueSat<'a>>,
    tone: ToneMap,
}

impl Render<'_> {
    /// Display-linear Rec.2020 of camera RGB `cam` (as `finish.rs`' `base_render`, peak 1).
    fn apply(&self, cam: [f32; 3]) -> [f32; 3] {
        let m = &self.matrix;
        let mut c: [f32; 3] = std::array::from_fn(|i| m[i][0] * cam[0] + m[i][1] * cam[1] + m[i][2] * cam[2]);
        if let Some(h) = &self.hue_sat {
            c = h.apply(c);
        }
        let yl = luminance_2020(c);
        let o = self.tone.apply(yl);
        let mut d = if yl > 1e-9 { c.map(|v| v * o / yl) } else { [0.0; 3] };
        let k = self.tone.chroma_scale(o);
        if k != 1.0 {
            d = d.map(|v| o + (v - o) * k);
        }
        let mx = d[0].max(d[1]).max(d[2]);
        if mx > 1.0 {
            let t = ((mx - 1.0) / (mx - o).max(1e-6)).clamp(0.0, 1.0);
            d = d.map(|v| v + (o - v) * t);
        }
        d
    }
}

fn mul(m: &[[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    std::array::from_fn(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2])
}

fn lab_to_xyz(lab: [f64; 3]) -> [f64; 3] {
    let fy = (lab[0] + 16.0) / 116.0;
    let (fx, fz) = (fy + lab[1] / 500.0, fy - lab[2] / 200.0);
    let inv = |f: f64| if f > 6.0 / 29.0 { f * f * f } else { 3.0 * (6.0f64 / 29.0).powi(2) * (f - 4.0 / 29.0) };
    [inv(fx) * WHITE[0], inv(fy) * WHITE[1], inv(fz) * WHITE[2]]
}

/// CIELAB (D65 white) of display-linear Rec.2020 `c`, for comparing renders.
fn lab_of(c: [f32; 3]) -> [f64; 3] {
    let xyz = REC2020.to_xyz().apply(c.map(f64::from));
    let w = lightcraft_color::D65.to_xyz();
    let f = |t: f64| if t > (6.0f64 / 29.0).powi(3) { t.cbrt() } else { t / (3.0 * (6.0f64 / 29.0).powi(2)) + 4.0 / 29.0 };
    let [fx, fy, fz] = std::array::from_fn(|i| f(xyz[i] / w[i]));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [out, model, group, bias, entries @ ..] = args.as_slice() else {
        return Err("usage: bake_look OUT.json MODEL GROUP BIAS ID=NAME=PROFILE.json…".into());
    };
    let bias: f32 = bias.parse().map_err(|_| "BIAS is a number of EV")?;
    let to_adobe = REC2020.to_space(&ADOBE_RGB).to_f32();
    let mut profiles = Vec::new();
    for entry in entries {
        let mut parts = entry.splitn(3, '=');
        let (Some(id), Some(name), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            return Err(format!("`{entry}` is not ID=NAME=PROFILE.json"));
        };
        let json = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
        let p: Profile = serde_json::from_str(&json).map_err(|e| format!("{path}: {e}"))?;
        profiles.push((id, name, path, p));
    }
    // one forward matrix for the file (its format has one): the first profile's colour, so its greys sit on the grid's
    // neutral axis; every node is rendered through its own profile's matrix whatever it is
    let first = profiles.first().ok_or("no profiles given")?;
    let forward = bradford(D65, D50).mul(&REC2020.to_xyz()).mul(&Mat3(first.3.matrix));
    let back = forward.inverse().ok_or("the first profile's matrix is not invertible")?;
    let mut looks = Vec::new();
    for (id, name, path, p) in &profiles {
        // the curve takes the raw with its bias undone; a look's camera RGB keeps it
        let scale = bias.exp2();
        let curve = CameraTone::new(p.tone.knots().map(|[x, y]| [x * scale, y]))
            .and_then(|t| t.with_chroma(*p.tone.chroma()))
            .ok_or(format!("{path}: the curve doesn't scale by {scale}"))?;
        let render = Render {
            matrix: Mat3(p.matrix).to_f32(),
            hue_sat: p.hue_sat.as_ref().and_then(HueSat::new),
            tone: ToneMap::camera(&curve, 0.0, 0.0, 0.0),
        };
        let node = |l: usize, a: usize, b: usize| {
            let at = |(lo, hi, n): (f64, f64, usize), i: usize| lo + (hi - lo) * i as f64 / (n - 1) as f64;
            let cam = back.apply(lab_to_xyz([at(LIGHTNESS, l), at(AB, a), at(AB, b)])).map(|v| v as f32);
            let d = render.apply(cam.map(|v| v.max(0.0)));
            mul(&to_adobe, d).map(|v| v.clamp(0.0, 1.0).powf(1.0 / ADOBE_GAMMA))
        };
        let (nl, nab) = (LIGHTNESS.2, AB.2);
        let mut nodes = Vec::with_capacity(nl * nab * nab);
        let mut hex = String::with_capacity(nl * nab * nab * 12);
        for l in 0..nl {
            for a in 0..nab {
                for b in 0..nab {
                    let v = node(l, a, b);
                    let q = v.map(|c| ((f64::from(c) - NODE_OFFSET) / NODE_STEP).round().clamp(0.0, 65535.0) as u16);
                    for c in q {
                        hex.push_str(&format!("{c:04x}"));
                    }
                    nodes.push(q.map(|c| (f64::from(c) * NODE_STEP + NODE_OFFSET) as f32));
                }
            }
        }
        // how much the grid loses against rendering the profile itself, over camera colours of photos
        let axis = |(lo, hi, n): (f64, f64, usize)| Axis { min: lo as f32, max: hi as f32, n };
        let look = Look::new(&format!("look:{id}"), model, name, group, "", forward.0, WHITE, axis(LIGHTNESS), axis(AB), nodes)?;
        let mut des = Vec::new();
        let mut seed = 0x2545_f491u32;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as f32 / u32::MAX as f32
        };
        for _ in 0..20000 {
            let y = 0.002 * (9.0f32 * rnd()).exp2();
            let cam = [y * (0.3 + 1.4 * rnd()), y, y * (0.3 + 1.4 * rnd())];
            let (a, b) = (lab_of(look.apply(cam)), lab_of(render.apply(cam)));
            des.push(((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt());
        }
        des.sort_by(f64::total_cmp);
        let mean = des.iter().sum::<f64>() / des.len() as f64;
        eprintln!("{id}: baked vs direct dE mean {mean:.2}, median {:.2}, 95th {:.2}", des[des.len() / 2], des[des.len() * 95 / 100]);
        looks.push(serde_json::json!({
            "id": id,
            "name": name,
            "description": format!("Fitted to the {model}'s own {name} conversions. Not made or endorsed by Fujifilm."),
            "nodes": hex,
        }));
    }
    let file = serde_json::json!({
        "version": 1,
        "model": model,
        "group": group,
        "input": {
            "forward_matrix": forward.0,
            "white": WHITE,
            "lightness": LIGHTNESS,
            "ab": AB,
        },
        "output": "Adobe RGB (1998), encoded with gamma 563/256",
        "node_offset": NODE_OFFSET,
        "node_step": NODE_STEP,
        // the bias the curves were placed for: a raw with another bias is brightened by the difference
        "raw_exposure_bias": bias,
        "looks": looks,
    });
    std::fs::write(out, serde_json::to_string(&file).map_err(|e| e.to_string())?).map_err(|e| format!("{out}: {e}"))?;
    Ok(())
}
