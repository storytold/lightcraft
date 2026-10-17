//! The macOS install: the new app from the disk image replaces the running bundle in place.
//!
//! All of it happens in this process, step by step, and every step that fails leaves the
//! installed app as it was:
//!
//! 1. The program has to run from an `.app` bundle: that bundle is what gets replaced. A program
//!    outside one (a source build, the CLI) is told where the release page is instead; nothing
//!    else on the machine (`/Applications` included) is ever a fallback target.
//! 2. The image is mounted read-only, hidden from Finder, at a fresh folder beside it in the
//!    download cache (`hdiutil attach -nobrowse -readonly -noautoopen`), and detached again
//!    however the install ends.
//! 3. The app is taken by its expected name, `<display name>.app`, never "the first app in the
//!    image"; a missing bundle or a symlink in its place is refused.
//! 4. It is copied beside the installed bundle as `.<name>.app.partial` (`ditto`, without
//!    resource forks, extended attributes or ACLs), and its `Info.plist` must name the release's
//!    version.
//! 5. The swap is two renames in one folder: installed → `.<name>.app.old`, staged → installed.
//!    If the second fails the first is undone; the `.old` copy is removed once the new one is in.
//! 6. The new bundle is opened (`open -n`) once this process has quit, so it does not find the
//!    library still locked by this one.
//!
//! macOS lets a running app's bundle be renamed and replaced: the running process keeps the files
//! it has open. Every tool is called by absolute path and every path is passed as its own
//! argument, so folders with spaces (or quotes) in their names need no quoting.
//!
//! Sources: own design, after the in-process DMG install of craft-launcher
//! (storytold/craft-launcher, Apache-2.0; no code copied) and the documented options of
//! `hdiutil(1)`, `ditto(1)`, `plutil(1)` and `open(1)`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

use super::Outcome;
use crate::error::UpdateError;
use crate::version::Version;

const HDIUTIL: &str = "/usr/bin/hdiutil";
const DITTO: &str = "/usr/bin/ditto";
const PLUTIL: &str = "/usr/bin/plutil";
const SH: &str = "/bin/sh";
/// An `Info.plist` larger than this is not one we wrote.
const MAX_PLIST: u64 = 1 << 20;
/// Waits (up to 10 minutes) for process `$1` to end, then opens bundle `$2` as a new instance.
/// When `$1` is still running after that (an install from the control port, a quit that was
/// cancelled), nothing is opened: a second instance would find the library locked. A fixed text:
/// the pid and the path are arguments, never spliced into it.
const RELAUNCH: &str =
    "i=0; while kill -0 \"$1\" 2>/dev/null; do [ \"$i\" -ge 6000 ] && exit 0; /bin/sleep 0.1; i=$((i+1)); done; exec /usr/bin/open -n \"$2\"";

/// Install the disk image `dmg` (release `version`) over the bundle this program runs from, and
/// start it again once this process has quit.
pub(super) fn install_dmg(dmg: &Path, version: &str) -> Result<Outcome, UpdateError> {
    install_dmg_at(dmg, super::app_bundle().as_deref(), version, &mut relaunch)
}

/// [`install_dmg`] over `bundle` (the program's `.app`, `None` outside one), with the relaunch
/// done by `launch`.
pub(super) fn install_dmg_at(
    dmg: &Path,
    bundle: Option<&Path>,
    version: &str,
    launch: &mut dyn FnMut(&Path) -> Result<(), String>,
) -> Result<Outcome, UpdateError> {
    let Some(bundle) = bundle else {
        return Err(UpdateError::Install(format!(
            "this copy of the app is not running from {}.app, so there is no installed app here to replace; download {} from {} and install it from there",
            lcc_brand::DISPLAY_NAME,
            plain(version),
            release_page(version)
        )));
    };
    let (Some(dir), Some(name)) = (bundle.parent(), bundle.file_name()) else {
        return Err(UpdateError::Install(format!("could not tell which folder {} is in", bundle.display())));
    };
    let name = name.to_string_lossy();
    let staged = dir.join(format!(".{name}.partial"));
    let old = dir.join(format!(".{name}.old"));
    // Gatekeeper runs a quarantined app from a random read-only folder until the user moves it:
    // there is nothing there to replace, and the copy would vanish at the next start
    if bundle.components().any(|c| c.as_os_str() == "AppTranslocation") {
        return Err(move_first(bundle));
    }
    if !bundle.is_dir() {
        // an install interrupted between its two renames leaves the previous app as `.old`
        let hint = if old.is_dir() { format!("; the previous version may be at {}: rename it back to use it", old.display()) } else { String::new() };
        return Err(UpdateError::Install(format!("{} is not an app bundle this update can replace{hint}", bundle.display())));
    }
    if !dmg.is_file() {
        return Err(UpdateError::Install(format!("the disk image {} is not there any more; download the update again", dmg.display())));
    }
    // what an earlier attempt may have left: the installed bundle exists, so these are spare
    clear(&staged)?;
    clear(&old)?;

    {
        let mount = Mount::attach(dmg)?;
        let source = mount.app()?;
        copy(&source, &staged)?;
    } // detached here, before anything is swapped
    if let Err(e) = check_version(&staged, version) {
        remove_quietly(&staged);
        return Err(e);
    }
    swap(bundle, &staged, &old)?;

    match launch(bundle) {
        Ok(()) => Ok(Outcome {
            message: format!("{} was installed as {}. It starts again as soon as this one quits.", plain(version), bundle.display()),
            exit: true,
        }),
        Err(e) => Ok(Outcome {
            message: format!(
                "{} was installed as {}, but it could not be started again ({e}). Quit the app and start it yourself.",
                plain(version),
                bundle.display()
            ),
            exit: true,
        }),
    }
}

/// The version without a leading `v`.
fn plain(version: &str) -> &str {
    let v = version.trim();
    v.strip_prefix(['v', 'V']).unwrap_or(v)
}

/// The release's page on the brand's repository.
fn release_page(version: &str) -> String {
    let repo = lcc_brand::REPOSITORY.trim().trim_end_matches('/').trim_end_matches(".git");
    let repo = if repo.contains("://") { repo.to_string() } else { format!("https://github.com/{repo}") };
    format!("{repo}/releases/tag/v{}", plain(version))
}

/// The bundle's `CFBundleShortVersionString`: the release's `major.minor.patch` (packaging drops
/// a prerelease suffix there, and keeps the full version in `AppFullVersion`).
fn short_version(version: &str) -> &str {
    plain(version).split(['-', '+']).next().unwrap_or_default()
}

/// A disk image mounted read-only at a fresh folder; detached and the folder removed on drop.
struct Mount {
    point: PathBuf,
}

impl Mount {
    fn attach(dmg: &Path) -> Result<Mount, UpdateError> {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = dmg.parent().ok_or_else(|| UpdateError::Install(format!("could not tell which folder {} is in", dmg.display())))?;
        let point = dir.join(format!(".mount-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        // `create_dir`, not `create_dir_all`: the folder must be new and ours
        std::fs::create_dir(&point)
            .map_err(|e| UpdateError::Install(format!("could not make a folder to open the disk image in ({}): {e}", point.display())))?;
        let out = Command::new(HDIUTIL)
            .args(["attach", "-nobrowse", "-readonly", "-noautoopen", "-mountpoint"])
            .arg(&point)
            .arg(dmg)
            .stdin(Stdio::null())
            .output();
        let failed = match out {
            Ok(o) if o.status.success() => None,
            Ok(o) => Some(tool_error(&o.stderr)),
            Err(e) => Some(e.to_string()),
        };
        if let Some(why) = failed {
            remove_quietly(&point);
            return Err(UpdateError::Install(format!("the disk image {} could not be opened ({why}); download the update again", dmg.display())));
        }
        Ok(Mount { point })
    }

    /// The app in the image: `<display name>.app`, a real folder, not a link.
    fn app(&self) -> Result<PathBuf, UpdateError> {
        let name = format!("{}.app", lcc_brand::DISPLAY_NAME);
        let app = self.point.join(&name);
        match std::fs::symlink_metadata(&app) {
            Err(_) => Err(UpdateError::Install(format!("the disk image has no {name}; it is not a release of this app"))),
            Ok(m) if m.file_type().is_symlink() => {
                Err(UpdateError::Install(format!("{name} in the disk image is a link, not the app; it is not a release of this app")))
            }
            Ok(m) if !m.is_dir() => Err(UpdateError::Install(format!("{name} in the disk image is not an app bundle"))),
            Ok(_) => Ok(app),
        }
    }
}

impl Drop for Mount {
    fn drop(&mut self) {
        let detach = |force: bool| {
            let mut cmd = Command::new(HDIUTIL);
            cmd.arg("detach").arg(&self.point).arg("-quiet");
            if force {
                cmd.arg("-force");
            }
            cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
        };
        if !detach(false) && !detach(true) {
            log::warn!("updates: could not detach the disk image at {}", self.point.display());
            return;
        }
        if let Err(e) = std::fs::remove_dir(&self.point) {
            log::warn!("updates: {}: {e}", self.point.display());
        }
    }
}

/// Copy the bundle `from` to `to` (which must not exist); a failed copy leaves nothing behind.
///
/// `--noextattr` also drops `com.apple.quarantine`, so Gatekeeper does not assess the new bundle
/// on its first start. That is acceptable because the download was checked against the release's
/// SHA-256 before it got here; anything that authenticates the release (a signature) belongs to that
/// download step, not here.
fn copy(from: &Path, to: &Path) -> Result<(), UpdateError> {
    let out = Command::new(DITTO).args(["--norsrc", "--noextattr", "--noacl"]).arg(from).arg(to).stdin(Stdio::null()).output();
    let failed = match out {
        Ok(o) if o.status.success() => return Ok(()),
        Ok(o) => tool_error(&o.stderr),
        Err(e) => e.to_string(),
    };
    remove_quietly(to);
    let dir = to.parent().unwrap_or(to);
    if failed.contains("Read-only file system") {
        return Err(move_first(dir));
    }
    Err(UpdateError::Install(format!(
        "could not copy the new version into {} ({failed}); check that the folder is writable and the disk has room. The installed app was not changed",
        dir.display()
    )))
}

/// The app runs from somewhere read-only (its disk image, a translocated copy): it has to be
/// moved somewhere writable before it can be updated.
fn move_first(place: &Path) -> UpdateError {
    UpdateError::Install(format!(
        "the app is running from a read-only place ({}), probably straight from its disk image or a download folder macOS keeps apart. Move {}.app to the Applications folder (or another folder you can write to), start it from there and update again",
        place.display(),
        lcc_brand::DISPLAY_NAME
    ))
}

/// The staged bundle must have a readable `Info.plist` that names the release's version.
fn check_version(bundle: &Path, version: &str) -> Result<(), UpdateError> {
    let contents = bundle.join("Contents");
    // `ditto` copies links as links: a `Contents` pointing elsewhere would show someone else's
    // Info.plist (and run someone else's code)
    if !std::fs::symlink_metadata(&contents).is_ok_and(|m| m.is_dir()) {
        return Err(UpdateError::Install(
            "the new version's Contents folder is missing or a link; the disk image is not a release of this app".into(),
        ));
    }
    let plist = contents.join("Info.plist");
    let unreadable = |why: String| {
        UpdateError::Install(format!(
            "the new version's Info.plist could not be read ({why}); the disk image is damaged or not a release of this app"
        ))
    };
    let meta = std::fs::symlink_metadata(&plist).map_err(|e| unreadable(e.to_string()))?;
    if !meta.is_file() || meta.len() > MAX_PLIST {
        return Err(unreadable("not a property list file".into()));
    }
    // `plutil` reads binary and XML property lists alike and writes XML
    let out = Command::new(PLUTIL)
        .args(["-convert", "xml1", "-o", "-"])
        .arg(&plist)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| unreadable(e.to_string()))?;
    if !out.status.success() {
        return Err(unreadable(tool_error(&out.stderr)));
    }
    let xml = String::from_utf8_lossy(&out.stdout);
    let found = plist_string(&xml, "CFBundleShortVersionString").ok_or_else(|| unreadable("it names no version".into()))?;
    let want = short_version(version);
    if found != want {
        return Err(UpdateError::Install(format!("the disk image holds version {found}, not {want} as the release says; it was not installed")));
    }
    // a prerelease shares its short version with the release: the full one tells them apart
    if let Some(full) = plist_string(&xml, "AppFullVersion")
        && let (Ok(have), Ok(want)) = (Version::parse(&full), Version::parse(version))
        && have.to_text() != want.to_text()
    {
        return Err(UpdateError::Install(format!(
            "the disk image holds version {}, not {} as the release says; it was not installed",
            have.to_text(),
            want.to_text()
        )));
    }
    Ok(())
}

/// The `<string>` value of `key` in the top-level `<dict>` of an XML property list (keys of
/// nested dictionaries, such as the document types', and comments are skipped).
fn plist_string(xml: &str, key: &str) -> Option<String> {
    let mut depth: usize = 0;
    let mut matched = false;
    let mut rest = xml;
    while let Some(lt) = rest.find('<') {
        rest = rest.get(lt..)?;
        if rest.starts_with("<!--") {
            let end = rest.find("-->")?;
            rest = rest.get(end.checked_add(3)?..)?;
            continue;
        }
        let gt = rest.find('>')?;
        let tag = rest.get(1..gt)?;
        rest = rest.get(gt.checked_add(1)?..)?;
        if tag.starts_with(['?', '!']) {
            continue;
        }
        let open = !tag.ends_with('/');
        let name = tag.trim_end_matches('/').split_whitespace().next().unwrap_or_default();
        if matched {
            // the element right after the key is its value
            if name != "string" || !open {
                return None;
            }
            return Some(unescape(rest.get(..rest.find("</string>")?)?.trim()));
        }
        match name {
            "dict" | "array" if open => depth = depth.saturating_add(1),
            "/dict" | "/array" => depth = depth.saturating_sub(1),
            "key" if open => {
                let end = rest.find("</key>")?;
                matched = depth == 1 && unescape(rest.get(..end)?.trim()) == key;
                rest = rest.get(end.checked_add("</key>".len())?..)?;
            }
            _ => {}
        }
    }
    None
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// Put `staged` where `bundle` is, keeping the old bundle as `old` until the new one is in.
pub(super) fn swap(bundle: &Path, staged: &Path, old: &Path) -> Result<(), UpdateError> {
    if let Err(e) = std::fs::rename(bundle, old) {
        remove_quietly(staged);
        return Err(UpdateError::Install(format!(
            "could not move {} aside to make room for the new version ({e}); it was not changed",
            bundle.display()
        )));
    }
    if let Err(e) = std::fs::rename(staged, bundle) {
        let back = std::fs::rename(old, bundle);
        remove_quietly(staged);
        return Err(UpdateError::Install(match back {
            Ok(()) => format!("could not put the new version in place ({e}); the installed app was put back"),
            Err(back) => format!(
                "could not put the new version in place ({e}), and the previous app could not be moved back ({back}): it is at {}; rename it to {} to use it again",
                old.display(),
                bundle.display()
            ),
        }));
    }
    if let Err(e) = std::fs::remove_dir_all(old) {
        log::warn!("updates: could not remove the previous version at {}: {e}", old.display());
    }
    Ok(())
}

/// Remove a file, link or folder at `path` left by an earlier update, if there is one.
fn clear(path: &Path) -> Result<(), UpdateError> {
    let removed = match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => Err(e),
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
    };
    removed
        .map_err(|e| UpdateError::Install(format!("could not remove {}, left by an earlier update ({e}); remove it and try again", path.display())))
}

fn remove_quietly(path: &Path) {
    if let Err(e) = clear(path) {
        log::warn!("updates: {e}");
    }
}

/// A tool's error output, shortened for a dialog.
fn tool_error(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let text = text.trim();
    let short: String = text.chars().take(300).collect();
    if short.is_empty() { "no details".into() } else { short }
}

/// Open `bundle` as a new instance once this process has quit (it holds the library's lock until
/// then). A short-lived `sh` waits for this process id; no file is written and no terminal opens.
fn relaunch(bundle: &Path) -> Result<(), String> {
    Command::new(SH)
        .arg("-c")
        .arg(RELAUNCH)
        .arg("sh")
        .arg(std::process::id().to_string())
        .arg(bundle)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_child| ())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install::bundle_of;

    const RELEASE: &str = "1.2.3";

    /// A scratch folder whose path holds spaces, removed (made writable first) when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Scratch {
            static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("lcc updater {tag} {} {n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch folder");
            Scratch(dir)
        }

        /// The folder the installed app lives in.
        fn apps(&self) -> PathBuf {
            self.0.join("Apps folder")
        }

        /// The download cache the disk image sits in.
        fn cache(&self) -> PathBuf {
            self.0.join("cache dir").join("updates")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = set_mode(&self.apps(), 0o755);
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
    }

    fn app_name() -> String {
        format!("{}.app", lcc_brand::DISPLAY_NAME)
    }

    fn info_plist(short: &str) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n<dict>\n\
             \x20 <key>CFBundleIdentifier</key>\n  <string>{}</string>\n\
             \x20 <key>CFBundleShortVersionString</key>\n  <string>{short}</string>\n\
             </dict>\n</plist>\n",
            lcc_brand::APP_ID
        )
    }

    /// A fake bundle at `at`: `Contents/MacOS/marker` holds `marker`, and `Contents/Info.plist`
    /// gives `short` as its version (no Info.plist when `None`).
    fn fake_app(at: &Path, short: Option<&str>, marker: &str) {
        std::fs::create_dir_all(at.join("Contents/MacOS")).expect("bundle folders");
        std::fs::write(at.join("Contents/MacOS/marker"), marker).expect("marker");
        if let Some(short) = short {
            std::fs::write(at.join("Contents/Info.plist"), info_plist(short)).expect("Info.plist");
        }
    }

    /// The installed app: version 0.0.1, marker "old".
    fn installed(s: &Scratch) -> PathBuf {
        let app = s.apps().join(app_name());
        fake_app(&app, Some("0.0.1"), "old");
        app
    }

    /// A read-only disk image in the cache folder, its root filled by `fill`.
    fn image(s: &Scratch, fill: impl FnOnce(&Path)) -> PathBuf {
        let src = s.0.join("image root");
        std::fs::create_dir_all(&src).expect("image root");
        fill(&src);
        std::fs::create_dir_all(s.cache()).expect("cache folder");
        let dmg = s.cache().join("update image.dmg");
        let out = Command::new("/usr/bin/hdiutil")
            .args(["create", "-quiet", "-ov", "-format", "UDRO", "-fs", "HFS+", "-volname", "Update Test", "-srcfolder"])
            .arg(&src)
            .arg(&dmg)
            .output()
            .expect("hdiutil create runs");
        assert!(out.status.success(), "hdiutil create failed: {}", String::from_utf8_lossy(&out.stderr));
        dmg
    }

    fn marker(app: &Path) -> String {
        std::fs::read_to_string(app.join("Contents/MacOS/marker")).unwrap_or_default()
    }

    /// Hidden names beside the installed app: a `.partial` or `.old` left behind shows up here.
    fn leftovers(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().to_string()).filter(|n| n.starts_with('.')).collect())
            .unwrap_or_default()
    }

    /// The cache folder holds nothing but the image: the mount point is detached and removed.
    fn assert_unmounted(s: &Scratch) {
        let names: Vec<String> =
            std::fs::read_dir(s.cache()).map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect()).unwrap_or_default();
        assert_eq!(names, vec!["update image.dmg".to_string()], "the mount point should be detached and removed");
    }

    fn run(dmg: &Path, bundle: Option<&Path>, version: &str) -> (Result<Outcome, UpdateError>, Vec<PathBuf>) {
        let mut launched = Vec::new();
        let r = install_dmg_at(dmg, bundle, version, &mut |p: &Path| {
            launched.push(p.to_path_buf());
            Ok(())
        });
        (r, launched)
    }

    #[test]
    fn a_program_outside_an_app_bundle_has_no_bundle() {
        assert_eq!(bundle_of(Path::new("/Users/me/src/app/target/release/app")), None);
        assert_eq!(bundle_of(Path::new("/Users/me/Apps/X.app/Contents/MacOS/X")), Some(PathBuf::from("/Users/me/Apps/X.app")),);
    }

    #[test]
    fn a_program_outside_an_app_bundle_is_refused_with_the_release_page() {
        let s = Scratch::new("no bundle");
        let dmg = image(&s, |root| fake_app(&root.join(app_name()), Some(RELEASE), "new"));
        let (r, launched) = run(&dmg, None, RELEASE);
        let err = r.expect_err("nothing to replace outside a bundle");
        let text = err.to_string();
        assert!(text.contains(&format!("/releases/tag/v{RELEASE}")), "the error should point to the release page: {text}");
        assert!(launched.is_empty(), "nothing should be started");
        assert_unmounted(&s);
    }

    #[test]
    fn an_install_in_folders_with_spaces_replaces_the_app_and_relaunches_it() {
        let s = Scratch::new("ok");
        let app = installed(&s);
        let dmg = image(&s, |root| fake_app(&root.join(app_name()), Some(RELEASE), "new"));
        let (r, launched) = run(&dmg, Some(&app), RELEASE);
        let out = r.expect("the install succeeds");
        assert!(out.exit, "the app has to quit so the new one can start");
        assert_eq!(marker(&app), "new", "the new bundle should be in place");
        assert_eq!(launched, vec![app.clone()], "the installed bundle should be started again");
        assert!(leftovers(&s.apps()).is_empty(), "no .partial or .old should be left: {:?}", leftovers(&s.apps()));
        assert_unmounted(&s);
    }

    #[test]
    fn an_unwritable_target_folder_leaves_the_installed_app_unchanged() {
        let s = Scratch::new("read only");
        let app = installed(&s);
        let dmg = image(&s, |root| fake_app(&root.join(app_name()), Some(RELEASE), "new"));
        set_mode(&s.apps(), 0o555).expect("make the folder read-only");
        let (r, launched) = run(&dmg, Some(&app), RELEASE);
        set_mode(&s.apps(), 0o755).expect("make the folder writable again");
        assert!(r.is_err(), "copying into a read-only folder should fail");
        assert_eq!(marker(&app), "old", "the installed app should be untouched");
        assert!(launched.is_empty());
        assert!(leftovers(&s.apps()).is_empty(), "nothing should be left behind: {:?}", leftovers(&s.apps()));
        assert_unmounted(&s);
    }

    #[test]
    fn a_new_bundle_without_info_plist_is_refused_and_cleaned_up() {
        let s = Scratch::new("no plist");
        let app = installed(&s);
        let dmg = image(&s, |root| fake_app(&root.join(app_name()), None, "new"));
        let (r, launched) = run(&dmg, Some(&app), RELEASE);
        let err = r.expect_err("a bundle without Info.plist is not an app");
        assert!(err.to_string().contains("Info.plist"), "{err}");
        assert_eq!(marker(&app), "old", "the installed app should be untouched");
        assert!(launched.is_empty());
        assert!(leftovers(&s.apps()).is_empty(), "nothing should be left behind: {:?}", leftovers(&s.apps()));
        assert_unmounted(&s);
    }

    #[test]
    fn a_failed_swap_puts_the_old_app_back() {
        let s = Scratch::new("swap");
        let app = installed(&s);
        let staged = s.apps().join(format!(".{}.partial", app_name()));
        let old = s.apps().join(format!(".{}.old", app_name()));
        // the staged bundle vanished before the swap: moving it in fails after the old one moved
        let err = swap(&app, &staged, &old).expect_err("nothing to move in");
        assert!(matches!(err, UpdateError::Install(_)), "{err}");
        assert_eq!(marker(&app), "old", "the old app should be back in place");
        assert!(leftovers(&s.apps()).is_empty(), "nothing should be left behind: {:?}", leftovers(&s.apps()));
    }

    #[test]
    fn an_image_whose_app_has_another_name_is_refused() {
        let s = Scratch::new("other name");
        let app = installed(&s);
        let dmg = image(&s, |root| fake_app(&root.join("Something Else.app"), Some(RELEASE), "new"));
        let (r, launched) = run(&dmg, Some(&app), RELEASE);
        let err = r.expect_err("only the app with our name is taken");
        assert!(err.to_string().contains(&app_name()), "the error should name the app it looked for: {err}");
        assert_eq!(marker(&app), "old");
        assert!(launched.is_empty());
        assert!(leftovers(&s.apps()).is_empty(), "nothing should be copied: {:?}", leftovers(&s.apps()));
        assert_unmounted(&s);
    }

    #[test]
    fn an_image_whose_app_is_a_symlink_is_refused() {
        let s = Scratch::new("symlink");
        let app = installed(&s);
        let elsewhere = s.0.join("elsewhere").join(app_name());
        fake_app(&elsewhere, Some(RELEASE), "new");
        let dmg = image(&s, |root| std::os::unix::fs::symlink(&elsewhere, root.join(app_name())).expect("symlink"));
        let (r, launched) = run(&dmg, Some(&app), RELEASE);
        let err = r.expect_err("a symlink is not the app");
        assert!(err.to_string().contains("link"), "{err}");
        assert_eq!(marker(&app), "old");
        assert!(launched.is_empty());
        assert!(leftovers(&s.apps()).is_empty(), "nothing should be copied: {:?}", leftovers(&s.apps()));
        assert_unmounted(&s);
    }

    #[test]
    fn a_new_bundle_with_another_version_is_refused() {
        let s = Scratch::new("version");
        let app = installed(&s);
        let dmg = image(&s, |root| fake_app(&root.join(app_name()), Some("1.2.4"), "new"));
        let (r, launched) = run(&dmg, Some(&app), RELEASE);
        let err = r.expect_err("the image holds another version than the release");
        let text = err.to_string();
        assert!(text.contains("1.2.4") && text.contains(RELEASE), "the error should name both versions: {text}");
        assert_eq!(marker(&app), "old");
        assert!(launched.is_empty());
        assert!(leftovers(&s.apps()).is_empty(), "nothing should be left behind: {:?}", leftovers(&s.apps()));
        assert_unmounted(&s);
    }

    /// A real install of a real build, run by hand (`crates/updater/README.md` → Testing the macOS
    /// install): `UPDATER_TEST_DMG` (the newer build's image), `UPDATER_TEST_BUNDLE` (a scratch copy
    /// of the older `.app`, never under `/Applications`), `UPDATER_TEST_VERSION` (the newer version),
    /// `UPDATER_TEST_CONFIG_DIR` and `UPDATER_TEST_LIBRARY` (scratch settings and library the
    /// relaunched app opens instead of the user's).
    #[test]
    #[ignore = "installs a real build over a scratch copy and starts it; run by hand"]
    fn a_real_build_is_replaced_in_place_and_started_again() {
        let path = |key: &str| PathBuf::from(std::env::var_os(key).unwrap_or_else(|| panic!("set {key}")));
        let (dmg, bundle, cfg, lib) =
            (path("UPDATER_TEST_DMG"), path("UPDATER_TEST_BUNDLE"), path("UPDATER_TEST_CONFIG_DIR"), path("UPDATER_TEST_LIBRARY"));
        let version = std::env::var("UPDATER_TEST_VERSION").expect("set UPDATER_TEST_VERSION");
        assert!(!bundle.starts_with("/Applications"), "run this on a scratch copy, never on /Applications");
        // the relaunched app must not check GitHub: the settings file is the engine's
        // `{version, updates: {checkAtStartup}}`, and anything else reads as "check on"
        let prefs: serde_json::Value =
            std::fs::read_to_string(cfg.join("updates.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        // a settings file newer than version 1 is ignored by the engine, which then checks
        let readable = prefs.get("version").is_none_or(|v| v.as_u64() == Some(1));
        assert!(
            readable && prefs.pointer("/updates/checkAtStartup") == Some(&serde_json::Value::Bool(false)),
            "write {{\"version\": 1, \"updates\": {{\"checkAtStartup\": false}}}} to {}/updates.json first",
            cfg.display()
        );
        // what `relaunch` does, plus scratch settings and library: an app started by `open` gets
        // LaunchServices' environment, not this shell's
        let mut launch = |b: &Path| {
            Command::new("/usr/bin/open")
                .arg("-n")
                .arg("--env")
                .arg(format!("{}_CONFIG_DIR={}", lcc_brand::ENV_PREFIX, cfg.display()))
                .arg(b)
                .arg("--args")
                .arg("--library")
                .arg(&lib)
                .status()
                .map_err(|e| e.to_string())
                .and_then(|s| if s.success() { Ok(()) } else { Err(format!("open exited with {s}")) })
        };
        let out = install_dmg_at(&dmg, Some(&bundle), &version, &mut launch).expect("the install succeeds");
        println!("{}", out.message);
        let plist = std::fs::read_to_string(bundle.join("Contents/Info.plist")).unwrap_or_default();
        assert_eq!(plist_string(&plist, "CFBundleShortVersionString").as_deref(), Some(short_version(&version)));
        assert!(leftovers(bundle.parent().expect("a folder")).iter().all(|n| !n.ends_with(".partial") && !n.ends_with(".old")));
    }

    #[test]
    fn a_new_bundle_of_another_prerelease_is_refused() {
        let s = Scratch::new("prerelease");
        let app = installed(&s);
        let dmg = image(&s, |root| {
            let new = root.join(app_name());
            fake_app(&new, None, "new");
            let plist = info_plist(RELEASE).replace("</dict>", "  <key>AppFullVersion</key>\n  <string>1.2.3-rc.1</string>\n</dict>");
            std::fs::write(new.join("Contents/Info.plist"), plist).expect("Info.plist");
        });
        let (r, launched) = run(&dmg, Some(&app), "1.2.3-rc.2");
        let err = r.expect_err("rc.1 is not the release rc.2");
        assert!(err.to_string().contains("1.2.3-rc.1") && err.to_string().contains("1.2.3-rc.2"), "{err}");
        assert_eq!(marker(&app), "old");
        assert!(launched.is_empty());
        assert!(leftovers(&s.apps()).is_empty(), "nothing should be left behind: {:?}", leftovers(&s.apps()));
        assert_unmounted(&s);
    }

    #[test]
    fn a_new_bundle_whose_contents_is_a_symlink_is_refused() {
        let s = Scratch::new("contents link");
        let app = installed(&s);
        // a bundle whose Contents points elsewhere: its Info.plist would be someone else's
        let elsewhere = s.0.join("elsewhere");
        fake_app(&elsewhere, Some(RELEASE), "new");
        let dmg = image(&s, |root| {
            let new = root.join(app_name());
            std::fs::create_dir_all(&new).expect("bundle folder");
            std::os::unix::fs::symlink(elsewhere.join("Contents"), new.join("Contents")).expect("symlink");
        });
        let (r, launched) = run(&dmg, Some(&app), RELEASE);
        let err = r.expect_err("a Contents that is a link is not this app's");
        assert!(err.to_string().contains("Contents"), "{err}");
        assert_eq!(marker(&app), "old");
        assert!(launched.is_empty());
        assert!(leftovers(&s.apps()).is_empty(), "nothing should be left behind: {:?}", leftovers(&s.apps()));
        assert_unmounted(&s);
    }

    #[test]
    fn an_app_run_from_a_translocated_copy_is_told_to_move_first() {
        let s = Scratch::new("translocated");
        let app = s.0.join("AppTranslocation").join("5D3F").join("d").join(app_name());
        fake_app(&app, Some("0.0.1"), "old");
        let dmg = image(&s, |root| fake_app(&root.join(app_name()), Some(RELEASE), "new"));
        let (r, launched) = run(&dmg, Some(&app), RELEASE);
        let err = r.expect_err("a translocated copy is read-only and temporary");
        assert!(err.to_string().contains("Applications"), "the error should say where to move the app: {err}");
        assert_eq!(marker(&app), "old");
        assert!(launched.is_empty());
    }

    #[test]
    fn an_app_on_a_read_only_disk_is_told_to_move_first() {
        let s = Scratch::new("read-only disk");
        // the installed app lives on a read-only volume (a disk image mounted read-only)
        let ro = s.0.join("ro image");
        fake_app(&ro.join(app_name()), Some("0.0.1"), "old");
        let ro_dmg = s.0.join("ro.dmg");
        let out = Command::new("/usr/bin/hdiutil")
            .args(["create", "-quiet", "-ov", "-format", "UDRO", "-fs", "HFS+", "-volname", "RO Test", "-srcfolder"])
            .arg(&ro)
            .arg(&ro_dmg)
            .output()
            .expect("hdiutil create runs");
        assert!(out.status.success(), "hdiutil create failed: {}", String::from_utf8_lossy(&out.stderr));
        let mounted = Mount::attach(&ro_dmg).expect("mount the read-only disk");
        let app = mounted.point.join(app_name());
        let dmg = image(&s, |root| fake_app(&root.join(app_name()), Some(RELEASE), "new"));
        let (r, launched) = run(&dmg, Some(&app), RELEASE);
        let err = r.expect_err("a read-only disk can't take the new version");
        assert!(err.to_string().contains("Applications"), "the error should say where to move the app: {err}");
        assert_eq!(marker(&app), "old");
        assert!(launched.is_empty());
        assert_unmounted(&s);
    }

    #[test]
    fn only_top_level_keys_are_read_from_info_plist() {
        let xml = "<plist version=\"1.0\"><dict>\
                   <key>UTExportedTypeDeclarations</key><array><dict>\
                   <key>CFBundleShortVersionString</key><string>9.9.9</string>\
                   </dict></array>\
                   <!-- old > <key>CFBundleShortVersionString</key><string>8.8.8</string> -->\
                   <key>CFBundleShortVersionString</key><string>1.2.3</string>\
                   </dict></plist>";
        assert_eq!(plist_string(xml, "CFBundleShortVersionString").as_deref(), Some("1.2.3"));
        let nested_only = "<plist><dict><key>A</key><dict><key>CFBundleShortVersionString</key><string>9.9.9</string></dict></dict></plist>";
        assert_eq!(plist_string(nested_only, "CFBundleShortVersionString"), None, "a nested key is not the bundle's");
    }

    #[test]
    fn the_version_is_read_from_an_xml_info_plist() {
        let xml = info_plist("2.0.1");
        assert_eq!(plist_string(&xml, "CFBundleShortVersionString").as_deref(), Some("2.0.1"));
        assert_eq!(plist_string(&xml, "CFBundleIdentifier").as_deref(), Some(lcc_brand::APP_ID));
        assert_eq!(plist_string(&xml, "CFBundleVersion"), None, "a missing key is None");
        assert_eq!(plist_string("<key>CFBundleShortVersionString</key><true/>", "CFBundleShortVersionString"), None);
        assert_eq!(plist_string("<key>CFBundleShortVersionString</key><string>1.0", "CFBundleShortVersionString"), None);
    }

    #[test]
    fn the_install_runs_no_terminal_and_calls_its_tools_by_absolute_path() {
        // the needles are split so this file's own text does not match them
        let sources = [include_str!("../install.rs"), include_str!("macos.rs")];
        for needle in [concat!("osa", "script"), concat!("\"Term", "inal\""), concat!("do ", "script")] {
            assert!(sources.iter().all(|s| !s.contains(needle)), "the install must not use {needle}");
        }
        assert!(!RELAUNCH.contains(" sleep ") && RELAUNCH.contains("/usr/bin/open"), "the relaunch calls its tools by absolute path");
        for tool in [HDIUTIL, DITTO, PLUTIL, SH] {
            assert!(tool.starts_with('/'), "{tool} must be an absolute path");
        }
        for tool in ["hdiutil", "ditto", "open", "plutil"] {
            let bare = format!("Command::new(\"{tool}\")");
            assert!(sources.iter().all(|s| !s.contains(&bare)), "{tool} must be called by absolute path");
        }
    }
}
