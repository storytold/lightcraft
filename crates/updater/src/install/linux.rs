//! Replacing a running AppImage: the swap script, the private folder it is written to, and the
//! clean-up on the next start.
//!
//! The app does not swap its own file while it runs: it starts a small POSIX `sh` script and quits.
//! The script waits for this process to exit (`kill -0 <pid>`, up to [`WAIT_FOR_EXIT`]), keeps the
//! running file as `<file>.old` (a hard link, else a copy), then renames the staged
//! `<file>.new-<pid>` over the running path in one atomic rename and starts it. The running path
//! names a whole AppImage at every moment, even if the script is stopped half way. If the app is
//! still running when the wait ends, or a step fails, the original file stays, the staged file is
//! deleted, nothing is started, and the reason is written to [`FAILURE_NOTE`] in the download
//! folder for the next start to log.
//!
//! The script lives in a fresh folder only this user can open (mode 0700, under
//! `$XDG_RUNTIME_DIR`, else the download folder), created with `mkdir` and `O_EXCL` so a file or
//! symbolic link planted under that name is an error rather than followed. It deletes itself and its
//! folder when it finishes.
//!
//! Everything here is `cfg(unix)`, not Linux only, so the script is tested by running it on any
//! Unix; only Linux installs AppImages with it.

// Only Linux replaces AppImages; elsewhere these are reached from the tests alone.
#![cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]

use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use crate::error::UpdateError;

/// How long the script waits for the app to quit before it gives up on the update.
pub(super) const WAIT_FOR_EXIT: Duration = Duration::from_secs(30);

/// The file in the download folder where a swap that did not happen says why.
pub(super) const FAILURE_NOTE: &str = "update-failed.txt";

/// The script's name inside its private folder.
const SCRIPT_NAME: &str = "swap.sh";

/// The longest failure note read back (it is one sentence).
const MAX_NOTE_BYTES: u64 = 4096;

/// `'…'` for a POSIX shell: an embedded `'` is closed, escaped and reopened.
pub(super) fn sh_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// What one swap moves, where it reports, and whom it waits for.
pub(super) struct Swap<'a> {
    /// The AppImage the app runs from (`$APPIMAGE`).
    pub running: &'a Path,
    /// The new version, already beside it and runnable.
    pub staged: &'a Path,
    /// Where the running file is kept once replaced.
    pub old: &'a Path,
    /// Where a failure is written ([`FAILURE_NOTE`]).
    pub note: &'a Path,
    /// The script's private folder, deleted with the script when it finishes.
    pub folder: &'a Path,
    /// The process to wait for (this app).
    pub pid: u32,
    /// How long to wait for it ([`WAIT_FOR_EXIT`]).
    pub wait: Duration,
}

/// The swap script for `swap`: POSIX `sh`, every path quoted as one argument.
pub(super) fn swap_script(swap: &Swap) -> String {
    let q = |p: &Path| sh_quote(&p.to_string_lossy());
    // polled every 0.1 s; at least one poll, so a zero wait still checks once
    let tenths = swap.wait.as_millis().div_ceil(100).max(1);
    let secs = swap.wait.as_secs_f64();
    format!(
        "#!/bin/sh\n\
         # written by the app's updater: wait for the app to quit, put the new AppImage in its place, start it\n\
         pid={pid}\n\
         running={running}\n\
         staged={staged}\n\
         old={old}\n\
         note={note}\n\
         folder={folder}\n\
         # the tools are looked up on PATH (NixOS has no /bin/mv); an empty PATH still finds them\n\
         PATH=\"${{PATH:+$PATH:}}/usr/bin:/bin\"\n\
         export PATH\n\
         trap 'rm -f -- \"$folder\"/{script}; rmdir -- \"$folder\" 2>/dev/null' EXIT\n\
         fail() {{\n\
         \x20 rm -f -- \"$staged\"\n\
         \x20 printf '%s\\n' \"$1\" > \"$note\" 2>/dev/null\n\
         \x20 exit 1\n\
         }}\n\
         n=0\n\
         # `kill -0` takes a zombie for alive: an app nobody reaps makes the wait time out and the\n\
         # update fail with the note, never swap early (the AppImage runtime and launchers reap it)\n\
         while kill -0 \"$pid\" 2>/dev/null; do\n\
         \x20 if [ \"$n\" -ge {tenths} ]; then fail 'The app was still running {secs} s after it was asked to quit, so the update was not installed. The previous version is unchanged.'; fi\n\
         \x20 sleep 0.1\n\
         \x20 n=$((n + 1))\n\
         done\n\
         if [ ! -f \"$staged\" ]; then fail 'The downloaded update was gone before it could be installed. The previous version is unchanged.'; fi\n\
         # whatever is left at .old goes first: `rm -f` removes a file or a link (never what it points\n\
         # to); a folder stays and stops the update, since `ln` and `cp` would put the copy inside it\n\
         rm -f -- \"$old\" 2>/dev/null\n\
         if [ -d \"$old\" ] && [ ! -L \"$old\" ]; then fail \"A folder is in the way at $old, so the update was not installed. The previous version is unchanged.\"; fi\n\
         if [ -e \"$old\" ] || [ -L \"$old\" ]; then fail \"$old could not be removed to keep the previous version there, so the update was not installed. The previous version is unchanged.\"; fi\n\
         if ! ln -- \"$running\" \"$old\" 2>/dev/null && ! cp -p -- \"$running\" \"$old\" 2>/dev/null; then\n\
         \x20 rm -f -- \"$old\"\n\
         \x20 fail 'A copy of the running version could not be kept, so the update was not installed. The previous version is unchanged.'\n\
         fi\n\
         if ! mv -f -- \"$staged\" \"$running\" 2>/dev/null; then fail 'The new version could not be put in place; the previous version is unchanged.'; fi\n\
         if command -v setsid >/dev/null 2>&1; then\n\
         \x20 setsid \"$running\" >/dev/null 2>&1 < /dev/null &\n\
         else\n\
         \x20 \"$running\" >/dev/null 2>&1 < /dev/null &\n\
         fi\n",
        pid = swap.pid,
        running = q(swap.running),
        staged = q(swap.staged),
        old = q(swap.old),
        note = q(swap.note),
        folder = q(swap.folder),
        script = SCRIPT_NAME,
    )
}

/// Where the script's folder goes: `$XDG_RUNTIME_DIR` (per user, 0700 by spec) when it is set to
/// an existing absolute folder, else `fallback` (the download folder in the user's cache).
pub(super) fn script_base(runtime_dir: Option<OsString>, fallback: &Path) -> PathBuf {
    runtime_dir.map(PathBuf::from).filter(|p| p.is_absolute() && p.is_dir()).unwrap_or_else(|| fallback.to_path_buf())
}

/// Create a fresh folder under `base` that only this user can open. The name is new each time;
/// one that is already taken (by a file, a folder or a symbolic link) is skipped, never followed.
pub(super) fn private_folder(base: &Path) -> io::Result<PathBuf> {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    let mut last = io::Error::new(io::ErrorKind::AlreadyExists, "no free folder name");
    for _ in 0..16 {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let name = format!("{}-update-{}-{:08x}{:04x}", lcc_brand::BINARY, std::process::id(), nanos, n);
        let path = base.join(name);
        match create_private_dir(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => last = e,
            Err(e) => return Err(e),
        }
    }
    Err(last)
}

/// `mkdir path` with mode 0700: fails on anything already there, a symbolic link included.
pub(super) fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new().mode(0o700).create(path)
}

/// Write `body` as the folder's script, mode 0700, with `O_EXCL`: a file or symbolic link already
/// there is an error, never written through.
pub(super) fn write_script(folder: &Path, body: &str) -> io::Result<PathBuf> {
    let path = folder.join(SCRIPT_NAME);
    let mut file = fs::OpenOptions::new().write(true).create_new(true).mode(0o700).open(&path)?;
    file.write_all(body.as_bytes())?;
    file.sync_all()?;
    Ok(path)
}

/// Write the swap script into a new private folder under `base` and start it. Any failure deletes
/// the folder and the staged file, so nothing is left half done.
pub(super) fn start(base: &Path, swap_for: impl FnOnce(&Path) -> String, staged: &Path) -> Result<PathBuf, UpdateError> {
    let started = (|| {
        let folder = private_folder(base).map_err(|e| UpdateError::Install(format!("could not make a private folder in {}: {e}", base.display())))?;
        let run = || -> Result<PathBuf, UpdateError> {
            let script = write_script(&folder, &swap_for(&folder))
                .map_err(|e| UpdateError::Install(format!("could not write the replacement script in {}: {e}", folder.display())))?;
            // its own process group, so a signal meant for the app (Ctrl-C in its terminal) does
            // not stop the swap half way
            Command::new("/bin/sh")
                .arg(&script)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0)
                .spawn()
                .map_err(|e| UpdateError::Install(format!("could not start the replacement: {e}")))?;
            Ok(script)
        };
        run().inspect_err(|_| {
            let _ = fs::remove_dir_all(&folder);
        })
    })();
    started.inspect_err(|_| {
        let _ = fs::remove_file(staged);
    })
}

/// On the start after an update: delete `<file>.old` beside the AppImage that `appimage` (the
/// `$APPIMAGE` value) names. Only that one regular file goes; without `$APPIMAGE`, or when it does
/// not name an existing file, nothing is touched. Returns the deleted path.
pub(super) fn remove_previous(appimage: Option<OsString>) -> io::Result<Option<PathBuf>> {
    let Some(var) = appimage.filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    let running = PathBuf::from(var);
    if !running.is_file() {
        return Ok(None);
    }
    let running = fs::canonicalize(&running).unwrap_or(running);
    let Some(name) = running.file_name() else {
        return Ok(None);
    };
    let mut old_name = name.to_os_string();
    old_name.push(".old");
    let old = running.with_file_name(old_name);
    match fs::symlink_metadata(&old) {
        // a folder or a link under that name is not ours
        Ok(m) if m.file_type().is_file() => fs::remove_file(&old).map(|()| Some(old)),
        Ok(_) => Ok(None),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// The reason a swap left in `stage` ([`FAILURE_NOTE`]), read once and deleted.
pub(super) fn take_failure_note(stage: &Path) -> Option<String> {
    let path = stage.join(FAILURE_NOTE);
    let meta = fs::symlink_metadata(&path).ok()?;
    if !meta.file_type().is_file() {
        return None;
    }
    let mut text = String::new();
    let read = fs::File::open(&path).and_then(|f| f.take(MAX_NOTE_BYTES).read_to_string(&mut text));
    if let Err(e) = fs::remove_file(&path) {
        log::warn!("updates: could not delete {}: {e}", path.display());
    }
    match read {
        Ok(_) => Some(text.trim().to_string()).filter(|t| !t.is_empty()),
        Err(e) => Some(format!("{} could not be read: {e}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::process::Child;
    use std::time::Instant;

    /// A fresh folder for one test, with a space and a quote in its name (AC-6), deleted when
    /// the test ends, failed or not.
    struct Scratch(PathBuf);

    impl std::ops::Deref for Scratch {
        type Target = PathBuf;
        fn deref(&self) -> &PathBuf {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(tag: &str) -> Scratch {
        let d = std::env::temp_dir().join(format!("lcc-updater-swap {tag} it's-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).expect("scratch dir");
        Scratch(d)
    }

    /// A stand-in AppImage: a script that writes `who` to `marker` when started.
    fn stand_in(path: &Path, who: &str, marker: &Path) {
        fs::write(path, format!("#!/bin/sh\nprintf '%s' {} > {}\n", sh_quote(who), sh_quote(&marker.to_string_lossy()))).expect("stand-in");
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod");
    }

    fn wait_until(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < limit {
            if done() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        done()
    }

    fn finished(script: &mut Child, limit: Duration) -> bool {
        wait_until(limit, || matches!(script.try_wait(), Ok(Some(_))))
    }

    struct Setup {
        dir: Scratch,
        running: PathBuf,
        staged: PathBuf,
        old: PathBuf,
        note: PathBuf,
        marker: PathBuf,
    }

    fn setup(tag: &str) -> Setup {
        let dir = scratch(tag);
        let running = dir.join("App Image's.AppImage");
        let staged = dir.join("App Image's.AppImage.new-1");
        let old = dir.join("App Image's.AppImage.old");
        let note = dir.join(FAILURE_NOTE);
        let marker = dir.join("started");
        stand_in(&running, "old", &marker);
        stand_in(&staged, "new", &marker);
        Setup { dir, running, staged, old, note, marker }
    }

    /// Write the script for `s` in a private folder and run it with `/bin/sh`.
    fn run_swap(s: &Setup, pid: u32, wait: Duration) -> (Child, PathBuf) {
        run_swap_with(s, pid, wait, None)
    }

    /// [`run_swap`], with the folder `shims` searched first for the script's tools, and in a
    /// process group of its own (so a test can stop it and its tools together).
    fn run_swap_with(s: &Setup, pid: u32, wait: Duration, shims: Option<&Path>) -> (Child, PathBuf) {
        let folder = private_folder(&s.dir).expect("private folder");
        let body = swap_script(&Swap { running: &s.running, staged: &s.staged, old: &s.old, note: &s.note, folder: &folder, pid, wait });
        let script = write_script(&folder, &body).expect("script");
        let mut cmd = Command::new("/bin/sh");
        cmd.arg(&script).process_group(0);
        if let Some(shims) = shims {
            let path = std::env::var_os("PATH").unwrap_or_default();
            let mut dirs = vec![shims.to_path_buf()];
            dirs.extend(std::env::split_paths(&path));
            cmd.env("PATH", std::env::join_paths(dirs).expect("PATH"));
        }
        (cmd.spawn().expect("run the script"), folder)
    }

    /// Put a `tool` in `dir/shims` (made if needed) that runs `before` (sh), then the real tool,
    /// found on this PATH now (no fixed `/bin` path: NixOS and the Nix sandbox have none).
    fn shim(dir: &Path, tool: &str, before: &str) -> PathBuf {
        let shims = dir.join("shims");
        fs::create_dir_all(&shims).expect("shims");
        let found = Command::new("/bin/sh").arg("-c").arg(format!("command -v {tool}")).output().expect("command -v");
        let real = String::from_utf8_lossy(&found.stdout).trim().to_string();
        assert!(found.status.success() && real.starts_with('/'), "the real {tool} is on PATH: {real:?}");
        let path = shims.join(tool);
        fs::write(&path, format!("#!/bin/sh\n{before}\nexec {} \"$@\"\n", sh_quote(&real))).expect("shim");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
        shims
    }

    /// A shim `mv` that runs `on_new` (sh) when an argument is a staged `.new-*` file.
    fn mv_shim(dir: &Path, on_new: &str) -> PathBuf {
        shim(dir, "mv", &format!("for a in \"$@\"; do case \"$a\" in *.new-*) {on_new} ;; esac; done"))
    }

    /// A shim `ln` that always fails, as on a file system without hard links.
    fn ln_fails(dir: &Path) -> PathBuf {
        shim(dir, "ln", "exit 1")
    }

    /// The pid of a process that has already exited (and been reaped).
    fn gone_app() -> u32 {
        let mut gone = Command::new("true").spawn().expect("stand-in app");
        let _ = gone.wait();
        gone.id()
    }

    fn content(p: &Path) -> String {
        fs::read_to_string(p).unwrap_or_default()
    }

    #[test]
    fn quoting_keeps_a_path_one_argument() {
        assert_eq!(sh_quote("/tmp/a b"), "'/tmp/a b'");
        assert_eq!(sh_quote("/tmp/it's"), "'/tmp/it'\\''s'");
    }

    #[test]
    fn the_swap_waits_until_the_app_has_quit() {
        let s = setup("waits");
        let mut app = Command::new("sleep").arg("30").spawn().expect("stand-in app");
        let (mut script, folder) = run_swap(&s, app.id(), Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(700));
        assert!(content(&s.running).contains("'old'"), "the running file must not be replaced while the app runs");
        assert!(s.staged.exists() && !s.old.exists(), "nothing is moved while the app runs");
        assert!(!s.marker.exists(), "nothing is started while the app runs");
        assert!(matches!(script.try_wait(), Ok(None)), "the script is still waiting");
        // the app quits (and is reaped, as its parent would)
        let _ = app.kill();
        let _ = app.wait();
        assert!(finished(&mut script, Duration::from_secs(10)), "the script finishes once the app has quit");
        assert!(wait_until(Duration::from_secs(10), || content(&s.marker) == "new"), "the new version is started");
        assert!(content(&s.running).contains("'new'"), "the new version is in the running file's place");
        assert!(content(&s.old).contains("'old'"), "the previous version is kept as .old");
        assert!(!s.staged.exists(), "the staged file was moved, not copied");
        assert!(!s.note.exists(), "a swap that worked leaves no failure note");
        assert!(!folder.exists(), "the script deletes itself and its folder");
    }

    #[test]
    fn an_app_that_does_not_quit_keeps_its_version_and_nothing_starts() {
        let s = setup("timeout");
        let mut app = Command::new("sleep").arg("30").spawn().expect("stand-in app");
        let (mut script, folder) = run_swap(&s, app.id(), Duration::from_millis(300));
        assert!(finished(&mut script, Duration::from_secs(10)), "the script gives up after its wait");
        std::thread::sleep(Duration::from_millis(200));
        assert!(content(&s.running).contains("'old'"), "the original file is untouched");
        assert!(!s.old.exists(), "the original was not moved aside");
        assert!(!s.staged.exists(), "the staged file is deleted");
        assert!(!s.marker.exists(), "nothing is started");
        assert!(content(&s.note).contains("still running"), "the reason is left for the next start: {:?}", content(&s.note));
        assert!(!folder.exists(), "the script deletes itself and its folder");
        let _ = app.kill();
        let _ = app.wait();
    }

    #[test]
    fn a_failed_swap_puts_the_original_back_and_starts_nothing() {
        let s = setup("failed");
        // the app has already quit; the staged file disappears before the swap
        let mut gone = Command::new("true").spawn().expect("stand-in app");
        let _ = gone.wait();
        fs::remove_file(&s.staged).expect("remove staged");
        let (mut script, folder) = run_swap(&s, gone.id(), Duration::from_secs(5));
        assert!(finished(&mut script, Duration::from_secs(10)), "the script finishes");
        std::thread::sleep(Duration::from_millis(200));
        assert!(content(&s.running).contains("'old'"), "the original file is in place");
        assert!(!s.old.exists() && !s.staged.exists(), "no .old or .new file is left behind");
        assert!(!s.marker.exists(), "nothing is started");
        assert!(!content(&s.note).is_empty(), "the reason is left for the next start");
        assert!(!folder.exists(), "the script deletes itself and its folder");
    }

    #[test]
    fn a_rename_that_fails_keeps_the_original_and_starts_nothing() {
        let s = setup("mv fails");
        let shims = mv_shim(&s.dir, "exit 1");
        let (mut script, folder) = run_swap_with(&s, gone_app(), Duration::from_secs(5), Some(&shims));
        assert!(finished(&mut script, Duration::from_secs(10)), "the script finishes");
        std::thread::sleep(Duration::from_millis(200));
        assert!(content(&s.running).contains("'old'"), "the original file is untouched at its path");
        assert!(!s.staged.exists(), "no .new file is left behind");
        assert!(!s.marker.exists(), "nothing is started");
        assert!(content(&s.note).contains("could not be put in place"), "the reason is left for the next start: {:?}", content(&s.note));
        assert!(!folder.exists(), "the script deletes itself and its folder");
    }

    #[test]
    fn a_swap_stopped_half_way_still_leaves_a_runnable_file() {
        let s = setup("stopped");
        let reached = s.dir.join("reached");
        // the rename of the new version hangs: the script is stopped there
        let shims = mv_shim(&s.dir, &format!(": > {}; sleep 30", sh_quote(&reached.to_string_lossy())));
        let (mut script, _folder) = run_swap_with(&s, gone_app(), Duration::from_secs(5), Some(&shims));
        assert!(wait_until(Duration::from_secs(10), || reached.exists()), "the script reaches the final rename");
        // stop the script and its tools at that point
        let killed = Command::new("kill").args(["-s", "KILL", "--"]).arg(format!("-{}", script.id())).status().expect("kill");
        assert!(killed.success(), "the script's process group is stopped");
        assert!(finished(&mut script, Duration::from_secs(10)), "the script is stopped");
        assert!(content(&s.running).contains("'old'"), "the running path still names the previous version");
        assert!(content(&s.old).contains("'old'"), "and a copy is kept as .old");
    }

    /// Run a whole swap for an app that has quit, with `ln` failing when `no_links` (so the copy
    /// fallback runs).
    fn swap_after_quit(s: &Setup, no_links: bool) {
        let shims = no_links.then(|| ln_fails(&s.dir));
        let (mut script, folder) = run_swap_with(s, gone_app(), Duration::from_secs(5), shims.as_deref());
        assert!(finished(&mut script, Duration::from_secs(10)), "the script finishes");
        std::thread::sleep(Duration::from_millis(200));
        assert!(!folder.exists(), "the script deletes itself and its folder");
    }

    #[test]
    fn a_link_left_at_old_is_replaced_never_written_through() {
        for no_links in [false, true] {
            for to_folder in [false, true] {
                let s = setup(&format!("old link {no_links} {to_folder}"));
                let victim = s.dir.join(if to_folder { "victim dir" } else { "victim" });
                if to_folder {
                    fs::create_dir(&victim).expect("victim dir");
                } else {
                    fs::write(&victim, "precious").expect("victim");
                }
                symlink(&victim, &s.old).expect("plant a link at .old");
                swap_after_quit(&s, no_links);
                let case = format!("(no hard links: {no_links}, link to a folder: {to_folder})");
                if to_folder {
                    assert_eq!(fs::read_dir(&victim).expect("list").count(), 0, "nothing is copied behind the link {case}");
                } else {
                    assert_eq!(content(&victim), "precious", "the link's target is untouched {case}");
                }
                let meta = fs::symlink_metadata(&s.old).expect(".old");
                assert!(meta.file_type().is_file() && content(&s.old).contains("'old'"), "the previous version is kept as a plain .old {case}");
                assert!(wait_until(Duration::from_secs(5), || content(&s.marker) == "new"), "the update went ahead {case}");
            }
            // a link to nothing, and a named pipe, are removed like any file
            for kind in ["dangling link", "fifo"] {
                let s = setup(&format!("old {kind} {no_links}"));
                if kind == "fifo" {
                    assert!(Command::new("mkfifo").arg(&s.old).status().expect("mkfifo").success(), "a fifo at .old");
                } else {
                    symlink(s.dir.join("nowhere"), &s.old).expect("plant a dangling link at .old");
                }
                swap_after_quit(&s, no_links);
                let case = format!("({kind}, no hard links: {no_links})");
                let meta = fs::symlink_metadata(&s.old).expect(".old");
                assert!(meta.file_type().is_file() && content(&s.old).contains("'old'"), "the previous version is kept as a plain .old {case}");
                assert!(wait_until(Duration::from_secs(5), || content(&s.marker) == "new"), "the update went ahead {case}");
            }
        }
    }

    /// Makes a folder writable again when dropped, so the scratch folder can be deleted.
    struct RestoreWritable(PathBuf);

    impl Drop for RestoreWritable {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
        }
    }

    #[test]
    fn an_old_file_that_cannot_be_removed_stops_the_update_safely() {
        let mut s = setup("old locked");
        // the AppImage in a folder of its own, made read-only; the note stays writable
        let app = s.dir.join("app");
        fs::create_dir(&app).expect("app dir");
        s.running = app.join("App Image's.AppImage");
        s.staged = app.join("App Image's.AppImage.new-1");
        s.old = app.join("App Image's.AppImage.old");
        stand_in(&s.running, "old", &s.marker);
        stand_in(&s.staged, "new", &s.marker);
        fs::write(&s.old, "an earlier version").expect(".old");
        fs::set_permissions(&app, fs::Permissions::from_mode(0o555)).expect("read-only");
        let _restore = RestoreWritable(app.clone());
        if fs::write(app.join("probe"), "").is_ok() {
            // running as root: permissions don't stop anything, so there is nothing to test
            return;
        }
        swap_after_quit(&s, false);
        assert!(content(&s.running).contains("'old'"), "the original stays in place");
        assert_eq!(content(&s.old), "an earlier version", ".old is untouched");
        assert!(!s.marker.exists(), "nothing is started");
        assert!(content(&s.note).contains("could not be removed"), "the note says why: {:?}", content(&s.note));
    }

    #[test]
    fn a_folder_at_old_stops_the_update_safely() {
        for no_links in [false, true] {
            let s = setup(&format!("old folder {no_links}"));
            fs::create_dir(&s.old).expect("folder at .old");
            swap_after_quit(&s, no_links);
            let case = format!("(no hard links: {no_links})");
            assert!(content(&s.running).contains("'old'"), "the original stays in place {case}");
            assert!(!s.staged.exists(), "the staged file is deleted {case}");
            assert!(!s.marker.exists(), "nothing is started {case}");
            assert_eq!(fs::read_dir(&s.old).expect("list").count(), 0, "nothing is copied into the folder {case}");
            assert!(content(&s.note).contains("A folder is in the way"), "the note says why {case}: {:?}", content(&s.note));
        }
    }

    #[test]
    fn a_script_that_cannot_be_written_deletes_the_staged_file() {
        let s = setup("unwritable");
        // the base is a file: no folder can be made in it
        let base = s.dir.join("not a folder");
        fs::write(&base, "x").expect("file");
        let err = start(&base, |_| String::from("exit 0\n"), &s.staged).expect_err("refused");
        assert!(matches!(err, UpdateError::Install(_)), "{err}");
        assert!(!s.staged.exists(), "the staged file is deleted when the swap can't start");
        assert!(content(&s.running).contains("'old'"), "the running file is untouched");
    }

    #[test]
    fn the_script_goes_to_a_private_folder_and_never_through_a_planted_link() {
        let dir = scratch("links");
        let victim = dir.join("victim");
        fs::write(&victim, "precious").expect("victim");
        // the name the script used to have in a shared temp folder
        let predictable = dir.join(format!("{}-update-{}.sh", lcc_brand::BINARY, std::process::id()));
        symlink(&victim, &predictable).expect("plant at the old name");

        let folder = private_folder(&dir).expect("private folder");
        let script = write_script(&folder, "exit 0\n").expect("script");
        assert_eq!(content(&victim), "precious", "the planted link is not written through");
        assert!(script.starts_with(&folder) && folder.parent() == Some(dir.as_path()));
        let mode = |p: &Path| fs::metadata(p).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode(&folder) & 0o077, 0, "only the user can open the folder");
        assert_eq!(mode(&script) & 0o077, 0, "only the user can read the script");

        // a link planted at the script's own name is an error, not followed
        let other = private_folder(&dir).expect("second folder");
        assert_ne!(other, folder, "every folder is new");
        symlink(&victim, other.join(SCRIPT_NAME)).expect("plant at the new name");
        let err = write_script(&other, "echo owned\n").expect_err("refused");
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists, "{err}");
        assert_eq!(content(&victim), "precious");

        // and a link planted at a folder's name is not used as the folder
        let elsewhere = dir.join("elsewhere");
        fs::create_dir(&elsewhere).expect("dir");
        let planted = dir.join("planted");
        symlink(&elsewhere, &planted).expect("plant a folder link");
        assert!(create_private_dir(&planted).is_err(), "an existing link is not a fresh folder");
        assert_eq!(fs::read_dir(&elsewhere).expect("list").count(), 0, "nothing was written behind the link");
    }

    #[test]
    fn the_script_folder_is_under_the_runtime_dir_when_there_is_one() {
        let dir = scratch("base");
        let fallback = dir.join("updates");
        assert_eq!(script_base(Some(dir.clone().into_os_string()), &fallback), *dir);
        assert_eq!(script_base(None, &fallback), fallback);
        assert_eq!(script_base(Some("relative".into()), &fallback), fallback, "a relative value is not trusted");
        assert_eq!(script_base(Some(dir.join("missing").into_os_string()), &fallback), fallback);
    }

    #[test]
    fn the_next_start_deletes_only_the_previous_appimage() {
        let dir = scratch("old");
        let running = dir.join("app.AppImage");
        let old = dir.join("app.AppImage.old");
        let other_old = dir.join("other.AppImage.old");
        let staged = dir.join("app.AppImage.new-7");
        for f in [&running, &old, &other_old, &staged] {
            fs::write(f, "x").expect("file");
        }
        assert_eq!(remove_previous(None).expect("ok"), None, "a start that isn't from an AppImage deletes nothing");
        assert!(old.exists());
        assert_eq!(remove_previous(Some(dir.join("gone.AppImage").into_os_string())).expect("ok"), None);
        assert!(old.exists());
        let removed = remove_previous(Some(running.clone().into_os_string())).expect("ok");
        assert!(removed.is_some_and(|p| p.file_name() == old.file_name()), "the .old beside $APPIMAGE goes");
        assert!(!old.exists());
        assert!(running.exists() && other_old.exists() && staged.exists(), "nothing else in the folder is touched");
        assert_eq!(remove_previous(Some(running.clone().into_os_string())).expect("ok"), None, "nothing left to delete");
        // a folder or a link under the .old name is not ours to delete
        fs::create_dir(&old).expect("dir");
        assert_eq!(remove_previous(Some(running.clone().into_os_string())).expect("ok"), None);
        assert!(old.is_dir());
    }

    #[test]
    fn a_failure_note_is_read_once() {
        let dir = scratch("note");
        assert_eq!(take_failure_note(&dir), None);
        fs::write(dir.join(FAILURE_NOTE), "The app was still running.\n").expect("note");
        assert_eq!(take_failure_note(&dir).as_deref(), Some("The app was still running."));
        assert_eq!(take_failure_note(&dir), None, "the note is deleted once read");
    }
}
