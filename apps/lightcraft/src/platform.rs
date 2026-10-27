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
