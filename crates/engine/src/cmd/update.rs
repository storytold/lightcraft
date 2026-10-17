//! Auto updates as commands: check the release feed, download what it offers, install it, and the
//! two settings that decide when all that happens ([`crate::update`]).
//!
//! The UI (Help ▸ Check for Updates…), the CLI and the control channel all dispatch these by id, so
//! an agent can drive an update exactly the way the dialog does.

use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd};
use crate::update::{self, Kind, Progress, Staged};
use crate::{EngineError, Result, Session};

fn status(s: &mut Session, _: &Value) -> Result<Value> {
    Ok(s.updater.status())
}

/// `update.check [wait]`: start a check (background), or run it here and answer with the outcome.
///
/// The feed is always the release's own. `feed` is refused rather than ignored: a caller that
/// meant to point the app elsewhere learns that it did not, and nothing changes.
fn check(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "update.check";
    if p.get("feed").is_some() {
        return Err(bad(C, "`feed` is not a parameter: the app reads only its own release feed"));
    }
    let wait = p.get("wait").and_then(Value::as_bool).unwrap_or(false);
    if wait && !s.updater.checking() {
        s.updater.check_now();
    } else if !s.updater.start_check() && !s.updater.checking() {
        return Err(bad(C, "a check could not be started; try again"));
    }
    if wait {
        s.updater.wait_for_check();
    }
    Ok(s.updater.status())
}

/// `update.download [wait]`: fetch the file the check offered.
fn download(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "update.download";
    let activity = s.activity.clone();
    s.updater.start_download(&activity).map_err(|e| bad(C, e))?;
    if p.get("wait").and_then(Value::as_bool) == Some(true) {
        s.updater.wait_for_download();
        if let Progress::Failed(why) = s.updater.progress() {
            return Err(bad(C, why.clone()));
        }
    }
    Ok(s.updater.status())
}

fn cancel_download(s: &mut Session, _: &Value) -> Result<Value> {
    Ok(json!({"cancelled": s.updater.cancel_download()}))
}

/// `update.install [confirm]`: apply the downloaded file. Without `confirm` it answers with what
/// installing would do (the dialog shows that first).
fn install(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "update.install";
    let Some(staged) = s.updater.staged().cloned() else {
        return Err(bad(C, "no update has been downloaded: run update.check then update.download"));
    };
    if staged.kind == Kind::Checksums {
        return Err(bad(C, "that release offers no file for this system"));
    }
    let plan = update::plan(&staged);
    if p.get("confirm").and_then(Value::as_bool) != Some(true) {
        return Ok(json!({
            "version": staged.version,
            "summary": plan.summary,
            "steps": plan.steps,
            "destination": plan.destination.map(|d| d.to_string_lossy().to_string()),
            "restart": restart_text(plan.restart),
            "file": staged.file,
            "bytes": staged.bytes,
            "sha256": staged.sha256,
        }));
    }
    let installed = update::install(&staged, s.desktop.as_ref()).map_err(|e| bad(C, e))?;
    let exit = installed.exit;
    let out = json!({"version": installed.version, "message": installed.message, "exit": exit});
    s.updater.set_installed(installed);
    Ok(out)
}

fn restart_text(restart: lcc_updater::Restart) -> &'static str {
    match restart {
        lcc_updater::Restart::QuitAndRelaunch => "quitAndRelaunch",
        lcc_updater::Restart::QuitOnly => "quit",
        lcc_updater::Restart::Manual => "manual",
    }
}

/// `update.discard`: delete the downloaded file.
fn discard(s: &mut Session, _: &Value) -> Result<Value> {
    match s.updater.staged().cloned() {
        Some(staged) => {
            update::discard(&staged);
            s.updater.clear_installed();
            Ok(s.updater.status())
        }
        None => Err(bad("update.discard", "no update has been downloaded")),
    }
}

/// `update.prefs`: the two settings, and whether they are remembered between runs.
fn prefs(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "update.prefs";
    let flag = |key: &str| -> Result<Option<bool>> {
        match p.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Bool(v)) => Ok(Some(*v)),
            Some(_) => Err(bad(C, format!("{key} must be true or false"))),
        }
    };
    let check_at_startup = flag("checkAtStartup")?;
    let prerelease = flag("prerelease")?;
    if check_at_startup.is_some() || prerelease.is_some() {
        s.updater.set_prefs(check_at_startup, prerelease).map_err(|e| EngineError::Other(format!("could not write {e}")))?;
    }
    let mut v = s.updater.status();
    v["persistent"] = json!(s.updater.persistent());
    v["staging"] = json!(s.updater.stage_dir().to_string_lossy());
    Ok(v)
}

/// `update.dismiss`: the user was told about `version` and does not want to be told again.
fn dismiss(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "update.dismiss";
    let version = match p.get("version").and_then(Value::as_str) {
        Some(v) if !v.is_empty() && v.len() <= 64 => v.to_string(),
        _ => match s.updater.available() {
            Some(a) => a.version.clone(),
            None => return Err(bad(C, "no version to dismiss")),
        },
    };
    let skipped = p.get("skipped").and_then(Value::as_bool).unwrap_or(true);
    s.updater.mark_seen(&version, skipped);
    Ok(s.updater.status())
}

/// The staged file, for a caller that wants to know where it is.
pub fn staged_of(s: &Session) -> Option<Staged> {
    s.updater.staged().cloned()
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            query "update.status",
            "Update Status",
            [],
            None,
            "{} → {version, platform, supported, check: never | running | current | found, download: idle | running | ready | failed | cancelled, checkAtStartup, prerelease, lastChecked, seenVersion, repo, page, staging, feed? (a feed other than the release's own), available?: {version, current, title, tag, published, notes, file, size, action, verified, kind}, progress?: {bytes, total}, staged?: {version, file, bytes, sha256, path, kind}, installed?: {version, message, exit}, error?} — what the updater knows right now",
            always,
            status
        ),
        cmd!(
            query "update.check",
            "Check for Updates",
            [],
            None,
            "{wait?: bool} — read the project's own release feed in the background and compare it with this build (one request; a recent answer is reused). Only files on the project's GitHub releases, over https, are offered. `wait: true` runs it now and answers with the outcome, for one-shot callers → status",
            always,
            check
        ),
        cmd!(
            query "update.download",
            "Download Update",
            [],
            None,
            "{wait?: bool} — fetch the file the check offered for this system into the cache folder (resumable, size- and SHA-256-checked; the activity stack shows it and can cancel it). `wait: true` blocks until it is in, for one-shot callers → status",
            always,
            download
        ),
        cmd!(
            query "update.downloadCancel",
            "Cancel Update Download",
            [],
            None,
            "{} → {cancelled} — stop the transfer; nothing is left behind",
            always,
            cancel_download
        ),
        cmd!(
            query "update.install",
            "Install Update",
            [],
            None,
            "{confirm?: bool} — without `confirm`: what installing the downloaded file would do ({summary, steps, destination, restart: quitAndRelaunch | quit | manual}). With `confirm: true`: do it — run the installer, replace the AppImage, or open the file's folder — and say whether the app has to quit ({message, exit})",
            always,
            install
        ),
        cmd!(
            query "update.discard",
            "Discard Downloaded Update",
            [],
            None,
            "{} — delete the downloaded file → status",
            always,
            discard
        ),
        cmd!(
            query "update.prefs",
            "Update Preferences",
            [],
            None,
            "{checkAtStartup?: bool, prerelease?: bool} — ask the feed when the app starts, and whether prereleases count as updates (saved in the app's own settings folder, `updates.json`; `--memory`, `--demo` and `{env}_NO_PREFS` runs keep them in memory only). The start-up check is off by default, and headless sessions never run it. A file that can't be written is an error naming it, and the settings stay as they were → status + {persistent, staging}",
            always,
            prefs
        ),
        cmd!(
            query "update.dismiss",
            "Dismiss Update",
            [],
            None,
            "{version?, skipped?: bool} — remember that the user answered about this version, so a start-up check does not ask again (defaults to the version on offer) → status",
            always,
            dismiss
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Session;

    fn session() -> Session {
        // in-memory only: no settings file, no library, no network
        let mut s = Session::new();
        s.updater = crate::update::Updater::new(None);
        s
    }

    #[test]
    fn installing_without_a_download_is_an_error() {
        let mut s = session();
        let err = s.execute("update.install", &json!({"confirm": true})).expect_err("nothing staged");
        assert!(err.to_string().contains("update.download"), "{err}");
        let err = s.execute("update.discard", &json!({})).expect_err("nothing staged");
        assert!(err.to_string().contains("no update"), "{err}");
    }

    #[test]
    fn a_download_without_an_update_is_an_error() {
        let mut s = session();
        let err = s.execute("update.download", &json!({})).expect_err("nothing on offer");
        assert!(err.to_string().contains("check"), "{err}");
    }

    #[test]
    fn the_settings_round_trip_and_are_validated() {
        let mut s = session();
        let v = s.execute("update.prefs", &json!({"checkAtStartup": true, "prerelease": true})).expect("prefs");
        assert_eq!((v["checkAtStartup"].as_bool(), v["prerelease"].as_bool()), (Some(true), Some(true)));
        assert!(s.updater.prefs().check_at_startup, "the start-up check is on (the default is off)");
        assert_eq!(v["persistent"], false, "an in-memory session remembers nothing");
        let err = s.execute("update.prefs", &json!({"prerelease": "yes"})).expect_err("not a bool");
        assert!(err.to_string().contains("true or false"), "{err}");
    }

    #[test]
    fn dismissing_remembers_the_version() {
        let mut s = session();
        s.updater.apply(lcc_updater::Checked::Found(Box::new(lcc_updater::Update {
            current: lcc_updater::Version::parse("1.0.0").expect("v"),
            version: lcc_updater::Version::parse("1.2.0").expect("v"),
            title: "App 1.2.0".into(),
            tag: "v1.2.0".into(),
            asset: lcc_updater::Asset {
                name: "app-1.2.0-windows-x64.msi".into(),
                url: "https://example.invalid/a.msi".into(),
                size: 10,
                sha256: None,
                kind: lcc_updater::Kind::Msi,
            },
            sha256: None,
            published: None,
            notes: String::new(),
            action: "install".into(),
        })));
        assert!(s.updater.available().is_some());
        let v = s.execute("update.dismiss", &json!({})).expect("dismiss");
        assert_eq!(v["seenVersion"]["version"], "1.2.0");
        assert_eq!(v["seenVersion"]["skipped"], true);
        // and no update on offer: dismissing with nothing to dismiss is an error
        let mut fresh = session();
        assert!(fresh.execute("update.dismiss", &json!({})).is_err());
    }

    #[test]
    fn the_install_plan_is_available_before_confirming() {
        let mut s = session();
        let dir = std::env::temp_dir().join(format!("lcc-cmd-update-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("app-1.2.0-windows-x64.msi");
        std::fs::write(&file, b"installer").expect("write");
        s.updater.set_progress(Progress::Ready(Box::new(Staged {
            version: "1.2.0".into(),
            path: file,
            bytes: 9,
            sha256: "ab".repeat(32),
            file: "app-1.2.0-windows-x64.msi".into(),
            kind: Kind::Msi,
            notes: String::new(),
        })));
        let v = s.execute("update.install", &json!({})).expect("plan");
        assert_eq!(v["version"], "1.2.0");
        assert!(v["summary"].as_str().is_some_and(|t| t.contains("installer")), "{v}");
        assert!(v["steps"].as_array().is_some_and(|a| !a.is_empty()));
        assert_eq!(v["file"], "app-1.2.0-windows-x64.msi");
        assert_eq!(v["sha256"], "ab".repeat(32));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A scratch settings folder nobody can write to (0o555) until it is dropped.
    #[cfg(unix)]
    struct ReadOnly(std::path::PathBuf);

    #[cfg(unix)]
    impl ReadOnly {
        /// `None` when the folder can still be written (running as root): the test is skipped.
        fn new(tag: &str) -> Option<ReadOnly> {
            use std::os::unix::fs::PermissionsExt;
            let dir = std::env::temp_dir().join(format!("lcc-engine-update-ro-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch");
            let guard = ReadOnly(dir.clone());
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).expect("chmod");
            if std::fs::write(dir.join("probe"), b"x").is_ok() {
                eprintln!("skipped: {} is still writable (running as root?)", dir.display());
                return None;
            }
            Some(guard)
        }
    }

    #[cfg(unix)]
    impl Drop for ReadOnly {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_settings_file_that_cannot_be_written_is_an_error_and_changes_nothing() {
        let Some(dir) = ReadOnly::new("prefs") else { return };
        let mut s = Session::new();
        s.updater = crate::update::Updater::new(Some(dir.0.clone()));
        let err = s.execute("update.prefs", &json!({"checkAtStartup": true})).expect_err("the folder can't be written");
        let text = err.to_string();
        assert!(text.contains("updates.json"), "names the file: {text}");
        assert!(text.to_lowercase().contains("permission"), "says why: {text}");
        assert!(!s.updater.prefs().check_at_startup, "the setting in memory is left as it was");
        assert!(!dir.0.join("updates.json").exists());
    }

    #[test]
    fn a_feed_parameter_is_refused_and_changes_nothing() {
        let mut s = session();
        let before = s.execute("update.status", &json!({})).expect("status");
        // port 1: should the check start anyway, it reaches nothing
        for feed in [json!("http://127.0.0.1:1/latest.json"), json!("https://example.invalid/latest.json"), json!(null)] {
            let err = s.execute("update.check", &json!({"feed": feed, "wait": true})).expect_err("feed is not a parameter");
            assert!(err.to_string().contains("feed"), "the error names the parameter: {err}");
        }
        assert_eq!(s.updater.manifest_url(), None, "the feed in use is still the release's own");
        assert!(!s.updater.checking(), "no check was started");
        let after = s.execute("update.status", &json!({})).expect("status");
        assert_eq!(before, after, "the status is what it was");
    }

    /// The test origin is a Rust-only setting: no `update.*` command reaches it, whatever it is
    /// sent.
    #[test]
    fn no_update_command_changes_the_trusted_origins() {
        let ids: Vec<String> = specs().iter().map(|c| c.id.to_string()).collect();
        assert!(ids.len() >= 8, "{ids:?}");
        for start in [None, Some("http://127.0.0.1:1".to_string())] {
            let mut s = session();
            s.updater.trust_test_origin(start.clone());
            let before = s.updater.options();
            for id in &ids {
                for key in ["feed", "allow", "origin", "extraOrigin", "extra_origin", "trust", "manifestUrl"] {
                    // `feed` with every call, so update.check refuses before it could reach the network
                    let p = json!({"feed": "http://127.0.0.1:1/latest.json", key: "http://127.0.0.1:1"});
                    let _ = s.execute(id, &p);
                }
            }
            let after = s.updater.options();
            assert_eq!(after.extra_origin, start, "the trusted origin is what the test set");
            assert_eq!((after.manifest_url, after.extra_origin), (before.manifest_url, before.extra_origin));
        }
    }
}
