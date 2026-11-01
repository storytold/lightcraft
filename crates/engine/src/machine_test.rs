//! "Test my machine": a few minutes of real export work, at several numbers of photos side by
//! side, with and without the GPU, to pick what this computer exports fastest with (export lanes,
//! the GPU for exports or not, how many exports share the GPU at once), and a plain report of it
//! ("Exports about 14 photos a minute …; what limits it: …"). Big machines were left mostly idle
//! by settings sized for small ones.
//!
//! The work is a wedding-style edit ([`bench_settings`]: tone, clarity, noise reduction,
//! sharpening and a radial mask, HDR on)
//! exported as full-size HDR gain map JPEGs and thrown away (nothing is written), from the photos
//! given (a few of the user's own raws, so their camera's decode counts) or else the demo photos.
//! It runs in a session of its own: the library is never touched.

use std::time::Instant;

use lightcraft_develop::{DevelopSettings, Mask, MaskComponent, MaskShape};
use lightcraft_geom::Point;
use serde::{Deserialize, Serialize};

use crate::export::ExportOptions;

/// One measured configuration.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TestRun {
    /// Photos side by side.
    pub lanes: usize,
    /// The GPU was used.
    pub gpu: bool,
    /// The processor rendered photos too while the GPU was busy ([`crate::export::export_mix`]).
    pub mix: bool,
    pub photos: usize,
    pub seconds: f64,
    pub photos_per_min: f64,
    /// Photos that waited for memory, and for their turn at the GPU.
    pub memory_waits: u64,
    pub gpu_waits: u64,
    /// The most device memory held at once (bytes).
    pub gpu_peak: u64,
}

/// What the test found and chose. Stored with the install's settings and shown in Settings ▸
/// Performance.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MachineReport {
    /// When it ran (ISO 8601).
    pub tested: String,
    /// What it exported ("4 of your photos (Canon EOS R5 C)", "6 demo photos").
    pub source: String,
    pub cores: usize,
    pub ram_mb: u64,
    /// The graphics card and its memory, when there is one.
    pub gpu: Option<String>,
    pub gpu_mb: u64,
    pub runs: Vec<TestRun>,
    /// The choice: photos side by side, the GPU for exports or not, exports sharing the GPU.
    pub lanes: usize,
    pub use_gpu: bool,
    pub gpu_jobs: usize,
    /// The processor renders photos too while the GPU is busy with others.
    pub mix: bool,
    /// Photos a minute with the choice.
    pub photos_per_min: f64,
    /// What limits it, in plain words.
    pub limit: String,
    /// The report in plain words.
    pub summary: String,
}

/// What to test with.
#[derive(Clone, Debug, Default)]
pub struct TestOptions {
    /// Photos to export (raws of the user's camera); empty: the demo photos.
    pub paths: Vec<String>,
    /// Numbers of photos side by side to try (empty: chosen from the cores).
    pub lanes: Vec<usize>,
    /// A shorter test: fewer configurations, fewer photos.
    pub quick: bool,
    /// Export at this long edge instead of full size (tests).
    pub long_edge: Option<u32>,
    /// Set to stop the test: the export running stops within a second ([`crate::export::run_batch_cancellable`]),
    /// as `progress` returning `false` does between steps.
    pub cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

/// The lanes to try on a computer with `cores` threads: an eighth, a quarter and half of them
/// (at least 1, at most [`crate::export::MAX_EXPORT_LANES`]); a quarter and half when `quick`.
pub fn lane_choices(cores: usize, quick: bool) -> Vec<usize> {
    let c = cores.max(1);
    let mut v: Vec<usize> = if quick { vec![c / 4, c / 2] } else { vec![c / 8, c / 4, c / 2] };
    for l in &mut v {
        *l = (*l).clamp(1, crate::export::MAX_EXPORT_LANES);
    }
    v.sort_unstable();
    v.dedup();
    v
}

/// The fastest run, preferring fewer lanes (less memory) when within 3 % of it.
pub fn best_run(runs: &[TestRun]) -> Option<&TestRun> {
    let top = runs.iter().map(|r| r.photos_per_min).fold(0.0f64, f64::max);
    runs.iter().filter(|r| r.photos_per_min >= top * 0.97).min_by_key(|r| (r.lanes, !r.gpu, r.mix))
}

/// Run the test. `progress(done, total, what)` after each step; `false` stops it (`Err("cancelled")`).
/// Changes nothing in the process: [`apply`] uses the result.
pub fn run_test(o: &TestOptions, progress: &mut dyn FnMut(usize, usize, &str) -> bool) -> Result<MachineReport, String> {
    let cores = std::thread::available_parallelism().map_or(1, |c| c.get());
    // (waits for the GPU device: its creation is not part of the measurements)
    let gpu = crate::gpu::available();
    let lanes = if o.lanes.is_empty() { lane_choices(cores, o.quick) } else { o.lanes.clone() };
    // a session of its own, with the photos to export
    let (mut session, ids, source) = if o.paths.is_empty() {
        let s = crate::Session::with_demo();
        let ids: Vec<_> = s.catalog.photos().map(|p| p.id).take(if o.quick { 4 } else { 6 }).collect();
        let n = ids.len();
        (s, ids, format!("{n} demo photos"))
    } else {
        let mut s = crate::Session::new().with_fs();
        let r = s.execute("library.import", &serde_json::json!({"paths": o.paths})).map_err(|e| e.to_string())?;
        let ids: Vec<_> =
            r["imported"].as_array().map(|a| a.iter().filter_map(|v| v.as_u64()).map(lightcraft_catalog::PhotoId).collect()).unwrap_or_default();
        let camera = ids.first().and_then(|id| s.catalog.photo(*id)).map(|p| camera_name(&p.meta.camera)).unwrap_or_default();
        let n = ids.len();
        (s, ids, if camera.is_empty() { format!("{n} of your photos") } else { format!("{n} of your photos ({camera})") })
    };
    if ids.is_empty() {
        return Err("no photos to test with".into());
    }
    let settings = bench_settings(false, true);
    for &id in &ids {
        session.set_develop(id, settings.clone(), "Test my machine").map_err(|e| e.to_string())?;
    }
    let mut opts = ExportOptions { hdr: true, quality: 90, ..ExportOptions::default() };
    if let Some(edge) = o.long_edge {
        opts.resize = Some(crate::export::Resize::long_edge(edge));
    }
    // (+1: warming up; +2: the processor alone, and the GPU and the processor at once, at the
    // best lanes, when there is a GPU)
    let total = lanes.len() + 1 + 2 * usize::from(gpu);
    let mut step = 0;
    let mut tick = |what: &str| {
        step += 1;
        if progress(step, total, what) { Ok(()) } else { Err("cancelled".to_string()) }
    };
    // (as many photos as lanes, so every lane works: at least 4 or 6)
    let per_run = |l: usize| l.max(if o.quick { 4 } else { 6 });
    // warm up (not counted): the decoder, the caches, the GPU's kernels; and one photo's peak on the GPU
    let warm = export_round(&mut session, o.cancel.as_ref(), &ids[..1], &opts, 1, gpu, false)?;
    let peak = warm.gpu_peak;
    tick("Warming up")?;
    let mut runs = Vec::new();
    for &l in &lanes {
        runs.push(export_round(&mut session, o.cancel.as_ref(), &repeat(&ids, per_run(l)), &opts, l, gpu, false)?);
        tick(&format!("{l} photos side by side"))?;
    }
    if gpu && let Some(best) = best_run(&runs).cloned() {
        runs.push(export_round(&mut session, o.cancel.as_ref(), &repeat(&ids, per_run(best.lanes)), &opts, best.lanes, false, false)?);
        tick("The processor alone")?;
        // photos the GPU is too busy for render on the processor meanwhile (note 28's hybrid)
        runs.push(export_round(&mut session, o.cancel.as_ref(), &repeat(&ids, per_run(best.lanes)), &opts, best.lanes, true, true)?);
        tick("The graphics card and the processor together")?;
    }
    let best = best_run(&runs).cloned().ok_or("no measurement")?;
    let vram = crate::gpu::adapter_memory().unwrap_or(0);
    let mut r = MachineReport {
        tested: crate::import::system_clock(),
        source,
        cores,
        ram_mb: crate::memory::total_ram().unwrap_or(0) as u64 >> 20,
        gpu: gpu.then(crate::gpu::adapter_name).flatten(),
        gpu_mb: vram >> 20,
        lanes: best.lanes,
        use_gpu: best.gpu,
        // exports sharing the GPU: as many as its memory holds at the measured peak of one
        gpu_jobs: gpu_jobs_for(vram, peak),
        mix: best.gpu && best.mix,
        photos_per_min: best.photos_per_min,
        limit: limit_text(&runs, &best),
        runs,
        summary: String::new(),
    };
    r.summary = summary(&r);
    Ok(r)
}

/// Exports sharing a card of `vram` bytes when one export's renders peak at `peak` bytes: what
/// the GPU's default share for exports (55 %) holds, 1 to 4.
pub fn gpu_jobs_for(vram: u64, peak: u64) -> usize {
    if vram == 0 || peak == 0 {
        return 1;
    }
    ((vram / 100 * 55 / peak) as usize).clamp(1, 4)
}

/// A camera's name as people say it: "Canon Canon EOS R5 C" (make and a model that repeats it) is
/// "Canon EOS R5 C".
fn camera_name(s: &str) -> String {
    let words: Vec<&str> = s.split_whitespace().collect();
    match words.as_slice() {
        [a, b, ..] if a.eq_ignore_ascii_case(b) => words.get(1..).unwrap_or(&[]).join(" "),
        _ => words.join(" "),
    }
}

fn repeat(ids: &[lightcraft_catalog::PhotoId], n: usize) -> Vec<lightcraft_catalog::PhotoId> {
    ids.iter().copied().cycle().take(n.max(1)).collect()
}

/// Export `ids` with `lanes` side by side, on the GPU or not; the files are thrown away.
fn export_round(
    session: &mut crate::Session,
    cancel: Option<&std::sync::Arc<std::sync::atomic::AtomicBool>>,
    ids: &[lightcraft_catalog::PhotoId],
    o: &ExportOptions,
    lanes: usize,
    gpu: bool,
    mix: bool,
) -> Result<TestRun, String> {
    for &id in ids {
        session.media.forget(id); // every original is decoded again, as in a real export
    }
    let (mem0, _) = crate::memory::export_gate().waits();
    let gpu0 = crate::gpu::batch_usage().waits;
    crate::gpu::reset_memory_peak();
    let t = Instant::now();
    let files = crate::export::export_batch_tuned(session, ids, o, lanes, gpu, mix, cancel.cloned())?;
    let seconds = t.elapsed().as_secs_f64().max(1e-3);
    if let Some(e) = files.iter().find_map(|f| f.get("error")) {
        return Err(format!("the test export failed: {e}"));
    }
    let (mem1, _) = crate::memory::export_gate().waits();
    Ok(TestRun {
        lanes,
        gpu,
        mix: gpu && mix,
        photos: files.len(),
        seconds,
        photos_per_min: files.len() as f64 * 60.0 / seconds,
        memory_waits: mem1.saturating_sub(mem0),
        gpu_waits: crate::gpu::batch_usage().waits.saturating_sub(gpu0),
        gpu_peak: crate::gpu::memory().peak,
    })
}

/// What limits the best run, in plain words.
fn limit_text(runs: &[TestRun], best: &TestRun) -> String {
    let more = runs.iter().filter(|r| r.gpu == best.gpu && r.lanes > best.lanes).map(|r| r.photos_per_min).fold(0.0f64, f64::max);
    if best.memory_waits * 2 > best.photos as u64 {
        "memory: photos waited for room before they could start".into()
    } else if best.gpu && best.gpu_waits * 2 > best.photos as u64 {
        "the graphics card: photos took turns at it".into()
    } else if more > 0.0 && more < best.photos_per_min * 1.03 {
        "the processor: more photos side by side don't go faster".into()
    } else {
        "the processor".into()
    }
}

/// The report in plain words.
fn summary(r: &MachineReport) -> String {
    let cpu = r.runs.iter().filter(|x| !x.gpu).map(|x| x.photos_per_min).fold(0.0f64, f64::max);
    let gpu_part = match (&r.gpu, r.use_gpu) {
        (Some(g), true) if r.mix => format!(
            "The graphics card ({g}) renders the edits, {} export{} at a time, and the processor renders the photos it has no room for.",
            r.gpu_jobs,
            if r.gpu_jobs == 1 { "" } else { "s" }
        ),
        (Some(g), true) => {
            format!("The graphics card ({g}) renders the edits, {} export{} at a time.", r.gpu_jobs, if r.gpu_jobs == 1 { "" } else { "s" })
        }
        (Some(g), false) => format!(
            "Exports render on the processor: it was as fast as with the graphics card ({g}){}.",
            if cpu > 0.0 { format!(", {cpu:.1} photos a minute") } else { String::new() }
        ),
        (None, _) => "No graphics card is used: exports render on the processor.".into(),
    };
    format!(
        "Exports about {:.0} photos a minute (full-size HDR JPEGs with a mask, from {}), {} photos side by side. {gpu_part} What limits it: {}.",
        r.photos_per_min, r.source, r.lanes, r.limit
    )
}

/// Use what the test chose from now on (lanes, the GPU for exports, exports sharing the GPU).
pub fn apply(r: &MachineReport) {
    crate::export::set_export_lanes(Some(r.lanes).filter(|l| *l > 0));
    crate::export::set_export_gpu(r.use_gpu);
    crate::export::set_export_mix(r.use_gpu && r.mix);
    crate::gpu::set_batch_limits(None, Some(r.gpu_jobs).filter(|j| *j > 0));
}

/// What exports use now: `{lanes, lanesSetting, gpu, gpuJobs, gpuLimitMb, gpuMemoryMb}`.
pub fn tuning() -> serde_json::Value {
    let b = crate::gpu::batch_usage();
    serde_json::json!({
        "lanes": crate::export::export_parallelism(usize::MAX),
        "lanesSetting": crate::export::export_lanes_override(),
        "gpu": crate::export::export_gpu(),
        "mix": crate::export::export_mix(),
        "gpuJobs": b.limit_jobs,
        "gpuLimitMb": b.limit_bytes >> 20,
        "gpuMemoryMb": crate::gpu::adapter_memory().map(|m| m >> 20),
        "gpuRestarts": crate::gpu::recoveries(),
    })
}

pub(crate) fn specs() -> Vec<crate::cmd::CommandSpec> {
    use crate::cmd::{always, bad, cmd};
    use serde_json::Value;
    vec![
        cmd!(query "app.testMachine", "Test My Machine", [], None,
        "{paths?: [file], quick?: bool, apply?: bool = true} — export a few photos (yours, else the demo's) at several numbers side by side, with and without the GPU, and use the fastest; returns the report {summary, lanes, useGpu, gpuJobs, photosPerMin, limit, runs}",
        always, |s, p| {
            const C: &str = "app.testMachine";
            let paths: Vec<String> = match p.get("paths") {
                None | Some(Value::Null) => Vec::new(),
                Some(Value::Array(a)) => a.iter().map(|v| v.as_str().map(str::to_string).ok_or_else(|| bad(C, "paths must be file paths"))).collect::<Result<_, _>>()?,
                Some(_) => return Err(bad(C, "paths must be a list of file paths")),
            };
            let quick = p.get("quick").and_then(Value::as_bool).unwrap_or(false);
            // (an export running in the background would share the test's settings and skew it)
            if s.activity.list().iter().any(|t| t.kind == "export") {
                return Err(bad(C, "wait for the export to finish, then test this computer"));
            }
            let guard = s.activity.start("benchmark", "Testing this computer", crate::activity::Cancel::Yes);
            let cancel = guard.cancel_flag();
            let r = run_test(&TestOptions { paths, quick, cancel: cancel.clone(), ..Default::default() }, &mut |done, total, what| {
                guard.progress(done as u64, total as u64);
                guard.detail(what);
                !cancel.as_ref().is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
            })
            .map_err(|e| bad(C, e))?;
            if p.get("apply").and_then(Value::as_bool).unwrap_or(true) {
                apply(&r);
            }
            Ok(serde_json::to_value(&r).unwrap_or_default())
        }),
        cmd!(query "app.exportTuning", "Export Performance", [], None,
        "{lanes?: number (0 = automatic), gpu?: bool, gpuJobs?: number (0 = automatic)} — photos exported side by side, the GPU for exports, exports sharing the GPU at once; returns what is in use",
        always, |_, p| {
            const C: &str = "app.exportTuning";
            if let Some(v) = p.get("lanes") {
                let n = v.as_u64().ok_or_else(|| bad(C, "lanes must be a number"))?;
                crate::export::set_export_lanes(Some(n as usize).filter(|n| *n > 0));
            }
            if let Some(v) = p.get("gpu") {
                crate::export::set_export_gpu(v.as_bool().ok_or_else(|| bad(C, "gpu must be true or false"))?);
            }
            if let Some(v) = p.get("gpuJobs") {
                let n = v.as_u64().ok_or_else(|| bad(C, "gpuJobs must be a number"))?;
                crate::gpu::set_batch_limits(None, Some(n as usize).filter(|n| *n > 0));
            }
            Ok(tuning())
        }),
    ]
}

/// A wedding-style edit for benchmarks: tone, clarity, noise reduction and sharpening, and unless
/// `plain` a radial mask; HDR when `hdr`.
pub fn bench_settings(plain: bool, hdr: bool) -> DevelopSettings {
    let mut s = DevelopSettings::default();
    s.light.exposure = 0.35;
    s.light.contrast = 12.0;
    s.light.highlights = -45.0;
    s.light.shadows = 35.0;
    s.light.whites = 10.0;
    s.light.blacks = -8.0;
    s.effects.clarity = 8.0;
    s.effects.texture = 5.0;
    s.effects.dehaze = 4.0;
    s.color.vibrance = 10.0;
    s.detail.sharpen_amount = 40.0;
    s.detail.nr_luminance = 15.0;
    s.detail.nr_color = 25.0;
    s.hdr.enabled = hdr;
    if plain {
        return s;
    }
    let mask = |id: u32, shape: MaskShape, f: fn(&mut Mask)| {
        let mut m = Mask {
            id,
            name: format!("mask {id}"),
            visible: true,
            components: vec![MaskComponent { name: None, op: Default::default(), invert: false, shape }],
            ..Default::default()
        };
        f(&mut m);
        m
    };
    s.masks = vec![mask(1, MaskShape::Radial { center: Point::new(0.5, 0.45), rx: 0.35, ry: 0.3, angle: 0.0, feather: 60.0, invert: true }, |m| {
        m.adjust.exposure = -0.3
    })];
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Review of PR #3506: the test doesn't start while this session exports (the export would
    /// share its settings and skew it).
    #[test]
    fn the_test_waits_for_a_running_export() {
        let mut s = crate::Session::with_demo();
        let export = s.activity.start("export", "Exporting", crate::activity::Cancel::No);
        let e = s.execute("app.testMachine", &serde_json::json!({"quick": true, "apply": false})).unwrap_err().to_string();
        assert!(e.contains("wait for the export"), "{e}");
        assert!(s.activity.list().iter().all(|t| t.kind != "benchmark"), "no test row");
        drop(export);
    }

    #[test]
    fn lanes_to_try_follow_the_cores() {
        assert_eq!(lane_choices(32, false), vec![4, 8, 16]);
        assert_eq!(lane_choices(32, true), vec![8, 16]);
        assert_eq!(lane_choices(4, false), vec![1, 2]);
        assert_eq!(lane_choices(1, false), vec![1]);
        assert_eq!(lane_choices(0, true), vec![1]);
        assert!(lane_choices(1024, false).iter().all(|l| *l <= crate::export::MAX_EXPORT_LANES));
    }

    #[test]
    fn camera_names_read_as_people_say_them() {
        assert_eq!(camera_name("Canon Canon EOS R5 C"), "Canon EOS R5 C");
        assert_eq!(camera_name("NIKON D750"), "NIKON D750");
        assert_eq!(camera_name("  Sony   ILCE-7M3 "), "Sony ILCE-7M3");
        assert_eq!(camera_name(""), "");
    }

    #[test]
    fn the_best_run_prefers_fewer_lanes_when_about_as_fast() {
        let run = |lanes, gpu, ppm| TestRun { lanes, gpu, photos_per_min: ppm, ..Default::default() };
        let runs = [run(4, true, 8.0), run(8, true, 11.9), run(16, true, 12.1), run(8, false, 9.0)];
        assert_eq!(best_run(&runs).map(|r| (r.lanes, r.gpu)), Some((8, true)), "8 lanes is within 3 % of 16");
        let runs = [run(8, true, 9.0), run(8, false, 12.0)];
        assert_eq!(best_run(&runs).map(|r| r.gpu), Some(false), "the processor alone was faster");
        assert!(best_run(&[]).is_none());
        // the GPU and the processor together only when clearly faster than the GPU alone
        let mixed = |lanes, mix, ppm| TestRun { lanes, gpu: true, mix, photos_per_min: ppm, ..Default::default() };
        assert!(!best_run(&[mixed(4, false, 20.0), mixed(4, true, 20.3)]).unwrap().mix, "about as fast: the GPU alone");
        assert!(best_run(&[mixed(4, false, 20.0), mixed(4, true, 26.0)]).unwrap().mix);
    }

    #[test]
    fn exports_sharing_the_gpu_follow_its_memory() {
        assert_eq!(gpu_jobs_for(12 << 30, 5 << 30), 1);
        assert_eq!(gpu_jobs_for(24 << 30, 3 << 30), 4);
        assert_eq!(gpu_jobs_for(12 << 30, 1 << 30), 4, "at most 4");
        assert_eq!(gpu_jobs_for(0, 1 << 30), 1);
        assert_eq!(gpu_jobs_for(8 << 30, 0), 1);
    }

    /// The whole test on two demo photos (one lane setting): a report in plain words, the
    /// choice applied only when asked, the settings it changed put back.
    #[test]
    fn a_quick_test_reports_and_restores() {
        let _serial = crate::export::TUNING_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let before = (crate::export::export_lanes_override(), crate::export::export_gpu());
        let mut steps = Vec::new();
        let r = run_test(&TestOptions { lanes: vec![2], quick: true, long_edge: Some(800), ..Default::default() }, &mut |d, t, what| {
            steps.push((d, t, what.to_string()));
            true
        })
        .unwrap();
        assert_eq!((crate::export::export_lanes_override(), crate::export::export_gpu()), before, "nothing changed until applied");
        assert!(r.photos_per_min > 0.0 && r.lanes == 2, "{r:?}");
        assert!(r.source.contains("demo photos"), "{}", r.source);
        assert!(r.summary.starts_with("Exports about") && r.summary.contains("What limits it"), "{}", r.summary);
        assert!(steps.last().is_some_and(|(d, t, _)| d == t), "{steps:?}");
        // stopping it
        let stopped = run_test(&TestOptions { lanes: vec![1], quick: true, long_edge: Some(400), ..Default::default() }, &mut |_, _, _| false);
        assert_eq!(stopped.err().as_deref(), Some("cancelled"));
        assert_eq!((crate::export::export_lanes_override(), crate::export::export_gpu()), before);
    }

    /// Review of PR #3499: the activity row's ✕ stops the export running inside a step, not only
    /// between steps (`progress` says go on throughout here).
    #[test]
    fn a_cancel_flag_stops_the_test_inside_a_step() {
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let mut steps = 0;
        let r =
            run_test(&TestOptions { lanes: vec![2], quick: true, long_edge: Some(400), cancel: Some(flag), ..Default::default() }, &mut |_, _, _| {
                steps += 1;
                true
            });
        assert_eq!(r.err().as_deref(), Some("cancelled"));
        assert_eq!(steps, 0, "stopped in the first export, before any step ended");
    }
}
