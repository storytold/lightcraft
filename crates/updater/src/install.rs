//! Installing what was downloaded.
//!
//! Every platform ends the same way: the new build is running, or the user is told in one sentence
//! what to do next. Nothing here panics, and nothing touches the current installation until the
//! replacement file is complete and checked ([`crate::download`]).
//!
//! | Platform | Artifact | What happens |
//! |---|---|---|
//! | Windows | `.msi` | the installer runs (`msiexec`), then the app starts again |
//! | macOS | `.dmg` | mounted, its app copied beside the running bundle, checked and swapped in, relaunched |
//! | Linux | `.AppImage` | once the app has quit, the running file is replaced and the new one started |
//! | anywhere else | `.tar.gz` / `.zip` | the folder it is in is shown, to unpack it yourself |
//!
//! The caller quits the app when [`Outcome::exit`] is `true` — the installer or the replacement
//! script needs the running program out of the way — and shows [`Outcome::message`] either way.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::download::Downloaded;
use crate::error::UpdateError;
use crate::target::Kind;

#[cfg(unix)]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

/// What the caller has to do once the install has been started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Restart {
    /// Quit now: the installer or replacement script takes over and starts the app again.
    QuitAndRelaunch,
    /// Keep running: the new build starts when the user starts it.
    QuitOnly,
    /// Quit when convenient: the download is laid out for a manual install.
    Manual,
}

/// What installing this file will do, in the order it happens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// One line for the dialog's title area.
    pub summary: String,
    /// The steps, in order, as short sentences.
    pub steps: Vec<String>,
    /// Where the file ends up, when that is a place we know.
    pub destination: Option<PathBuf>,
    pub restart: Restart,
}

/// The result of starting the install.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    /// What to tell the user.
    pub message: String,
    /// Whether the caller must quit for it to finish.
    pub exit: bool,
}

impl Outcome {
    /// The outcome for a manual install: the file is there, the user unpacks it.
    pub fn manual(file: &Path) -> Outcome {
        Outcome {
            message: format!("The update was downloaded to {}. Quit the app and unpack it over this installation.", file.display()),
            exit: false,
        }
    }
}

/// What installing `file` (an asset of `kind`) on this machine would do.
pub fn plan(kind: Kind, file: &Path, version: &str) -> Plan {
    match kind {
        Kind::Msi => Plan {
            summary: format!("Run the {version} installer"),
            steps: vec![
                "The app quits so the installer can replace its files.".into(),
                "Windows asks for permission when the installer needs it.".into(),
                "The app starts again when the installer is done.".into(),
            ],
            destination: None,
            restart: Restart::QuitAndRelaunch,
        },
        Kind::Dmg => Plan {
            summary: format!("Install {version} from the disk image"),
            steps: vec![
                "The disk image is opened and its app copied next to this one, then checked.".into(),
                "The new app takes this one's place (this one is put back if anything fails).".into(),
                "The new version starts as soon as this one quits.".into(),
            ],
            destination: install_dir(),
            restart: Restart::QuitAndRelaunch,
        },
        Kind::AppImage => Plan {
            summary: format!("Replace this program with {version}"),
            steps: vec![
                "Once this program has quit, the new version takes its place (the old one is kept as .old).".into(),
                "The new version starts as soon as this one has quit.".into(),
            ],
            destination: current_exe(),
            restart: Restart::QuitAndRelaunch,
        },
        Kind::Archive => Plan {
            summary: format!("Download {version} and install it by hand"),
            steps: vec![
                format!("The file is saved beside this download ({}) and its folder is shown.", file.display()),
                "Quit the app and unpack it over this installation.".into(),
            ],
            destination: Some(file.to_path_buf()),
            restart: Restart::Manual,
        },
        Kind::Checksums => Plan {
            summary: "Nothing to install: this is the release's checksum list".into(),
            steps: vec!["Pick the file for this system instead.".into()],
            destination: None,
            restart: Restart::Manual,
        },
    }
}

/// Apply `downloaded`: start the installer, replace the AppImage, or lay the file out. See
/// [`Outcome::exit`].
pub fn install(kind: Kind, downloaded: &Downloaded, version: &str) -> Result<Outcome, UpdateError> {
    install_with(kind, downloaded, version, &mut |f: &Path| reveal(f).map_err(|e| e.to_string()))
}

/// [`install`], with the manual install's "show the file" done by `show` (so a headless caller
/// can record it instead of opening the file manager).
pub fn install_with(
    kind: Kind,
    downloaded: &Downloaded,
    version: &str,
    show: &mut dyn FnMut(&Path) -> Result<(), String>,
) -> Result<Outcome, UpdateError> {
    match kind {
        Kind::Msi => install_msi(&downloaded.path),
        Kind::Dmg => install_dmg(&downloaded.path, version),
        Kind::AppImage => install_appimage(&downloaded.path),
        Kind::Archive => show(&downloaded.path).map_err(UpdateError::Install).map(|()| Outcome::manual(&downloaded.path)),
        Kind::Checksums => Err(UpdateError::Install("that file is the release's checksum list, not an update".into())),
    }
    .map_err(|e| match e {
        UpdateError::Install(_) => e,
        other => UpdateError::Install(other.to_string()),
    })
    .map(|mut out| {
        if out.message.is_empty() {
            out.message = format!("Installing {version} has started.");
        }
        out
    })
}

/// The `.app` bundle this program runs from, when it is inside one.
fn app_bundle() -> Option<PathBuf> {
    bundle_of(&current_exe()?)
}

/// The `.app` bundle the executable `exe` is inside, if any.
fn bundle_of(exe: &Path) -> Option<PathBuf> {
    exe.ancestors().find(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("app"))).map(Path::to_path_buf)
}

/// The folder the app bundle is in. `None` outside a bundle: then there is nothing to replace,
/// and no other folder (`/Applications` included) is ever a fallback.
fn install_dir() -> Option<PathBuf> {
    app_bundle()?.parent().map(Path::to_path_buf)
}

fn current_exe() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(std::fs::canonicalize(&exe).unwrap_or(exe))
}

// ---------- Windows

#[cfg(windows)]
fn install_msi(msi: &Path) -> Result<Outcome, UpdateError> {
    // `/passive` shows the installer's progress bar without asking questions, `/norestart` keeps
    // the machine up. The installer needs this process gone to replace the exe, so it is started
    // and the caller exits; when it needs elevation Windows asks through `msiexec` itself.
    // `Command` quotes the path itself when it holds spaces; quoting it again here would hand
    // msiexec literal `\"` characters.
    let child = Command::new("msiexec")
        .arg("/i")
        .arg(msi)
        .arg("/passive")
        .arg("/norestart")
        .spawn()
        .map_err(|e| UpdateError::Install(format!("could not start the installer ({}): {e}", msi.display())))?;
    Ok(Outcome { message: format!("The installer is running (process {}). The app will start again when it finishes.", child.id()), exit: true })
}

#[cfg(not(windows))]
fn install_msi(msi: &Path) -> Result<Outcome, UpdateError> {
    Err(UpdateError::Install(format!("{} is a Windows installer; this system runs it another way", msi.display())))
}

// ---------- macOS (the install itself is in `install/macos.rs`)

#[cfg(target_os = "macos")]
fn install_dmg(dmg: &Path, version: &str) -> Result<Outcome, UpdateError> {
    macos::install_dmg(dmg, version)
}

#[cfg(not(target_os = "macos"))]
fn install_dmg(dmg: &Path, _version: &str) -> Result<Outcome, UpdateError> {
    Err(UpdateError::Install(format!("{} is a macOS disk image; this system runs it another way", dmg.display())))
}

// ---------- Linux (and other Unix)

#[cfg(all(unix, not(target_os = "macos")))]
fn install_appimage(new: &Path) -> Result<Outcome, UpdateError> {
    use std::os::unix::fs::PermissionsExt;

    if !new.is_file() {
        return Err(UpdateError::Install(format!("{} is not there", new.display())));
    }
    // Inside an AppImage `current_exe` is the binary in the read-only squashfs mount; the file
    // to replace is the one the runtime names in `$APPIMAGE`. Without it this copy was installed
    // another way (a package, a tarball, `cargo install`) and must not be overwritten by an
    // AppImage.
    let running = appimage_path(std::env::var_os("APPIMAGE")).ok_or_else(|| {
        UpdateError::Install(format!(
            "this copy of the app is not running from an AppImage; the update was downloaded to {}, install it the way this copy was installed",
            new.display()
        ))
    })?;
    // the download folder (in the user's cache): the failure note goes there, and the script too
    // when there is no `$XDG_RUNTIME_DIR`
    let stage = new.parent().unwrap_or_else(|| Path::new("."));
    // the new file goes beside the old one first, so replacing it is a rename on the same file
    // system (atomic, and it cannot fail half way across devices)
    let staged = running.with_file_name(format!("{}.new-{}", file_name(&running), std::process::id()));
    std::fs::copy(new, &staged).map_err(|e| UpdateError::Install(format!("could not place the new version next to {}: {e}", running.display())))?;
    if let Err(e) = std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755)) {
        let _ = std::fs::remove_file(&staged);
        return Err(UpdateError::Install(format!("could not make {} runnable: {e}", staged.display())));
    }
    let old = running.with_file_name(format!("{}.old", file_name(&running)));
    let note = stage.join(linux::FAILURE_NOTE);
    // A script finishes the swap once this process has exited: writing into a running executable
    // fails (ETXTBSY), but renaming another file over it is allowed, so the new one is renamed
    // into its place (atomically: the path always names a whole AppImage) and started.
    // Waiting for the exit matters: the new version would otherwise find the library still open.
    let base = linux::script_base(std::env::var_os("XDG_RUNTIME_DIR"), stage);
    linux::start(
        &base,
        |folder| {
            linux::swap_script(&linux::Swap {
                running: &running,
                staged: &staged,
                old: &old,
                note: &note,
                folder,
                pid: std::process::id(),
                wait: linux::WAIT_FOR_EXIT,
            })
        },
        &staged,
    )?;
    Ok(Outcome {
        message: format!(
            "The new version replaces {} and starts once the app has quit. The previous file is kept as {} until then.",
            running.display(),
            old.display()
        ),
        exit: true,
    })
}

#[cfg(not(all(unix, not(target_os = "macos"))))]
fn install_appimage(new: &Path) -> Result<Outcome, UpdateError> {
    Err(UpdateError::Install(format!("{} is an AppImage; this system installs it another way", new.display())))
}

/// Whether this copy runs from an AppImage (the runtime names the file in `$APPIMAGE`). Only such a
/// copy can take an AppImage update; a package, a tarball or a Flatpak can't.
pub fn runs_from_appimage() -> bool {
    std::env::var_os("APPIMAGE").is_some_and(|v| Path::new(&v).is_file())
}

/// The AppImage file to replace, from the runtime's `$APPIMAGE` (an existing file, canonical).
#[cfg(all(unix, not(target_os = "macos")))]
fn appimage_path(var: Option<std::ffi::OsString>) -> Option<PathBuf> {
    let path = PathBuf::from(var?);
    if !path.is_file() {
        return None;
    }
    Some(std::fs::canonicalize(&path).unwrap_or(path))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| lcc_brand::BINARY.to_string())
}

/// Tidy up after an update, once, when the app starts: log why the last AppImage swap did not
/// happen (if it left a note in `stage`, the download folder), and delete the `<file>.old` an
/// update left beside the AppImage this copy runs from (`$APPIMAGE`; nothing without it). Never
/// fails and never panics: anything it can't do is logged.
pub fn clean_up_after_update(stage: &Path) {
    #[cfg(unix)]
    if let Some(note) = linux::take_failure_note(stage) {
        log::warn!("updates: the last update was not installed: {note}");
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    match linux::remove_previous(std::env::var_os("APPIMAGE")) {
        Ok(Some(old)) => log::info!("updates: deleted the previous version, {}", old.display()),
        Ok(None) => {}
        Err(e) => log::warn!("updates: could not delete the previous version beside $APPIMAGE: {e}"),
    }
    #[cfg(not(unix))]
    let _ = stage;
}

// ---------- anything else: show the file

/// Show `file` in the platform's file manager.
pub fn reveal(file: &Path) -> Result<(), UpdateError> {
    if !file.is_file() {
        return Err(UpdateError::Install(format!("{} is not there any more", file.display())));
    }
    let dir = file.parent().unwrap_or(file);
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("explorer");
        c.arg(format!("/select,{}", file.display()));
        c
    } else if cfg!(target_os = "macos") {
        let mut c = Command::new("/usr/bin/open");
        c.arg("-R").arg(file);
        c
    } else {
        let mut c = Command::new("xdg-open");
        c.arg(dir);
        c
    };
    cmd.spawn().map(|_| ()).map_err(|e| UpdateError::Install(format!("could not show {}: {e}", dir.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn downloaded(name: &str) -> Downloaded {
        Downloaded { path: PathBuf::from("/tmp").join(name), bytes: 1, sha256: String::new(), name: name.into() }
    }

    #[test]
    fn plans_say_what_happens() {
        let p = plan(Kind::Msi, Path::new("/tmp/a.msi"), "1.2.3");
        assert!(p.summary.contains("1.2.3") && p.summary.contains("installer"));
        assert!(!p.steps.is_empty());
        assert_eq!(p.restart, Restart::QuitAndRelaunch);
        let a = plan(Kind::Archive, Path::new("/tmp/a.zip"), "1.2.3");
        assert_eq!(a.restart, Restart::Manual);
        assert!(a.destination.is_some());
        let c = plan(Kind::Checksums, Path::new("/tmp/SHA256SUMS.txt"), "1.2.3");
        assert!(c.summary.contains("Nothing to install"));
    }

    #[test]
    fn a_disk_image_plan_names_no_folder_when_this_program_is_not_in_an_app_bundle() {
        // a cargo test binary is not inside an .app: there is nothing to replace, and above all
        // not whatever happens to be in /Applications
        assert_eq!(app_bundle(), None);
        let p = plan(Kind::Dmg, Path::new("/tmp/a.dmg"), "1.2.3");
        assert_eq!(p.destination, None, "a program outside a bundle must not plan an install into {:?}", p.destination);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn installing_a_disk_image_from_outside_an_app_bundle_points_to_the_release_page() {
        // the test binary is not inside an .app: the install is refused before the image is read
        let err = install(Kind::Dmg, &downloaded("nowhere/a.dmg"), "1.2.3").expect_err("refused");
        assert!(matches!(err, UpdateError::Install(_)), "{err}");
        assert!(err.to_string().contains("/releases/tag/v1.2.3"), "the error should point to the release page: {err}");
    }

    #[test]
    fn a_checksum_list_is_never_installed() {
        let err = install(Kind::Checksums, &downloaded("SHA256SUMS.txt"), "1.2.3").expect_err("refused");
        assert!(matches!(err, UpdateError::Install(_)), "{err}");
        assert!(err.to_string().contains("checksum"));
    }

    #[test]
    fn a_windows_installer_is_refused_on_other_systems() {
        if cfg!(windows) {
            return;
        }
        let err = install(Kind::Msi, &downloaded("a.msi"), "1.2.3").expect_err("refused");
        assert!(err.to_string().contains("Windows installer"), "{err}");
    }

    #[test]
    fn a_missing_archive_is_an_error_not_a_panic() {
        let err = install(Kind::Archive, &downloaded("nowhere/a.zip"), "1.2.3").expect_err("refused");
        assert!(matches!(err, UpdateError::Install(_)), "{err}");
    }

    #[test]
    fn an_appimage_that_is_not_there_is_refused() {
        if cfg!(all(unix, not(target_os = "macos"))) {
            let err = install(Kind::AppImage, &downloaded("nowhere.AppImage"), "1.2.3").expect_err("refused");
            assert!(err.to_string().contains("not there"), "{err}");
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn only_a_real_appimage_file_is_replaced() {
        assert_eq!(appimage_path(None), None, "not started from an AppImage");
        assert_eq!(appimage_path(Some("/nowhere/app.AppImage".into())), None);
        let exe = current_exe().expect("test exe");
        assert!(appimage_path(Some(exe.clone().into_os_string())).is_some());
    }

    #[test]
    fn the_exe_is_known() {
        // the test binary is a real running executable; `current_exe` must find it
        assert!(current_exe().is_some());
        if cfg!(target_os = "macos") {
            assert_eq!(app_bundle(), None, "a cargo test binary is not inside an .app");
        }
    }
}
