//! `lightcraft-cli bench-cull FOLDER`: time what culling a shoot costs, headless, on real files.
//!
//! What a wedding photographer does most is open a folder of a few thousand raws, look at each
//! one, rate it and move on. This times the engine side of each step on the files themselves (the
//! desktop app's own frame timings come from `ui.inspect` → `perf.loupeSteps`, see
//! `tools/cull-bench.py`):
//!
//! - browse: reading the folder into the Local view (`library.browse`, the whole folder);
//! - grid thumbnails: the camera JPEG at 512 px for the first screen, on as many threads as the
//!   app renders with;
//! - camera preview at loupe sizes (1280, 2560) and at full size (the 100 % view), one at a time;
//! - LightCraft's render of the loupe (2560 px) from a cold source;
//! - rating: `photo.rate` on many photos in a row, journal included;
//! - import: adding N files in place to a fresh library, and copying M of them into it (`--n 0`:
//!   no import).
//!
//! `--files K` reads only the first K files (by name) of the folder, as browsing reads them: a
//! sample of a folder on a network share, where reading thousands of raws takes hours.
//!
//! Each number is printed as `name: X ms wall` (median, with p95 and n), the format
//! `cargo xtask bench` parses, and the lot as one JSON object with `--json FILE`. Nothing is
//! written next to the photos: every library is made in a new folder of the run's own under
//! `--work` (default: a folder in the system temp directory; nothing already there is deleted),
//! XMP auto-write stays off, and the source folder is only read.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use lightcraft_engine::Session;
use lightcraft_engine::catalog::PhotoId;
use serde_json::{Value, json};

const USAGE: &str = "lightcraft-cli bench-cull FOLDER [--files K] [--n N] [--copy M] [--work DIR] [--json FILE]";

/// Threads the app renders thumbnails with (`Renderer::default`: cores − 1, at most 6).
fn render_threads() -> usize {
    std::thread::available_parallelism().map(|n| n.get().saturating_sub(1)).unwrap_or(2).clamp(2, 6)
}

/// Median, p95 and count of `ms` (milliseconds).
fn stats(ms: &[f64]) -> Value {
    if ms.is_empty() {
        return Value::Null;
    }
    let mut v = ms.to_vec();
    v.sort_by(f64::total_cmp);
    let at = |q: f64| v.get(((v.len() - 1) as f64 * q).round() as usize).copied().unwrap_or(0.0);
    json!({"p50": round(at(0.5)), "p95": round(at(0.95)), "max": round(at(1.0)), "n": v.len()})
}

fn round(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

fn ms_since(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

/// Print one result line the way `xtask bench` parses it (`name: X ms wall`).
fn line(name: &str, s: &Value) {
    if let Some(p50) = s.get("p50").and_then(Value::as_f64) {
        println!("{name}: {p50:.1} ms wall  (p95 {} ms, n {})", s["p95"], s["n"]);
    }
}

/// A new, empty folder `name` in this run's own folder; never deletes anything already there.
fn fresh_dir(root: &Path, name: &str) -> Result<PathBuf, String> {
    let d = root.join(name);
    std::fs::create_dir(&d).map_err(|e| format!("{}: {e}", d.display()))?;
    Ok(d)
}

/// A folder of this run's own under `work`, which must not exist yet: a run never deletes or
/// reuses what is already under `--work`.
fn run_dir(work: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(work).map_err(|e| format!("{}: {e}", work.display()))?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    static RUNS: AtomicUsize = AtomicUsize::new(0);
    let n = RUNS.fetch_add(1, Ordering::Relaxed);
    let d = work.join(format!("bench-cull-run-{}-{stamp}-{n}", std::process::id()));
    std::fs::create_dir(&d).map_err(|e| format!("{}: {e}", d.display()))?;
    Ok(d)
}

fn library(dir: &Path) -> Result<Session, String> {
    let mut s = Session::new().with_fs().with_system_clock();
    s.open_library(dir, false).map_err(|e| format!("{}: {e}", dir.display()))?;
    // nothing is ever written next to the photos
    s.execute("library.xmpPreferences", &json!({"autoWrite": false})).map_err(|e| e.to_string())?;
    Ok(s)
}

pub fn run(args: &[String]) -> Result<(), String> {
    let mut folder = None;
    let mut n = 2000usize;
    let mut copy = 200usize;
    let mut work = None;
    let mut json_out = None;
    let mut files = None;
    let mut i = 0;
    while i < args.len() {
        let value = |i: usize| args.get(i + 1).cloned().ok_or_else(|| format!("{} needs a value\n{USAGE}", args[i]));
        match args[i].as_str() {
            "--n" => {
                n = value(i)?.parse().map_err(|_| "--n needs a number".to_string())?;
                i += 1;
            }
            "--copy" => {
                copy = value(i)?.parse().map_err(|_| "--copy needs a number".to_string())?;
                i += 1;
            }
            "--files" => {
                files = Some(value(i)?.parse::<usize>().map_err(|_| "--files needs a number".to_string())?);
                i += 1;
            }
            "--work" => {
                work = Some(PathBuf::from(value(i)?));
                i += 1;
            }
            "--json" => {
                json_out = Some(PathBuf::from(value(i)?));
                i += 1;
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            a if a.starts_with("--") => return Err(format!("unknown option {a}\n{USAGE}")),
            a => folder = Some(PathBuf::from(a)),
        }
        i += 1;
    }
    let folder = folder.ok_or_else(|| USAGE.to_string())?;
    if !folder.is_dir() {
        return Err(format!("{}: not a folder", folder.display()));
    }
    let work = work.unwrap_or_else(|| std::env::temp_dir().join(format!("lightcraft-bench-cull-{}", std::process::id())));
    let work = run_dir(&work)?;
    let mut out = json!({"folder": folder.display().to_string(), "threads": render_threads()});

    // --- browse: the whole folder into the Local view -------------------------------------------
    let mut s = library(&fresh_dir(&work, "lib-browse")?)?;
    // `--files K`: only the first K files by name, read as browsing reads them (a sample of a slow
    // network folder)
    let t = Instant::now();
    let (photos, ids): (u64, Vec<PhotoId>) = match files {
        None => {
            let r = s.execute("library.browse", &json!({"path": folder.display().to_string()})).map_err(|e| e.to_string())?;
            (r["photos"].as_u64().unwrap_or(0), s.visible_cloned())
        }
        Some(k) => {
            let mut list: Vec<String> = std::fs::read_dir(&folder)
                .map_err(|e| format!("{}: {e}", folder.display()))?
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file() && lightcraft_engine::import::is_supported(p))
                .map(|p| p.display().to_string())
                .collect();
            list.sort();
            list.truncate(k);
            let r = s.execute("library.import", &json!({"paths": list, "mode": "add", "local": true})).map_err(|e| e.to_string())?;
            let ids: Vec<PhotoId> = r["imported"].as_array().into_iter().flatten().filter_map(Value::as_u64).map(PhotoId).collect();
            (ids.len() as u64, ids)
        }
    };
    let browse_ms = ms_since(t);
    let what = if files.is_some() { "first files" } else { "whole folder" };
    println!("browse {photos} photos ({what}): {browse_ms:.1} ms wall");
    out["browse"] = json!({"ms": round(browse_ms), "photos": photos, "perPhotoMs": round(browse_ms / photos.max(1) as f64)});
    let paths: Vec<String> = ids
        .iter()
        .filter_map(|id| match &s.catalog.photo(*id)?.source {
            lightcraft_engine::catalog::Source::File { path } => Some(path.clone()),
            _ => None,
        })
        .collect();
    if paths.is_empty() {
        return Err(format!("{}: no photos", folder.display()));
    }
    let loader = s.media.preview_loader.clone().ok_or("no preview loader")?;

    // --- grid thumbnails: the first screen, in parallel ------------------------------------------
    let screen: Vec<String> = paths.iter().take(48).cloned().collect();
    let next = Arc::new(AtomicUsize::new(0));
    let t = Instant::now();
    let per: Vec<(f64, f64)> = std::thread::scope(|sc| {
        let workers: Vec<_> = (0..render_threads())
            .map(|_| {
                let (next, loader, screen) = (next.clone(), loader.clone(), &screen);
                sc.spawn(move || {
                    let mut done = Vec::new();
                    while let Some(p) = screen.get(next.fetch_add(1, Ordering::Relaxed)) {
                        let a = Instant::now();
                        let _ = loader(p, 512);
                        done.push((ms_since(a), ms_since(t)));
                    }
                    done
                })
            })
            .collect();
        workers.into_iter().flat_map(|w| w.join().unwrap_or_default()).collect()
    });
    let first = per.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
    let all = per.iter().map(|p| p.1).fold(0.0, f64::max);
    let each = stats(&per.iter().map(|p| p.0).collect::<Vec<_>>());
    println!("grid: first thumbnail: {first:.1} ms wall");
    println!("grid: {} thumbnails: {all:.1} ms wall", per.len());
    line("grid: one 512 px thumbnail", &each);
    out["thumbs"] = json!({"firstMs": round(first), "allMs": round(all), "count": per.len(), "each": each});

    // --- camera previews at loupe sizes, one at a time (what a step costs without look-ahead) -----
    let sample: Vec<&String> = paths.iter().skip(48).take(20).collect();
    for edge in [1280usize, 2560, usize::MAX] {
        let ms: Vec<f64> = sample
            .iter()
            .map(|p| {
                let a = Instant::now();
                let _ = loader(p, edge);
                ms_since(a)
            })
            .collect();
        let name = if edge == usize::MAX { "camera preview full size (100 %)".to_string() } else { format!("camera preview {edge} px") };
        let st = stats(&ms);
        line(&name, &st);
        out[if edge == usize::MAX { "cameraFull".to_string() } else { format!("camera{edge}") }] = st;
    }

    // --- LightCraft's loupe render (2560 px) from a cold source --------------------------------
    let ms: Vec<f64> = ids
        .iter()
        .skip(80)
        .take(5)
        .filter_map(|id| {
            let job = s.loupe_job(*id, 2560, 1708, true)?;
            let a = Instant::now();
            let r = job.run();
            let t = ms_since(a);
            r.rendered.is_ok().then_some(t)
        })
        .collect();
    let st = stats(&ms);
    line("loupe render 2560 px (cold)", &st);
    out["loupeRender"] = st;

    // --- rating: a culling pass of keypresses ---------------------------------------------------
    let ms: Vec<f64> = ids
        .iter()
        .take(50)
        .enumerate()
        .filter_map(|(k, id)| {
            // the key acts on the selected photo, as in the loupe
            s.execute("library.select", &json!({"ids": [id.0], "active": id.0})).ok()?;
            let a = Instant::now();
            s.execute("photo.rate", &json!({"rating": (k % 5) + 1})).ok()?;
            Some(ms_since(a))
        })
        .collect();
    let st = stats(&ms);
    line("rate one photo (journal written)", &st);
    out["rate"] = st;
    drop(s);

    // --- import: add in place, then copy ----------------------------------------------------------
    if n == 0 {
        return finish(out, json_out);
    }
    let add: Vec<String> = paths.iter().take(n).cloned().collect();
    let mut s = library(&fresh_dir(&work, "lib-add")?)?;
    let t = Instant::now();
    let r = s.execute("library.import", &json!({"paths": add, "mode": "add"})).map_err(|e| e.to_string())?;
    let add_ms = ms_since(t);
    let added = r["imported"].as_array().map(Vec::len).unwrap_or(0);
    println!("import {added} photos in place: {add_ms:.1} ms wall");
    out["importAdd"] = json!({"ms": round(add_ms), "photos": added, "perPhotoMs": round(add_ms / added.max(1) as f64)});
    drop(s);
    if copy > 0 {
        let some: Vec<String> = paths.iter().take(copy).cloned().collect();
        let lib = fresh_dir(&work, "lib-copy")?;
        let mut s = library(&lib)?;
        let t = Instant::now();
        let r = s
            .execute("library.import", &json!({"paths": some, "mode": "copy", "destination": lib.join("Originals").display().to_string()}))
            .map_err(|e| e.to_string())?;
        let copy_ms = ms_since(t);
        let copied = r["imported"].as_array().map(Vec::len).unwrap_or(0);
        println!("import {copied} photos by copying: {copy_ms:.1} ms wall");
        out["importCopy"] = json!({"ms": round(copy_ms), "photos": copied, "perPhotoMs": round(copy_ms / copied.max(1) as f64)});
        drop(s);
        let _ = std::fs::remove_dir_all(&lib);
    }
    finish(out, json_out)
}

fn finish(out: Value, json_out: Option<PathBuf>) -> Result<(), String> {
    if let Some(p) = json_out {
        std::fs::write(&p, serde_json::to_string_pretty(&out).unwrap_or_default()).map_err(|e| format!("{}: {e}", p.display()))?;
    }
    println!("{}", serde_json::to_string(&out).unwrap_or_default());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `--work` may hold anything: a run makes a new folder of its own there and never deletes
    /// what is already in it (it used to remove `lib-browse`, `lib-add` and `lib-copy` there).
    #[test]
    fn a_run_never_deletes_what_is_under_work() {
        // a folder this test owns (made new), so the cleanup below removes nothing else
        let work = run_dir(&std::env::temp_dir()).unwrap();
        let keep = work.join("lib-browse").join("keep.txt");
        std::fs::create_dir_all(keep.parent().unwrap()).unwrap();
        std::fs::write(&keep, b"mine").unwrap();
        let a = run_dir(&work).unwrap();
        let b = run_dir(&work).unwrap();
        assert_ne!(a, b);
        fresh_dir(&a, "lib-browse").unwrap();
        assert!(fresh_dir(&a, "lib-browse").is_err(), "an existing folder is never reused");
        assert_eq!(std::fs::read(&keep).unwrap(), b"mine");
        let _ = std::fs::remove_dir_all(&work);
    }
}
