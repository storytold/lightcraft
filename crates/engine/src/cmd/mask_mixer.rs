//! A mask's local Color Mixer (LR-MASK-MIXER; Lightroom's masks have none, Capture One has one per
//! layer): the 8 bands' Hue / Saturation / Luminance, applied within the mask after the photo's colour
//! tools with the global mixer's maths (`lightcraft_pipeline::finish::local_mix`). `mask.mixer` sets
//! sliders; `mask.mixerTargeted` is its targeted adjustment, as `develop.targeted` is the global mixer's.

use lightcraft_develop::{DevelopSettings, MIXER_BANDS, Mixer};
use serde_json::{Value, json};

use super::color::{after_mixer_neutral, encoded_to_oklch, probe};
use super::masks::mask_id;
use super::{CommandSpec, bad, cmd, f64_req, has_active, str_param};
use crate::{Result, Session};

/// The slider of a `mask.mixer` key (`<band>.<hue|sat|lum>`, as the global `mixer.<band>.<…>` controls).
fn mixer_field<'a>(m: &'a mut Mixer, key: &str) -> Option<&'a mut f64> {
    let (band, attr) = key.split_once('.')?;
    let b = m.band_mut(MIXER_BANDS.iter().position(|n| *n == band)?)?;
    Some(match attr {
        "hue" => &mut b.hue,
        "sat" => &mut b.sat,
        "lum" => &mut b.lum,
        _ => return None,
    })
}

/// A mask's Color Mixer as the commands report it: `{"<band>.<hue|sat|lum>": value}`.
pub fn mixer_json(m: &Mixer) -> Value {
    let mut o = serde_json::Map::new();
    for (band, b) in MIXER_BANDS.iter().zip(m.bands()) {
        for (attr, v) in [("hue", b.hue), ("sat", b.sat), ("lum", b.lum)] {
            o.insert(format!("{band}.{attr}"), json!(v));
        }
    }
    Value::Object(o)
}

/// `mask.mixer`'s `values`, checked before anything changes: each key a band's slider, each value a
/// finite number (clamped to −100..100).
fn mixer_values(p: &Value, c: &str) -> Result<Vec<(String, f64)>> {
    let Some(v) = p.get("values").filter(|v| !v.is_null()) else { return Ok(Vec::new()) };
    let o = v.as_object().ok_or_else(|| bad(c, "`values` must be {\"<band>.<hue|sat|lum>\": number, …}"))?;
    let mut probe = Mixer::default();
    o.iter()
        .map(|(k, v)| {
            if mixer_field(&mut probe, k).is_none() {
                return Err(bad(c, format!("unknown slider `{k}` (<band>.<hue|sat|lum>, band one of {})", MIXER_BANDS.join(", "))));
            }
            let v = v.as_f64().filter(|v| v.is_finite()).ok_or_else(|| bad(c, format!("`{k}` must be a number")))?;
            Ok((k.clone(), v.clamp(-100.0, 100.0)))
        })
        .collect()
}

/// Run `f` on mask `mid`'s Color Mixer of the active photo as one undo step (Auto Sync carries it as
/// any mask edit); returns what `f` returns.
fn edit_mixer(s: &mut Session, c: &str, mid: u32, f: impl FnOnce(&mut Mixer) -> Value) -> Result<Value> {
    let photo = s.active().ok_or_else(|| bad(c, "no active photo"))?;
    let mut d = (*s.develop_of(photo).unwrap_or_default()).clone();
    let m = d.masks.iter_mut().find(|m| m.id == mid).ok_or_else(|| bad(c, format!("no mask {mid}")))?;
    let out = f(&mut m.mixer);
    s.set_develop(photo, d, "Mask Color Mixer")?;
    Ok(out)
}

/// Everything from the masks' local Color Mixers on, neutralized: the colour they see (the photo's
/// colour tools applied, as in the per-pixel stage).
fn local_mixer_neutral(d: &mut DevelopSettings) {
    let (vib, sat, grading, points) = (d.color.vibrance, d.color.saturation, d.grading, d.point_colors.clone());
    after_mixer_neutral(d);
    // the local mixers come after the whole colour stage: those stay
    (d.color.vibrance, d.color.saturation, d.grading, d.point_colors) = (vib, sat, grading, points);
    for m in &mut d.masks {
        m.mixer = Mixer::default();
        m.curve = Default::default();
    }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "mask.mixer",
            "Set Mask Color Mixer",
            [],
            None,
            "{id?, values?: {\"<band>.<hue|sat|lum>\": −100..100, …} (band: red, orange, yellow, green, aqua, blue, purple, magenta), reset?: bool} — the mask's local Color Mixer (applied within the mask after the photo's colour tools, with the global mixer's maths); `reset` first sets every band back to 0 → {mixer}",
            has_active,
            |s, p| {
                let c = "mask.mixer";
                let vals = mixer_values(p, c)?;
                let reset = p.get("reset").and_then(Value::as_bool).unwrap_or(false);
                if vals.is_empty() && !reset {
                    return Err(bad(c, "give `values` or `reset`"));
                }
                let mid = mask_id(p, s.active_mask, c)?;
                edit_mixer(s, c, mid, |m| {
                    if reset {
                        *m = Mixer::default();
                    }
                    for (k, v) in &vals {
                        if let Some(f) = mixer_field(m, k) {
                            *f = *v;
                        }
                    }
                    json!({"mixer": mixer_json(m)})
                })
            }
        ),
        cmd!(
            "mask.mixerTargeted",
            "Mask Color Mixer Targeted Adjustment",
            [],
            None,
            "{id?, target: hue|sat|lum, x, y (normalized image coords), delta (slider units)} — moves the mask's Color Mixer bands of the colour under the point (as `develop.targeted` does the global mixer's) → the changed sliders",
            has_active,
            |s, p| {
                let c = "mask.mixerTargeted";
                let (x, y, delta) = (f64_req(p, "x", c)?, f64_req(p, "y", c)?, f64_req(p, "delta", c)?);
                if !(x.is_finite() && y.is_finite() && delta.is_finite()) {
                    return Err(bad(c, "`x`, `y` and `delta` must be numbers"));
                }
                let target = str_param(p, "target").unwrap_or("hue").to_string();
                if !matches!(target.as_str(), "hue" | "sat" | "lum") {
                    return Err(bad(c, "target must be hue, sat or lum"));
                }
                let photo = s.active().ok_or_else(|| bad(c, "no active photo"))?;
                let mid = mask_id(p, s.active_mask, c)?;
                if !s.develop_of(photo).is_some_and(|d| d.masks.iter().any(|m| m.id == mid)) {
                    return Err(bad(c, format!("no mask {mid}")));
                }
                let e = probe(s, c, x, y, local_mixer_neutral)?;
                let [_, _, h] = encoded_to_oklch(e);
                let w = lightcraft_pipeline::colorops::band_weights(h);
                let top = w.iter().copied().fold(0.0f32, f32::max).max(1e-6);
                let delta = delta.clamp(-200.0, 200.0);
                edit_mixer(s, c, mid, |m| {
                    let mut changed = serde_json::Map::new();
                    for (i, band) in MIXER_BANDS.iter().enumerate() {
                        let k = f64::from(w.get(i).copied().unwrap_or(0.0) / top);
                        if k < 0.05 {
                            continue;
                        }
                        let key = format!("{band}.{target}");
                        if let Some(f) = mixer_field(m, &key) {
                            *f = (*f + delta * k).clamp(-100.0, 100.0);
                            changed.insert(key, json!(*f));
                        }
                    }
                    Value::Object(changed)
                })
            }
        ),
    ]
}
