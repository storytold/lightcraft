//! Negative Craft commands. Analysis reads a thumbnail; the conversion itself is the film stage.

use lightcraft_catalog::{Op, PhotoId};
use lightcraft_develop::{DevelopSettings, Film, FilmCapture, FilmFrame, FilmIntent, FilmPrint, FilmScan, FilmStock};
use lightcraft_pipeline::film::{self, DRIFT_WARN};
use serde_json::{Value, json};

use super::{CommandSpec, bad, cmd, f64_req, has_active, has_selection, ok};
use crate::{Result, Session, media::SourceLevel};

/// Source changes (stock, intent, scan gamma) are remeasured when conversion is already on,
/// so the print follows the new choice. The color look is left as it was.
fn commit_measured(s: &mut Session, id: PhotoId, d: DevelopSettings, label: &str, cmd: &str) -> Result<Value> {
    if !d.film.enabled {
        commit(s, id, d, label)?;
        return ok();
    }
    // Measure first, then commit once. A failed read leaves the previous print, and one undo
    // restores both the source choice and the measurement it produced.
    let img = scanner_image(s, id, cmd)?;
    let analysis = analyze_view(&img, d.crop.geometry, &d.film);
    let out = stats_json(&analysis.stats, analysis.frame_error.as_deref());
    let mut d = d;
    d.film = analysis.film;
    commit(s, id, d, label)?;
    Ok(out)
}

/// The thumbnail in the space density is measured in: camera RGB when the file has its own
/// matrix, otherwise the working image.
pub(crate) fn scanner_image(s: &mut Session, id: PhotoId, cmd: &str) -> Result<lightcraft_raster::Rgb32f> {
    let src = s.source_now(id, SourceLevel::Thumb).map_err(|e| bad(cmd, e))?;
    let d = s.develop_of(id).unwrap_or_default();
    let mut img = oriented(&src, &d);
    if let Some(space) = film::scanner_space(&s.source_info(id)) {
        film::apply_matrix(&mut img, &space.to_camera);
    }
    Ok(img)
}

fn run_analyze(s: &mut Session, id: PhotoId, cmd: &str) -> Result<Value> {
    let img = scanner_image(s, id, cmd)?;
    let d = s.develop_of(id).unwrap_or_default();
    let analysis = analyze_view(&img, d.crop.geometry, &d.film);
    let out = stats_json(&analysis.stats, analysis.frame_error.as_deref());
    let mut d = (*d).clone();
    d.film = analysis.film;
    commit(s, id, d, "Negative Craft")?;
    Ok(out)
}

fn active(s: &Session, c: &str) -> Result<PhotoId> {
    s.active().ok_or_else(|| bad(c, "no active photo"))
}

/// Place the print again from the current frame and margin. The mask base and paper contrast stay.
/// A crop is the whole measurement: pixels outside it are not in the sample.
pub(crate) fn reprint(img: &lightcraft_raster::Rgb32f, d: &mut DevelopSettings) {
    let cropped = if d.crop.geometry.is_identity() { None } else { Some(film::within_crop(img, d.crop.geometry)) };
    let img = cropped.as_ref().unwrap_or(img);
    let mut film = d.film.clone();
    if cropped.is_some() {
        film.frame = FilmFrame::default();
    }
    if film::trimmed_sample_count(img, &film) < 8 {
        return;
    }
    let placed = film::place(img, &film);
    d.film.d_lo = placed.d_lo;
    d.film.d_hi = placed.d_hi;
    d.film.set_fog(placed.fog);
    d.film.set_axis(placed.axis);
    d.film.set_white(placed.white);
    d.film.mid_a = placed.mid_a;
    d.film.mid_b = placed.mid_b;
    d.film.grade = placed.grade;
    d.film.exposure = placed.exposure;
}

fn commit(s: &mut Session, id: PhotoId, d: DevelopSettings, label: &str) -> Result<()> {
    s.set_develop(id, d, label)
}

fn oriented(src: &std::sync::Arc<lightcraft_raster::Rgb32f>, d: &DevelopSettings) -> lightcraft_raster::Rgb32f {
    src.oriented(d.orientation)
}

/// Convert sees the current crop and nothing outside it. An uncropped scan is the whole thumbnail.
fn analyze_view(img: &lightcraft_raster::Rgb32f, crop: lightcraft_geom::CropGeometry, film: &Film) -> film::Analysis {
    if crop.is_identity() { film::analyze_image(img, film) } else { film::analyze_picture(&film::within_crop(img, crop), film) }
}

/// The raster and frame a reprint measures. The crop replaces the rebate quad for that measurement.
fn view_for_measure(img: &lightcraft_raster::Rgb32f, d: &DevelopSettings) -> (lightcraft_raster::Rgb32f, Film) {
    if d.crop.geometry.is_identity() {
        return (img.clone(), d.film.clone());
    }
    let mut film = d.film.clone();
    film.frame = FilmFrame::default();
    (film::within_crop(img, d.crop.geometry), film)
}

fn stats_json(stats: &film::FrameStats, frame_error: Option<&str>) -> Value {
    let mut v = json!({
        "base": stats.base,
        "gamma": stats.gamma,
        "exposure": stats.exposure,
        "rebateLevel": stats.rebate_level,
        "dominant": stats.dominant,
        "rebateBlack": stats.rebate_black,
        "bins": stats.bins,
    });
    if let Some(err) = frame_error
        && let Some(obj) = v.as_object_mut()
    {
        obj.insert("frameError".into(), json!(err));
    }
    v
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!("film.analyze", "Analyze Negative", ["Photo"], None, "{} — find the film border and print this photo", has_active, |s, _| {
            let id = active(s, "film.analyze")?;
            run_analyze(s, id, "film.analyze")
        }),
        cmd!(
            "film.analyzeRoll",
            "Analyze Roll",
            ["Photo"],
            None,
            "{} — share mask color and the gray axis; each frame is exposed again so its brightness holds",
            has_selection,
            |s, _| {
                let ids = s.selected();
                if ids.is_empty() {
                    return Err(bad("film.analyzeRoll", "no photos selected"));
                }
                let intent = s.active().and_then(|id| s.develop_of(id)).map(|d| d.film.intent).unwrap_or_default();
                let mut prepared = Vec::new();
                for id in &ids {
                    let img = scanner_image(s, *id, "film.analyzeRoll")?;
                    let d = s.develop_of(*id).unwrap_or_default();
                    let mut film = d.film.clone();
                    film.intent = intent;
                    let analysis = analyze_view(&img, d.crop.geometry, &film);
                    prepared.push((*id, analysis));
                }
                let stats: Vec<_> = prepared.iter().map(|(_, a)| a.stats.clone()).collect();
                let shared = film::share_roll(&stats);
                let mut warnings = Vec::new();
                let mut ops = Vec::new();
                for ((id, analysis), share) in prepared.iter().zip(&shared) {
                    let mut d = s.develop_of(*id).unwrap_or_default().as_ref().clone();
                    d.film = analysis.film.clone();
                    d.film.set_base(share.base);
                    d.film.set_gamma(share.gamma);
                    d.film.set_fog(share.fog);
                    d.film.set_axis(share.axis);
                    let img = scanner_image(s, *id, "film.analyzeRoll")?;
                    let img = film::within_crop(&img, d.crop.geometry);
                    let placed = film::place_keeping_axis(&img, &d.film);
                    d.film.d_lo = placed.d_lo;
                    d.film.d_hi = placed.d_hi;
                    d.film.set_white(placed.white);
                    d.film.mid_a = placed.mid_a;
                    d.film.mid_b = placed.mid_b;
                    d.film.grade = placed.grade;
                    // The shared axis changes the densities. Exposure is solved again with it,
                    // per frame, so the gray correction does not also move the midtone.
                    d.film.exposure = placed.exposure;
                    d.film.intent = intent;
                    d.film.enabled = true;
                    // One batch, not set_develop: Auto Sync must not copy the last frame onto the others.
                    ops.push(s.develop_op(*id, d, "Negative Craft Roll").ok_or_else(|| bad("film.analyzeRoll", "photo not found"))?);
                    if share.drift.abs() > DRIFT_WARN {
                        warnings.push(json!({"id": id.0, "stops": share.drift}));
                    }
                }
                s.commit("Negative Craft Roll", Op::Batch { ops })?;
                Ok(json!({"frames": ids.len(), "warnings": warnings}))
            }
        ),
        cmd!(
            query "film.inspect",
            "Inspect Negative",
            [],
            None,
            "{} — density histogram and rebate readout, without changing settings",
            has_active,
            |s, _| {
                let id = active(s, "film.inspect")?;
                let img = scanner_image(s, id, "film.inspect")?;
                let d = s.develop_of(id).unwrap_or_default();
                let (img, film) = view_for_measure(&img, &d);
                Ok(stats_json(&film::inspect(&img, &film), None))
            }
        ),
        cmd!("film.setEnabled", "Negative Craft On", [], None, "{enabled: bool}", has_active, |s, p| {
            let on = p.get("enabled").and_then(Value::as_bool).ok_or_else(|| bad("film.setEnabled", "missing enabled"))?;
            let id = active(s, "film.setEnabled")?;
            if !on {
                let mut d = s.develop_of(id).unwrap_or_default().as_ref().clone();
                d.film.enabled = false;
                commit(s, id, d, "Negative Craft")?;
                return ok();
            }
            // Turning conversion on measures the film border. A photo that cannot be read still
            // switches on, so the checkbox matches the click.
            match run_analyze(s, id, "film.setEnabled") {
                Ok(v) => Ok(v),
                Err(e) => {
                    let mut d = s.develop_of(id).unwrap_or_default().as_ref().clone();
                    d.film.enabled = true;
                    commit(s, id, d, "Negative Craft")?;
                    Err(e)
                }
            }
        }),
        cmd!("film.setStock", "Film Stock", [], None, "{stock: colorNeg|bwNeg|colorPos|bwPos}", has_active, |s, p| {
            let stock: FilmStock =
                serde_json::from_value(p.get("stock").cloned().unwrap_or(Value::Null)).map_err(|e| bad("film.setStock", e.to_string()))?;
            let id = active(s, "film.setStock")?;
            let mut d = s.develop_of(id).unwrap_or_default().as_ref().clone();
            d.film.stock = stock;
            commit_measured(s, id, d, "Film Stock", "film.setStock")
        }),
        cmd!("film.setIntent", "Analysis Intent", [], None, "{intent: stock|scene}", has_active, |s, p| {
            let intent: FilmIntent =
                serde_json::from_value(p.get("intent").cloned().unwrap_or(Value::Null)).map_err(|e| bad("film.setIntent", e.to_string()))?;
            let id = active(s, "film.setIntent")?;
            let mut d = s.develop_of(id).unwrap_or_default().as_ref().clone();
            d.film.intent = intent;
            commit_measured(s, id, d, "Analysis Intent", "film.setIntent")
        }),
        cmd!("film.setScan", "Film Scan", [], None, "{scan: camera|minilab|flat}", has_active, |s, p| {
            let scan: FilmScan =
                serde_json::from_value(p.get("scan").cloned().unwrap_or(Value::Null)).map_err(|e| bad("film.setScan", e.to_string()))?;
            let id = active(s, "film.setScan")?;
            let mut d = s.develop_of(id).unwrap_or_default().as_ref().clone();
            d.film.scan = scan;
            commit_measured(s, id, d, "Film Scan", "film.setScan")
        }),
        cmd!("film.setPrint", "Film Print", [], None, "{print: neutral|portrait|vivid|lab}", has_active, |s, p| {
            let print: FilmPrint =
                serde_json::from_value(p.get("print").cloned().unwrap_or(Value::Null)).map_err(|e| bad("film.setPrint", e.to_string()))?;
            let id = active(s, "film.setPrint")?;
            let mut d = s.develop_of(id).unwrap_or_default().as_ref().clone();
            d.film.print = print;
            commit(s, id, d, "Film Print")?;
            ok()
        }),
        cmd!("film.setCapture", "Scan Encoding", [], None, "{capture: linear|gamma18|gamma22}", has_active, |s, p| {
            let capture: FilmCapture =
                serde_json::from_value(p.get("capture").cloned().unwrap_or(Value::Null)).map_err(|e| bad("film.setCapture", e.to_string()))?;
            let id = active(s, "film.setCapture")?;
            let mut d = s.develop_of(id).unwrap_or_default().as_ref().clone();
            d.film.capture = capture;
            commit_measured(s, id, d, "Scan Encoding", "film.setCapture")
        }),
        cmd!("film.pickBase", "Pick Film Base", [], None, "{x, y} normalized coordinates on the clear film", has_active, |s, p| {
            pick(s, p, "film.pickBase", Pick::Base)
        }),
        cmd!("film.pickGray", "Pick Gray", [], None, "{x, y} normalized coordinates of something that should be gray", has_active, |s, p| {
            pick(s, p, "film.pickGray", Pick::Gray)
        }),
        cmd!("film.pickShadow", "Pick Shadow Neutral", [], None, "{x, y} normalized coordinates of a dark neutral", has_active, |s, p| {
            pick(s, p, "film.pickShadow", Pick::Shadow)
        }),
        cmd!("film.pickHighlight", "Pick Highlight Neutral", [], None, "{x, y} normalized coordinates of a light neutral", has_active, |s, p| {
            pick(s, p, "film.pickHighlight", Pick::Highlight)
        }),
        cmd!("film.setFrame", "Set Film Frame", [], None, "{corners: [[x, y], [x, y], [x, y], [x, y]]} normalized", has_active, |s, p| {
            let corners = corners_param(p)?;
            let id = active(s, "film.setFrame")?;
            let mut d = s.develop_of(id).unwrap_or_default().as_ref().clone();
            d.film.frame = FilmFrame { corners };
            commit(s, id, d, "Film Frame")?;
            ok()
        }),
        cmd!("film.cropToImage", "Crop to the Picture", [], None, "{} — crop the scan to the picture edge Convert found", has_active, |s, _| {
            let id = active(s, "film.cropToImage")?;
            let img = scanner_image(s, id, "film.cropToImage")?;
            let d0 = s.develop_of(id).unwrap_or_default();
            let mut d = (*d0).clone();
            // Measure again. An older quad can still contain the printing on the rebate.
            if let Some(frame) = film::detect_gate(&img, d.film.stock).or_else(|| film::detect_frame(&img, d.film.stock)) {
                d.film.frame = frame;
            } else if d.film.frame.is_full() {
                return Err(bad("film.cropToImage", "no film border found"));
            }
            let geo = film::crop_for_frame(&d.film.frame, img.width as f64, img.height as f64)
                .ok_or_else(|| bad("film.cropToImage", "no film border found"))?;
            d.crop.geometry = geo;
            commit(s, id, d, "Crop")?;
            ok()
        }),
    ]
}

enum Pick {
    Base,
    Shadow,
    Highlight,
    Gray,
}

fn pick(s: &mut Session, p: &Value, cmd: &str, which: Pick) -> Result<Value> {
    let (x, y) = (f64_req(p, "x", cmd)?, f64_req(p, "y", cmd)?);
    let id = active(s, cmd)?;
    let img = scanner_image(s, id, cmd)?;
    let d0 = s.develop_of(id).unwrap_or_default();
    let sample = film::sample(&img, x, y).map(|p| film::transmission(p, d0.film.capture)).ok_or_else(|| bad(cmd, "the photo has no pixels"))?;
    let mut d = (*d0).clone();
    match which {
        Pick::Base => {
            if let Some(msg) = film::border_error(d.film.stock, sample, film::bright_level(&img, d0.film.capture)) {
                return Err(bad(cmd, msg));
            }
            d.film.set_base(sample);
            d.film.enabled = true;
        }
        Pick::Shadow => {
            d.film.shadow_pick = Some(sample);
            rebalance(&mut d.film);
            d.film.enabled = true;
        }
        Pick::Highlight => {
            d.film.highlight_pick = Some(sample);
            rebalance(&mut d.film);
            d.film.enabled = true;
        }
        Pick::Gray => {
            let density = film::density_at(&img, x, y, &d.film);
            let mut fog = d.film.fog();
            let mut axis = d.film.axis();
            let g = density[1].max(1e-3);
            for c in [0usize, 2] {
                let intercept = density[c] - axis[c] * g;
                if intercept < 0.0 {
                    axis[c] = (density[c] / g).clamp(0.25, 3.2);
                    fog[c] = 0.0;
                } else {
                    fog[c] = intercept.clamp(0.0, 0.8);
                }
            }
            fog[1] = 0.0;
            axis[1] = 1.0;
            d.film.set_fog(fog);
            d.film.set_axis(axis);
            d.film.enabled = true;
        }
    }
    if d.film.stock.negative() {
        let placed = if matches!(which, Pick::Gray) { film::place_keeping_axis(&img, &d.film) } else { film::place(&img, &d.film) };
        d.film.d_lo = placed.d_lo;
        d.film.d_hi = placed.d_hi;
        d.film.set_fog(placed.fog);
        d.film.set_axis(placed.axis);
        d.film.set_white(placed.white);
        d.film.mid_a = placed.mid_a;
        d.film.mid_b = placed.mid_b;
        d.film.grade = placed.grade;
        d.film.exposure = placed.exposure;
    }
    let base = d.film.base();
    let gamma = d.film.gamma();
    commit(s, id, d, "Negative Craft Pick")?;
    Ok(json!({"sample": sample, "base": base, "gamma": gamma}))
}

fn rebalance(film: &mut Film) {
    let mut picks = Vec::new();
    if let Some(p) = film.shadow_pick {
        picks.push((p, 1.0));
    }
    if let Some(p) = film.highlight_pick {
        picks.push((p, 1.0));
    }
    if !picks.is_empty() {
        film.set_gamma(film::gamma_from_picks(film.base(), &picks, film.gamma_g));
    }
}

fn corners_param(p: &Value) -> Result<[[f64; 2]; 4]> {
    let arr = p.get("corners").and_then(Value::as_array).ok_or_else(|| bad("film.setFrame", "missing corners"))?;
    if arr.len() != 4 {
        return Err(bad("film.setFrame", "corners needs four points"));
    }
    let mut corners = [[0.0; 2]; 4];
    for (i, c) in arr.iter().enumerate() {
        let pair = c.as_array().ok_or_else(|| bad("film.setFrame", "a corner is not a pair"))?;
        let x = pair.first().and_then(Value::as_f64).ok_or_else(|| bad("film.setFrame", "a corner needs x"))?;
        let y = pair.get(1).and_then(Value::as_f64).ok_or_else(|| bad("film.setFrame", "a corner needs y"))?;
        if !x.is_finite() || !y.is_finite() {
            return Err(bad("film.setFrame", "a corner is not a finite number"));
        }
        if let Some(slot) = corners.get_mut(i) {
            *slot = [x.clamp(-0.25, 1.25), y.clamp(-0.25, 1.25)];
        }
    }
    Ok(corners)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn white_balance_reads_the_print_not_the_negative() {
        use std::sync::Arc;

        use lightcraft_develop::Film;
        use lightcraft_raster::Rgb32f;

        use crate::media::SourceLevel;

        let mut s = crate::Session::with_demo();
        let id = s.active().expect("demo photo");
        let negative = [0.55f32, 0.22, 0.08];
        let image = Arc::new(Rgb32f::filled(24, 24, negative));
        for level in [SourceLevel::Thumb, SourceLevel::Preview, SourceLevel::Full] {
            s.media.insert(id, level, image.clone());
        }
        let mut d = (*s.develop_of(id).expect("settings")).clone();
        d.film = Film { enabled: true, scan: lightcraft_develop::FilmScan::Flat, ..Film::default() };
        d.film.set_base([0.85, 0.40, 0.16]);
        s.set_develop(id, d, "Film").expect("settings");

        let spread = |p: [f32; 3]| p[0].max(p[1]).max(p[2]) - p[0].min(p[1]).min(p[2]);
        s.execute("develop.wbPick", &json!({"x": 0.5, "y": 0.5})).expect("pick");
        let info = s.source_info(id);
        let picked = s.develop_of(id).expect("settings");
        let mut positive = Rgb32f::filled(8, 8, negative);
        lightcraft_pipeline::film::apply_before_white_balance(&mut positive, &info, &picked.film);
        let before = positive.get(0, 0);
        assert!(spread(before) > 0.02, "the fixture print should be off colour, got {before:?}");
        let mut corrected = positive.clone();
        lightcraft_pipeline::local::white_balance(&mut corrected, &info, &picked);
        let after = corrected.get(0, 0);
        assert!(spread(after) < spread(before) * 0.35, "selector left a cast {before:?} -> {after:?}");

        s.execute("develop.wb", &json!({"mode": "auto"})).expect("auto");
        let auto = s.develop_of(id).expect("settings");
        let mut corrected = positive;
        lightcraft_pipeline::local::white_balance(&mut corrected, &info, &auto);
        let after = corrected.get(0, 0);
        assert!(spread(after) < spread(before) * 0.35, "auto left a cast {before:?} -> {after:?}");
    }

    #[test]
    fn auto_tone_and_bw_mix_read_the_print_not_the_negative() {
        use std::sync::Arc;

        use lightcraft_develop::Film;
        use lightcraft_raster::Rgb32f;

        use crate::media::SourceLevel;

        let mut s = crate::Session::with_demo();
        let id = s.active().expect("demo photo");
        // The blank film itself: bright orange, and the print of that base is black.
        let mask = [0.85f32, 0.40, 0.16];
        let image = Arc::new(Rgb32f::filled(32, 32, mask));
        for level in [SourceLevel::Thumb, SourceLevel::Preview, SourceLevel::Full] {
            s.media.insert(id, level, image.clone());
        }
        let exposure = |v: &serde_json::Value| v.get("exposure").and_then(|n| n.as_f64()).expect("exposure");
        let band = |v: &serde_json::Value, i: usize| {
            v.get("mix").and_then(|m| m.as_array()).and_then(|a| a.get(i)).and_then(|n| n.as_f64()).expect("mix band")
        };
        let off = s.execute("develop.auto", &json!({})).expect("auto off");
        let exposure_off = exposure(&off);

        let mut d = (*s.develop_of(id).expect("settings")).clone();
        d.film = Film { enabled: true, scan: lightcraft_develop::FilmScan::Flat, ..Film::default() };
        d.film.set_base([0.85, 0.40, 0.16]);
        s.set_develop(id, d, "Film").expect("settings");

        let on = s.execute("develop.auto", &json!({})).expect("auto on");
        let exposure_on = exposure(&on);
        assert!(exposure_off < 0.0, "the orange mask looked dark: {exposure_off}");
        assert!(exposure_on > 1.0, "auto toned the negative, not the black print: {exposure_on} (off {exposure_off})");

        // A light yellow beside a dark blue. On the negative, yellow is the light band and blue the
        // dark one. The print is a different picture, so that mix cannot come back the same.
        // Exposure is cleared so it does not crush either.
        let chart = Rgb32f::from_fn(64, 32, |x, _| if x < 32 { [0.55, 0.50, 0.05] } else { [0.01, 0.02, 0.12] });
        let chart = Arc::new(chart);
        for level in [SourceLevel::Thumb, SourceLevel::Preview, SourceLevel::Full] {
            s.media.insert(id, level, chart.clone());
        }
        let mut d = (*s.develop_of(id).expect("settings")).clone();
        d.film.enabled = false;
        d.light.exposure = 0.0;
        s.set_develop(id, d, "Film").expect("settings");
        let mix_off = s.execute("develop.autoBwMix", &json!({})).expect("mix off");
        let mut d = (*s.develop_of(id).expect("settings")).clone();
        d.film.enabled = true;
        d.film.scan = lightcraft_develop::FilmScan::Flat;
        d.film.set_base([1.0, 1.0, 1.0]);
        d.light.exposure = 0.0;
        s.set_develop(id, d, "Film").expect("settings");
        let mix_on = s.execute("develop.autoBwMix", &json!({})).expect("mix on");
        let (yellow_off, blue_off) = (band(&mix_off, 2), band(&mix_off, 5));
        assert!(yellow_off > 0.0 && blue_off < 0.0, "the negative mix did not separate {mix_off}");
        assert_ne!(mix_off.get("mix"), mix_on.get("mix"), "the mix still saw the negative {mix_on}");
    }

    #[test]
    fn margin_reprints_and_keeps_the_stored_mask() {
        use std::sync::Arc;

        use lightcraft_develop::Film;
        use lightcraft_raster::Rgb32f;

        use crate::media::SourceLevel;

        let mut s = crate::Session::with_demo();
        let id = s.active().expect("demo photo");
        let base = [0.90f32, 0.42, 0.16];
        let picture = [0.32f32, 0.15, 0.06];
        let leak = [0.95f32, 0.20, 0.05];
        let image = Arc::new(Rgb32f::from_fn(40, 40, |x, y| if x < 8 || y < 8 || x >= 32 || y >= 32 { leak } else { picture }));
        for level in [SourceLevel::Thumb, SourceLevel::Preview, SourceLevel::Full] {
            s.media.insert(id, level, image.clone());
        }
        let mut d = (*s.develop_of(id).expect("settings")).clone();
        d.film = Film { enabled: true, scan: lightcraft_develop::FilmScan::Flat, grade: 1.4, ..Film::default() };
        d.film.set_base([base[0] as f64, base[1] as f64, base[2] as f64]);
        let mask = d.film.base();
        s.set_develop(id, d, "Film").expect("settings");
        let before = s.develop_of(id).expect("settings").film.exposure;
        s.execute("develop.set", &json!({"control": "film.inset", "value": 25.0})).expect("margin");
        let film = s.develop_of(id).expect("settings").film.clone();
        assert!((film.inset - 25.0).abs() < 1e-9);
        assert!((film.exposure - before).abs() > 0.05, "margin did not reprint: {before} -> {}", film.exposure);
        assert!((film.grade - 1.4).abs() < 1e-9, "margin replaced contrast");
        let kept = film.base();
        assert!((kept[0] - mask[0]).abs() < 1e-9 && (kept[2] - mask[2]).abs() < 1e-9, "margin moved the mask");
    }

    #[test]
    fn convert_after_a_crop_ignores_the_surround() {
        use std::sync::Arc;

        use lightcraft_raster::Rgb32f;

        use crate::media::SourceLevel;

        let mut s = crate::Session::with_demo();
        let id = s.active().expect("demo photo");
        let orange = [0.80f32, 0.38, 0.14];
        let junk = [0.15f32, 0.75, 0.20];
        let picture = orange.map(|c| c * 10f32.powf(-0.45));
        let image = Arc::new(Rgb32f::from_fn(80, 40, |x, y| {
            if x < 24 {
                junk
            } else if x >= 76 || !(3..37).contains(&y) {
                orange
            } else {
                picture
            }
        }));
        for level in [SourceLevel::Thumb, SourceLevel::Preview, SourceLevel::Full] {
            s.media.insert(id, level, image.clone());
        }
        s.execute("film.analyze", &json!({})).expect("full");
        let open = s.develop_of(id).expect("settings").film.exposure;
        let mut d = (*s.develop_of(id).expect("settings")).clone();
        d.crop.geometry.rect = lightcraft_geom::Rect::new(24.0 / 80.0, 0.0, 1.0, 1.0);
        s.set_develop(id, d, "Crop").expect("crop");
        s.execute("film.analyze", &json!({})).expect("cropped");
        let cut = s.develop_of(id).expect("settings").film.exposure;
        assert!((open - cut).abs() > 0.15, "the surround still placed the print {open} -> {cut}");
    }

    #[test]
    fn analyze_turns_conversion_on_and_a_bad_frame_is_refused() {
        let mut s = crate::Session::with_demo();
        let id = s.active().expect("demo photo");
        let r = s.execute("film.analyze", &json!({})).expect("analyze");
        assert!(r.get("base").is_some());
        let film = s.develop_of(id).expect("settings").film.clone();
        assert!(film.enabled);
        assert!(s.execute("film.setFrame", &json!({"corners": [[0.0, 0.0]]})).is_err());
        assert_eq!(s.develop_of(id).expect("settings").film.frame, film.frame);
        let before = s.develop_of(id).expect("settings").film.exposure;
        s.execute("film.setFrame", &json!({"corners": [[0.10, 0.15], [0.90, 0.15], [0.90, 0.85], [0.10, 0.85]]})).expect("frame");
        s.execute("film.cropToImage", &json!({})).expect("crop");
        let cropped = s.develop_of(id).expect("settings");
        assert!((cropped.film.exposure - before).abs() < 1e-9, "cropping does not reprint");
        let r = cropped.crop.geometry.rect;
        assert!(r.x1 - r.x0 > 0.2 && r.y1 - r.y0 > 0.2, "crop collapsed {r:?}");
        let placed = s.develop_of(id).expect("settings").film.clone();
        s.execute("preset.apply", &json!({"id": "lc.nc-paper"})).expect("look");
        let look = s.develop_of(id).expect("settings").film.clone();
        assert!(look.enabled);
        assert!(look.black > 0.0);
        assert!((look.grade - placed.grade).abs() < 1e-9, "a look must not replace the print contrast");
        assert!((look.exposure - placed.exposure).abs() < 1e-9, "a look must not replace the print exposure");
        assert_eq!(look.base(), placed.base());
        let roll = s.execute("film.analyzeRoll", &json!({})).expect("roll");
        assert!(roll.get("frames").is_some());
    }

    #[test]
    fn a_source_change_is_one_undo_with_its_new_measurement() {
        let mut s = crate::Session::with_demo();
        let id = s.active().expect("demo photo");
        s.execute("film.analyze", &json!({})).expect("analyze");
        let before = s.develop_of(id).expect("settings").film.clone();
        let steps = s.undo.len();
        s.execute("film.setStock", &json!({"stock": "bwNeg"})).expect("stock");
        assert_eq!(s.undo.len(), steps + 1, "the stock and its remeasurement are one step");
        let after = s.develop_of(id).expect("settings").film.clone();
        assert_eq!(after.stock, lightcraft_develop::FilmStock::BwNeg);
        assert!(after.enabled);
        s.undo_step().expect("undo");
        let restored = s.develop_of(id).expect("settings").film.clone();
        assert_eq!(restored.stock, before.stock);
        assert_eq!(restored.base(), before.base());
        assert!((restored.exposure - before.exposure).abs() < 1e-9);
    }

    #[test]
    fn a_roll_is_one_undo_and_auto_sync_does_not_copy_one_frame_onto_another() {
        let ids: Vec<_> = {
            let s = crate::Session::with_demo();
            s.catalog.photos().map(|p| p.id).take(2).collect()
        };
        assert_eq!(ids.len(), 2, "the demo library has two photos");
        let run = |sync: bool| {
            let mut s = crate::Session::with_demo();
            s.auto_sync = sync;
            s.selection.ids = ids.clone();
            s.selection.active = ids.last().copied();
            let steps = s.undo.len();
            s.execute("film.analyzeRoll", &json!({})).expect("roll");
            assert_eq!(s.undo.len(), steps + 1, "the roll is one undo step");
            let films: Vec<_> = ids.iter().map(|id| s.develop_of(*id).expect("settings").film.clone()).collect();
            assert!(films.iter().all(|f| f.enabled));
            s.undo_step().expect("undo");
            for id in &ids {
                assert!(!s.develop_of(*id).expect("settings").film.enabled, "one undo clears every frame");
            }
            films
        };
        let plain = run(false);
        let synced = run(true);
        for (a, b) in plain.iter().zip(&synced) {
            assert_eq!(a.base(), b.base());
            assert!((a.exposure - b.exposure).abs() < 1e-9);
            assert_eq!(a.gamma(), b.gamma());
        }
    }

    #[test]
    fn warm_is_amber_on_a_neutral_negative() {
        use lightcraft_develop::Film;
        use lightcraft_pipeline::film::FilmKernel;
        let look = crate::presets::builtin().into_iter().find(|p| p.id == "lc.nc-warm").expect("warm look");
        let spec = look.settings.get("film").and_then(|v| v.as_object()).expect("film keys");
        let num = |k: &str| spec.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);
        assert!(num("temp") > 0.0, "warmth is temperature");
        assert!(num("tint") <= 0.0, "positive tint removes green");
        assert!(num("high_tint") <= 0.0, "highlight tint was turning the warm look purple");
        assert!(num("presat") <= 0.0, "presat is applied before the inversion and cools a negative");

        let base = [0.80f64, 0.38, 0.14];
        let pix = base.map(|v| (v * 10f64.powf(-0.45)) as f32);
        let plain = {
            let mut n = Film { enabled: true, ..Film::default() };
            n.set_base(base);
            FilmKernel::from_film(&n).apply(pix)
        };
        let mut warm = Film { enabled: true, ..Film::default() };
        warm.set_base(base);
        warm.presat = num("presat");
        warm.temp = num("temp");
        warm.tint = num("tint");
        warm.shadow_temp = num("shadow_temp");
        warm.shadow_tint = num("shadow_tint");
        warm.mid_temp = num("mid_temp");
        warm.mid_tint = num("mid_tint");
        warm.high_temp = num("high_temp");
        warm.high_tint = num("high_tint");
        let printed = FilmKernel::from_film(&warm).apply(pix);
        assert!(printed[0] > plain[0] * 1.05, "warm should add red {printed:?} vs {plain:?}");
        assert!(printed[2] < plain[2] * 0.95, "warm should take out blue {printed:?} vs {plain:?}");
        assert!(printed[1] >= plain[1] * 0.99, "warm should hold green {printed:?} vs {plain:?}");
    }
}
