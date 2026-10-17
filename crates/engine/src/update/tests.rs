//! The updater's session state: settings, the check's outcome, a download's progress, and the
//! status a frontend reads. The network itself is the crate's own business (`lcc-updater`), so
//! these tests feed the state directly.

use super::*;

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lcc-engine-update-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("scratch");
    d
}

fn update(version: &str) -> lcc_updater::Update {
    lcc_updater::Update {
        current: lcc_updater::Version::parse("1.0.0").expect("v"),
        version: lcc_updater::Version::parse(version).expect("v"),
        title: format!("App {version}"),
        tag: format!("v{version}"),
        asset: lcc_updater::Asset {
            name: format!("app-{version}-windows-x64.msi"),
            url: "https://example.invalid/a.msi".into(),
            size: 1_000,
            sha256: None,
            kind: lcc_updater::Kind::Msi,
        },
        sha256: Some("ab".repeat(32)),
        published: Some("2026-10-12".into()),
        notes: "### Fixed\n- things".into(),
        action: "run the 64-bit installer and then start the app again".into(),
    }
}

#[test]
fn a_check_that_finds_a_release_reports_it() {
    let mut u = Updater::new(Some(scratch("found")));
    assert_eq!(u.check(), &Check::Never);
    u.apply(lcc_updater::Checked::Found(Box::new(update("1.2.0"))));
    let a = u.available().expect("available");
    assert_eq!((a.version.as_str(), a.current.as_str()), ("1.2.0", "1.0.0"));
    assert_eq!(a.file, "app-1.2.0-windows-x64.msi");
    assert_eq!(a.kind, Kind::Msi);
    assert!(a.sha256.is_some());
    let s = u.status();
    assert_eq!((s["check"].as_str(), s["download"].as_str()), (Some("found"), Some("idle")));
    assert_eq!(s["available"]["version"], "1.2.0");
    assert_eq!(s["available"]["kind"], "msi");
    assert_eq!(s["supported"], json!(platform_supported()));
    assert!(s["lastChecked"].as_str().is_some(), "{s}");
    let _ = std::fs::remove_dir_all(u.stage_dir());
}

#[test]
fn a_current_build_and_a_failure_are_both_reported() {
    let dir = scratch("current");
    let mut u = Updater::new(Some(dir.clone()));
    u.apply(lcc_updater::Checked::Current("1.0.0".into()));
    assert_eq!(u.check(), &Check::Current);
    assert!(u.available().is_none());
    u.apply(lcc_updater::Checked::Failed("no network".into()));
    assert_eq!(u.check(), &Check::Never, "a failure is not an answer");
    assert_eq!(u.status()["error"], "no network");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn settings_are_written_and_read_back() {
    let dir = scratch("prefs");
    {
        let mut u = Updater::new(Some(dir.clone()));
        assert!(!u.prefs().check_at_startup, "off by default");
        assert!(!u.prefs().prerelease);
        assert!(u.set_prefs(Some(true), Some(true)).expect("saved"), "a change is saved");
        assert!(!u.set_prefs(Some(true), Some(true)).expect("saved"), "the same values change nothing");
        u.mark_seen("1.2.0", true);
    }
    let again = Updater::new(Some(dir.clone()));
    assert!(again.prefs().check_at_startup);
    assert!(again.prefs().prerelease);
    assert!(again.already_seen("1.2.0"));
    assert!(!again.already_seen("1.3.0"));
    let file: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("updates.json")).expect("read")).expect("json");
    assert_eq!(file["version"], 1);
    assert_eq!(file["updates"]["seenVersion"]["skipped"], true);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_settings_file_from_a_newer_build_is_left_alone() {
    let dir = scratch("newer");
    std::fs::write(dir.join("updates.json"), br#"{"version": 99, "updates": {"checkAtStartup": true}}"#).expect("write");
    let u = Updater::new(Some(dir.clone()));
    assert!(!u.prefs().check_at_startup, "the defaults are used rather than guessed at");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_start_up_check_is_off_unless_the_settings_say_so() {
    // no updates.json (a first start, or settings kept elsewhere)
    let dir = scratch("default-off");
    assert!(!Updater::new(Some(dir.clone())).prefs().check_at_startup, "no file: off");
    // a file that doesn't mention the check keeps what it does say, and the check is off
    std::fs::write(dir.join("updates.json"), br#"{"version": 1, "updates": {"prerelease": true}}"#).expect("write");
    let u = Updater::new(Some(dir.clone()));
    assert!(!u.prefs().check_at_startup, "no key: off");
    assert!(u.prefs().prerelease, "the rest of the file is read");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_existing_choice_about_the_start_up_check_is_kept() {
    for stored in [true, false] {
        let dir = scratch(&format!("kept-{stored}"));
        let text = format!(r#"{{"version": 1, "updates": {{"checkAtStartup": {stored}, "prerelease": false}}}}"#);
        std::fs::write(dir.join("updates.json"), text).expect("write");
        let mut u = Updater::new(Some(dir.clone()));
        assert_eq!(u.prefs().check_at_startup, stored, "read as it was saved");
        // another setting saved later keeps the choice, and the file stays at version 1
        assert!(u.set_prefs(None, Some(true)).expect("saved"));
        let file: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("updates.json")).expect("read")).expect("json");
        assert_eq!(file["version"], 1, "{file}");
        assert_eq!(file["updates"]["checkAtStartup"], stored, "{file}");
        assert_eq!(Updater::new(Some(dir.clone())).prefs().check_at_startup, stored, "and read back the same");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn a_damaged_settings_file_is_survivable() {
    let dir = scratch("damaged");
    std::fs::write(dir.join("updates.json"), b"{not json").expect("write");
    let u = Updater::new(Some(dir.clone()));
    assert_eq!(u.prefs(), &Prefs::default());
    // and the file is not removed by reading it
    assert!(dir.join("updates.json").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_in_memory_session_writes_nothing() {
    let mut u = Updater::new(None);
    assert!(!u.persistent());
    assert!(u.set_prefs(Some(true), None).expect("saved"));
    u.mark_seen("9.9.9", false);
    // staged in a temporary folder, which is where a download would land
    assert!(u.stage_dir().to_string_lossy().contains("updates"));
}

#[test]
fn a_staged_file_is_reported_and_can_be_discarded() {
    let dir = scratch("staged");
    let file = dir.join("app-1.2.0-windows-x64.msi");
    std::fs::write(&file, b"installer").expect("write");
    let mut u = Updater::new(Some(dir.clone()));
    u.apply(lcc_updater::Checked::Found(Box::new(update("1.2.0"))));
    let staged = Staged {
        version: "1.2.0".into(),
        path: file.clone(),
        bytes: 9,
        sha256: "cd".repeat(32),
        file: "app-1.2.0-windows-x64.msi".into(),
        kind: Kind::Msi,
        notes: String::new(),
    };
    u.progress = Progress::Ready(Box::new(staged.clone()));
    let s = u.status();
    assert_eq!(s["download"], "ready");
    assert_eq!(s["staged"]["version"], "1.2.0");
    assert_eq!(s["staged"]["kind"], "msi");
    discard(&staged);
    assert!(!file.exists());
    discard(&staged); // a second time is not an error
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_newer_release_replaces_a_staged_older_one() {
    let dir = scratch("replace");
    let mut u = Updater::new(Some(dir.clone()));
    u.progress = Progress::Ready(Box::new(Staged {
        version: "1.1.0".into(),
        path: dir.join("old.msi"),
        bytes: 1,
        sha256: String::new(),
        file: "old.msi".into(),
        kind: Kind::Msi,
        notes: String::new(),
    }));
    u.apply(lcc_updater::Checked::Found(Box::new(update("1.2.0"))));
    assert_eq!(u.progress(), &Progress::Idle, "the newer release is what would be installed");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn installing_without_an_update_is_refused_not_panicked() {
    let mut u = Updater::new(None);
    let activity = Activity::default();
    let err = u.start_download(&activity).expect_err("nothing to download");
    assert!(err.contains("check again"), "{err}");
}

#[test]
fn an_appimage_is_not_downloaded_for_a_copy_installed_another_way() {
    let dir = scratch("not-appimage");
    let activity = Activity::default();
    let mut u = Updater::new(Some(dir.clone()));
    let mut found = update("1.2.0");
    found.asset = lcc_updater::Asset {
        name: "app-1.2.0-linux-x86_64.AppImage".into(),
        url: "http://127.0.0.1:1/app.AppImage".into(),
        size: 123,
        sha256: None,
        kind: lcc_updater::Kind::AppImage,
    };
    u.apply(lcc_updater::Checked::Found(Box::new(found)));
    u.appimage = false;
    let err = u.start_download(&activity).expect_err("a .deb install can't take an AppImage");
    assert!(err.contains("1.2.0") && err.contains("AppImage"), "{err}");
    assert!(!u.downloading());
    assert!(activity.list().is_empty(), "nothing started");
    assert!(!u.stage.exists(), "nothing staged");
    assert_eq!(u.status()["error"], err.as_str(), "the dialog says why");
    // a copy that runs from an AppImage still downloads it
    u.appimage = true;
    u.start_download(&activity).expect("start");
    u.wait_for_download();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_install_outcome_is_shown_once() {
    let mut u = Updater::new(None);
    assert!(u.installed().is_none());
    u.set_installed(Installed { version: "1.2.0".into(), message: "the installer is running".into(), exit: true });
    let s = u.status();
    assert_eq!(s["installed"]["version"], "1.2.0");
    assert_eq!(s["installed"]["exit"], true);
    u.clear_installed();
    assert!(u.status().get("installed").is_none());
}

#[test]
fn the_platform_and_the_clock_are_worded_for_a_dialog() {
    let text = platform_text();
    assert!(text.contains('-'), "{text}");
    assert_eq!(platform_supported(), lcc_updater::Target::current().is_some());
    // 2026-10-12T09:31:00Z
    assert_eq!(iso_from_unix(1_760_261_460), "2025-10-12T09:31:00Z");
    assert_eq!(iso_from_unix(0), "1970-01-01T00:00:00Z");
    assert_eq!(iso_from_unix(86_399), "1970-01-01T23:59:59Z");
    assert_eq!(iso_from_unix(951_782_400), "2000-02-29T00:00:00Z", "a leap day");
    let now = iso_now();
    assert_eq!(now.len(), 20, "{now}");
    assert!(now.ends_with('Z'));
}

#[test]
fn a_check_started_with_no_network_ends_as_a_message() {
    // 127.0.0.1:1 is closed: the check must come back as an error, not a panic or a hang
    let dir = scratch("offline");
    let mut u = Updater::new(Some(dir.clone()));
    let opts = lcc_updater::Options {
        manifest_url: Some("http://127.0.0.1:1/latest.json".into()),
        extra_origin: Some("http://127.0.0.1:1".into()),
        ..u.options()
    };
    let checked = match lcc_updater::check(&opts, None, None) {
        Ok(_) => lcc_updater::Checked::Current("1.0.0".into()),
        Err(e) => lcc_updater::Checked::Failed(e.to_string()),
    };
    assert!(matches!(checked, lcc_updater::Checked::Failed(_)), "nothing listens there");
    u.apply(checked);
    assert_eq!(u.check(), &Check::Never);
    assert!(u.status()["error"].as_str().is_some_and(|e| !e.is_empty()));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_options_follow_the_prerelease_setting() {
    let mut u = Updater::new(None);
    assert!(!u.options().prerelease);
    u.set_prefs(None, Some(true)).expect("saved");
    assert!(u.options().prerelease);
    assert_eq!(u.options().repo, lcc_brand::REPOSITORY);
}

/// A local server that answers every request with a small install payload: the whole update
/// pipeline, with no internet.
fn serve_installer(body: Vec<u8>, slow: bool) -> String {
    use std::io::{BufRead, BufReader, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!("http://{}/release", l.local_addr().expect("addr"));
    std::thread::spawn(move || {
        for mut s in l.incoming().flatten() {
            let mut r = BufReader::new(s.try_clone().expect("clone"));
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                    break;
                }
            }
            let _ = s.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes());
            for chunk in body.chunks(8192) {
                if s.write_all(chunk).is_err() {
                    return;
                }
                if slow {
                    std::thread::sleep(std::time::Duration::from_millis(40));
                }
            }
        }
    });
    base
}

/// Offer `url` as the update for `version`, as a check would.
fn offer(u: &mut Updater, version: &str, url: &str, bytes: usize) {
    let file = format!("app-{version}-windows-x64.msi");
    u.apply(lcc_updater::Checked::Found(Box::new(lcc_updater::Update {
        current: lcc_updater::Version::parse("1.0.0").expect("v"),
        version: lcc_updater::Version::parse(version).expect("v"),
        title: format!("App {version}"),
        tag: format!("v{version}"),
        asset: lcc_updater::Asset { name: file, url: url.to_string(), size: bytes as u64, sha256: None, kind: lcc_updater::Kind::Msi },
        sha256: None,
        published: Some("2026-10-12".into()),
        notes: "### Fixed".into(),
        action: "run the installer".into(),
    })));
}

#[test]
fn a_download_runs_in_the_activity_stack_and_stages_the_file() {
    let dir = scratch("download");
    let body: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    let url = serve_installer(body.clone(), false);
    let activity = Activity::default();
    let mut u = Updater::new(Some(dir.clone()));
    offer(&mut u, "1.2.0", &format!("{url}/app.msi"), body.len());
    u.start_download(&activity).expect("start");
    assert!(u.downloading(), "the transfer is in flight");
    let rows = activity.list();
    assert_eq!(rows.len(), 1, "the download holds an activity row");
    assert_eq!((rows[0].kind, rows[0].label.as_str()), (ACTIVITY_KIND, ACTIVITY_LABEL));
    assert!(rows[0].cancellable);
    assert_eq!(rows[0].unit, Unit::Bytes);
    u.wait_for_download();
    let staged = u.staged().expect("staged").clone();
    assert_eq!(staged.version, "1.2.0");
    assert_eq!(staged.bytes, body.len() as u64);
    assert_eq!(std::fs::read(&staged.path).expect("read"), body);
    assert_eq!(staged.sha256.len(), 64, "the file's own hash is recorded");
    assert!(activity.list().is_empty(), "the row goes with the transfer");
    let s = u.status();
    assert_eq!(s["download"], "ready");
    assert_eq!(s["staged"]["bytes"], body.len());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_cancelled_download_leaves_the_old_installation_alone() {
    let dir = scratch("cancel");
    let body: Vec<u8> = (0..400_000u32).map(|i| (i % 97) as u8).collect();
    let url = serve_installer(body.clone(), true);
    let activity = Activity::default();
    let mut u = Updater::new(Some(dir.clone()));
    offer(&mut u, "1.2.0", &format!("{url}/app.msi"), body.len());
    u.start_download(&activity).expect("start");
    assert!(u.cancel_download(), "there was a transfer to stop");
    u.wait_for_download();
    assert_eq!(u.progress(), &Progress::Cancelled);
    assert!(activity.list().is_empty());
    assert!(u.staged().is_none(), "nothing is offered for install");
    // the partial file is not kept under its real name
    assert!(!dir.join("updates").join("app-1.2.0-windows-x64.msi").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_download_that_cannot_reach_the_server_is_a_message() {
    let dir = scratch("down");
    let activity = Activity::default();
    let mut u = Updater::new(Some(dir.clone()));
    offer(&mut u, "1.2.0", "http://127.0.0.1:1/app.msi", 1_000);
    u.start_download(&activity).expect("start");
    u.wait_for_download();
    let Progress::Failed(why) = u.progress() else { panic!("{:?}", u.progress()) };
    assert!(!why.is_empty(), "{why}");
    assert!(u.status()["error"].as_str().is_some_and(|e| !e.is_empty()));
    assert!(activity.list().is_empty(), "the row does not stay behind");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A tiny release server: `GET /latest.json` (the manifest this run builds), `GET /sums` (the
/// release's `SHA256SUMS.txt`) and `GET /app.msi` (the installer). Returns its base URL. With
/// `file_url`, the manifest offers the installer from there instead of from this server.
fn serve_feed(installer: Vec<u8>, sha: String, file: String, file_url: Option<&str>) -> String {
    use std::io::{BufRead, BufReader, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    let base = format!("http://{addr}");
    let manifest = serde_json::json!({
        "version": "9.9.9",
        "name": "App 9.9.9",
        "notes": "### Fixed\n- the whole path",
        "pub_date": "2026-10-12T00:00:00Z",
        "releases": [{
            "tag": "v9.9.9",
            "name": "App 9.9.9",
            "notes": "### Fixed\n- the whole path",
            "date": "2026-10-12",
            "prerelease": false,
            "assets": [
                {"name": file, "url": file_url.map_or_else(|| format!("{base}/app.msi"), str::to_string), "size": installer.len()},
                {"name": "SHA256SUMS.txt", "url": format!("{base}/sums"), "size": 90},
            ],
        }],
    })
    .to_string();
    let sums = format!("{sha}  {file}\n");
    std::thread::spawn(move || {
        for stream in l.incoming().flatten() {
            let mut s = stream;
            let mut r = BufReader::new(s.try_clone().expect("clone"));
            let mut request = String::new();
            let _ = r.read_line(&mut request);
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                    break;
                }
            }
            let path = request.split_whitespace().nth(1).unwrap_or("").to_string();
            let body: &[u8] = match path.as_str() {
                "/latest.json" => manifest.as_bytes(),
                "/sums" => sums.as_bytes(),
                "/app.msi" => &installer,
                _ => {
                    let _ = s.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    continue;
                }
            };
            let _ = s.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes());
            let _ = s.write_all(body);
        }
    });
    base
}

/// The whole path a release takes: a feed is read, the newer version is picked, its file is fetched
/// over HTTP, checked against the release's SHA-256 and staged, and the plan is what this platform
/// would do with it.
#[test]
fn the_whole_path_from_a_feed_to_a_staged_file() {
    let dir = scratch("e2e");
    let installer: Vec<u8> = (0..120_000u32).map(|i| (i % 233) as u8).collect();
    let installer_sha = {
        use sha2::Digest as _;
        let mut h = sha2::Sha256::new();
        h.update(&installer);
        h.finalize().iter().map(|b| format!("{b:02x}")).collect::<String>()
    };
    let target = lcc_updater::Target::current().expect("a platform to update");
    let file = format!("app-9.9.9-{}-{}.msi", target.os.slug(), target.arch);
    let base = serve_feed(installer.clone(), installer_sha.clone(), file.clone(), None);

    let activity = Activity::default();
    let mut u = Updater::new(Some(dir.clone()));
    u.set_manifest_url(Some(format!("{base}/latest.json")));
    // the local server stands in for the release: its feed and files are trusted, over http
    u.trust_test_origin(Some(base.clone()));
    assert_eq!(u.manifest_url(), Some(format!("{base}/latest.json").as_str()));
    // 1. the check reads the feed and picks the file for this platform, with the release's hash
    u.check_now();
    let available = u.available().cloned().expect("an update");
    assert_eq!(available.version, "9.9.9");
    assert_eq!(available.file, file);
    assert_eq!(available.size, installer.len() as u64);
    assert_eq!(available.sha256.as_deref(), Some(installer_sha.as_str()), "the checksum list is read too");
    assert!(available.notes.contains("the whole path"));
    assert_eq!(available.action, target.describe(), "the plan is worded for this machine");
    // 2. the download fetches, checks and stages it
    u.start_download(&activity).expect("start");
    u.wait_for_download();
    let staged = u.staged().cloned().expect("a staged file");
    assert_eq!(std::fs::read(&staged.path).expect("read"), installer);
    assert_eq!(staged.sha256, installer_sha);
    assert_eq!(staged.version, "9.9.9");
    assert!(activity.list().is_empty(), "the row goes with the transfer");
    // 3. what installing would do here
    let plan = plan(&staged);
    assert!(!plan.summary.is_empty());
    assert!(!plan.steps.is_empty());
    // 4. a second check finds the same release and keeps the staged file
    u.check_now();
    assert!(u.staged().is_some(), "the same release does not throw its own download away");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_feed_other_than_the_releases_own_shows_in_the_status() {
    let mut u = Updater::new(None);
    assert!(u.status().get("feed").is_none(), "the release's own feed is not called out");
    u.set_manifest_url(Some("https://mirror.example/latest.json".into()));
    assert_eq!(u.status()["feed"], "https://mirror.example/latest.json");
    u.set_manifest_url(None);
    assert!(u.status().get("feed").is_none(), "back to the release's own feed");
}

/// A feed that offers a file from anywhere but the release's own downloads offers no update, so
/// there is nothing to download.
#[test]
fn a_feed_offering_a_file_from_elsewhere_has_nothing_to_download() {
    let dir = scratch("elsewhere");
    let target = lcc_updater::Target::current().expect("a platform to update");
    let file = format!("app-9.9.9-{}-{}.msi", target.os.slug(), target.arch);
    let base = serve_feed(b"not ours".to_vec(), "ab".repeat(32), file, Some("https://example.invalid/app.msi"));
    let mut u = Updater::new(Some(dir.clone()));
    u.set_manifest_url(Some(format!("{base}/latest.json")));
    u.trust_test_origin(Some(base.clone()));
    u.check_now();
    assert!(u.available().is_none(), "nothing is offered");
    let error = u.status()["error"].as_str().unwrap_or_default().to_string();
    assert!(error.contains("no download for this system"), "{error}");
    let err = u.start_download(&Activity::default()).expect_err("nothing to fetch");
    assert!(err.contains("no update to download"), "{err}");
    assert!(!dir.join("updates").exists(), "nothing was staged");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Without the test origin, the same local feed is refused before it is read: http is not enough.
#[test]
fn a_local_http_feed_is_refused_without_the_test_origin() {
    let dir = scratch("untrusted");
    let target = lcc_updater::Target::current().expect("a platform to update");
    let file = format!("app-9.9.9-{}-{}.msi", target.os.slug(), target.arch);
    let base = serve_feed(b"installer".to_vec(), "ab".repeat(32), file, None);
    let mut u = Updater::new(Some(dir.clone()));
    u.set_manifest_url(Some(format!("{base}/latest.json")));
    u.check_now();
    assert!(u.available().is_none());
    let error = u.status()["error"].as_str().unwrap_or_default().to_string();
    assert!(error.contains("the release feed") && error.contains("https is required"), "the feed itself is refused: {error}");
    assert_eq!(u.status()["feed"], format!("{base}/latest.json"), "the override is shown");
    let _ = std::fs::remove_dir_all(&dir);
}
