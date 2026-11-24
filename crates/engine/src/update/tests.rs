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
fn the_status_says_how_many_seconds_ago_the_last_check_finished() {
    use std::sync::atomic::{AtomicU64, Ordering};
    let clock = std::sync::Arc::new(AtomicU64::new(1_000));
    let seen = clock.clone();
    let mut u = Updater::new(Some(scratch("ago")));
    u.set_test_clock(Box::new(move || seen.load(Ordering::SeqCst)));
    assert!(u.status()["lastCheckedAgo"].is_null(), "no check yet: nothing to say");
    u.apply(lcc_updater::Checked::Current("1.0.0".into()));
    assert_eq!(u.status()["lastCheckedAgo"], json!(0));
    clock.store(1_000 + 125, Ordering::SeqCst);
    assert_eq!(u.status()["lastCheckedAgo"], json!(125));
    // a clock that moved back: negative, never a panic
    clock.store(10, Ordering::SeqCst);
    assert_eq!(u.status()["lastCheckedAgo"], json!(-990));
    assert_eq!(u.status()["lastChecked"], json!(iso_from_unix(1_000)));
}

#[test]
fn a_found_release_says_whether_installing_it_starts_the_install() {
    let mut u = Updater::new(Some(scratch("offers")));
    // an installer starts and relaunches
    u.apply(lcc_updater::Checked::Found(Box::new(update("1.2.0"))));
    assert_eq!(u.status()["available"]["offersInstall"], json!(true));
    // a checksum list is laid out by hand
    let mut manual = update("1.3.0");
    manual.asset.kind = lcc_updater::Kind::Checksums;
    u.apply(lcc_updater::Checked::Found(Box::new(manual)));
    assert_eq!(u.status()["available"]["offersInstall"], json!(false));
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
    u.mark_seen("99.9.9", false);
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
    u.trust_test_key(Some(test_key(5).1));
    let activity = Activity::default();
    let err = u.start_download(&activity).expect_err("nothing to download");
    assert!(err.contains("check again"), "{err}");
}

#[test]
fn an_appimage_is_not_downloaded_for_a_copy_installed_another_way() {
    let dir = scratch("not-appimage");
    let activity = Activity::default();
    let mut u = Updater::new(Some(dir.clone()));
    u.trust_test_key(Some(test_key(5).1));
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
    u.package_managed = false;
    assert_eq!(u.status()["available"]["selfUpdate"], false, "the dialog offers no download");
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
    assert_eq!(iso_from_unix(4_107_542_400), "2100-03-01T00:00:00Z", "2100 is no leap year");
    assert_eq!(iso_from_unix(u64::MAX), "292277026596-12-04T15:30:07Z", "far past chrono's years, never a panic");
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

/// A throwaway key for these tests, never a release key: the signing half, and the key the
/// updater is told to trust in its place.
fn test_key(seed: u8) -> (ed25519_dalek::SigningKey, lcc_updater::UpdateKey) {
    use base64::Engine as _;
    let signing = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
    let text = base64::engine::general_purpose::STANDARD.encode(signing.verifying_key().as_bytes());
    (signing, lcc_updater::UpdateKey::parse(&text).expect("a key"))
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    let mut h = sha2::Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn sign(key: &ed25519_dalek::SigningKey, list: &[u8]) -> Vec<u8> {
    use base64::Engine as _;
    use ed25519_dalek::Signer as _;
    base64::engine::general_purpose::STANDARD.encode(key.sign(list).to_bytes()).into_bytes()
}

/// What a release folder holds besides its file: `SHA256SUMS.txt` and its signature (`None`: 404).
#[derive(Clone)]
struct Sums {
    list: Option<Vec<u8>>,
    sig: Option<Vec<u8>>,
}

/// The genuine list for `file` under `name`, signed by `key`.
fn signed_sums(key: &ed25519_dalek::SigningKey, name: &str, file: &[u8]) -> Sums {
    let list = format!("{}  {name}\n", sha256_hex(file)).into_bytes();
    let sig = sign(key, &list);
    Sums { list: Some(list), sig: Some(sig) }
}

/// Write one HTTP response.
fn respond(s: &mut std::net::TcpStream, body: Option<&[u8]>, slow: bool) {
    use std::io::Write;
    let Some(body) = body else {
        let _ = s.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return;
    };
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

/// The request's path, with the headers read past.
fn read_request(s: &std::net::TcpStream) -> String {
    use std::io::{BufRead, BufReader};
    let mut r = BufReader::new(s.try_clone().expect("clone"));
    let mut request = String::new();
    let _ = r.read_line(&mut request);
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
            break;
        }
    }
    request.split_whitespace().nth(1).unwrap_or("").to_string()
}

/// A local release folder: its signed checksum list, and `body` at any other path. The whole
/// update pipeline, with no internet.
fn serve_installer(body: Vec<u8>, slow: bool, sums: Sums) -> String {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let base = format!("http://{}/release", l.local_addr().expect("addr"));
    std::thread::spawn(move || {
        for mut s in l.incoming().flatten() {
            let path = read_request(&s);
            if path.ends_with("/SHA256SUMS.txt") {
                respond(&mut s, sums.list.as_deref(), false);
            } else if path.ends_with("/SHA256SUMS.txt.sig") {
                respond(&mut s, sums.sig.as_deref(), false);
            } else {
                respond(&mut s, Some(&body), slow);
            }
        }
    });
    base
}

/// Offer `url` as the update for `version`, as a check would.
fn offer(u: &mut Updater, version: &str, url: &str, bytes: usize) {
    let file = offered_name(version);
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

fn offered_name(version: &str) -> String {
    format!("app-{version}-windows-x64.msi")
}

#[test]
fn a_download_runs_in_the_activity_stack_and_stages_the_file() {
    let dir = scratch("download");
    let (signing, key) = test_key(5);
    let body: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    let url = serve_installer(body.clone(), false, signed_sums(&signing, &offered_name("1.2.0"), &body));
    let activity = Activity::default();
    let mut u = Updater::new(Some(dir.clone()));
    u.trust_test_key(Some(key));
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
    let (signing, key) = test_key(5);
    let body: Vec<u8> = (0..400_000u32).map(|i| (i % 97) as u8).collect();
    let url = serve_installer(body.clone(), true, signed_sums(&signing, &offered_name("1.2.0"), &body));
    let activity = Activity::default();
    let mut u = Updater::new(Some(dir.clone()));
    u.trust_test_key(Some(key));
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
    let (_, key) = test_key(5);
    let activity = Activity::default();
    let mut u = Updater::new(Some(dir.clone()));
    u.trust_test_key(Some(key));
    offer(&mut u, "1.2.0", "http://127.0.0.1:1/app.msi", 1_000);
    u.start_download(&activity).expect("start");
    u.wait_for_download();
    let Progress::Failed(why) = u.progress() else { panic!("{:?}", u.progress()) };
    assert!(!why.is_empty(), "{why}");
    assert!(u.status()["error"].as_str().is_some_and(|e| !e.is_empty()));
    assert!(activity.list().is_empty(), "the row does not stay behind");
    let _ = std::fs::remove_dir_all(&dir);
}

/// What the tiny release server below publishes.
struct Published {
    /// What the server hands out as that file (the genuine one, or a swapped one).
    served: Vec<u8>,
    /// The file's name.
    file: String,
    /// The hash the feed states for it (an attacker who swaps the file updates this too).
    feed_sha: Option<String>,
    sums: Sums,
    /// The installer's URL, when the manifest offers it from elsewhere.
    file_url: Option<String>,
}

impl Published {
    /// A genuine release, signed by `key`.
    fn genuine(key: &ed25519_dalek::SigningKey, installer: Vec<u8>, file: String) -> Published {
        let sums = signed_sums(key, &file, &installer);
        Published { served: installer.clone(), feed_sha: Some(sha256_hex(&installer)), file, sums, file_url: None }
    }
}

/// A tiny release server: `GET /latest.json` (the manifest this run builds), `GET /SHA256SUMS.txt`
/// and `GET /SHA256SUMS.txt.sig` (the release's signed checksum list) and `GET /app.msi` (the
/// installer). Returns its base URL and how many times the installer was fetched.
fn serve_feed(release: Published) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    let base = format!("http://{addr}");
    let mut asset = serde_json::json!({
        "name": release.file,
        "url": release.file_url.clone().unwrap_or_else(|| format!("{base}/app.msi")),
        "size": release.served.len(),
    });
    if let Some(sha) = &release.feed_sha {
        asset["sha256"] = json!(sha);
    }
    let manifest = serde_json::json!({
        "version": "99.9.9",
        "name": "App 99.9.9",
        "notes": "### Fixed\n- the whole path",
        "pub_date": "2026-10-12T00:00:00Z",
        "releases": [{
            "tag": "v99.9.9",
            "name": "App 99.9.9",
            "notes": "### Fixed\n- the whole path",
            "date": "2026-10-12",
            "prerelease": false,
            "assets": [asset],
        }],
    })
    .to_string();
    let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = hits.clone();
    std::thread::spawn(move || {
        for mut s in l.incoming().flatten() {
            let path = read_request(&s);
            let body: Option<&[u8]> = match path.as_str() {
                "/latest.json" => Some(manifest.as_bytes()),
                "/SHA256SUMS.txt" => release.sums.list.as_deref(),
                "/SHA256SUMS.txt.sig" => release.sums.sig.as_deref(),
                "/app.msi" => {
                    counted.fetch_add(1, Ordering::SeqCst);
                    Some(&release.served)
                }
                _ => None,
            };
            respond(&mut s, body, false);
        }
    });
    (base, hits)
}

/// The file name this platform's update has in the test release.
fn this_platforms_file() -> String {
    let target = lcc_updater::Target::current().expect("a platform to update");
    format!("app-99.9.9-{}-{}.msi", target.os.slug(), target.arch)
}

/// An updater pointed at the local release server, trusting `key` in place of the brand's.
fn updater_for(dir: &Path, base: &str, key: Option<lcc_updater::UpdateKey>) -> Updater {
    let mut u = Updater::new(Some(dir.to_path_buf()));
    u.set_manifest_url(Some(format!("{base}/latest.json")));
    // the local server stands in for the release: its feed and files are trusted, over http
    u.trust_test_origin(Some(base.to_string()));
    u.trust_test_key(key);
    u
}

/// The whole path a release takes: a feed is read, the newer version is picked, the release's
/// signed checksum list is checked, its file is fetched over HTTP, checked against the signed
/// SHA-256 and staged, and the plan is what this platform would do with it.
#[test]
fn the_whole_path_from_a_feed_to_a_staged_file() {
    let dir = scratch("e2e");
    let (signing, key) = test_key(5);
    let installer: Vec<u8> = (0..120_000u32).map(|i| (i % 233) as u8).collect();
    let installer_sha = sha256_hex(&installer);
    let file = this_platforms_file();
    let release = Published { feed_sha: None, ..Published::genuine(&signing, installer.clone(), file.clone()) };
    let (base, _) = serve_feed(release);

    let activity = Activity::default();
    let mut u = updater_for(&dir, &base, Some(key));
    assert_eq!(u.manifest_url(), Some(format!("{base}/latest.json").as_str()));
    // 1. the check reads the feed and picks the file for this platform
    u.check_now();
    let available = u.available().cloned().expect("an update");
    assert_eq!(available.version, "99.9.9");
    assert_eq!(available.file, file);
    assert_eq!(available.size, installer.len() as u64);
    assert!(available.notes.contains("the whole path"));
    let target = lcc_updater::Target::current().expect("a platform");
    assert_eq!(available.action, target.describe(), "the plan is worded for this machine");
    assert_eq!(u.status()["available"]["verified"], true, "this build can check the signature");
    // 2. the download checks the signed list, fetches, checks and stages the file
    u.start_download(&activity).expect("start");
    u.wait_for_download();
    let staged = u.staged().cloned().expect("a staged file");
    assert_eq!(std::fs::read(&staged.path).expect("read"), installer);
    assert_eq!(staged.sha256, installer_sha);
    assert_eq!(staged.version, "99.9.9");
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

/// Every way a release can fail to vouch for its file — through `update.download` as the dialog
/// sends it: nothing is staged, nothing is left in the cache folder, the install command has
/// nothing to install, and the error says why.
#[test]
fn an_update_the_signed_list_does_not_vouch_for_is_not_installed() {
    let (signing, key) = test_key(5);
    let (other, _) = test_key(6);
    let file = this_platforms_file();
    let installer: Vec<u8> = (0..60_000u32).map(|i| (i % 211) as u8).collect();
    let genuine = Published::genuine(&signing, installer.clone(), file.clone());
    let list = genuine.sums.list.clone().expect("list");
    let swapped: Vec<u8> = installer.iter().map(|b| b.wrapping_add(1)).collect();
    let cases = vec![
        // the file is swapped, and the feed's hash with it
        (
            "a tampered file",
            Published {
                served: swapped.clone(),
                feed_sha: Some(sha256_hex(&swapped)),
                ..Published::genuine(&signing, installer.clone(), file.clone())
            },
            "did not check out",
        ),
        // the list is edited to name the swapped file; the signature is the genuine list's
        (
            "a tampered SHA256SUMS.txt",
            Published {
                served: swapped.clone(),
                feed_sha: Some(sha256_hex(&swapped)),
                sums: Sums { list: Some(format!("{}  {file}\n", sha256_hex(&swapped)).into_bytes()), sig: genuine.sums.sig.clone() },
                ..Published::genuine(&signing, installer.clone(), file.clone())
            },
            "not signed by this app's update key",
        ),
        (
            "a signature made with another key",
            Published {
                sums: Sums { list: Some(list.clone()), sig: Some(sign(&other, &list)) },
                ..Published::genuine(&signing, installer.clone(), file.clone())
            },
            "not signed by this app's update key",
        ),
        (
            "a missing .sig",
            Published { sums: Sums { list: Some(list.clone()), sig: None }, ..Published::genuine(&signing, installer.clone(), file.clone()) },
            "SHA256SUMS.txt.sig is missing",
        ),
        (
            "a checksum list that can't be fetched",
            Published { sums: Sums { list: None, sig: genuine.sums.sig.clone() }, ..Published::genuine(&signing, installer.clone(), file.clone()) },
            "could not be read",
        ),
    ];
    for (what, release, says) in cases {
        let dir = scratch("unvouched");
        let (base, _) = serve_feed(release);
        let mut s = crate::Session::new();
        s.updater = updater_for(&dir, &base, Some(key.clone()));
        // what an earlier download of this release left in the cache folder
        let stage = s.updater.stage_dir().to_path_buf();
        std::fs::create_dir_all(&stage).expect("stage");
        std::fs::write(stage.join(&file), &swapped).expect("left over");
        let v = s.execute("update.check", &json!({"wait": true})).expect("check");
        assert_eq!(v["check"], "found", "{what}: the check still reports the release: {v}");
        let err = s.execute("update.download", &json!({"wait": true})).expect_err(what);
        assert!(err.to_string().contains(says), "{what}: the error names the cause: {err}");
        assert!(s.updater.staged().is_none(), "{what}: nothing is offered for install");
        assert_eq!(s.execute("update.status", &json!({})).expect("status")["download"], "failed", "{what}");
        assert_eq!(std::fs::read_dir(&stage).expect("stage").count(), 0, "{what}: the staged file is deleted");
        let err = s.execute("update.install", &json!({"confirm": true})).expect_err(what);
        assert!(err.to_string().contains("no update has been downloaded"), "{what}: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A build without an update key checks for updates and points at the release, but downloads
/// and installs nothing.
#[test]
fn without_an_update_key_the_check_works_and_nothing_is_installed() {
    let dir = scratch("nokey");
    let (signing, _) = test_key(5);
    let file = this_platforms_file();
    let installer = b"a genuine, signed installer".to_vec();
    let (base, hits) = serve_feed(Published::genuine(&signing, installer, file.clone()));
    let mut s = crate::Session::new();
    s.updater = updater_for(&dir, &base, None);
    let v = s.execute("update.check", &json!({"wait": true})).expect("check");
    assert_eq!(v["check"], "found", "{v}");
    assert_eq!(v["available"]["version"], "99.9.9");
    assert_eq!(v["available"]["page"], format!("{}/releases/tag/v99.9.9", lcc_brand::REPOSITORY.trim_end_matches('/')));
    assert_eq!(v["available"]["verified"], false, "the dialog can tell");
    let err = s.execute("update.download", &json!({"wait": true})).expect_err("no key");
    assert!(err.to_string().contains("this build can't verify updates"), "{err}");
    assert_eq!(s.updater.status()["error"], lcc_updater::NO_KEY, "the dialog shows why");
    // a file in the cache folder (put there by anything) is not installed either
    s.updater.set_progress(Progress::Ready(Box::new(Staged {
        version: "99.9.9".into(),
        path: dir.join(&file),
        bytes: 1,
        sha256: "ab".repeat(32),
        file: file.clone(),
        kind: Kind::Msi,
        notes: String::new(),
    })));
    for p in [json!({"confirm": true}), json!({})] {
        let err = s.execute("update.install", &p).expect_err("no key");
        assert!(err.to_string().contains("this build can't verify updates"), "{err}");
    }
    assert!(s.updater.installed().is_none());
    assert_eq!(hits.load(Ordering::SeqCst), 0, "the file was never fetched");
    assert!(!dir.join("updates").join(&file).exists());
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
    let (signing, key) = test_key(5);
    let release = Published {
        file_url: Some("https://example.invalid/app.msi".into()),
        ..Published::genuine(&signing, b"not ours".to_vec(), this_platforms_file())
    };
    let (base, _) = serve_feed(release);
    let mut u = updater_for(&dir, &base, Some(key));
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
    let (signing, key) = test_key(5);
    let (base, _) = serve_feed(Published::genuine(&signing, b"installer".to_vec(), this_platforms_file()));
    let mut u = Updater::new(Some(dir.clone()));
    u.trust_test_key(Some(key));
    u.set_manifest_url(Some(format!("{base}/latest.json")));
    u.check_now();
    assert!(u.available().is_none());
    let error = u.status()["error"].as_str().unwrap_or_default().to_string();
    assert!(error.contains("the release feed") && error.contains("https is required"), "the feed itself is refused: {error}");
    assert_eq!(u.status()["feed"], format!("{base}/latest.json"), "the override is shown");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A file swapped in the cache folder between the download's check and the install is hashed
/// again, refused and deleted: the installer never sees it.
#[test]
fn a_staged_file_swapped_before_install_is_not_installed() {
    let dir = scratch("swapped");
    let file = dir.join("app-99.9.9-linux-x64.tar.gz");
    std::fs::write(&file, b"the checked download").expect("write");
    let mut s = crate::Session::new();
    s.updater = Updater::new(Some(dir.clone()));
    s.updater.trust_test_key(Some(test_key(5).1));
    let desktop = std::sync::Arc::new(crate::desktop::Recorder::default());
    s.desktop = desktop.clone();
    s.updater.set_progress(Progress::Ready(Box::new(Staged {
        version: "99.9.9".into(),
        path: file.clone(),
        bytes: 20,
        sha256: sha256_hex(b"the checked download"),
        file: "app-99.9.9-linux-x64.tar.gz".into(),
        kind: Kind::Archive,
        notes: String::new(),
    })));
    // swapped after the check (same size, other bytes)
    std::fs::write(&file, b"THE CHECKED DOWNLOAD").expect("swap");
    let err = s.execute("update.install", &json!({"confirm": true})).expect_err("refused");
    assert!(err.to_string().contains("changed after it was downloaded"), "the error says why: {err}");
    assert!(!file.exists(), "the swapped file is deleted");
    assert!(desktop.requests().is_empty(), "nothing was handed on");
    assert!(s.updater.installed().is_none());
    assert_eq!(s.updater.status()["download"], "failed", "the dialog shows the cause");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_release_page_link_stays_on_the_repositorys_releases() {
    let repo = lcc_brand::REPOSITORY.trim_end_matches('/');
    assert_eq!(release_page("v99.9.9"), format!("{repo}/releases/tag/v99.9.9"));
    assert_eq!(release_page("v1.0.0-rc.1+build"), format!("{repo}/releases/tag/v1.0.0-rc.1+build"));
    // a tag that could steer the link anywhere else gives the list of releases
    for tag in ["", "../../../evil", "v1?x=1", "v1#a", "v1/../../x", "..", "v 1", "v1\u{0}"] {
        assert_eq!(release_page(tag), releases_page(), "{tag:?}");
    }
}

/// AC-8: an older release's genuine signed list and file, served under a newer release, are not
/// installed as that release: the vouched-for file must carry the offered version in its name.
#[test]
fn an_older_signed_release_offered_as_a_newer_one_is_not_installed() {
    let (signing, key) = test_key(5);
    let target = lcc_updater::Target::current().expect("a platform to update");
    // signed for 1.0.0, offered by the feed as 99.9.9
    let old = format!("app-1.0.0-{}-{}.msi", target.os.slug(), target.arch);
    let installer = b"the genuine 1.0.0 installer".to_vec();
    let dir = scratch("replayed");
    let (base, hits) = serve_feed(Published::genuine(&signing, installer.clone(), old.clone()));
    let mut s = crate::Session::new();
    s.updater = updater_for(&dir, &base, Some(key));
    let stage = s.updater.stage_dir().to_path_buf();
    std::fs::create_dir_all(&stage).expect("stage");
    std::fs::write(stage.join(&old), &installer).expect("left over");
    let v = s.execute("update.check", &json!({"wait": true})).expect("check");
    assert_eq!(v["available"]["version"], "99.9.9", "the feed offers it as 99.9.9: {v}");
    let err = s.execute("update.download", &json!({"wait": true})).expect_err("an old release");
    assert!(err.to_string().contains("is not version 99.9.9"), "the error names the cause: {err}");
    assert!(s.updater.staged().is_none(), "nothing is offered for install");
    assert_eq!(s.updater.status()["download"], "failed");
    assert_eq!(std::fs::read_dir(&stage).expect("stage").count(), 0, "the staged file is deleted");
    assert_eq!(hits.load(Ordering::SeqCst), 0, "the file itself is never fetched");
    let err = s.execute("update.install", &json!({"confirm": true})).expect_err("nothing to install");
    assert!(err.to_string().contains("no update has been downloaded"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_portable_marker_beside_the_program_makes_the_check_portable() {
    let dir = scratch("portable");
    assert!(!portable_in(&dir), "no marker, an installed copy");
    std::fs::write(dir.join("portable.txt"), b"").unwrap();
    assert!(portable_in(&dir), "portable.txt beside the program");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_package_install_is_told_apart_from_an_appimage_and_a_hand_install() {
    use std::ffi::OsStr;
    let usr = Some(Path::new("/usr/bin/app"));
    assert!(package_managed(None, usr), "a .deb/.rpm puts the binary under /usr");
    assert!(package_managed(Some(OsStr::new("")), Some(Path::new("/opt/app/app"))));
    assert!(!package_managed(Some(OsStr::new("/home/me/App.AppImage")), usr), "an AppImage updates itself");
    assert!(!package_managed(None, Some(Path::new("/usr/local/bin/app"))), "a hand install is not a package");
    assert!(!package_managed(None, Some(Path::new("/home/me/.cargo/bin/app"))));
    assert!(!package_managed(None, None), "an unknown executable is not assumed packaged");
}

#[test]
fn a_package_install_is_sent_to_its_package_manager() {
    let dir = scratch("package-managed");
    let activity = Activity::default();
    let mut u = Updater::new(Some(dir.clone()));
    u.trust_test_key(Some(test_key(6).1));
    let mut found = update("1.3.0");
    found.asset = lcc_updater::Asset {
        name: "app-1.3.0-linux-x86_64.AppImage".into(),
        url: "http://127.0.0.1:1/app.AppImage".into(),
        size: 123,
        sha256: None,
        kind: lcc_updater::Kind::AppImage,
    };
    u.apply(lcc_updater::Checked::Found(Box::new(found)));
    u.appimage = false;
    u.package_managed = true;
    let status = u.status();
    assert_eq!(status["available"]["packageManaged"], true);
    assert_eq!(status["available"]["selfUpdate"], false);
    let err = u.start_download(&activity).expect_err("a package install isn't replaced by an AppImage");
    assert!(err.contains("package manager") && err.contains("1.3.0"), "{err}");
    assert!(activity.list().is_empty(), "nothing started");
    let _ = std::fs::remove_dir_all(&dir);
}
