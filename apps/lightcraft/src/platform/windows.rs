//! Windows file reveal and URL opening (issues #234, #272).

/// The Windows command that opens `url` in the default browser. `explorer <url>` opened File
/// Explorer instead for links with a query or fragment, like the Map button's OpenStreetMap link
/// (issue #234). The URL protocol handler hands the URL to the browser as is; no shell runs, so
/// `?`, `&` and `#` need no escaping.
fn windows_open_url_command(url: &str) -> std::process::Command {
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
fn explorer_select_arg(path: &str) -> String {
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
fn explorer_select_command(path: &str) -> std::process::Command {
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

/// Reveal one path, selecting it in Explorer. A spawned Explorer counts as revealed (Explorer's
/// exit status is meaningless), as the previous inline dispatch did.
pub(crate) fn reveal_one(path: &str) -> Result<(), String> {
    explorer_select_command(path).status().map(|_| ()).map_err(|e| e.to_string())
}

/// Open a URL in the default browser.
pub(crate) fn open_url(url: &str) -> Result<(), String> {
    windows_open_url_command(url).status().map(|_| ()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #234: Windows opens links with the URL protocol handler (the default browser), not
    /// File Explorer, and passes the whole map link, `?`, `&` and `#` included, as one argument.
    #[test]
    fn windows_links_open_in_the_browser() {
        let url = "https://www.openstreetmap.org/?mlat=48.856600&mlon=2.352200#map=15/48.856600/2.352200";
        let c = windows_open_url_command(url);
        assert_eq!(c.get_program(), "rundll32");
        let args: Vec<&std::ffi::OsStr> = c.get_args().collect();
        assert_eq!(args, ["url.dll,FileProtocolHandler", url]);
    }

    /// Issue #272: Explorer gets the file to select quoted after `/select,`, with backslashes and
    /// without the verbatim prefix, even when the path has spaces (else it opens Documents).
    #[test]
    fn explorer_select_quotes_the_path() {
        assert_eq!(explorer_select_arg(r"C:\Users\Me\My Photos\IMG 1.jpg"), r#"/select,"C:\Users\Me\My Photos\IMG 1.jpg""#);
        assert_eq!(explorer_select_arg("D:/Example/Photos/IMG_0001.CR3"), r#"/select,"D:\Example\Photos\IMG_0001.CR3""#);
        assert_eq!(explorer_select_arg(r"\\server\share\My Photos\a.jpg"), r#"/select,"\\server\share\My Photos\a.jpg""#);
        assert_eq!(explorer_select_arg(r"\\?\D:\My Photos\a.jpg"), r#"/select,"D:\My Photos\a.jpg""#);
        assert_eq!(explorer_select_arg(r"\\?\UNC\server\share\a.jpg"), r#"/select,"\\server\share\a.jpg""#);
        // a `"` can't end the quoted path and add arguments to the raw command line
        assert_eq!(explorer_select_arg(r#"C:\a" "C:\b.jpg"#), r#"/select,"C:\a C:\b.jpg""#);
        let c = explorer_select_command(r"C:\My Photos\a.jpg");
        assert_eq!(c.get_program(), "explorer");
        let args: Vec<&std::ffi::OsStr> = c.get_args().collect();
        assert_eq!(args, [r#"/select,"C:\My Photos\a.jpg""#]);
    }
}
