//! Reveal fallback for FreeBSD and other Unix systems that are neither Linux nor macOS
//! (issues #4652, #4658): `xdg-open` only, without the Linux D-Bus attempt. A folder opens
//! itself, a file opens its parent folder. Mirrors `linux`'s fallback behaviour.

/// Open `dir` with the default handler (`xdg-open`). Never waits for the file manager
/// (issue #4658): the old `.status()` blocked the command until its window closed. The child
/// is reaped on a thread so no zombie is left behind.
fn xdg_open_dir(dir: &str) -> Result<(), String> {
    let mut child = std::process::Command::new("xdg-open").arg(dir).spawn().map_err(|e| e.to_string())?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Reveal one path: a folder opens itself, a file opens its parent folder (issue #4652).
pub(crate) fn reveal_one(path: &str) -> Result<(), String> {
    if std::path::Path::new(path).is_dir() {
        return xdg_open_dir(path);
    }
    let dir = std::path::Path::new(path).parent().map(|parent| parent.to_string_lossy().to_string()).unwrap_or_else(|| ".".into());
    xdg_open_dir(&dir)
}

/// Reveal several files: without a selecting file manager it opens the first file's folder,
/// as a single reveal did before (issue #4652).
pub(crate) fn reveal_files(paths: &[String]) -> Result<(), String> {
    if paths.is_empty() {
        return Err("no photo selected".into());
    }
    reveal_one(&paths[0])
}

/// Open a URL in the default browser.
pub(crate) fn open_url(url: &str) -> Result<(), String> {
    std::process::Command::new("xdg-open").arg(url).status().map(|_| ()).map_err(|e| e.to_string())
}
