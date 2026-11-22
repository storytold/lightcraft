/// The Windows command that opens `url` in the default browser. `explorer <url>` opened File
/// Explorer instead for links with a query or fragment, like the Map button's OpenStreetMap link
/// (issue #234). The URL protocol handler hands the URL to the browser as is; no shell runs, so
/// `?`, `&` and `#` need no escaping.
pub(crate) fn windows_open_url_command(url: &str) -> std::process::Command {
    let mut c = std::process::Command::new("rundll32");
    c.args(["url.dll,FileProtocolHandler", url]);
    c
}

/// Explorer's `/select,<path>` switch (issue #272). Explorer parses its own command line and only
/// selects the file when the path after the comma is quoted (`/select,"C:\My Photos\a.jpg"`),
/// spelled with backslashes and free of the verbatim prefix (`\\?\`, `\\?\UNC\`) that
/// `canonicalize` adds; given anything else it opens Documents. Windows file names can't contain
/// `"`; a path that does anyway (a hostile catalog) has them dropped, so the raw command line
/// always holds exactly this one argument.
pub(crate) fn explorer_select_arg(path: &str) -> String {
    let path = path.replace('/', "\\").replace('"', "");
    let path = match path.strip_prefix(r"\\?\UNC\") {
        Some(unc) => format!(r"\\{unc}"),
        None => path.strip_prefix(r"\\?\").unwrap_or(path.as_str()).to_string(),
    };
    format!("/select,\"{path}\"")
}

/// `explorer /select,"<path>"`. Rust's own argument quoting wraps the whole switch in quotes
/// (`"/select,C:\My Photos\a.jpg"`) whenever the path has a space, which Explorer doesn't
/// recognise, so on Windows the switch goes on the command line exactly as built.
pub(crate) fn explorer_select_command(path: &str) -> std::process::Command {
    let mut c = std::process::Command::new("explorer");
    let arg = explorer_select_arg(path);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.raw_arg(arg);
    }
    #[cfg(not(windows))]
    {
        c.arg(arg);
    }
    c
}

/// `file://` URI for `org.freedesktop.FileManager1.ShowItems` (issue #4652). Percent-encodes
/// every byte outside the unreserved set, so spaces and non-ASCII names survive the D-Bus call.
/// Absolute paths (`/photos/a b.jpg`) become `file:///photos/a%20b.jpg`.
pub(crate) fn file_uri(path: &str) -> String {
    let mut out = String::with_capacity(path.len() + 7);
    out.push_str("file://");
    for byte in path.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => out.push(*byte as char),
            _ => {
                out.push('%');
                out.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0').to_ascii_uppercase());
                out.push(char::from_digit(u32::from(byte & 0xF), 16).unwrap_or('0').to_ascii_uppercase());
            }
        }
    }
    out
}

/// `ShowItems` on the running file manager via D-Bus (Linux only: `zbus` is a Linux dependency).
/// Thunar and other managers without `FileManager1` answer with an error and the caller falls
/// back to `xdg-open`.
#[cfg(target_os = "linux")]
pub(crate) fn linux_show_items(uris: &[String]) -> Result<(), String> {
    let connection = zbus::blocking::connection::Builder::session()
        .map_err(|e| e.to_string())?
        .method_timeout(std::time::Duration::from_secs(2))
        .build()
        .map_err(|e| e.to_string())?;
    let proxy =
        zbus::blocking::Proxy::new(&connection, "org.freedesktop.FileManager1", "/org/freedesktop/FileManager1", "org.freedesktop.FileManager1")
            .map_err(|e| e.to_string())?;
    let startup_id = "";
    proxy.call::<_, _, ()>("ShowItems", &(uris, startup_id)).map_err(|e| e.to_string())?;
    Ok(())
}

/// Open `dir` with the default handler (`xdg-open`), as the reveal fallback did before #4652.
/// Only called on Linux/FreeBSD (see `linux_reveal_one`); kept unconditional so the Windows and
/// macOS builds still reference the reveal helpers (runtime `cfg!` dispatch in `session`).
fn xdg_open_dir(dir: &str) -> Result<(), String> {
    std::process::Command::new("xdg-open")
        .arg(dir)
        .status()
        .map_err(|e| e.to_string())
        .and_then(|status| if status.success() { Ok(()) } else { Err(format!("reveal failed: {status}")) })
}

/// Reveal one path on Linux/FreeBSD (issue #4652): a folder opens as before, a file is selected
/// in the file manager and only falls back to opening its parent folder. Unconditional (with the
/// D-Bus attempt Linux-only) so every platform build references it via `session`'s runtime `cfg!`.
pub(crate) fn linux_reveal_one(path: &str) -> Result<(), String> {
    if std::path::Path::new(path).is_dir() {
        return xdg_open_dir(path);
    }
    #[cfg(target_os = "linux")]
    {
        let uri = file_uri(path);
        if linux_show_items(std::slice::from_ref(&uri)).is_ok() {
            return Ok(());
        }
    }
    let dir = std::path::Path::new(path).parent().map(|parent| parent.to_string_lossy().to_string()).unwrap_or_else(|| ".".into());
    xdg_open_dir(&dir)
}

/// Reveal several files on Linux (issue #4652): one `ShowItems` call selects them in the same
/// window; without the service it opens the first file's folder, as a single reveal did before.
/// Unconditional (D-Bus attempt Linux-only) so every platform build references it.
pub(crate) fn linux_reveal_files(paths: &[String]) -> Result<(), String> {
    if paths.is_empty() {
        return Err("no photo selected".into());
    }
    if paths.len() == 1 {
        return linux_reveal_one(&paths[0]);
    }
    #[cfg(target_os = "linux")]
    {
        let uris: Vec<String> = paths.iter().map(|path| file_uri(path)).collect();
        if linux_show_items(&uris).is_ok() {
            return Ok(());
        }
    }
    linux_reveal_one(&paths[0])
}
/// The file dialogs' parent, set once the window exists (`services`). A dialog with a parent is
/// a child of the app's window; without one, the xdg-desktop-portal picker on Wayland is a stray
/// top-level ("Failed to associate portal window with parent window" in the portal's log): a
/// tiling compositor puts it beside the app, and a fullscreen window hides it entirely, so
/// Import Photos… looks like a hang. rfd exports the Wayland surface through xdg_foreign itself;
/// `FileDialog` is `Clone` and `Send`, so one parented template serves every picker, including
/// the one on its own thread (#191). Only set on Linux and FreeBSD: on macOS a parented dialog
/// becomes a sheet and on Windows a modal, a behaviour change this does not make.
pub(crate) static DIALOG_PARENT: std::sync::OnceLock<rfd::FileDialog> = std::sync::OnceLock::new();

/// A file dialog owned by the app's window, or a plain one before the window exists.
pub(crate) fn file_dialog() -> rfd::FileDialog {
    DIALOG_PARENT.get().cloned().unwrap_or_default()
}
