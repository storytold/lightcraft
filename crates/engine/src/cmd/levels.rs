//! Levels commands: set one channel's black, midtone, white and output levels, reset them, pick the black or white
//! point from the photo, and the histogram the tool draws behind its markers.
//!
//! The tool works on the encoded values after Exposure and before the Tone Curve. The picker and the histogram
//! therefore read the photo with the Levels (and everything after them) neutral, so what they show is what the Levels
//! receive.

use lcc_develop::{DevelopSettings, Levels};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, f64_req, has_active, str_param};
use crate::media::SourceLevel;
use crate::{Result, Session};

/// Long edge of the proxy the histogram is counted on.
const HISTOGRAM_EDGE: usize = 384;

/// Rec.601 luma weights, the rule the pickers use (measured against Capture One).
const LUMA: [f32; 3] = [0.299, 0.587, 0.114];

/// The channel of a command's `channel` parameter (`rgb` is the master).
fn channel_param(p: &Value, c: &str) -> Result<&'static str> {
    match str_param(p, "channel").unwrap_or("master") {
        "master" | "rgb" => Ok("master"),
        "red" => Ok("red"),
        "green" => Ok("green"),
        "blue" => Ok("blue"),
        other => Err(bad(c, format!("unknown channel `{other}` (one of rgb, red, green, blue)"))),
    }
}

/// The photo with the Levels and everything after them neutral: what the Levels receive.
fn before_levels(d: &mut DevelopSettings) {
    d.levels = Levels::default();
    super::color::after_mixer_neutral(d);
}

fn set_levels(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "develop.levels";
    let id = s.active().ok_or_else(|| bad(C, "no active photo"))?;
    let ch = channel_param(p, C)?;
    let num = |key: &str| -> Result<Option<f64>> {
        match p.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v.as_f64().filter(|n| n.is_finite()).map(Some).ok_or_else(|| bad(C, format!("`{key}` must be a number"))),
        }
    };
    let (black, white, midtone, marker) = (num("black")?, num("white")?, num("midtone")?, num("marker")?);
    let (out_black, out_white) = (num("outBlack")?, num("outWhite")?);
    if [black, white, midtone, marker, out_black, out_white].iter().all(Option::is_none) {
        return Err(bad(C, "give at least one of black, midtone, marker, white, outBlack, outWhite"));
    }
    let mut d = (*s.develop_of(id).unwrap_or_default()).clone();
    let Some(c) = d.levels.channel_mut(ch) else { return Err(bad(C, "unknown channel")) };
    if let Some(v) = black {
        c.black = v;
        c.after_black_set();
    }
    if let Some(v) = white {
        c.white = v;
        c.after_white_set();
    }
    if let Some(v) = midtone {
        c.midtone = v.clamp(-1.0, 1.0);
    }
    if let Some(v) = marker {
        c.set_midtone_position(v);
    }
    if let Some(v) = out_black {
        c.out_black = v.clamp(0.0, 255.0);
    }
    if let Some(v) = out_white {
        c.out_white = v.clamp(0.0, 255.0);
    }
    let now = *c;
    s.set_develop(id, d, "Levels")?;
    Ok(
        json!({"channel": ch, "black": now.black, "midtone": now.midtone, "white": now.white, "outBlack": now.out_black, "outWhite": now.out_white, "marker": now.midtone_position()}),
    )
}

fn reset(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "levels.reset";
    let id = s.active().ok_or_else(|| bad(C, "no active photo"))?;
    let channel = str_param(p, "channel").unwrap_or("all");
    let mut d = (*s.develop_of(id).unwrap_or_default()).clone();
    match channel {
        "all" => d.levels = Levels::default(),
        _ => {
            let ch = channel_param(p, C)?;
            if let Some(c) = d.levels.channel_mut(ch) {
                *c = Default::default();
            }
        }
    }
    let label = if channel == "all" { "Reset Levels".to_string() } else { format!("Reset Levels ({channel})") };
    s.set_develop(id, d, &label)?;
    Ok(json!({"channel": channel}))
}

/// The proxy of the active photo with the Levels neutral, as 8-bit encoded pixels.
fn render_before(s: &mut Session, c: &str) -> Result<lcc_raster::Rgba8> {
    let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
    let src = s.source_now(id, SourceLevel::Thumb).map_err(|e| bad(c, e))?;
    let info = s.source_info(id);
    let mut d = (*s.develop_of(id).unwrap_or_default()).clone();
    before_levels(&mut d);
    let req = lcc_pipeline::RenderRequest::fit(HISTOGRAM_EDGE, HISTOGRAM_EDGE);
    Ok(lcc_pipeline::render(&src, &info, &d, &req).image)
}

fn histogram(s: &mut Session, _p: &Value) -> Result<Value> {
    let img = render_before(s, "levels.histogram")?;
    let h = lcc_raster::Histogram::of_srgb8(&img);
    Ok(json!({"r": h.r, "g": h.g, "b": h.b, "luma": h.luma, "total": h.total}))
}

fn pick(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "develop.levelsPick";
    let target = str_param(p, "target").ok_or_else(|| bad(C, "missing target (black or white)"))?;
    if !matches!(target, "black" | "white") {
        return Err(bad(C, format!("unknown target `{target}` (black or white)")));
    }
    let (x, y) = (f64_req(p, "x", C)?, f64_req(p, "y", C)?);
    let rgb = super::color::probe(s, C, x, y, before_levels)?;
    let luma = rgb.iter().zip(LUMA).map(|(v, w)| v * w).sum::<f32>();
    let value = (f64::from(luma) * 255.0).round().clamp(0.0, 255.0);
    let id = s.active().ok_or_else(|| bad(C, "no active photo"))?;
    let mut d = (*s.develop_of(id).unwrap_or_default()).clone();
    let c = &mut d.levels.master;
    if target == "black" {
        c.black = value;
        c.after_black_set();
    } else {
        c.white = value;
        c.after_white_set();
    }
    s.set_develop(id, d, "Levels")?;
    Ok(json!({"target": target, "value": value}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "develop.levels",
            "Set Levels",
            [],
            None,
            "{channel?: rgb|red|green|blue (default rgb), black?, white?, outBlack?, outWhite?: 0..255, midtone?: -1..1 (0 neutral, positive brightens), marker?: the midtone marker's place on the 0..255 scale (sets the midtone)}: sets those values of one channel in one history step; black and white keep a distance of 2 (black above white inverts) → {channel, black, midtone, white, outBlack, outWhite, marker}",
            has_active,
            set_levels
        ),
        cmd!(
            "levels.reset",
            "Reset Levels",
            [],
            None,
            "{channel?: all|rgb|red|green|blue (default all)}: the Levels (or one channel's) back to neutral",
            has_active,
            reset
        ),
        cmd!(
            "develop.levelsPick",
            "Pick Levels Point",
            [],
            None,
            "{target: black|white, x, y (normalized image coords)}: samples the photo there as the Levels receive it (Rec.601 luma of 3 x 3 proxy pixels) and sets the RGB channel's black or white point to it → {target, value}",
            has_active,
            pick
        ),
        cmd!(
            query "levels.histogram",
            "Levels Histogram",
            [],
            None,
            "{} → {r, g, b, luma: 256 counts each, total}: the active photo as the Levels receive it (Levels and later stages neutral)",
            always,
            histogram
        ),
    ]
}
