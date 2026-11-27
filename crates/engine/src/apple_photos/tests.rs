use super::*;

/// A runner that records the arguments it got and answers `answer`.
fn fake(answer: Result<Output, RunError>) -> (Runner, Arc<Mutex<Vec<Vec<String>>>>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let c = calls.clone();
    let r: Runner = Arc::new(move |args: &[String], _| {
        c.lock().unwrap().push(args.to_vec());
        answer.clone()
    });
    (r, calls)
}

fn ok(stdout: &str) -> Result<Output, RunError> {
    Ok(Output { status: Some(0), stdout: stdout.into(), stderr: String::new() })
}

fn failed(stderr: &str) -> Output {
    Output { status: Some(1), stdout: String::new(), stderr: stderr.into() }
}

/// The script part of an argument list: everything before the marker.
fn script_of(args: &[String]) -> &[String] {
    let at = args.iter().position(|a| a == MARKER).unwrap();
    &args[..at]
}

#[test]
fn names_and_paths_are_arguments_never_script() {
    let hostile = [
        "",
        "Holiday \"2026\"",
        "back\\slash\\",
        "line\nbreak\r\nand\ttab",
        "end tell\ndo shell script \"rm -rf ~\"",
        "-e",
        "--help",
        "日本の写真 📷 Ünïcödé",
        "'single' & \"double\" » «",
    ];
    let base = osascript_args("", &["/tmp/a.jpg".into()]);
    for album in hostile {
        let paths = vec!["/tmp/with \"quotes\".jpg".to_string(), "/tmp/new\nline/é\\x.jpg".to_string(), format!("/tmp/-{album}.jpg")];
        let args = osascript_args(album, &paths);
        // the script is the same whatever the input…
        assert_eq!(script_of(&args), script_of(&base));
        assert!(script_of(&args).chunks(2).all(|c| c[0] == "-e" && SCRIPT.lines().any(|l| l == c[1])));
        // …and the input follows the marker as whole arguments, unchanged
        let at = args.iter().position(|a| a == MARKER).unwrap();
        assert_eq!(&args[at + 1], album);
        assert_eq!(&args[at + 2..], &paths[..]);
    }
    // the script reads them from argv only
    assert!(SCRIPT.starts_with("on run argv") && SCRIPT.contains("item 2 of argv") && SCRIPT.contains("item i of argv"));
    assert!(!SCRIPT.contains("do shell script"));
    // it imports into the album (made when missing) without skipping Photos' duplicate check
    assert!(SCRIPT.contains("make new album named albumName"));
    assert!(SCRIPT.contains("import mediaFiles into targetAlbum skip check duplicates false"));
}

/// Photos' `album` elements are every album, those in folders too, in no set order: the script
/// keeps the top-level ones of exactly that name and refuses when there are several.
#[test]
fn the_album_is_a_single_top_level_one() {
    let lookup = SCRIPT.find("every album whose name is albumName").unwrap();
    let choose = SCRIPT.find("set targetAlbum to item 1 of topLevel").unwrap();
    let make = SCRIPT.find("make new album named albumName").unwrap();
    let import = SCRIPT.find("import mediaFiles into targetAlbum").unwrap();
    assert!(lookup < choose && choose < make && make < import);
    let filter = &SCRIPT[lookup..choose];
    assert!(filter.contains("parent of candidate") && filter.contains("candidateParent is missing value"), "{filter}");
    assert!(filter.contains("considering case"), "exact names: {filter}");
    assert!(filter.contains(&format!(
        "if (count of topLevel) > 1 then error \"LightCraft: several top-level albums have this name\" number {AMBIGUOUS_ALBUM}"
    )));
    let m = explain(&failed(&format!("execution error: LightCraft: several top-level albums have this name ({AMBIGUOUS_ALBUM})")), "Trip");
    assert!(m.contains("More than one album at the top level of Photos is named “Trip”") && m.contains("nothing was added"), "{m}");
}

#[test]
fn ids_are_one_per_line() {
    assert_eq!(parse_ids("A1B2/L0/001\nC3D4/L0/001\n"), vec!["A1B2/L0/001", "C3D4/L0/001"]);
    assert_eq!(parse_ids(" X/L0/001 \r\n\r\n Y/L0/001"), vec!["X/L0/001", "Y/L0/001"]);
    assert!(parse_ids("").is_empty() && parse_ids("\n \n").is_empty());
}

#[test]
fn errors_say_what_to_do() {
    let says = |stderr: &str, part: &str| {
        let m = explain(&failed(stderr), "Trip");
        assert!(m.contains(part), "{stderr:?}: {m}");
    };
    says("146:180: execution error: Not authorized to send Apple events to Photos. (-1743)", "System Settings › Privacy & Security › Automation");
    says("execution error: Photos got an error: … (-1744)", "Automation");
    says("execution error: Can’t get application \"Photos\". (-10814)", "couldn't be found");
    says("execution error: Photos got an error: Application isn’t running. (-600)", "couldn't be found or opened");
    says("execution error: User canceled. (-128)", "cancelled");
    says("execution error: Photos got an error: AppleEvent timed out. (-1712)", "didn't answer in time");
    says("execution error: File not found (-43)", "couldn't find a file");
    says("execution error: Photos got an error: Something odd happened. (-2700)", "Something odd happened");
    assert!(explain(&Output { status: Some(3), ..Default::default() }, "").contains("exited with 3"));
    assert!(explain(&Output { status: None, ..Default::default() }, "").contains("stopped"));
    assert!(explain_run_error(&RunError::NoPhotos).contains("couldn't be found"));
    assert!(explain_run_error(&RunError::Spawn("denied".into())).contains("denied"));
    assert!(explain_run_error(&RunError::Timeout(Duration::from_secs(65))).contains("65 s"));
    // a stray parenthesis is not an error number
    assert!(explain(&failed("execution error: (not a number)"), "").contains("not a number"));
}

fn files(tag: &str, n: usize) -> (std::path::PathBuf, Vec<String>) {
    let dir = std::env::temp_dir().join(format!("lc-photos-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let paths = (0..n)
        .map(|i| {
            let p = dir.join(format!("photo {i} é.jpg"));
            std::fs::write(&p, b"jpeg").unwrap();
            p.to_string_lossy().to_string()
        })
        .collect();
    (dir, paths)
}

#[test]
fn an_import_passes_the_files_and_returns_the_ids() {
    let (dir, paths) = files("import", 2);
    let (runner, calls) = fake(ok("ID-1/L0/001\nID-2/L0/001\n"));
    let got = import(&runner, &paths, "  Trip “2026”  ").unwrap();
    assert_eq!(got, Imported { ids: vec!["ID-1/L0/001".into(), "ID-2/L0/001".into()], requested: 2, album: Some("Trip “2026”".into()) });
    assert_eq!(got.warning(), None);
    let args = calls.lock().unwrap()[0].clone();
    let at = args.iter().position(|a| a == MARKER).unwrap();
    assert_eq!(&args[at + 1..], &[vec!["Trip “2026”".to_string()], paths.clone()].concat()[..]);
    // fewer back than asked: a partial import, said so
    let (runner, _) = fake(ok("ID-1/L0/001\n"));
    let got = import(&runner, &paths, "").unwrap();
    assert_eq!((got.ids.len(), got.album.clone()), (1, None));
    assert!(got.warning().unwrap().contains("1 of 2"));
    assert_eq!(got.to_json()["warning"], json!(got.warning().unwrap()));
    // a failed run is the mapped message
    let (runner, _) = fake(Ok(failed("execution error: Not authorized to send Apple events to Photos. (-1743)")));
    assert!(import(&runner, &paths, "x").unwrap_err().contains("Automation"));
    // a timeout names the request's limit
    let (runner, _) = fake(Err(RunError::Timeout(Duration::from_secs(70))));
    assert!(import(&runner, &paths, "x").unwrap_err().contains(&format!("{} s", timeout_for(2).as_secs())));
    let _ = std::fs::remove_dir_all(dir);
}

/// Codex review of #643: every path was an argument of one osascript run, so a big export
/// (10,000 files) went over macOS's 1 MiB argument limit and Photos got nothing.
#[test]
fn big_imports_run_in_batches_within_the_argument_limit() {
    let long = format!("/{}", "d/".repeat(500)); // about 1,000 bytes, near the longest path macOS opens
    let paths: Vec<String> = (0..10_000).map(|i| format!("{long}{i:05}.jpg")).collect();
    let runs = Arc::new(Mutex::new(Vec::<(usize, usize, Duration)>::new()));
    let r = runs.clone();
    let runner: Runner = Arc::new(move |args: &[String], limit| {
        let files = args.len() - args.iter().position(|a| a == MARKER).unwrap() - 2;
        r.lock().unwrap().push((files, args.iter().map(|a| a.len() + 1).sum(), limit));
        Ok(Output { status: Some(0), stdout: (0..files).map(|i| format!("ID-{i}\n")).collect(), stderr: String::new() })
    });
    let got = execute(&runner, &paths, "Big").unwrap();
    assert_eq!((got.ids.len(), got.requested, got.warning()), (10_000, 10_000, None));
    let runs = runs.lock().unwrap().clone();
    assert_eq!(runs.len(), 10_000 / BATCH);
    assert!(runs.iter().all(|&(files, _, _)| files == BATCH));
    let biggest = runs.iter().map(|&(_, bytes, _)| bytes).max().unwrap();
    assert!(biggest < 1 << 18, "{biggest} bytes of arguments in one run");
    // every run gets what is left of the request's limit, never more
    assert!(runs.iter().all(|&(_, _, limit)| limit <= timeout_for(10_000)));

    // a later batch failing: the error says how far Photos got
    let calls = Arc::new(Mutex::new(0));
    let c = calls.clone();
    let runner: Runner = Arc::new(move |args: &[String], _| {
        let mut n = c.lock().unwrap();
        *n += 1;
        let files = args.len() - args.iter().position(|a| a == MARKER).unwrap() - 2;
        if *n == 2 {
            return Ok(failed("execution error: Photos got an error: User canceled. (-128)"));
        }
        Ok(Output { status: Some(0), stdout: (0..files).map(|i| format!("ID-{i}\n")).collect(), stderr: String::new() })
    });
    let e = execute(&runner, &paths[..450], "Big").unwrap_err();
    assert!(e.contains("cancelled") && e.contains("200 of the 450 files"), "{e}");
    assert_eq!(*calls.lock().unwrap(), 2, "nothing more is sent after a failed batch");

    // the batches share the request's one deadline: time a batch takes comes off the next one's
    let limits = Arc::new(Mutex::new(Vec::<Duration>::new()));
    let l = limits.clone();
    let runner: Runner = Arc::new(move |args: &[String], limit| {
        l.lock().unwrap().push(limit);
        std::thread::sleep(Duration::from_millis(300));
        let files = args.len() - args.iter().position(|a| a == MARKER).unwrap() - 2;
        Ok(Output { status: Some(0), stdout: (0..files).map(|i| format!("ID-{i}\n")).collect(), stderr: String::new() })
    });
    execute(&runner, &paths[..450], "Big").unwrap();
    let limits = limits.lock().unwrap().clone();
    assert_eq!(limits.len(), 3);
    let whole = timeout_for(450);
    assert!(limits[0] <= whole && limits[0] > whole - Duration::from_millis(250), "{limits:?}");
    assert!(limits[1] <= whole - Duration::from_millis(300) && limits[2] <= whole - Duration::from_millis(600), "{limits:?}");
}

#[test]
fn bad_requests_never_reach_osascript() {
    let (dir, paths) = files("bad", 1);
    let (runner, calls) = fake(ok(""));
    assert!(import(&runner, &[], "").unwrap_err().contains("no files"));
    assert!(import(&runner, &["relative/a.jpg".into()], "").unwrap_err().contains("absolute"));
    assert!(import(&runner, &[format!("{}/missing.jpg", dir.display())], "").unwrap_err().contains("no such file"));
    assert!(import(&runner, &[format!("{}/a\0b.jpg", dir.display())], "").unwrap_err().contains("NUL"));
    assert!(import(&runner, &paths, "a\0b").unwrap_err().contains("NUL"));
    assert!(calls.lock().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn after_an_export_the_written_files_are_added() {
    let (dir, paths) = files("after", 2);
    let exported = vec![
        json!({"path": paths[0], "width": 10}),
        json!({"skipped": "/elsewhere/x.jpg"}),
        json!({"photo": 3, "error": "x"}),
        json!({"path": paths[1]}),
    ];
    let (runner, calls) = fake(ok("A/L0/001\nB/L0/001"));
    let imports = Imports::default();
    let out = imports.reserve(runner.clone(), " Album ").unwrap().run(&exported, false);
    assert_eq!(out["imported"], 2);
    assert_eq!(out["album"], "Album");
    assert_eq!((out["job"].as_u64(), out["running"].as_bool()), (Some(1), Some(false)));
    let args = calls.lock().unwrap()[0].clone();
    assert_eq!(&args[args.len() - 2..], &paths[..], "only the files written, in order");
    // nothing written, or cancelled: Photos isn't asked, and the slot is free again
    let out = imports.reserve(runner.clone(), "").unwrap().run(&[json!({"skipped": "x"})], false);
    assert!(out["skipped"].as_str().unwrap().contains("wrote no files"), "{out}");
    assert_eq!(out.get("cancelled"), None, "{out}");
    let out = imports.reserve(runner.clone(), "").unwrap().run(&exported, true);
    assert!(out["skipped"].as_str().unwrap().contains("cancelled"), "{out}");
    assert_eq!(out["cancelled"], true, "the app decides on the flag, not on the words: {out}");
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert!(imports.running().is_none());
    let _ = std::fs::remove_dir_all(dir);
}

/// An export that will add its files reserves the slot before writing them: a competing
/// import is refused, naming the export's job, and the export is never turned away after
/// writing. Every way the export can end frees the slot.
#[test]
fn an_export_reserves_the_import_before_writing() {
    let (dir, paths) = files("reserve", 1);
    let (runner, calls) = fake(ok("A\n"));
    let imports = Imports::default();
    let reserved = imports.reserve(runner.clone(), "Trip").unwrap();
    assert_eq!(reserved.job().map(|j| j.json()), Some(json!({"job": 1, "running": true, "exporting": true, "requested": 0, "album": "Trip"})));
    // while the export writes: other imports and exports are refused, naming it
    let e = import_job(&runner, &imports, paths.clone(), "", false, None).unwrap_err();
    assert!(e.contains("An export is about to add its files to Apple Photos (import 1)"), "{e}");
    assert!(imports.reserve(runner.clone(), "").err().unwrap().contains("(import 1)"));
    assert!(calls.lock().unwrap().is_empty());
    // the export hands over its files: they are imported under its job
    let out = reserved.run(&[json!({"path": paths[0]})], false);
    assert_eq!((out["job"].as_u64(), out["imported"].as_u64(), out.get("exporting")), (Some(1), Some(1), None), "{out}");
    // an export that fails or never runs (its reservation dropped) frees the slot
    let dropped = imports.reserve(runner.clone(), "").unwrap();
    let id = dropped.job().unwrap().id;
    drop(dropped);
    let j = imports.get(id).unwrap();
    assert!(j.finished() && !j.take_announcement(), "skipped quietly");
    assert!(j.json()["skipped"].as_str().unwrap().contains("ended before writing"));
    // a file the export reported but that is gone: Photos isn't asked, the slot is freed
    let out = imports.reserve(runner.clone(), "").unwrap().run(&[json!({"path": format!("{}/gone.jpg", dir.display())})], false);
    assert!(out["error"].as_str().unwrap().contains("no such file"), "{out}");
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert!(imports.running().is_none());
    assert_eq!(import_job(&runner, &imports, paths, "", false, None).unwrap().id, 4);
    let _ = std::fs::remove_dir_all(dir);
}

/// Review of #236: a runner that panics, here or on a worker, ends its import as failed and
/// frees the slot; it never stays "running", refusing every later import.
#[test]
fn a_panicking_runner_frees_the_slot() {
    let (dir, paths) = files("panic", 1);
    let panicky: Runner = Arc::new(|_: &[String], _| panic!("runner exploded"));
    let (good, _) = fake(ok("A\n"));
    let imports = Imports::default();
    let stopped = |j: &ImportJob| j.json()["error"].as_str().is_some_and(|e| e.contains("stopped unexpectedly"));
    // waiting here
    let job = import_job(&panicky, &imports, paths.clone(), "", false, None).unwrap();
    assert!(job.finished() && stopped(&job), "{}", job.json());
    assert!(imports.running().is_none());
    // on a worker
    let job = import_job(&panicky, &imports, paths.clone(), "", true, None).unwrap();
    let t0 = std::time::Instant::now();
    while !job.finished() {
        assert!(t0.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(stopped(&job), "{}", job.json());
    // an export's reservation, waiting and not
    let out = imports.reserve(panicky.clone(), "").unwrap().run(&[json!({"path": paths[0]})], false);
    assert!(out["error"].as_str().unwrap().contains("stopped unexpectedly"), "{out}");
    assert!(imports.running().is_none());
    let started = imports.reserve(panicky, "").unwrap().start(&[json!({"path": paths[0]})]);
    let job = imports.get(started["job"].as_u64().unwrap()).unwrap();
    while !job.finished() {
        assert!(t0.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(stopped(&job));
    // the slot is free: the next import runs
    assert_eq!(import_job(&good, &imports, paths, "", false, None).unwrap().json()["imported"], 1);
    let _ = std::fs::remove_dir_all(dir);
}

/// A process about to end waits for an import still running, so it never abandons osascript.
#[test]
fn waiting_for_idle_outlasts_a_running_import() {
    let (dir, paths) = files("idle", 1);
    let (runner, go) = delayed();
    let imports = Imports::default();
    let job = import_job(&runner, &imports, paths, "", true, None).unwrap();
    assert!(!job.finished());
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        go.send(()).unwrap();
    });
    imports.wait_idle();
    assert!(job.finished() && imports.running().is_none());
    release.join().unwrap();
    // nothing running: returns at once
    imports.wait_idle();
    let _ = std::fs::remove_dir_all(dir);
}

/// Review of #236: shutdown while an export is still writing the files it reserved the import
/// for. The deadline follows the job: the write allowance while the export writes, then the
/// import's own time limit from when Photos was asked, not one deadline fixed at the start
/// (which gave up with the import still inside its limit). Scaled down: 300 ms of writing,
/// imports limited to 1.5 s (+ 0.1 s).
#[test]
fn shutdown_follows_an_export_into_its_import() {
    let (dir, paths) = files("shutdown", 2);
    let limits = WaitLimits {
        writing: Duration::from_millis(300),
        import: |_| Duration::from_millis(1500),
        margin: Duration::from_millis(100),
        cap: Duration::from_secs(20),
    };
    let taking = |ms: u64| -> Runner {
        Arc::new(move |args: &[String], _| {
            std::thread::sleep(Duration::from_millis(ms));
            let n = args.len() - args.iter().position(|a| a == MARKER).unwrap() - 2;
            Ok(Output { status: Some(0), stdout: "ID\n".repeat(n), stderr: String::new() })
        })
    };
    let imports = Imports::default();
    // the export is still writing when the wait begins; Photos then takes 1 s, past the write
    // allowance but inside the import's own limit
    let reserved = imports.reserve(taking(1000), "").unwrap();
    let written: Vec<Value> = paths.iter().map(|p| json!({"path": p})).collect();
    let exporter = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        reserved.run(&written, false)
    });
    let t0 = std::time::Instant::now();
    assert!(imports.wait_idle_within(limits), "waited for the import, not just the write allowance");
    assert!(t0.elapsed() >= Duration::from_millis(1000), "{:?}", t0.elapsed());
    assert_eq!(exporter.join().unwrap()["imported"], 2);
    // an import past its own limit is given up on at its deadline (not the cap)
    let job = import_job(&taking(3000), &imports, paths.clone(), "", true, None).unwrap();
    let t0 = std::time::Instant::now();
    assert!(!imports.wait_idle_within(limits));
    let waited = t0.elapsed();
    assert!(waited >= Duration::from_millis(1500) && waited < Duration::from_millis(2900), "{waited:?}");
    while !job.finished() {
        std::thread::sleep(Duration::from_millis(20));
    }
    // an export that never hands its files over: the write allowance, then it's left
    let held = imports.reserve(taking(0), "").unwrap();
    let t0 = std::time::Instant::now();
    assert!(!imports.wait_idle_within(limits));
    let waited = t0.elapsed();
    assert!(waited >= Duration::from_millis(300) && waited < Duration::from_millis(1500), "{waited:?}");
    drop(held);
    assert!(imports.wait_idle_within(limits));
    // and the cap holds over everything
    let job = import_job(&taking(1000), &imports, paths, "", true, None).unwrap();
    let t0 = std::time::Instant::now();
    assert!(!imports.wait_idle_within(WaitLimits { cap: Duration::from_millis(200), ..limits }));
    assert!(t0.elapsed() < Duration::from_millis(900));
    while !job.finished() {
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// A runner that waits for `go` (at most 20 s, so a regression fails instead of hanging), then
/// answers one id per file.
fn delayed() -> (Runner, std::sync::mpsc::Sender<()>) {
    let (go, wait) = std::sync::mpsc::channel::<()>();
    let wait = Mutex::new(wait);
    let r: Runner = Arc::new(move |args: &[String], _| {
        let _ = wait.lock().unwrap().recv_timeout(Duration::from_secs(20));
        let files = args.len() - args.iter().position(|a| a == MARKER).unwrap() - 2;
        Ok(Output { status: Some(0), stdout: (0..files).map(|i| format!("ID-{i}\n")).collect(), stderr: String::new() })
    });
    (r, go)
}

/// Imports run as jobs: in the background the caller gets the job at once and its outcome
/// later; while one runs a second is refused (a retry must not add the files twice).
#[test]
fn a_slow_import_runs_as_a_job_and_refuses_a_second_one() {
    let (dir, paths) = files("job", 2);
    let (runner, go) = delayed();
    let imports = Imports::default();
    let t0 = std::time::Instant::now();
    let job = import_job(&runner, &imports, paths.clone(), " Trip ", true, None).unwrap();
    assert!(t0.elapsed() < Duration::from_secs(5), "the caller doesn't wait for Photos");
    assert_eq!(job.json(), json!({"job": 1, "running": true, "requested": 2, "album": "Trip"}));
    assert_eq!(imports.running().map(|j| j.id), Some(1));
    let e = import_job(&runner, &imports, paths.clone(), "", true, None).unwrap_err();
    assert!(e.contains("still adding 2 file(s) from an earlier request (import 1)"), "{e}");
    let e = imports.reserve(runner.clone(), "").err().unwrap();
    assert!(e.contains("still adding"), "{e}");
    go.send(()).unwrap();
    while !job.finished() {
        assert!(t0.elapsed() < Duration::from_secs(20), "the job finishes");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(job.take_announcement() && !job.take_announcement(), "announced once");
    let done = imports.get(1).unwrap().json();
    assert_eq!((done["running"].as_bool(), done["imported"].as_u64(), done["album"].as_str()), (Some(false), Some(2), Some("Trip")), "{done}");
    // finished: the next one may start; a failed one says why
    let (fails, _) = fake(Err(RunError::Spawn("denied".into())));
    let job = import_job(&fails, &imports, paths, "", false, None).unwrap();
    assert_eq!(job.id, 2);
    assert!(job.json()["error"].as_str().unwrap().contains("denied"));
    assert!(imports.running().is_none() && imports.latest().is_some_and(|j| j.id == 2));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_session_remembers_its_last_imports() {
    let (dir, paths) = files("kept", 1);
    let (runner, _) = fake(ok("A\n"));
    let imports = Imports::default();
    for _ in 0..KEPT_JOBS + 3 {
        import_job(&runner, &imports, paths.clone(), "", false, None).unwrap();
    }
    let all = imports.all();
    assert_eq!(all.len(), KEPT_JOBS);
    assert_eq!((all[0].id, all[KEPT_JOBS - 1].id), (4, KEPT_JOBS as u64 + 3));
    assert!(imports.get(1).is_none());
    let _ = std::fs::remove_dir_all(dir);
}

/// The activity stack (issue #345): an import of its own shows a row while Photos works (no ✕,
/// no count, the album as detail) and loses it when Photos is done; an export's own import
/// shows none of its own, the export's row covers it.
#[test]
fn imports_of_their_own_show_in_the_activity_stack() {
    let (dir, paths) = files("activity", 2);
    let activity = Activity::default();
    let seen = Arc::new(Mutex::new(Vec::new()));
    // Photos answers when the test lets it (`go`), so no assertion races the import's end
    let (go, wait) = std::sync::mpsc::channel::<()>();
    let wait = Mutex::new(wait);
    let (a, rows) = (activity.clone(), seen.clone());
    let runner: Runner = Arc::new(move |args: &[String], _| {
        rows.lock().unwrap().push(a.list());
        let _ = wait.lock().unwrap().recv_timeout(Duration::from_secs(20));
        let n = args.len() - args.iter().position(|x| x == MARKER).unwrap() - 2;
        Ok(Output { status: Some(0), stdout: "ID\n".repeat(n), stderr: String::new() })
    });
    let imports = Imports::default();
    let ended = |job: &ImportJob| {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while !(job.finished() && activity.list().is_empty()) {
            assert!(std::time::Instant::now() < deadline, "the import ends and its row goes");
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    // waiting for Photos here
    go.send(()).unwrap();
    let job = import_job(&runner, &imports, paths.clone(), "Trip", false, Some(&activity)).unwrap();
    assert_eq!(job.json()["imported"], 2);
    assert!(activity.list().is_empty(), "the row goes when Photos is done");
    let during = seen.lock().unwrap().last().cloned().unwrap();
    assert_eq!(during.len(), 1, "{during:?}");
    assert_eq!((during[0].kind, during[0].label.as_str(), during[0].detail.as_str()), (ACTIVITY_KIND, ACTIVITY_LABEL, "Trip"));
    assert_eq!((during[0].cancellable, during[0].total), (false, 0), "no ✕ and no count");
    // on a worker thread: the row stays while Photos works, and goes once the import ends
    let job = import_job(&runner, &imports, paths.clone(), "", true, Some(&activity)).unwrap();
    assert_eq!(activity.list().first().map(|r| r.kind), Some(ACTIVITY_KIND));
    go.send(()).unwrap();
    ended(&job);
    // an export's own import: the export's row covers it
    let exported: Vec<Value> = paths.iter().map(|p| json!({"path": p})).collect();
    go.send(()).unwrap();
    imports.reserve(runner.clone(), "").unwrap().shown_in(activity.clone()).run(&exported, false);
    assert!(seen.lock().unwrap().last().unwrap().is_empty(), "no row of its own while the export's worker waits");
    // ...unless it is left running on its own once the export is done
    let started = imports.reserve(runner.clone(), "").unwrap().shown_in(activity.clone()).start(&exported);
    assert_eq!(started["running"], true);
    assert_eq!(activity.list().first().map(|r| r.kind), Some(ACTIVITY_KIND));
    go.send(()).unwrap();
    let job = imports.get(started["job"].as_u64().unwrap()).unwrap();
    ended(&job);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn timeouts_grow_with_the_batch_and_stay_bounded() {
    assert_eq!(timeout_for(1), Duration::from_secs(605));
    assert_eq!(timeout_for(100), Duration::from_secs(1100));
    assert_eq!(timeout_for(usize::MAX), Duration::from_secs(7200));
}
