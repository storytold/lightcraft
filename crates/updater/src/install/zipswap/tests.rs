use super::*;
use std::io::Write;

/// The test's folder in Temp, removed when the test ends.
fn scratch(name: &str) -> lcc_testkit::TempDir {
    lcc_testkit::TempDir::new(&format!("lcc-zipswap-{name}"))
}

/// A zip of `files` (path, contents), deflated.
fn zip_of(path: &Path, files: &[(&str, &str)]) {
    let mut w = zip::ZipWriter::new(File::create(path).unwrap());
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, body) in files {
        w.start_file(*name, opts).unwrap();
        w.write_all(body.as_bytes()).unwrap();
    }
    w.finish().unwrap();
}

/// A copy unpacked from an older zip, with the user's own files beside it.
fn copy_in(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join("app.exe"), "old app").unwrap();
    fs::write(dir.join("app-cli.exe"), "old cli").unwrap();
    fs::write(dir.join("LICENSE.txt"), "old licence").unwrap();
    fs::write(dir.join("portable.txt"), "").unwrap();
    fs::create_dir_all(dir.join("AppData")).unwrap();
    fs::write(dir.join("AppData").join("settings.json"), "{}").unwrap();
}

fn read(p: &Path) -> String {
    fs::read_to_string(p).unwrap()
}

fn manifest(dir: &Path) -> Option<Manifest> {
    fs::read_to_string(manifest_path(dir, "app.exe")).ok().and_then(|t| Manifest::parse(&t))
}

/// Swap `files` (as a zip with a top folder) into the copy in `dir`; the program started, if any.
fn update(root: &Path, dir: &Path, files: &[(&str, &str)]) -> Result<Vec<PathBuf>, UpdateError> {
    let zip = root.join("u.zip");
    let named: Vec<(String, &str)> = files.iter().map(|(n, b)| (format!("app-2.0.0-windows-x64-portable/{n}"), *b)).collect();
    let named: Vec<(&str, &str)> = named.iter().map(|(n, b)| (n.as_str(), *b)).collect();
    zip_of(&zip, &named);
    let mut started = Vec::new();
    swap_in(&zip, dir, "app.exe", "1.0.0", "2.0.0", &mut |exe| {
        started.push(exe.to_path_buf());
        Ok(())
    })?;
    Ok(started)
}

/// The new version's start, as a zip update starts it.
fn start_new(dir: &Path) -> (Start, Vec<PathBuf>, bool) {
    let mut waited = false;
    let mut started = Vec::new();
    let s = on_start(&dir.join("app.exe"), true, &mut || waited = true, &mut |exe| {
        started.push(exe.to_path_buf());
        Ok(())
    });
    (s, started, waited)
}

/// The start that counted ends without confirming (it crashed): its lock goes with it.
fn crash(dir: &Path) {
    drop(take_running(&manifest_path(dir, "app.exe")));
}

#[test]
fn an_update_swaps_in_starts_waits_and_is_confirmed() {
    let root = scratch("swap");
    let dir = root.join("copy");
    copy_in(&dir);
    let started =
        update(&root, &dir, &[("app.exe", "new app"), ("app-cli.exe", "new cli"), ("LICENSE.txt", "new licence"), ("OFL-Inter.txt", "font")])
            .unwrap();
    assert_eq!(started, [dir.join("app.exe")]);
    assert_eq!(read(&dir.join("app.exe")), "new app");
    assert_eq!(read(&dir.join("OFL-Inter.txt")), "font", "a file the update adds is added");
    // the previous files are kept until the new version confirms; the user's files are untouched
    assert_eq!(read(&dir.join("app.exe.update-old")), "old app");
    assert!(dir.join("portable.txt").is_file());
    assert_eq!(read(&dir.join("AppData").join("settings.json")), "{}");
    let m = manifest(&dir).unwrap();
    assert_eq!((m.state, m.attempts, m.files.last().map(PathBuf::as_path)), (State::Swapped, 0, Some(Path::new("app.exe"))));
    // no unpack folder is left
    assert!(fs::read_dir(&dir).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().starts_with(".app-update-")));

    // the new version starts: it waits for the old one, counts the start, and goes on
    let (s, relaunched, waited) = start_new(&dir);
    assert_eq!((s, relaunched.is_empty(), waited), (Start::AfterUpdate, true, true));
    assert_eq!(manifest(&dir).unwrap().attempts, 1);
    // once it is up, the previous files and the manifest go
    confirm(&dir.join("app.exe"));
    for f in ["app.exe", "app-cli.exe", "LICENSE.txt"] {
        assert!(!dir.join(format!("{f}.update-old")).exists(), "{f}");
    }
    assert!(!manifest_path(&dir, "app.exe").exists());
    assert!(!dir.join(".app-update.json.tmp").exists());
    assert_eq!(read(&dir.join("app.exe")), "new app");
    assert!(dir.join("portable.txt").is_file() && dir.join("AppData").join("settings.json").is_file());
}

/// A second start while the first start of the new version is still coming up (it holds the
/// manifest) leaves the update alone: one by hand, or a child that inherited the variable.
#[test]
fn a_second_start_before_the_first_confirms() {
    for (case, after_update) in [("renderer relaunch", true), ("by hand", false)] {
        let root = scratch(&format!("second-{after_update}"));
        let dir = root.join("copy");
        copy_in(&dir);
        update(&root, &dir, &[("app.exe", "new app"), ("app-cli.exe", "new cli")]).unwrap();
        let (first, _, waited) = start_new(&dir);
        assert!(waited);
        assert_eq!(first, Start::AfterUpdate);
        let second = on_start(&dir.join("app.exe"), after_update, &mut || {}, &mut |_| Ok(()));
        // a process that inherited the variable waited like the relaunch did, so it goes on as one
        assert!(matches!(second, Start::Go | Start::AfterUpdate), "{case}: a start while the first is still coming up undid the update");
        assert_eq!(read(&dir.join("app.exe")), "new app", "{case}");
        assert_eq!(manifest(&dir).unwrap().attempts, 1, "{case}: the second start isn't counted");
        // the first comes up and confirms
        confirm(&dir.join("app.exe"));
        assert!(!manifest_path(&dir, "app.exe").exists(), "{case}");
    }
}

/// The renderer's relaunch carries on the start that made it: once that one has ended (its lock
/// gone), the relaunch neither counts nor undoes, and confirms in its place.
#[test]
fn the_renderers_relaunch_carries_on_the_counted_start() {
    let root = scratch("again");
    let dir = root.join("copy");
    copy_in(&dir);
    update(&root, &dir, &[("app.exe", "new app")]).unwrap();
    assert_eq!(start_new(&dir).0, Start::AfterUpdate);
    // the first renderer failed: the start relaunches and ends
    crash(&dir);
    let s = on_start_again(&dir.join("app.exe"), true, &mut || {}, &mut |_| panic!("no undo"));
    assert_eq!(s, Start::AfterUpdate);
    assert_eq!(read(&dir.join("app.exe")), "new app");
    assert_eq!(manifest(&dir).unwrap().attempts, 1);
    // a start by hand meanwhile sees it coming up
    assert_eq!(on_start(&dir.join("app.exe"), false, &mut || {}, &mut |_| panic!("no undo")), Start::Go);
    confirm(&dir.join("app.exe"));
    assert!(!dir.join("app.exe.update-old").exists() && !manifest_path(&dir, "app.exe").exists());
    // a relaunch of a start that wasn't counted (it found another start coming up) counts itself
    let dir = root.join("uncounted");
    copy_in(&dir);
    update(&root, &dir, &[("app.exe", "new app")]).unwrap();
    assert_eq!(on_start_again(&dir.join("app.exe"), false, &mut || {}, &mut |_| panic!("no undo")), Start::Go);
    assert_eq!(manifest(&dir).unwrap().attempts, 1);
    crash(&dir);
}

/// A relaunch that ends on purpose (its library is open in another copy) takes its start back: the
/// next start counts afresh instead of undoing.
#[test]
fn a_start_taken_back_is_not_a_failed_one() {
    let root = scratch("release");
    let dir = root.join("copy");
    copy_in(&dir);
    update(&root, &dir, &[("app.exe", "new app")]).unwrap();
    assert_eq!(start_new(&dir).0, Start::AfterUpdate);
    release_start(&dir.join("app.exe"));
    assert_eq!(manifest(&dir).unwrap().attempts, 0);
    assert_eq!(on_start(&dir.join("app.exe"), false, &mut || {}, &mut |_| panic!("no undo")), Start::Go);
    assert_eq!(read(&dir.join("app.exe")), "new app");
    assert_eq!(manifest(&dir).unwrap().attempts, 1);
    crash(&dir);
}

/// The new version started once and never came up (it crashed before its first frame): its next
/// start puts the previous version back and starts it.
#[test]
fn a_new_version_that_never_came_up_is_undone_on_its_next_start() {
    let root = scratch("undo");
    let dir = root.join("copy");
    copy_in(&dir);
    update(&root, &dir, &[("app.exe", "new app"), ("app-cli.exe", "new cli"), ("NEW.txt", "added")]).unwrap();
    assert_eq!(start_new(&dir).0, Start::AfterUpdate);
    // no confirm: it crashed. The next start (by hand, so without the variable) undoes the update
    crash(&dir);
    let mut started = Vec::new();
    let s = on_start(&dir.join("app.exe"), false, &mut || panic!("nothing to wait for"), &mut |exe| {
        // the undoing start has let go of the manifest: the previous version's clean-up can run
        assert!(Held::open(&manifest_path(exe.parent().unwrap(), "app.exe")).unwrap().is_some(), "the manifest is still held");
        started.push(exe.to_path_buf());
        Ok(())
    });
    assert_eq!(s, Start::Undone(dir.join("app.exe")));
    assert_eq!(started, [dir.join("app.exe")], "the previous version is started");
    assert_eq!(read(&dir.join("app.exe")), "old app");
    assert_eq!(read(&dir.join("app-cli.exe")), "old cli");
    assert!(!dir.join("NEW.txt").exists(), "a file the update added is moved aside");
    assert_eq!(read(&dir.join("app.exe.update-failed")), "new app");
    assert_eq!(manifest(&dir).unwrap().state, State::Undone);
    // the previous version's start finds the undone update and deletes what it left
    assert_eq!(on_start(&dir.join("app.exe"), false, &mut || {}, &mut |_| panic!("no start")), Start::Go);
    clean_up(&dir, "app.exe");
    assert!(!dir.join("app.exe.update-failed").exists() && !dir.join("NEW.txt.update-failed").exists());
    assert!(!manifest_path(&dir, "app.exe").exists());
    assert!(dir.join("portable.txt").is_file());
}

/// The old version stopped half way through moving files (the power went off): the next start puts
/// every file back as it was.
#[test]
fn a_swap_that_stopped_half_way_is_put_back_at_the_next_start() {
    let dir = scratch("interrupted");
    copy_in(&dir);
    // as left by a swap that moved app-cli.exe aside, put the new one in, then moved LICENSE.txt
    // aside and stopped
    fs::rename(dir.join("app-cli.exe"), dir.join("app-cli.exe.update-old")).unwrap();
    fs::write(dir.join("app-cli.exe"), "new cli").unwrap();
    fs::rename(dir.join("LICENSE.txt"), dir.join("LICENSE.txt.update-old")).unwrap();
    let m = Manifest {
        from: "1.0.0".into(),
        to: "2.0.0".into(),
        files: ["app-cli.exe", "LICENSE.txt", "app.exe"].map(PathBuf::from).to_vec(),
        added: Vec::new(),
        state: State::Swapping,
        attempts: 0,
    };
    fs::write(manifest_path(&dir, "app.exe"), m.to_json()).unwrap();
    // the program itself was never swapped: it is what runs now, so this start goes on, on the
    // previous files
    assert_eq!(on_start(&dir.join("app.exe"), false, &mut || {}, &mut |_| panic!("no start")), Start::Go);
    assert_eq!(read(&dir.join("app-cli.exe")), "old cli");
    assert_eq!(read(&dir.join("LICENSE.txt")), "old licence");
    assert_eq!(read(&dir.join("app.exe")), "old app");
}

#[test]
fn a_program_that_cannot_start_puts_everything_back() {
    let root = scratch("rollback");
    let dir = root.join("copy");
    copy_in(&dir);
    let zip = root.join("u.zip");
    zip_of(&zip, &[("top/app.exe", "new app"), ("top/app-cli.exe", "new cli"), ("top/NEW.txt", "added")]);
    let err = swap_in(&zip, &dir, "app.exe", "1", "2", &mut |_| Err("refused".into())).unwrap_err();
    assert!(err.to_string().contains("refused") && err.to_string().contains("left as it was"), "{err}");
    assert_eq!(read(&dir.join("app.exe")), "old app");
    assert_eq!(read(&dir.join("app-cli.exe")), "old cli");
    assert!(!dir.join("NEW.txt").exists(), "a file the update added is removed again");
    assert!(!dir.join("app.exe.update-old").exists() && !manifest_path(&dir, "app.exe").exists());
}

#[test]
fn an_update_waits_for_the_last_one_to_be_confirmed() {
    let root = scratch("twice");
    let dir = root.join("copy");
    copy_in(&dir);
    update(&root, &dir, &[("app.exe", "new app")]).unwrap();
    // quitting was cancelled and Install pressed again: the first update isn't finished
    let err = update(&root, &dir, &[("app.exe", "newer app")]).unwrap_err();
    assert!(err.to_string().contains("isn't finished"), "{err}");
    assert_eq!(read(&dir.join("app.exe")), "new app");
    // once it is confirmed, the next one goes ahead
    confirm(&dir.join("app.exe"));
    update(&root, &dir, &[("app.exe", "newer app")]).unwrap();
    assert_eq!(read(&dir.join("app.exe")), "newer app");
    assert_eq!(read(&dir.join("app.exe.update-old")), "new app");
}

/// While one process swaps (it holds the manifest's lock), another one's start and clean-up leave
/// everything alone.
#[test]
fn nothing_is_touched_while_another_process_holds_the_manifest() {
    let dir = scratch("locked");
    copy_in(&dir);
    fs::rename(dir.join("app-cli.exe"), dir.join("app-cli.exe.update-old")).unwrap();
    fs::create_dir_all(dir.join(".app-update-1")).unwrap();
    let held = Held::create(&manifest_path(&dir, "app.exe")).unwrap();
    let m = Manifest {
        from: "1".into(),
        to: "2".into(),
        files: vec!["app-cli.exe".into(), "app.exe".into()],
        added: Vec::new(),
        state: State::Swapping,
        attempts: 0,
    };
    held.write(&m).unwrap();
    // `File` locks are per handle, so a second open in this process is refused like another process
    assert_eq!(on_start(&dir.join("app.exe"), false, &mut || {}, &mut |_| panic!("no start")), Start::Go);
    clean_up(&dir, "app.exe");
    assert!(dir.join("app-cli.exe.update-old").exists() && !dir.join("app-cli.exe").exists(), "the swap's half-moved file is left to it");
    assert!(dir.join(".app-update-1").exists(), "its unpack folder too");
    held.remove();
}

/// A process that only inherited the variable (a child of the new version) waits for nothing.
#[test]
fn only_a_version_an_update_put_in_place_waits() {
    let dir = scratch("inherited");
    copy_in(&dir);
    assert_eq!(on_start(&dir.join("app.exe"), true, &mut || panic!("nothing to wait for"), &mut |_| panic!("no start")), Start::Go);
}

#[test]
fn a_hostile_or_wrong_zip_changes_nothing() {
    let root = scratch("hostile");
    let dir = root.join("copy");
    copy_in(&dir);
    let cases: Vec<(&str, Vec<(&str, &str)>, &[&str])> = vec![
        ("escape", vec![("app.exe", "new"), ("../evil.txt", "x")], &["outside its folder"]),
        ("backslash escape", vec![("app.exe", "new"), ("..\\evil.txt", "x")], &["outside its folder", "can't hold"]),
        ("drive", vec![("app.exe", "new"), ("C:evil.txt", "x")], &["outside its folder", "can't hold"]),
        ("stream", vec![("app.exe", "new"), ("app.exe:hidden", "x")], &["outside its folder", "can't hold"]),
        ("device", vec![("app.exe", "new"), ("NUL", "x")], &["can't hold", "outside its folder"]),
        ("device with extension", vec![("app.exe", "new"), ("top/nul.txt", "x")], &["can't hold", "outside its folder"]),
        ("numbered device", vec![("app.exe", "new"), ("COM1.log", "x")], &["can't hold", "outside its folder"]),
        ("trailing dot", vec![("app.exe", "new"), ("x.", "x")], &["can't hold"]),
        ("trailing space", vec![("app.exe", "new"), ("x ", "x")], &["can't hold"]),
        ("twice", vec![("app.exe", "new"), ("a.txt", "1"), ("a.txt", "2")], &["twice"]),
        ("twice by case", vec![("app.exe", "new"), ("a.txt", "1"), ("A.TXT", "2")], &["twice"]),
        ("no program", vec![("top/other.exe", "x"), ("top/LICENSE.txt", "x")], &["no app.exe"]),
        ("program not at the top", vec![("a/b/app.exe", "x"), ("a/c.txt", "x")], &["no app.exe"]),
        ("our own name", vec![("app.exe", "new"), ("app-cli.exe.update-old", "x")], &["updater uses itself"]),
        ("our own name, other case", vec![("app.exe", "new"), ("app-cli.exe.UPDATE-OLD", "x")], &["updater uses itself"]),
        ("our manifest", vec![("app.exe", "new"), (".app-update.json", "{}")], &["updater uses itself"]),
        ("our manifest's lock", vec![("app.exe", "new"), (".APP-update.json.lock", "")], &["updater uses itself"]),
        ("empty", vec![], &["no app.exe"]),
    ];
    for (name, files, why) in cases {
        let zip = root.join("case.zip");
        // the zip writer refuses some names itself; those can't reach an update either
        let written = std::panic::catch_unwind(|| zip_of(&zip, &files));
        if written.is_err() {
            continue;
        }
        let err = swap_in(&zip, &dir, "app.exe", "1", "2", &mut |_| panic!("{name}: nothing may start")).unwrap_err();
        // Windows refuses names it can't hold as paths outside the folder, too
        let held = why.contains(&"can't hold") && err.to_string().contains("outside its folder");
        assert!(held || why.iter().any(|w| err.to_string().contains(w)), "{name}: {err}");
        assert_eq!(read(&dir.join("app.exe")), "old app", "{name}");
        assert!(!root.join("evil.txt").exists() && !dir.join("app.exe.update-old").exists(), "{name}");
        assert!(!manifest_path(&dir, "app.exe").exists(), "{name}");
    }
    // not a zip at all, and a zip that isn't there
    let junk = root.join("junk.zip");
    fs::write(&junk, b"PK\x03\x04 truncated").unwrap();
    for zip in [junk, root.join("missing.zip")] {
        let err = swap_in(&zip, &dir, "app.exe", "1", "2", &mut |_| panic!("nothing may start")).unwrap_err();
        assert!(matches!(err, UpdateError::Install(_)), "{err}");
        assert_eq!(read(&dir.join("app.exe")), "old app");
    }
}

#[test]
fn windows_names_are_checked_everywhere() {
    for good in ["app.exe", "OFL-Inter.txt", "a b.txt", "console.txt", "com10", "nulls", "LPT", "x.y.z"] {
        assert!(safe_name(good), "{good}");
    }
    for bad in
        ["", "x.", "x ", "a:b", "C:x", "a\\b", "a/b", "CON", "con.txt", "Nul", "aux.tar.gz", "COM1", "lpt9.log", "a\u{1}b", "a*b", "a?b", "a|b"]
    {
        assert!(!safe_name(bad), "{bad:?}");
    }
}

/// A name with a leading `/` is read as relative to the zip (the zip format has no absolute
/// names): it lands inside the copy, never at the drive's root.
#[test]
fn a_rooted_name_stays_inside_the_copy() {
    let root = scratch("rooted");
    let dir = root.join("copy");
    copy_in(&dir);
    let zip = root.join("u.zip");
    zip_of(&zip, &[("/app.exe", "new app"), ("/rooted.txt", "x")]);
    swap_in(&zip, &dir, "app.exe", "1", "2", &mut |_| Ok(())).unwrap();
    assert_eq!(read(&dir.join("rooted.txt")), "x");
    assert_eq!(read(&dir.join("app.exe")), "new app");
}

#[test]
fn the_top_folder_is_dropped_only_when_everything_is_in_it() {
    let p = |v: &[&'static str]| single_top_folder(v.iter().map(Path::new));
    assert_eq!(p(&["a/x", "a/y/z"]), Some(PathBuf::from("a")));
    assert_eq!(p(&["a/x", "b/y"]), None);
    assert_eq!(p(&["a/x", "y"]), None);
    assert_eq!(p(&["x"]), None);
    assert_eq!(p(&[]), None);
}

/// The clean-up acts on the manifest's files and the program's own folder only: a person's file
/// that happens to end in `.update-old`, in the data folder or beside the program, is left alone.
#[test]
fn clean_up_touches_only_what_an_update_left() {
    let dir = scratch("clean");
    copy_in(&dir);
    fs::write(dir.join("AppData").join("mine.update-old"), "the person's").unwrap();
    fs::write(dir.join("notes.txt.update-old"), "the person's too").unwrap();
    fs::create_dir_all(dir.join(".app-update-1")).unwrap();
    fs::write(dir.join(".app-update-1").join("app.exe"), "half unpacked").unwrap();
    fs::write(dir.join(".APP-UPDATE-probe-2"), "").unwrap();
    let ours = dir.join(format!(".app-update-{}", std::process::id()));
    fs::create_dir_all(&ours).unwrap();
    clean_up(&dir, "app.exe");
    assert!(!dir.join(".app-update-1").exists() && !dir.join(".APP-UPDATE-probe-2").exists(), "a stopped process's unpack folder and probe go");
    assert!(ours.exists(), "this process's own unpack folder is left alone");
    assert!(dir.join("AppData").join("mine.update-old").exists() && dir.join("notes.txt.update-old").exists());
}

#[test]
fn a_hostile_manifest_is_not_followed() {
    let dir = scratch("hostile-manifest");
    copy_in(&dir);
    fs::write(dir.join("..").join("outside.update-old"), "elsewhere").unwrap();
    for text in [
        r#"{"from":"1","to":"2","files":["../outside"],"added":[],"state":"confirmed","attempts":0}"#,
        r#"{"from":"1","to":"2","files":["C:\\outside"],"added":[],"state":"undone","attempts":0}"#,
        r#"{"from":"1","to":"2","files":["/outside"],"added":[],"state":"swapped","attempts":5}"#,
        r#"{"from":"1","to":"2","files":["a//b"],"added":[],"state":"swapped","attempts":5}"#,
        r#"{"from":"1","to":"2","files":["app.exe"],"added":["other.exe"],"state":"swapped","attempts":5}"#,
        r#"{"from":"1","to":"2","files":"app.exe","added":[],"state":"swapped","attempts":0}"#,
        r#"{"from":"1","to":"2","files":["app.exe"],"added":[],"state":"whatever","attempts":0}"#,
        "not json",
        "",
        r#"{"from":"1","to":"2","fi"#,
    ] {
        assert_eq!(Manifest::parse(text), None, "{text}");
        // whichever reads it first deletes it, and follows nothing in it
        for reader in ["start", "clean-up", "confirm"] {
            fs::write(manifest_path(&dir, "app.exe"), text).unwrap();
            match reader {
                "start" => assert_eq!(on_start(&dir.join("app.exe"), false, &mut || {}, &mut |_| panic!("no start")), Start::Go, "{text}"),
                "clean-up" => clean_up(&dir, "app.exe"),
                _ => confirm(&dir.join("app.exe")),
            }
            assert!(!manifest_path(&dir, "app.exe").exists(), "{reader}: {text}");
            assert_eq!(read(&dir.join("app.exe")), "old app", "{text}");
        }
    }
    assert!(dir.join("..").join("outside.update-old").exists());
    let _ = fs::remove_file(dir.join("..").join("outside.update-old"));
}

/// A manifest left empty (a write cut off on a file system that can't rename over a file, or a
/// crash of a build that truncated before writing) doesn't refuse later updates.
#[test]
fn an_empty_manifest_does_not_block_updates() {
    let root = scratch("empty");
    let dir = root.join("copy");
    copy_in(&dir);
    fs::write(manifest_path(&dir, "app.exe"), "").unwrap();
    clean_up(&dir, "app.exe");
    assert!(update(&root, &dir, &[("app.exe", "new app")]).is_ok());
}

/// A write that stopped before its rename leaves the previous manifest whole and a temporary file,
/// which is ignored and then deleted.
#[test]
fn a_write_cut_off_before_its_rename_leaves_the_manifest_whole() {
    let root = scratch("cut-off");
    let dir = root.join("copy");
    copy_in(&dir);
    update(&root, &dir, &[("app.exe", "new app")]).unwrap();
    let tmp = dir.join(".app-update.json.tmp");
    fs::write(&tmp, r#"{"from":"1","to":"2","fi"#).unwrap();
    assert_eq!(manifest(&dir).unwrap().state, State::Swapped);
    assert_eq!(start_new(&dir).0, Start::AfterUpdate);
    assert_eq!(manifest(&dir).unwrap().attempts, 1);
    confirm(&dir.join("app.exe"));
    clean_up(&dir, "app.exe");
    assert!(!tmp.exists() && !manifest_path(&dir, "app.exe").exists());
    // and one with no manifest beside it
    fs::write(&tmp, "x").unwrap();
    clean_up(&dir, "app.exe");
    assert!(!tmp.exists());
}

/// On a file system that can't rename over a file in one step (FAT), a write can stop after the
/// manifest is gone and before its new contents, written in full, are renamed in: those contents
/// are the manifest.
#[test]
fn a_whole_last_write_stands_in_for_a_lost_manifest() {
    let root = scratch("lost");
    let dir = root.join("copy");
    copy_in(&dir);
    update(&root, &dir, &[("app.exe", "new app"), ("app-cli.exe", "new cli")]).unwrap();
    assert_eq!(start_new(&dir).0, Start::AfterUpdate);
    crash(&dir);
    // the counted start's write, cut off half way through the rename
    fs::rename(manifest_path(&dir, "app.exe"), dir.join(".app-update.json.tmp")).unwrap();
    // so the next start still knows a start was counted, and undoes the update
    let s = on_start(&dir.join("app.exe"), false, &mut || {}, &mut |_| Ok(()));
    assert_eq!(s, Start::Undone(dir.join("app.exe")));
    assert_eq!(read(&dir.join("app.exe")), "old app");
    assert_eq!(read(&dir.join("app-cli.exe")), "old cli");
}

/// The lock's file stays (deleting it could let two processes each lock a file of that name), and
/// the next update goes ahead with it there.
#[test]
fn the_lock_file_stays_and_does_not_stop_the_next_update() {
    let root = scratch("lock-stays");
    let dir = root.join("copy");
    copy_in(&dir);
    update(&root, &dir, &[("app.exe", "new app")]).unwrap();
    assert_eq!(start_new(&dir).0, Start::AfterUpdate);
    confirm(&dir.join("app.exe"));
    assert!(dir.join(".app-update.json.lock").is_file() && !manifest_path(&dir, "app.exe").exists());
    update(&root, &dir, &[("app.exe", "newer app")]).unwrap();
    assert_eq!(read(&dir.join("app.exe")), "newer app");
}

/// A manifest too large to be read back whole is refused before any file moves (it would be
/// deleted as unreadable, and the undo with it).
#[test]
fn an_update_too_large_to_record_changes_nothing() {
    let root = scratch("huge");
    let dir = root.join("copy");
    copy_in(&dir);
    let long = "n".repeat(250);
    let names: Vec<String> = (0..2200).map(|i| format!("top/{long}{i}")).collect();
    let mut files: Vec<(&str, &str)> = names.iter().map(|n| (n.as_str(), "x")).collect();
    files.push(("top/app.exe", "new app"));
    zip_of(&root.join("u.zip"), &files);
    let err = swap_in(&root.join("u.zip"), &dir, "app.exe", "1", "2", &mut |_| panic!("nothing may start")).unwrap_err();
    assert!(err.to_string().contains("file names"), "{err}");
    assert_eq!(read(&dir.join("app.exe")), "old app");
    assert!(!manifest_path(&dir, "app.exe").exists() && !dir.join(format!("{long}0")).exists());
}

/// A read-only previous file (a copy restored from a backup) is made deletable when it is moved
/// aside, so the confirm can delete it.
#[test]
fn a_read_only_file_is_replaced_and_its_previous_copy_deleted() {
    let root = scratch("readonly");
    let dir = root.join("copy");
    copy_in(&dir);
    for f in ["app.exe", "LICENSE.txt"] {
        let mut p = fs::metadata(dir.join(f)).unwrap().permissions();
        p.set_readonly(true);
        fs::set_permissions(dir.join(f), p).unwrap();
    }
    update(&root, &dir, &[("app.exe", "new app"), ("LICENSE.txt", "new licence")]).unwrap();
    assert_eq!(read(&dir.join("app.exe")), "new app");
    confirm(&dir.join("app.exe"));
    assert!(!dir.join("app.exe.update-old").exists() && !dir.join("LICENSE.txt.update-old").exists());
    assert!(!manifest_path(&dir, "app.exe").exists());
}

/// On Windows the running program can be renamed but not deleted: the swap works while it runs,
/// and the confirm leaves its file (and the manifest) for a later start.
#[cfg(windows)]
#[test]
fn a_running_program_is_renamed_aside_and_deleted_once_it_has_quit() {
    let root = scratch("running");
    let dir = root.join("copy");
    copy_in(&dir);
    // a real program that keeps running: a copy of Windows' ping
    let ping = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32").join("PING.EXE");
    fs::copy(&ping, dir.join("app.exe")).unwrap();
    let mut child =
        std::process::Command::new(dir.join("app.exe")).args(["-n", "30", "127.0.0.1"]).stdout(std::process::Stdio::null()).spawn().unwrap();
    let swapped = update(&root, &dir, &[("app.exe", "new app")]);
    if swapped.is_ok() {
        confirm(&dir.join("app.exe"));
    }
    let still_there = dir.join("app.exe.update-old").exists() && manifest_path(&dir, "app.exe").exists();
    let _ = child.kill();
    let _ = child.wait();
    swapped.unwrap();
    assert_eq!(read(&dir.join("app.exe")), "new app");
    assert!(still_there, "the running program's file can't be deleted yet");
    // once it has quit, a later start's clean-up deletes it
    let deadline = Instant::now() + Duration::from_secs(10);
    while manifest_path(&dir, "app.exe").exists() && Instant::now() < deadline {
        clean_up(&dir, "app.exe");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(!dir.join("app.exe.update-old").exists() && !manifest_path(&dir, "app.exe").exists());
}
