//! Installing what was downloaded.
//!
//! Every platform ends the same way: the new build is running, or the user is told in one sentence
//! what to do next. Nothing here panics, and nothing touches the current installation until the
//! replacement file is complete and checked ([`crate::download`]).
//!
//! | Platform | Artifact | What happens |
//! |---|---|---|
//! | Windows | `.msi` | a helper waits for the app to quit, the installer runs (`msiexec`), and the app starts again |
//! | macOS | `.dmg` | mounted, its app copied beside the running bundle, checked and swapped in, relaunched |
//! | Linux | `.AppImage` | once the app has quit, the running file is replaced and the new one started |
//! | Windows, a copy unpacked from the portable zip | `.zip` | unpacked beside the program, its files swapped in, the new version started once the app has quit, undone if it can't start ([`zipswap`]) |
//! | anywhere else | `.tar.gz` / `.zip` | the folder it is in is shown, to unpack it yourself |
//!
//! The caller quits the app when [`Outcome::exit`] is `true` — the installer or the replacement
//! script needs the running program out of the way — and shows [`Outcome::message`] either way.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::download::Downloaded;
use crate::error::UpdateError;
use crate::target::Kind;

#[cfg(unix)]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
pub mod zipswap;

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

/// What installing `file` (an asset of `kind`) on this copy (`copy`, from [`copy_kind`]) would do.
pub fn plan(kind: Kind, file: &Path, version: &str, copy: &CopyKind) -> Plan {
    match kind {
        Kind::Msi => Plan {
            summary: format!("Run the {version} installer"),
            steps: vec![
                "The app quits so the installer can replace its files.".into(),
                "The installer runs and Windows asks for permission when it needs it.".into(),
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
        Kind::Archive if let Some((dir, _)) = copy.swaps() => Plan {
            summary: format!("Update this copy to {version}"),
            steps: vec![
                "The new version is unpacked beside this one and checked, and replaces this copy's files.".into(),
                "When the app quits, the new version starts. Your settings, and portable.txt with the portable data, stay as they are.".into(),
                "The previous files are kept until the new version is up; if it can't start, they are put back.".into(),
            ],
            destination: Some(dir.to_path_buf()),
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

/// Apply `downloaded`: start the installer, replace the AppImage or this copy's files, or lay the
/// file out. See [`Outcome::exit`]. The copy's kind is worked out again here, right before anything
/// changes.
pub fn install(kind: Kind, downloaded: &Downloaded, version: &str) -> Result<Outcome, UpdateError> {
    install_with(kind, downloaded, version, &copy_kind(), &mut |f: &Path| reveal(f).map_err(|e| e.to_string()))
}

/// [`install`], with the manual install's "show the file" done by `show` (so a headless caller
/// can record it instead of opening the file manager).
pub fn install_with(
    kind: Kind,
    downloaded: &Downloaded,
    version: &str,
    copy: &CopyKind,
    show: &mut dyn FnMut(&Path) -> Result<(), String>,
) -> Result<Outcome, UpdateError> {
    match kind {
        Kind::Msi => install_msi(&downloaded.path),
        Kind::Dmg => install_dmg(&downloaded.path, version),
        Kind::AppImage => install_appimage(&downloaded.path),
        Kind::Archive => match copy.swaps() {
            Some((dir, exe_name)) => install_zip(&downloaded.path, dir, exe_name, version),
            None => show(&downloaded.path).map_err(UpdateError::Install).map(|()| Outcome::manual(&downloaded.path)),
        },
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

/// The `.app` bundle this program runs from, when it is that bundle's app.
fn app_bundle() -> Option<PathBuf> {
    let exe = current_exe()?;
    app_bundle_of(&exe, command_line_tool(&exe))
}

/// The `.app` bundle `exe` is the app of. The CLI (`cli`) isn't, inside the bundle too
/// (`Contents/MacOS/<cli>`): it never replaces the app it is part of, nor starts it
/// ([`cli_in_bundle`]).
fn app_bundle_of(exe: &Path, cli: bool) -> Option<PathBuf> {
    if cli {
        return None;
    }
    bundle_of(exe)
}

/// Set by the CLI when it starts ([`mark_command_line_tool`]).
static COMMAND_LINE_TOOL: AtomicBool = AtomicBool::new(false);

/// Mark this process as the CLI. A desktop package can run the app's own executable as the CLI (a
/// link named `<cli>`, or the Windows console launcher), so the executable no longer tells the two
/// apart: the CLI calls this first, and then never installs an update over the app it is part of
/// (the macOS bundle, a Windows MSI or zip copy).
pub fn mark_command_line_tool() {
    COMMAND_LINE_TOOL.store(true, Ordering::Relaxed);
}

/// Whether this process was marked as the CLI ([`mark_command_line_tool`]).
pub fn is_command_line_tool() -> bool {
    COMMAND_LINE_TOOL.load(Ordering::Relaxed)
}

/// Whether this process, running `exe`, is the CLI: marked so ([`mark_command_line_tool`]), or
/// `exe` has its name ([`is_cli`]).
fn command_line_tool(exe: &Path) -> bool {
    is_command_line_tool() || is_cli(exe)
}

/// Whether `exe` is the CLI (by its brand name, `<cli>` or `<cli>.exe`).
fn is_cli(exe: &Path) -> bool {
    exe.file_stem().is_some_and(|name| name.eq_ignore_ascii_case(lcc_brand::CLI_BINARY))
}

/// Why the CLI (`cli`) running `exe` doesn't install a disk image, when `exe` is inside the app's
/// bundle: the app installs it, and that replaces the CLI too.
#[cfg(any(target_os = "macos", test))]
fn cli_in_bundle(exe: &Path, cli: bool) -> Option<UpdateError> {
    let bundle = bundle_of(exe).filter(|_| cli)?;
    Some(UpdateError::Install(format!(
        "{} is part of {} and doesn't install updates itself: open {} and install the update there, which updates {} too",
        lcc_brand::CLI_BINARY,
        bundle.display(),
        lcc_brand::DISPLAY_NAME,
        lcc_brand::CLI_BINARY
    )))
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

/// What kind of copy this is, for updating it. Worked out once per check ([`copy_kind`]) and
/// handed to [`plan`] and [`install_with`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CopyKind {
    /// Windows Installer installed it: it updates by running the new MSI.
    Msi,
    /// Unpacked from the portable zip on Windows (a release program, `<binary>.exe`, the MSI did
    /// not install): it takes the portable zip. `swappable` when it can replace its own files (its
    /// folder can be written and isn't under Program Files); otherwise the zip is laid out for a
    /// manual install.
    Zip { dir: PathBuf, exe_name: String, swappable: bool },
    /// Anything else: another system, a development build (`app.exe`), or a copy Windows Installer
    /// couldn't say anything about.
    Other,
}

impl CopyKind {
    /// The folder and program of a copy that replaces its own files.
    fn swaps(&self) -> Option<(&Path, &str)> {
        match self {
            CopyKind::Zip { dir, exe_name, swappable: true } => Some((dir, exe_name)),
            _ => None,
        }
    }

    /// Whether this copy replaces its own files from the zip.
    pub fn swappable(&self) -> bool {
        self.swaps().is_some()
    }
}

/// What kind of copy this program is ([`CopyKind`]). Asks Windows Installer and writes a probe file
/// beside the program, so a caller works it out once per check.
pub fn copy_kind() -> CopyKind {
    if !cfg!(windows) {
        return CopyKind::Other;
    }
    let Ok(exe) = std::env::current_exe() else { return CopyKind::Other };
    let (Some(dir), Some(name)) = (exe.parent(), exe.file_name().map(|n| n.to_string_lossy().to_string())) else { return CopyKind::Other };
    if !updates_itself(&name, is_command_line_tool()) {
        return CopyKind::Other;
    }
    match crate::msi::installed_by_msi(&exe) {
        Ok(true) => CopyKind::Msi,
        Ok(false) => {
            // Program Files is where installers put programs: never swapped in place, even by an
            // administrator who can write there (an MSI's folder would be repaired back)
            let swappable = !under_program_files(dir) && zipswap::writable(dir, &name);
            CopyKind::Zip { dir: dir.to_path_buf(), exe_name: name, swappable }
        }
        Err(e) => {
            log::warn!("updates: could not tell whether the installer put {} there ({e}); it is updated by hand", exe.display());
            CopyKind::Other
        }
    }
}

/// Whether `dir` is inside a Program Files folder (`%ProgramFiles%`, `%ProgramW6432%`,
/// `%ProgramFiles(x86)%`).
fn under_program_files(dir: &Path) -> bool {
    let roots: Vec<PathBuf> = ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"]
        .iter()
        .filter_map(std::env::var_os)
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .collect();
    inside_any(dir, &roots)
}

/// Whether `dir` is one of `roots` or inside one, compared without case after resolving both.
fn inside_any(dir: &Path, roots: &[PathBuf]) -> bool {
    let canonical = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()).to_string_lossy().to_lowercase();
    let dir = canonical(dir);
    roots.iter().map(|r| canonical(r)).any(|root| {
        let root = root.trim_end_matches(['\\', '/']);
        dir == root || dir.strip_prefix(root).is_some_and(|rest| rest.starts_with(['\\', '/']))
    })
}

/// The pipe the new version's standard input reads from: held, unwritten, until this process ends,
/// which is how the new version learns that it has ([`zipswap::on_start`]).
static PIPE_TO_NEW_VERSION: std::sync::Mutex<Option<std::process::ChildStdin>> = std::sync::Mutex::new(None);

/// Swap the zip's files into the copy in `dir` and start the new `exe_name` there.
fn install_zip(zip: &Path, dir: &Path, exe_name: &str, version: &str) -> Result<Outcome, UpdateError> {
    let current = crate::Version::current().to_text();
    let files = zipswap::swap_in(zip, dir, exe_name, &current, version, &mut |exe| {
        let mut child = without_start_variables(&mut Command::new(exe))
            .env(lcc_brand::env_var(zipswap::AFTER_UPDATE), "1")
            .stdin(std::process::Stdio::piped())
            .current_dir(dir)
            .spawn()
            .map_err(|e| e.to_string())?;
        *PIPE_TO_NEW_VERSION.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = child.stdin.take();
        Ok(())
    })?;
    log::info!("updates: {files} files of {} replaced by {version}", dir.display());
    Ok(Outcome { message: format!("{version} is in place. The app restarts when it quits."), exit: true })
}

pub use zipswap::Start;

/// At the start of a copy unpacked from the zip, before the library is opened: when a zip update
/// started this process, wait until the version it replaced has ended (however long quitting takes,
/// so the two never run at once); then count this start of an unconfirmed update, or undo an update
/// whose new version never came up and start the previous version ([`Start::Undone`]: end now).
/// The app's own relaunch after its window's renderer failed carries on the start that made it.
pub fn on_start() -> Start {
    let Some(exe) = zip_copy_exe() else { return Start::Go };
    let after_update = lcc_brand::env_is_set(zipswap::AFTER_UPDATE);
    let on_start = if lcc_brand::env_is_set(zipswap::WINDOW_RETRY) { zipswap::on_start_again } else { zipswap::on_start };
    on_start(
        &exe,
        after_update,
        // the pipe closes when the old process ends; nothing is ever written to it
        &mut || {
            let _ = std::io::copy(&mut std::io::stdin().lock(), &mut std::io::sink());
        },
        &mut |previous| {
            without_start_variables(&mut Command::new(previous))
                .current_dir(previous.parent().unwrap_or(Path::new(".")))
                .spawn()
                .map(|_| ())
                .map_err(|e| e.to_string())
        },
    )
}

/// `c` without the variables that tell a start how it was made (this process may have them, as
/// the renderer's relaunch or a version an update started): a version started now is a start of its
/// own, with its own renderer retries. In every spelling [`lcc_brand::env`] reads.
fn without_start_variables(c: &mut Command) -> &mut Command {
    for name in [zipswap::AFTER_UPDATE, zipswap::WINDOW_RETRY] {
        for prefix in std::iter::once(lcc_brand::ENV_PREFIX).chain(lcc_brand::LEGACY_ENV_PREFIXES.iter().copied()) {
            c.env_remove(format!("{prefix}_{name}"));
        }
    }
    c
}

/// In a copy unpacked from the zip, once the app is up (its first frame, its library open): an
/// update that put this version in place worked, so the previous files go.
pub fn confirm_update() {
    if let Some(exe) = zip_copy_exe() {
        zipswap::confirm(&exe);
    }
}

/// In a copy unpacked from the zip, a start of a new version that ends on purpose before it is up
/// (its library is open in another copy): its start is taken back, so the next start doesn't undo
/// the update.
pub fn release_start() {
    if let Some(exe) = zip_copy_exe() {
        zipswap::release_start(&exe);
    }
}

/// This program, when it could be a copy unpacked from the zip (a release program on Windows). No
/// Windows Installer question: only a copy a zip update swapped has a manifest beside it.
fn zip_copy_exe() -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    updates_itself(&exe.file_name()?.to_string_lossy(), is_command_line_tool()).then_some(exe)
}

/// Whether the Windows program `exe_name` (this process, the CLI when `cli`) is the app that
/// updates its own copy: the app's executable, unless it runs as the CLI (started by the console
/// launcher), which leaves its copy to the app.
fn updates_itself(exe_name: &str, cli: bool) -> bool {
    !cli && exe_name.eq_ignore_ascii_case(&format!("{}.exe", lcc_brand::BINARY))
}

/// A PowerShell single-quoted string: a `'` inside is doubled, nothing else is special (no
/// variables, no escapes), so any path survives verbatim.
#[cfg(any(windows, test))]
fn ps_single_quoted(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// The one-line script the Windows update helper runs: wait for the app that started it (by pid; it
/// quits right after, and the wait is what keeps `msiexec` from overlapping the running app, which
/// would make Restart Manager defer replacing its files), run the installer passively without a
/// reboot, and — only when the installer succeeded — start the app again.
///
/// `msiexec` exit codes: `0` is success, `3010` is success with a reboot required (`/norestart`
/// suppressed it). `3010` deliberately starts nothing: files can still be scheduled for rename at
/// the reboot, and Windows starts the app on the user's desktop again after it. Every other code
/// is a failed install, and starting the old program over it would only mislead.
///
/// `relaunch` is the installed program (`None`: its path is not known, so nothing is started — the
/// installer still runs). An upgrade replaces the exe in place at the same path, so the path this
/// process runs from is the new version's path once the installer is done.
#[cfg(any(windows, test))]
fn msi_update_script(pid: u32, msi: &str, relaunch: Option<&str>) -> String {
    // Wait-Process on a pid that is already gone is an error, not a stop: SilentlyContinue makes
    // the helper carry on (the app may have quit before the helper came up).
    let start = relaunch.map_or(String::new(), |exe| format!(" if ($LASTEXITCODE -eq 0) {{ Start-Process -FilePath {} }}", ps_single_quoted(exe)));
    format!("Wait-Process -Id {pid} -ErrorAction SilentlyContinue; msiexec /i {} /passive /norestart;{start}", ps_single_quoted(msi))
}

#[cfg(windows)]
fn install_msi(msi: &Path) -> Result<Outcome, UpdateError> {
    use std::os::windows::process::CommandExt;

    // The installer cannot be started directly: it must not run while this process lives (its
    // files are replaced under it), and nothing would start the app again afterwards — the
    // installer's own "Launch" checkbox lives in its interactive ExitDialog, which `/passive`
    // never shows. Instead a small helper runs the whole sequence ([`msi_update_script`]):
    // Windows PowerShell is built into every Windows, and `-Command` runs it whatever the
    // execution policy is (that only governs script files).
    // DETACHED_PROCESS gives the helper no console of ours to die with, so it outlives this
    // process; `-WindowStyle Hidden` keeps any console it might make off the screen. Both are
    // belt and braces: the helper only waits, runs msiexec, maybe starts the app.
    // `creation_flags` is a safe std API — no `unsafe` here.
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    let relaunch = std::env::current_exe().map(|exe| exe.to_string_lossy().to_string()).ok();
    let script = msi_update_script(std::process::id(), &msi.to_string_lossy(), relaunch.as_deref());
    let child = Command::new("powershell.exe")
        .arg("-NoProfile")
        .arg("-WindowStyle")
        .arg("Hidden")
        .arg("-Command")
        .arg(&script)
        .creation_flags(DETACHED_PROCESS)
        .spawn()
        .map_err(|e| UpdateError::Install(format!("could not start the installer ({}): {e}", msi.display())))?;
    Ok(Outcome {
        message: format!("The installer starts once the app has quit (process {}). The app starts again when the installer is done.", child.id()),
        exit: true,
    })
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
/// happen (if it left a note in `stage`, the download folder), delete the `<file>.old` an
/// update left beside the AppImage this copy runs from (`$APPIMAGE`; nothing without it), and
/// finish what a zip update left in a Windows copy unpacked from the zip ([`zipswap::clean_up`]). Never
/// fails and never panics: anything it can't do is logged.
pub fn clean_up_after_update(stage: &Path) {
    if let Some(exe) = zip_copy_exe()
        && let (Some(dir), Some(name)) = (exe.parent(), exe.file_name())
    {
        zipswap::clean_up(dir, &name.to_string_lossy());
    }
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
        let other = CopyKind::Other;
        let p = plan(Kind::Msi, Path::new("/tmp/a.msi"), "1.2.3", &other);
        assert!(p.summary.contains("1.2.3") && p.summary.contains("installer"));
        // the sequence the MSI path promises, in order: quit, install, start again
        let steps = p.steps.join(" ");
        assert!(steps.contains("quits so the installer"), "{steps}");
        assert!(steps.contains("installer runs"), "{steps}");
        assert!(steps.contains("starts again when the installer is done"), "{steps}");
        assert_eq!(p.restart, Restart::QuitAndRelaunch);
        let a = plan(Kind::Archive, Path::new("/tmp/a.zip"), "1.2.3", &other);
        assert_eq!(a.restart, Restart::Manual);
        assert!(a.destination.is_some());
        let c = plan(Kind::Checksums, Path::new("/tmp/SHA256SUMS.txt"), "1.2.3", &other);
        assert!(c.summary.contains("Nothing to install"));
    }

    /// A version the updater starts (the new one, or the previous one after an undo) is a start of
    /// its own: it doesn't take this process's relaunch or after-update variables, in any spelling.
    #[test]
    fn a_started_version_does_not_inherit_how_this_one_started() {
        let mut c = Command::new("x");
        without_start_variables(&mut c);
        let removed: Vec<String> = c.get_envs().filter(|(_, v)| v.is_none()).map(|(k, _)| k.to_string_lossy().to_string()).collect();
        for prefix in std::iter::once(lcc_brand::ENV_PREFIX).chain(lcc_brand::LEGACY_ENV_PREFIXES.iter().copied()) {
            for name in [zipswap::AFTER_UPDATE, zipswap::WINDOW_RETRY] {
                assert!(removed.iter().any(|k| k.eq_ignore_ascii_case(&format!("{prefix}_{name}"))), "{prefix}_{name}");
            }
        }
    }

    /// The MSI helper waits for the app that started it, runs the installer passively without a
    /// reboot, and starts the app again only when the installer succeeded.
    #[test]
    fn the_msi_helper_waits_installs_and_relaunches() {
        let script = msi_update_script(1234, r"C:\Updates\app 1.2.3.msi", Some(r"C:\Program Files\app\app.exe"));
        assert!(script.starts_with("Wait-Process -Id 1234 -ErrorAction SilentlyContinue; "), "{script}");
        assert!(script.contains(r"msiexec /i 'C:\Updates\app 1.2.3.msi' /passive /norestart;"), "{script}");
        // relaunch on success alone: 3010 (success, reboot required) and every failure start nothing
        assert!(script.ends_with(r"if ($LASTEXITCODE -eq 0) { Start-Process -FilePath 'C:\Program Files\app\app.exe' }"), "{script}");
        // without a program to start, the installer runs all the same
        let bare = msi_update_script(7, r"C:\x.msi", None);
        assert!(bare.ends_with("/norestart;"), "{bare}");
        assert!(!bare.contains("Start-Process"), "{bare}");
    }

    /// Paths keep their meaning inside the helper script: PowerShell single-quoted strings double
    /// their quotes and treat nothing else specially, spaces included.
    #[test]
    fn the_msi_helper_quotes_paths_the_way_powershell_reads_them() {
        assert_eq!(ps_single_quoted(r"C:\plain\path.msi"), r"'C:\plain\path.msi'");
        assert_eq!(ps_single_quoted("it's O'Brien's.msi"), r"'it''s O''Brien''s.msi'");
        let script = msi_update_script(1, r"C:\Users\O'Brien\app 1.2.3.msi", Some(r"C:\it's me\app.exe"));
        assert!(script.contains(r"'C:\Users\O''Brien\app 1.2.3.msi'"), "{script}");
        assert!(script.contains(r"'C:\it''s me\app.exe'"), "{script}");
    }

    /// A zip copy that can replace its own files plans to; one that can't (its folder can't be
    /// written, or it is under Program Files) is a manual install, like any other copy.
    #[test]
    fn a_zip_copy_swaps_only_when_it_can() {
        let copy = |swappable| CopyKind::Zip { dir: PathBuf::from("/tmp/copy"), exe_name: "app.exe".into(), swappable };
        let swaps = plan(Kind::Archive, Path::new("/tmp/a.zip"), "1.2.3", &copy(true));
        assert_eq!((swaps.restart, swaps.destination), (Restart::QuitAndRelaunch, Some(PathBuf::from("/tmp/copy"))));
        assert_eq!(plan(Kind::Archive, Path::new("/tmp/a.zip"), "1.2.3", &copy(false)).restart, Restart::Manual);
        assert_eq!(plan(Kind::Archive, Path::new("/tmp/a.zip"), "1.2.3", &CopyKind::Msi).restart, Restart::Manual);
        assert!(copy(true).swappable() && !copy(false).swappable() && !CopyKind::Msi.swappable());
    }

    #[test]
    fn program_files_is_recognised_whatever_the_case() {
        let root = std::env::temp_dir().join(format!("lcc-pf-{}", std::process::id()));
        let inside = root.join("Some App");
        std::fs::create_dir_all(&inside).unwrap();
        let roots = [root.clone()];
        assert!(inside_any(&inside, &roots));
        assert!(inside_any(&root, &roots));
        assert!(inside_any(&PathBuf::from(inside.to_string_lossy().to_uppercase()), &roots) || !cfg!(windows));
        // a sibling whose name only starts the same is not inside
        let sibling = PathBuf::from(format!("{} (x86)", root.display()));
        std::fs::create_dir_all(&sibling).unwrap();
        assert!(!inside_any(&sibling, &roots));
        assert!(!inside_any(&std::env::temp_dir(), &roots));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&sibling);
    }

    /// The test program is not a release program: no MSI, no zip copy, nothing swapped or waited for.
    #[test]
    fn a_development_build_is_neither_an_msi_nor_a_zip_copy() {
        assert_eq!(copy_kind(), CopyKind::Other);
        assert_eq!(on_start(), Start::Go);
    }

    #[test]
    fn a_disk_image_plan_names_no_folder_when_this_program_is_not_in_an_app_bundle() {
        // a cargo test binary is not inside an .app: there is nothing to replace, and above all
        // not whatever happens to be in /Applications
        assert_eq!(app_bundle(), None);
        let p = plan(Kind::Dmg, Path::new("/tmp/a.dmg"), "1.2.3", &CopyKind::Other);
        assert_eq!(p.destination, None, "a program outside a bundle must not plan an install into {:?}", p.destination);
    }

    /// The CLI shipped inside the app's bundle is not the app: it has no bundle to replace, plans
    /// no install into its folder, and is told to update through the app.
    #[test]
    fn the_cli_inside_the_bundle_never_installs_the_app() {
        let app = Path::new("/Applications/X.app/Contents/MacOS/X");
        let cli = PathBuf::from(format!("/Applications/X.app/Contents/MacOS/{}", lcc_brand::CLI_BINARY));
        assert_eq!(app_bundle_of(app, is_cli(app)), Some(PathBuf::from("/Applications/X.app")));
        assert_eq!(app_bundle_of(&cli, is_cli(&cli)), None, "the CLI is not the bundle's app");
        assert!(cli_in_bundle(app, is_cli(app)).is_none(), "the app installs its own updates");
        let err = cli_in_bundle(&cli, is_cli(&cli)).expect("the CLI in the bundle is refused");
        assert!(err.to_string().contains(lcc_brand::DISPLAY_NAME) && err.to_string().contains("install the update there"), "{err}");
        // a CLI outside any bundle (a zip copy, a source build) is told about the release page instead, as before
        let outside = PathBuf::from(format!("/usr/local/bin/{}", lcc_brand::CLI_BINARY));
        assert!(cli_in_bundle(&outside, is_cli(&outside)).is_none());
    }

    /// The app's own executable started as the CLI (a link named `<cli>` resolves to it) is still
    /// the CLI once marked: it plans no install into the bundle and is told to update through the
    /// app. (The mark is process-wide; no other test here needs it off.)
    #[test]
    fn the_app_executable_marked_as_the_cli_never_installs_the_app() {
        let app = Path::new("/Applications/X.app/Contents/MacOS/X");
        mark_command_line_tool();
        assert!(is_command_line_tool());
        assert_eq!(app_bundle_of(app, command_line_tool(app)), None, "the CLI is not the bundle's app, whatever executable it runs");
        let err = cli_in_bundle(app, command_line_tool(app)).expect("the CLI in the bundle is refused");
        assert!(err.to_string().contains("install the update there"), "{err}");
    }

    /// On Windows the CLI runs the app's executable through the console launcher: it never takes
    /// the app's copy (MSI or zip) as its own to update.
    #[test]
    fn the_app_executable_updates_its_windows_copy_only_as_the_app() {
        let app = format!("{}.exe", lcc_brand::BINARY);
        assert!(updates_itself(&app, false), "the app updates its copy");
        assert!(updates_itself(&app.to_uppercase(), false), "case aside");
        assert!(!updates_itself(&app, true), "the CLI leaves the copy to the app");
        assert!(!updates_itself(&format!("{}.exe", lcc_brand::CLI_BINARY), false), "nor any other program");
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
    fn installs_that_cannot_work_are_refused() {
        // a checksum list is never installed
        {
            let err = install(Kind::Checksums, &downloaded("SHA256SUMS.txt"), "1.2.3").expect_err("refused");
            assert!(matches!(err, UpdateError::Install(_)), "{err}");
            assert!(err.to_string().contains("checksum"));
        }
        // a missing archive is an error not a panic
        {
            let err = install(Kind::Archive, &downloaded("nowhere/a.zip"), "1.2.3").expect_err("refused");
            assert!(matches!(err, UpdateError::Install(_)), "{err}");
        }
        // an appimage that is not there is refused
        {
            if cfg!(all(unix, not(target_os = "macos"))) {
                let err = install(Kind::AppImage, &downloaded("nowhere.AppImage"), "1.2.3").expect_err("refused");
                assert!(err.to_string().contains("not there"), "{err}");
            }
        }
        // a windows installer is refused on other systems
        {
            if cfg!(windows) {
                return;
            }
            let err = install(Kind::Msi, &downloaded("a.msi"), "1.2.3").expect_err("refused");
            assert!(err.to_string().contains("Windows installer"), "{err}");
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
}
