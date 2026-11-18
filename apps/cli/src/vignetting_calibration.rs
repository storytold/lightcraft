//! `<cli> vignetting-calibration REFERENCE.raw [--grid N] [--smooth S] [--check B.raw [--force]] [--save MAP.json] [--save-settings PARTIAL.json] [--json]`:
//! measure a flat-field reference (a raw shot through a diffuser, Capture One's LCC "Uniform Light"), print its
//! light fall-off, its optical centre and the lens database's vignetting profile for the same shot, and with
//! `--check` how much fall-off the map leaves in a second reference of the same setup ([`lcc_raw::vignetting_calibration`]).
//!
//! Every fall-off is in EV, positive = darker than the brightest part of the frame. Radii are measured from the
//! centre of the developed frame (the default crop, unrotated) in units of its half diagonal: the frame the lens
//! profile is expressed in and `Warp::gain` evaluates it in.

use std::path::Path;

use lcc_geom::{Orientation, Point};
use lcc_raw::SensorWindow;
use lcc_raw::vignetting_calibration::{
    self, CentreFit, CheckReport, OffsetFit, SensorFrame, VignettingCalibrationOptions, VignettingCalibrationSource, VignettingMeasurement,
};
use serde_json::{Value, json};

/// Radii of the radial table, in % of the half diagonal.
const RADII: [u32; 5] = [0, 25, 50, 75, 100];
/// The lens profile's ratio is shown only where the profile's own fall-off is at least this (EV).
const MIN_PROFILE_EV: f64 = 0.1;
/// Focal lengths and apertures this close count as the same.
const SAME: f64 = 0.05;

pub fn run(args: &[String]) -> Result<(), String> {
    let mut opts = VignettingCalibrationOptions::default();
    let (mut files, mut check, mut save, mut save_settings) = (Vec::new(), None, None, None);
    let (mut force, mut json_out) = (false, false);
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = |name: &str| -> Result<String, String> { it.next().cloned().ok_or(format!("{name} needs a value")) };
        match arg.as_str() {
            "--grid" => {
                let v = value("--grid")?;
                opts.long_cells = v.parse().map_err(|_| format!("--grid: `{v}` is not a number of cells"))?;
            }
            "--smooth" => {
                let v = value("--smooth")?;
                opts.smoothing = v.parse().map_err(|_| format!("--smooth: `{v}` is not a number of cells"))?;
            }
            "--check" => check = Some(value("--check")?),
            "--save" => save = Some(value("--save")?),
            "--save-settings" => save_settings = Some(value("--save-settings")?),
            "--force" => force = true,
            "--json" => json_out = true,
            a if a.starts_with("--") => return Err(format!("unknown option `{a}`")),
            f => files.push(f.to_string()),
        }
    }
    let [reference] = files.as_slice() else {
        return Err("vignetting-calibration needs one reference raw: REFERENCE.raw".into());
    };
    let mut a = Shot::load(reference)?;
    let mut map = vignetting_calibration::build(&a.normalized, a.source.clone(), &opts).map_err(|e| format!("{reference}: {e}"))?;
    map.window = a.window;
    // (the samples aren't needed any more; a second reference is decoded next)
    a.normalized.data = Vec::new();
    if let Some(out) = &save {
        let text = serde_json::to_string(&map).map_err(|e| e.to_string())?;
        std::fs::write(out, text).map_err(|e| format!("{out}: {e}"))?;
    }
    if let Some(out) = &save_settings {
        // partial develop settings: `render --settings`, `develop.merge`, a preset (the amount at its default)
        let mut embedded = lcc_pipeline::vignetting_calibration::embed(&map).ok_or("the map doesn't fit the develop settings' form")?;
        embedded.source.file_name = Path::new(reference).file_name().map(|n| n.to_string_lossy().into_owned());
        // the portable form: the id, and the map for the catalog that reads it to store
        let id = lcc_develop::VignettingMapId::of(&embedded);
        let v = json!({"optics": {"vignetting_calibration": id.as_str(), "vignetting_calibration_map": embedded, "vignetting_calibration_amount": 100.0}});
        let text = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
        std::fs::write(out, text).map_err(|e| format!("{out}: {e}"))?;
    }
    let measured = Profile::of(&a.frame, &|p| map.ev_at(p.x, p.y));
    let centre = vignetting_calibration::fit_centre(&map.ev_grid());
    let lens = lens_profile(&a);
    let checked = match &check {
        Some(path) => Some(check_against(&map, &a, path, &opts, force)?),
        None => None,
    };
    if json_out {
        let v = json!({
            "file": reference,
            "source": map.source,
            "sensor": map.sensor,
            "grid": map.grid,
            "window": map.window,
            "smoothing": opts.smoothing,
            "falloff": measured.json(),
            "centre": centre_json(centre.as_ref(), &measured),
            "lensProfile": match &lens {
                Ok((name, p)) => json!({"name": name, "falloff": p.json(), "ratio": ratios(&measured, p)}),
                Err(why) => json!({"none": why}),
            },
            "check": checked.as_ref().map(Checked::json),
            "saved": save,
            "savedSettings": save_settings,
        });
        println!("{}", serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?);
        return Ok(());
    }
    println!("Reference: {reference}");
    println!("  {}", describe(&map.source));
    println!(
        "  active area {} × {} px{}, grid {} × {} cells, smoothing σ {} cell(s)",
        map.sensor[0],
        map.sensor[1],
        describe_window(map.window.as_ref()),
        map.grid[0],
        map.grid[1],
        opts.smoothing
    );
    println!("Fall-off (EV, positive = darker than the brightest part):");
    print_corners(&measured);
    print_centre(centre.as_ref(), &measured);
    match &lens {
        Ok((name, p)) => {
            println!("Radial, from the frame centre (r = % of the half diagonal; lens profile: {name}):");
            println!("     r   measured   min–max          profile   measured/profile");
            for ((m, p), ratio) in measured.rings.iter().zip(&p.rings).zip(ratios(&measured, p)) {
                let ratio = ratio.map_or("–".to_string(), |r| format!("{r:.2}"));
                println!("  {:>3} %  {:>8.3}   {:>6.3}–{:<6.3}   {:>8.3}   {ratio}", m.pct, m.mean, m.min, m.max, p.mean);
            }
        }
        Err(why) => {
            println!("Radial, from the frame centre (r = % of the half diagonal; lens profile: {why}):");
            println!("     r   measured   min–max");
            for m in &measured.rings {
                println!("  {:>3} %  {:>8.3}   {:>6.3}–{:<6.3}", m.pct, m.mean, m.min, m.max);
            }
        }
    }
    println!("  (a large min–max spread at one radius means the fall-off is off-centre)");
    if let Some(c) = &checked {
        c.print();
    }
    if let Some(out) = &save {
        println!("Map saved: {out}");
    }
    if let Some(out) = &save_settings {
        println!("Develop settings saved: {out} (apply with render --settings or develop.merge)");
    }
    Ok(())
}

/// A decoded reference.
struct Shot {
    path: String,
    normalized: lcc_raw::Normalized,
    frame: SensorFrame,
    /// Where the active area sits on the full sensor, when the file says.
    window: Option<SensorWindow>,
    /// The picture's place on the sensor for the lens profile (`lcc_engine::lens_profiles::ImageArea`), as the app
    /// finds it: `None` for the full sensor or without a window.
    area: Option<lcc_engine::lens_profiles::ImageArea>,
    meta: lcc_meta::Metadata,
    source: VignettingCalibrationSource,
}

impl Shot {
    fn load(path: &str) -> Result<Shot, String> {
        let bytes = std::fs::read(Path::new(path)).map_err(|e| format!("{path}: {e}"))?;
        let mut sha = lcc_hash::Sha1::new();
        sha.update(&bytes);
        let sha1 = sha.finish().to_hex();
        let raw = lcc_raw::decode(&bytes).map_err(|e| format!("{path}: {e}"))?;
        // the file's metadata (as the lens lookup reads it), with the decoder's for whatever it lacks
        let mut meta = lcc_meta::extract(&bytes);
        let r = &raw.metadata;
        meta.make = meta.make.or_else(|| r.make.clone());
        meta.model = meta.model.or_else(|| r.model.clone());
        meta.serial_number = meta.serial_number.or_else(|| r.serial_number.clone());
        meta.lens_make = meta.lens_make.or_else(|| r.lens_make.clone());
        meta.lens_model = meta.lens_model.or_else(|| r.lens_model.clone());
        meta.lens_spec = meta.lens_spec.or(r.lens_spec);
        meta.focal_length = meta.focal_length.or(r.focal_length);
        meta.focal_length_35mm = meta.focal_length_35mm.or(r.focal_length_35mm);
        meta.f_number = meta.f_number.or(r.f_number);
        let normalized = raw.normalized().map_err(|e| format!("{path}: {e}"))?;
        let window = lcc_raw::sensor_window(&bytes, [normalized.width, normalized.height]);
        drop(bytes);
        let frame = SensorFrame::of(&raw);
        let area = lcc_engine::lens_profiles::ImageArea::of(&raw.info(), window);
        let source = VignettingCalibrationSource::from_metadata(&meta, Some(sha1));
        Ok(Shot { path: path.to_string(), normalized, frame, window, area, meta, source })
    }
}

/// One radius of the radial table.
struct Ring {
    pct: u32,
    mean: f64,
    min: f64,
    max: f64,
}

/// A fall-off sampled at the developed frame's corners and radii.
struct Profile {
    /// Top-left, top-right, bottom-left, bottom-right of the unrotated frame.
    corners: [f64; 4],
    rings: Vec<Ring>,
}

impl Profile {
    /// Sample `ev` (a function of sensor coordinates) over `frame`'s developed picture.
    fn of(frame: &SensorFrame, ev: &dyn Fn(Point) -> f64) -> Profile {
        Profile::in_developed(frame, &|p| ev(frame.cropped_to_sensor(p)))
    }

    /// Sample `ev` (a function of developed-frame coordinates).
    fn in_developed(frame: &SensorFrame, ev: &dyn Fn(Point) -> f64) -> Profile {
        let (w, h) = (frame.developed[0] as f64, frame.developed[1] as f64);
        let corners = [ev(Point::new(0.0, 0.0)), ev(Point::new(w, 0.0)), ev(Point::new(0.0, h)), ev(Point::new(w, h))];
        let (cx, cy, hd) = (w / 2.0, h / 2.0, w.hypot(h) / 2.0);
        let rings = RADII
            .iter()
            .map(|&pct| {
                let values: Vec<f64> = match pct {
                    0 => vec![ev(Point::new(cx, cy))],
                    // only the corners are this far out
                    100 => corners.to_vec(),
                    _ => (0..360)
                        .filter_map(|k| {
                            let t = (k as f64).to_radians();
                            let r = hd * pct as f64 / 100.0;
                            let p = Point::new(cx + r * t.cos(), cy + r * t.sin());
                            (p.x >= 0.0 && p.x <= w && p.y >= 0.0 && p.y <= h).then(|| ev(p))
                        })
                        .collect(),
                };
                // (positions a map doesn't cover have no value: NaN)
                let values: Vec<f64> = values.into_iter().filter(|v| v.is_finite()).collect();
                if values.is_empty() {
                    return Ring { pct, mean: f64::NAN, min: f64::NAN, max: f64::NAN };
                }
                let n = values.len() as f64;
                Ring {
                    pct,
                    mean: values.iter().sum::<f64>() / n,
                    min: values.iter().copied().fold(f64::INFINITY, f64::min),
                    max: values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                }
            })
            .collect();
        Profile { corners, rings }
    }

    /// The mean of the four corners' fall-off (those with a value).
    fn corner_mean(&self) -> f64 {
        let v: Vec<f64> = self.corners.iter().copied().filter(|c| c.is_finite()).collect();
        if v.is_empty() { f64::NAN } else { v.iter().sum::<f64>() / v.len() as f64 }
    }

    fn json(&self) -> Value {
        let [tl, tr, bl, br] = self.corners;
        json!({
            "corners": {"topLeft": tl, "topRight": tr, "bottomLeft": bl, "bottomRight": br},
            "radial": self.rings.iter().map(|r| json!({"rPct": r.pct, "mean": r.mean, "min": r.min, "max": r.max})).collect::<Vec<_>>(),
        })
    }
}

/// measured / profile at each radius, where the profile's fall-off is at least [`MIN_PROFILE_EV`].
fn ratios(measured: &Profile, profile: &Profile) -> Vec<Option<f64>> {
    measured.rings.iter().zip(&profile.rings).map(|(m, p)| (p.mean >= MIN_PROFILE_EV).then(|| m.mean / p.mean)).collect()
}

/// The lens database's vignetting for the shot, sampled where the measured fall-off is, or why there is none.
///
/// Evaluated as `Warp::gain` evaluates it (`crates/pipeline/src/optics.rs`): the app looks the profile up for the
/// developed size, the file's orientation and the picture's place on the sensor (`lens_profiles::resolve_raw` →
/// `lookup_framed`, which gives a cropped image area the part of the profile under it); here the same lookup with
/// `Orientation::Normal`, the same profile before `reorient_lens` re-expresses it for the rotated frame. The
/// profile's own radius (lensfun's `pa` model: 1 at half the calibration frame's diagonal) is converted by
/// `Profile::correction` (`crates/lensdb/src/lib.rs`) to a DNG `FixVignetteRadial` with 1 at this frame's half
/// diagonal, centred on it; its gain is `embedded_vignette_gain` at `Warp::gain`'s full strength, and the fall-off
/// in EV is its log2.
fn lens_profile(shot: &Shot) -> Result<(String, Profile), String> {
    // the database folder chosen in the app's Settings, as the app loads it at start (the environment still wins)
    let _settings = lcc_engine::lens_download::LensSettings::load(lcc_engine::config::config_dir());
    let db = lcc_engine::lens_profiles::database().ok_or("no lens database configured (Settings ▸ Folders ▸ Lens profiles)")?;
    let (w, h) = (shot.frame.developed[0] as f64, shot.frame.developed[1] as f64);
    let lens = shot.source.lens.clone().unwrap_or_else(|| "the lens".into());
    let at = format!(
        "{} at {} f/{}",
        lens,
        shot.source.focal_length.map_or("? mm".into(), |f| format!("{f} mm")),
        shot.source.aperture.map_or("?".into(), |a| a.to_string())
    );
    let (correction, applied) = lcc_engine::lens_profiles::lookup_framed(&db, &shot.meta, None, w, h, Orientation::Normal, shot.area.as_ref())
        .ok_or(format!("no profile in the lens database for {at}"))?;
    let v = correction.vignette.ok_or(format!("the lens database's profile has no vignetting for {at}"))?;
    let name = match applied {
        lcc_engine::lens_profiles::Applied::Found(n) | lcc_engine::lens_profiles::Applied::Chosen(n) => n,
        lcc_engine::lens_profiles::Applied::Embedded => lens,
    };
    let ev = |p: Point| lcc_pipeline::optics::embedded_vignette_gain(&v, p, w, h).max(1e-9).log2();
    Ok((name, Profile::in_developed(&shot.frame, &ev)))
}

/// The second reference of `--check`, and what the map leaves of its fall-off.
struct Checked {
    path: String,
    source: VignettingCalibrationSource,
    window: Option<SensorWindow>,
    size: [usize; 2],
    forced: Vec<String>,
    /// Whether the map covers all of the second reference (else the check is over the area they share).
    covers_all: bool,
    before: Profile,
    after: Profile,
    centre: Option<CentreFit>,
    report: CheckReport,
    /// The offset search, or why it couldn't run.
    offset: Result<OffsetFit, String>,
}

fn check_against(map: &VignettingMeasurement, a: &Shot, path: &str, opts: &VignettingCalibrationOptions, force: bool) -> Result<Checked, String> {
    let b = Shot::load(path)?;
    let size = [b.normalized.width, b.normalized.height];
    let differs = differences(&a.source, &b.source);
    if !differs.is_empty() && !force {
        return Err(format!("{path} is not the reference's setup ({}); --force compares anyway", differs.join(", ")));
    }
    // the same active area, or a known image area of the same sensor (never another sensor, whatever --force says)
    let offset = map.relative_offset(size, b.window.as_ref()).map_err(|e| format!("{path}: {e}"))?;
    let covers_all = map.placement(size, b.window.as_ref()).is_ok();
    let grid = vignetting_calibration::grid_for(size[0], size[1], opts.long_cells);
    let cells = vignetting_calibration::cell_means(&b.normalized, grid, opts).map_err(|e| format!("{path}: {e}"))?;
    let report = vignetting_calibration::check(map, &cells, offset, opts).map_err(|e| format!("{path}: {e}"))?;
    let found = vignetting_calibration::find_offset(map, &cells, offset).map_err(|e| e.to_string());
    let before = Profile::of(&b.frame, &|p| report.before.at(p.x, p.y));
    let after = Profile::of(&b.frame, &|p| report.after.at(p.x, p.y));
    let centre = vignetting_calibration::fit_centre(&report.before);
    Ok(Checked { path: b.path, source: b.source, window: b.window, size, forced: differs, covers_all, before, after, centre, report, offset: found })
}

/// What differs between two references' setups: camera, lens, focal length, aperture.
fn differences(a: &VignettingCalibrationSource, b: &VignettingCalibrationSource) -> Vec<String> {
    let mut out = Vec::new();
    let text = |s: &Option<String>| s.as_deref().unwrap_or("?").to_string();
    if (&a.make, &a.model) != (&b.make, &b.model) {
        out.push(format!("camera {} {} vs {} {}", text(&a.make), text(&a.model), text(&b.make), text(&b.model)));
    }
    if a.lens != b.lens {
        out.push(format!("lens {} vs {}", text(&a.lens), text(&b.lens)));
    }
    let near = |x: Option<f64>, y: Option<f64>| match (x, y) {
        (Some(x), Some(y)) => (x - y).abs() <= SAME,
        (None, None) => true,
        _ => false,
    };
    let num = |v: Option<f64>| v.map_or("?".into(), |v| v.to_string());
    if !near(a.focal_length, b.focal_length) {
        out.push(format!("focal length {} vs {} mm", num(a.focal_length), num(b.focal_length)));
    }
    if !near(a.aperture, b.aperture) {
        out.push(format!("aperture f/{} vs f/{}", num(a.aperture), num(b.aperture)));
    }
    out
}

impl Checked {
    fn json(&self) -> Value {
        json!({
            "file": self.path,
            "source": self.source,
            "size": self.size,
            "window": self.window,
            "forced": self.forced,
            "coversAll": self.covers_all,
            "cellsUsed": self.report.cells_used,
            "before": self.before.json(),
            "after": self.after.json(),
            "centre": centre_json(self.centre.as_ref(), &self.before),
            "maxAbsResidual": self.report.max_abs,
            "peakToPeak": self.report.peak_to_peak,
            "pass": self.report.passes(),
            "offset": match &self.offset {
                Ok(o) => json!(o),
                Err(why) => json!({"none": why}),
            },
        })
    }

    fn print(&self) {
        println!("Check: {}", self.path);
        println!("  {}", describe(&self.source));
        println!("  active area {} × {} px{}", self.size[0], self.size[1], describe_window(self.window.as_ref()));
        if !self.forced.is_empty() {
            println!("  compared despite: {}", self.forced.join(", "));
        }
        println!("  Its own fall-off before correction (EV, from its brightest smoothed region):");
        print_corners(&self.before);
        print_centre(self.centre.as_ref(), &self.before);
        let area = if self.covers_all { String::new() } else { format!(", over the {} cells the map covers", self.report.cells_used) };
        println!("  After the map at amount 1.0 (EV from the median corrected cell, positive = darker{area}):");
        print_corners(&self.after);
        println!("     r   before   after (mean, min–max)");
        for (b, a) in self.before.rings.iter().zip(&self.after.rings) {
            if a.mean.is_finite() {
                println!("  {:>3} %  {:>6.3}   {:>+7.3}  {:>+7.3}–{:<+7.3}", b.pct, b.mean, a.mean, a.min, a.max);
            } else {
                println!("  {:>3} %  {:>6.3}   –  (not covered by the map)", b.pct, b.mean);
            }
        }
        match &self.offset {
            Ok(o) => {
                println!(
                    "  placement: expected offset {:+.0}, {:+.0} px (from the sensor windows); best fit {:+.0}, {:+.0} px",
                    o.expected[0], o.expected[1], o.found[0], o.found[1]
                );
                let zero = o.rms_zero.map_or(String::new(), |z| format!(", with no offset {z:.4}"));
                println!(
                    "    residual spread: {:.4} EV at the fit, {:.4} at the expected offset, {:.4} at 16 px off{zero} ({} cells)",
                    o.rms_found, o.rms_expected, o.rms_16px, o.cells
                );
            }
            Err(why) => println!("  placement: no offset search ({why})"),
        }
        let verdict = if self.report.passes() { "PASS" } else { "FAIL" };
        println!(
            "  max |residual| {:.3} EV, peak-to-peak {:.3} EV: {verdict} (needs < 0.05 and < 0.1)",
            self.report.max_abs, self.report.peak_to_peak
        );
    }
}

fn describe_window(w: Option<&SensorWindow>) -> String {
    match w {
        Some(w) if w.size != w.sensor => {
            format!(" (at {}, {} on the {} × {} px sensor)", w.origin[0], w.origin[1], w.sensor[0], w.sensor[1])
        }
        Some(_) => " (the full sensor)".into(),
        None => String::new(),
    }
}

/// Below this mean corner fall-off (EV) there is too little fall-off to place an optical centre.
const MIN_CENTRE_EV: f64 = 0.1;

fn centre_json(c: Option<&CentreFit>, p: &Profile) -> Value {
    let corner = p.corner_mean();
    match c {
        Some(c) => json!({
            "dxPct": c.dx_pct,
            "dyPct": c.dy_pct,
            "rmsEv": c.rms_ev,
            "uneven": c.uneven(corner),
            "tooFlat": (corner.is_nan() || corner < MIN_CENTRE_EV),
        }),
        None => Value::Null,
    }
}

fn print_corners(p: &Profile) {
    let f = |v: f64| if v.is_finite() { format!("{v:+.3}") } else { "–".into() };
    let [tl, tr, bl, br] = p.corners.map(f);
    println!("  corners: top-left {tl}  top-right {tr}  bottom-left {bl}  bottom-right {br}  (unrotated frame)");
}

fn print_centre(c: Option<&CentreFit>, p: &Profile) {
    let corner = p.corner_mean();
    match c {
        _ if (corner.is_nan() || corner < MIN_CENTRE_EV) => println!("  optical centre: too little fall-off to place one"),
        Some(c) => {
            let note = if c.uneven(corner) { " (uneven: more than a lens's radial fall-off; a sky gradient or uneven light?)" } else { "" };
            println!("  optical centre: {:+.1} % x, {:+.1} % y from the frame centre (fit RMS {:.4} EV){note}", c.dx_pct, c.dy_pct, c.rms_ev);
        }
        None => println!("  optical centre: no fit"),
    }
}

fn describe(s: &VignettingCalibrationSource) -> String {
    let text = |v: &Option<String>| v.clone().unwrap_or_else(|| "?".into());
    format!(
        "{} {} (serial {}) · {} · {} · f/{}",
        text(&s.make),
        text(&s.model),
        text(&s.serial),
        text(&s.lens),
        s.focal_length.map_or("? mm".into(), |f| format!("{f} mm")),
        s.aperture.map_or("?".into(), |a| a.to_string())
    )
}
