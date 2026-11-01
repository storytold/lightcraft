//! `lightcraft-cli bench-cull FOLDER`: time what culling a shoot costs, headless, on real files.
//!
//! What a wedding photographer does most is open a folder of a few thousand raws, look at each
//! one, rate it and move on. This times the engine side of each step on the files themselves (the
//! desktop app's own frame timings come from `ui.inspect` â†’ `perf.loupeSteps`, see
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

/// Threads the app renders thumbnails with (`Renderer::default`: cores âˆ’ 1, at most 6).
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

/// [`stats`] of the previews that loaded, with the count of those that didn't (`failed`): a file
/// without a usable camera preview is not timed as one.
fn with_failed(ms: &[f64], failed: usize) -> Value {
    match stats(ms) {
        Value::Object(mut m) => {
            m.insert("failed".into(), json!(failed));
            Value::Object(m)
        }
        _ => json!({"n": 0, "failed": failed}),
    }
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

/// `take` items of `v` after the first `skip` (photos the stages before have read, so cold in the
/// file cache), or, when `v` has no more than `skip`, its first `take`: a small folder is measured
/// on photos already read rather than not at all.
fn sample_after<T>(v: &[T], skip: usize, take: usize) -> &[T] {
    let from = if v.len() > skip { skip } else { 0 };
    let rest = v.get(from..).unwrap_or_default();
    rest.get(..take.min(rest.len())).unwrap_or_default()
}

/// Whether `path` names a file in `folder` (the source folder, which a run only reads), checked
/// twice: where its own entry sits (the folder holding it, the final link not followed: writing the
/// results replaces that entry, so a link to a file elsewhere that sits in the source folder, maybe
/// standing for a photo, would be replaced; CodeRabbit on #3080), and where it resolves to: a file
/// that is there by itself (a link to a photo there is the photo), a link that points nowhere yet
/// by where writing through it would create the file, and anything else by its nearest folder that
/// exists (a `--work` folder may not be there yet).
fn inside_source(path: &Path, folder: &Path) -> bool {
    let Ok(folder) = std::fs::canonicalize(folder) else { return false };
    entry_inside(path, &folder) || resolves_inside(path, &folder, 0)
}

/// Whether the folder holding `path`'s own entry (its nearest folder that exists, resolved) is in
/// `folder` (already resolved). The entry itself is not followed.
fn entry_inside(path: &Path, folder: &Path) -> bool {
    let mut p = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    loop {
        if let Ok(c) = std::fs::canonicalize(p) {
            return c.starts_with(folder);
        }
        match p.parent() {
            Some(up) if !up.as_os_str().is_empty() => p = up,
            Some(_) => p = Path::new("."),
            None => return false,
        }
    }
}

/// [`inside_source`] for a `folder` already resolved; `hops`: links followed so far (a loop of
/// links counts as inside: refused).
fn resolves_inside(path: &Path, folder: &Path, hops: u32) -> bool {
    if let Ok(c) = std::fs::canonicalize(path) {
        return c.starts_with(folder);
    }
    if let Ok(target) = std::fs::read_link(path) {
        if hops >= 40 {
            return true;
        }
        let target = path.parent().map_or_else(|| target.clone(), |dir| dir.join(&target));
        return resolves_inside(&target, folder, hops + 1);
    }
    entry_inside(path, folder)
}

/// Refuse `--json` or `--work` in the folder being measured: nothing is written next to the photos,
/// the results and the run's libraries included. Checked before anything is made.
fn check_outputs(folder: &Path, json: Option<&Path>, work: &Path) -> Result<(), String> {
    if let Some(j) = json.filter(|j| inside_source(j, folder)) {
        return Err(format!("{}: --json must not be in the folder being measured", j.display()));
    }
    if inside_source(&work.join("run"), folder) {
        return Err(format!("{}: --work must not be in the folder being measured", work.display()));
    }
    Ok(())
}

/// The first `k` supported files of `folder` by name (`--files K`), hidden ones left out as browsing lists the folder.
fn first_files(folder: &Path, k: usize) -> Result<Vec<String>, String> {
    let mut list: Vec<String> = std::fs::read_dir(folder)
        .map_err(|e| format!("{}: {e}", folder.display()))?
        .flatten()
        .filter(|e| !lightcraft_engine::walk::is_hidden_entry(e))
        .map(|e| e.path())
        .filter(|p| p.is_file() && lightcraft_engine::import::is_supported(p))
        .map(|p| p.display().to_string())
        .collect();
    list.sort();
    list.truncate(k);
    Ok(list)
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
    check_outputs(&folder, json_out.as_deref(), &work)?;
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
            let list = first_files(&folder, k)?;
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
    // (`None`: no camera preview loaded, counted apart)
    let per: Vec<Option<(f64, f64)>> = std::thread::scope(|sc| {
        let workers: Vec<_> = (0..render_threads())
            .map(|_| {
                let (next, loader, screen) = (next.clone(), loader.clone(), &screen);
                sc.spawn(move || {
                    let mut done = Vec::new();
                    while let Some(p) = screen.get(next.fetch_add(1, Ordering::Relaxed)) {
                        let a = Instant::now();
                        let loaded = loader(p, 512).is_some();
                        done.push(loaded.then(|| (ms_since(a), ms_since(t))));
                    }
                    done
                })
            })
            .collect();
        workers.into_iter().flat_map(|w| w.join().unwrap_or_default()).collect()
    });
    let failed = per.iter().filter(|p| p.is_none()).count();
    let per: Vec<(f64, f64)> = per.into_iter().flatten().collect();
    let first = per.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
    let all = per.iter().map(|p| p.1).fold(0.0, f64::max);
    let each = stats(&per.iter().map(|p| p.0).collect::<Vec<_>>());
    if !per.is_empty() {
        println!("grid: first thumbnail: {first:.1} ms wall");
        println!("grid: {} thumbnails: {all:.1} ms wall", per.len());
    }
    if failed > 0 {
        println!("grid: {failed} without a camera preview (not timed)");
    }
    line("grid: one 512 px thumbnail", &each);
    // (no thumbnail loaded: no first or last time either)
    let (first, all) = if per.is_empty() { (Value::Null, Value::Null) } else { (json!(round(first)), json!(round(all))) };
    out["thumbs"] = json!({"firstMs": first, "allMs": all, "count": per.len(), "failed": failed, "each": each});

    // --- camera previews at loupe sizes, one at a time (what a step costs without look-ahead) -----
    let sample = sample_after(&paths, 48, 20);
    for edge in [1280usize, 2560, usize::MAX] {
        let timed: Vec<Option<f64>> = sample
            .iter()
            .map(|p| {
                let a = Instant::now();
                let loaded = loader(p, edge).is_some();
                loaded.then(|| ms_since(a))
            })
            .collect();
        let ms: Vec<f64> = timed.iter().flatten().copied().collect();
        let name = if edge == usize::MAX { "camera preview full size (100 %)".to_string() } else { format!("camera preview {edge} px") };
        let st = with_failed(&ms, timed.len().saturating_sub(ms.len()));
        line(&name, &st);
        out[if edge == usize::MAX { "cameraFull".to_string() } else { format!("camera{edge}") }] = st;
    }

    // --- LightCraft's loupe render (2560 px) from a cold source --------------------------------
    let ms: Vec<f64> = sample_after(&ids, 80, 5)
        .iter()
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
        write_replacing(&p, serde_json::to_string_pretty(&out).unwrap_or_default().as_bytes())?;
    }
    println!("{}", serde_json::to_string(&out).unwrap_or_default());
    Ok(())
}

/// Write `bytes` to `path` through a new file next to it, renamed over `path`: an existing `path`
/// is replaced as a name, never written into. A hard link there shares its data with another file
/// (maybe a photo in the source folder, which [`inside_source`] can't tell), and writing into it
/// would overwrite that file too (CodeRabbit on #3077). The temporary file is removed on error.
fn write_replacing(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let err = |e: std::io::Error| format!("{}: {e}", path.display());
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = path.file_name().ok_or_else(|| format!("{}: not a file name", path.display()))?;
    static TEMPS: AtomicUsize = AtomicUsize::new(0);
    let n = TEMPS.fetch_add(1, Ordering::Relaxed);
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(name);
    tmp_name.push(format!(".{}-{n}.tmp", std::process::id()));
    let tmp = dir.join(tmp_name);
    let written =
        std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp).and_then(|mut f| f.write_all(bytes).and_then(|()| f.sync_all()));
    if let Err(e) = written.and_then(|()| std::fs::rename(&tmp, path)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(err(e));
    }
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

    /// A folder too small for the disjoint samples is measured on the photos it has (CodeRabbit on #3077).
    #[test]
    fn a_small_folder_is_sampled_from_its_start() {
        let v: Vec<u32> = (0..100).collect();
        assert_eq!(sample_after(&v, 48, 20), &v[48..68]);
        assert_eq!(sample_after(&v, 90, 20), &v[90..100]);
        assert_eq!(sample_after(&v[..3], 48, 20), &v[..3]);
        assert_eq!(sample_after(&v[..48], 48, 5), &v[..5]);
        assert!(sample_after::<u32>(&[], 48, 20).is_empty());
    }

    /// `--files K` leaves hidden files out, as browsing does (CodeRabbit on #3080).
    #[test]
    fn a_sample_of_the_folder_leaves_hidden_files_out() {
        let work = run_dir(&std::env::temp_dir()).unwrap();
        for name in [".hidden.jpg", "a.jpg", "b.jpg", "notes.txt"] {
            std::fs::write(work.join(name), b"x").unwrap();
        }
        let names: Vec<String> =
            first_files(&work, 2).unwrap().iter().map(|p| Path::new(p).file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["a.jpg", "b.jpg"]);
        let _ = std::fs::remove_dir_all(&work);
    }

    /// Previews that didn't load are counted apart, not timed (CodeRabbit on #3080).
    #[test]
    fn previews_that_did_not_load_are_counted_not_timed() {
        let v = with_failed(&[10.0, 20.0], 1);
        assert_eq!((v["n"].as_u64(), v["failed"].as_u64()), (Some(2), Some(1)));
        let none = with_failed(&[], 3);
        assert_eq!((none["n"].as_u64(), none["failed"].as_u64()), (Some(0), Some(3)));
        assert!(none.get("p50").is_none(), "nothing timed: no median");
    }

    /// A file link at `at` to `target`.
    #[cfg(unix)]
    fn link(target: &Path, at: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(target, at)
    }

    /// A file link at `at` to `target` (Windows: needs Developer Mode or the right to make links).
    #[cfg(windows)]
    fn link(target: &Path, at: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_file(target, at)
    }

    #[cfg(not(any(unix, windows)))]
    fn link(_: &Path, _: &Path) -> std::io::Result<()> {
        Err(std::io::Error::other("no file links here"))
    }

    /// `--json` naming a link (made elsewhere) to a file in the source folder is refused: writing
    /// the results would follow it and overwrite a photo (CodeRabbit on #3080). So is a link that
    /// points to a file not made yet in the source folder, and a loop of links.
    #[test]
    fn a_json_link_into_the_source_folder_is_refused() {
        let work = run_dir(&std::env::temp_dir()).unwrap();
        let src = fresh_dir(&work, "src").unwrap();
        let photo = src.join("a.jpg");
        std::fs::write(&photo, b"photo").unwrap();
        let to_photo = work.join("report.json");
        if let Err(e) = link(&photo, &to_photo) {
            let _ = std::fs::remove_dir_all(&work);
            lightcraft_testkit::skip!(System, "can't make a file link here ({e}); Windows needs Developer Mode");
            return;
        }
        assert!(inside_source(&to_photo, &src), "a link to a photo in the source folder");
        let e = check_outputs(&src, Some(&to_photo), &work.join("elsewhere")).unwrap_err();
        assert!(e.contains("--json"), "{e}");
        let to_new = work.join("new.json");
        link(&src.join("not-yet.json"), &to_new).unwrap();
        assert!(inside_source(&to_new, &src), "a link to a file not made yet in the source folder");
        let (x, y) = (work.join("x.json"), work.join("y.json"));
        link(&y, &x).unwrap();
        link(&x, &y).unwrap();
        assert!(inside_source(&x, &src), "a loop of links");
        let to_elsewhere = work.join("fine.json");
        link(&work.join("out.json"), &to_elsewhere).unwrap();
        assert!(!inside_source(&to_elsewhere, &src), "a link to a file outside is fine");
        assert_eq!(std::fs::read(&photo).unwrap(), b"photo");
        let _ = std::fs::remove_dir_all(&work);
    }

    /// `--json` naming a link that sits in the source folder and points to a file outside it is
    /// refused: writing the results would replace the link itself, an entry of the source folder
    /// (CodeRabbit on #3080). The link stays as it was.
    #[test]
    fn a_json_link_in_the_source_folder_to_a_file_outside_is_refused() {
        let work = run_dir(&std::env::temp_dir()).unwrap();
        let src = fresh_dir(&work, "src").unwrap();
        let outside = work.join("out.json");
        std::fs::write(&outside, b"outside").unwrap();
        let in_src = src.join("a.jpg");
        if let Err(e) = link(&outside, &in_src) {
            let _ = std::fs::remove_dir_all(&work);
            lightcraft_testkit::skip!(System, "can't make a file link here ({e}); Windows needs Developer Mode");
            return;
        }
        assert!(inside_source(&in_src, &src), "a link in the source folder, wherever it points");
        let e = check_outputs(&src, Some(&in_src), &work.join("elsewhere")).unwrap_err();
        assert!(e.contains("--json"), "{e}");
        assert!(std::fs::symlink_metadata(&in_src).unwrap().file_type().is_symlink(), "the link stays");
        assert_eq!(std::fs::read_link(&in_src).unwrap(), outside);
        assert_eq!(std::fs::read(&outside).unwrap(), b"outside");
        // a new file in the source folder (nothing there yet) is refused too
        assert!(check_outputs(&src, Some(&src.join("new.json")), &work.join("elsewhere")).is_err());
        let _ = std::fs::remove_dir_all(&work);
    }

    /// `--json` naming a hard link (made elsewhere) to a photo in the source folder passes
    /// [`inside_source`] (a hard link has no target to resolve), so the results replace the link
    /// instead of writing into it: the photo keeps its bytes (CodeRabbit on #3077).
    #[test]
    fn a_json_hard_link_to_a_photo_leaves_the_photo_alone() {
        let work = run_dir(&std::env::temp_dir()).unwrap();
        let src = fresh_dir(&work, "src").unwrap();
        let photo = src.join("a.jpg");
        std::fs::write(&photo, b"photo").unwrap();
        let report = work.join("report.json");
        if let Err(e) = std::fs::hard_link(&photo, &report) {
            let _ = std::fs::remove_dir_all(&work);
            lightcraft_testkit::skip!(System, "can't make a hard link here ({e})");
            return;
        }
        finish(json!({"browse": 1}), Some(report.clone())).unwrap();
        assert_eq!(std::fs::read(&photo).unwrap(), b"photo", "the photo is unchanged");
        let written: Value = serde_json::from_slice(&std::fs::read(&report).unwrap()).unwrap();
        assert_eq!(written["browse"], 1);
        let left: Vec<_> = std::fs::read_dir(&work).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(left.len(), 2, "no temporary file is left: {left:?}");
        // a folder that isn't there: an error, nothing panics
        assert!(write_replacing(&work.join("missing").join("r.json"), b"x").is_err());
        let _ = std::fs::remove_dir_all(&work);
    }

    /// `--json` naming a file in the source folder is refused (CodeRabbit on #3077); elsewhere it is fine.
    #[test]
    fn the_results_never_go_in_the_source_folder() {
        let work = run_dir(&std::env::temp_dir()).unwrap();
        let src = fresh_dir(&work, "src").unwrap();
        let sub = fresh_dir(&src, "sub").unwrap();
        assert!(inside_source(&src.join("a.png"), &src));
        assert!(inside_source(&sub.join("x.json"), &src));
        assert!(inside_source(&src.join("sub").join("..").join("a.png"), &src));
        assert!(!inside_source(&work.join("out.json"), &src));
        let _ = std::fs::remove_dir_all(&work);
    }

    /// `--work` in the source folder is refused before anything is made there, also when it doesn't
    /// exist yet (CodeRabbit on #3077); elsewhere it is fine.
    #[test]
    fn the_work_folder_never_goes_in_the_source_folder() {
        let work = run_dir(&std::env::temp_dir()).unwrap();
        let src = fresh_dir(&work, "src").unwrap();
        let inside = src.join("out").join("deeper");
        let e = check_outputs(&src, None, &inside).unwrap_err();
        assert!(e.contains("--work"), "{e}");
        assert!(check_outputs(&src, None, &src).is_err(), "the source folder itself");
        assert!(!src.join("out").exists(), "nothing was made in the source folder");
        assert!(check_outputs(&src, None, &work.join("elsewhere")).is_ok());
        assert!(check_outputs(&src, Some(&src.join("r.json")), &work.join("elsewhere")).is_err());
        let _ = std::fs::remove_dir_all(&work);
    }
}
