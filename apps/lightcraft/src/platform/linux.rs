//! Linux file reveal and URL opening (issues #4652, #4658): select files via
//! `org.freedesktop.FileManager1.ShowItems`, falling back to `xdg-open`.

/// `file://` URI for `org.freedesktop.FileManager1.ShowItems` (issue #4652). Percent-encodes
/// every byte outside the unreserved set, so spaces and non-ASCII names survive the D-Bus call.
/// Absolute paths (`/photos/a b.jpg`) become `file:///photos/a%20b.jpg`.
fn file_uri(path: &str) -> String {
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

/// `ShowItems` on the running file manager via D-Bus (`zbus` is a Linux dependency). Managers
/// without `FileManager1` answer with an error and the caller falls back to `xdg-open`.
fn show_items(uris: &[String]) -> Result<(), String> {
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
/// Never waits for the file manager (issue #4658): the old `.status()` blocked the command until
/// its window closed. The child is reaped on a thread so no zombie is left behind.
fn xdg_open_dir(dir: &str) -> Result<(), String> {
    let mut child = std::process::Command::new("xdg-open").arg(dir).spawn().map_err(|e| e.to_string())?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Reveal one path: a folder opens as before, a file is selected in the file manager and only
/// falls back to opening its parent folder (issue #4652).
pub(crate) fn reveal_one(path: &str) -> Result<(), String> {
    if std::path::Path::new(path).is_dir() {
        return xdg_open_dir(path);
    }
    let uri = file_uri(path);
    if show_items(std::slice::from_ref(&uri)).is_ok() {
        return Ok(());
    }
    let dir = std::path::Path::new(path).parent().map(|parent| parent.to_string_lossy().to_string()).unwrap_or_else(|| ".".into());
    xdg_open_dir(&dir)
}

/// Reveal several files: one `ShowItems` call selects them in the same window; without the
/// service it opens the first file's folder, as a single reveal did before (issue #4652).
pub(crate) fn reveal_files(paths: &[String]) -> Result<(), String> {
    if paths.is_empty() {
        return Err("no photo selected".into());
    }
    if paths.len() == 1 {
        return reveal_one(&paths[0]);
    }
    let uris: Vec<String> = paths.iter().map(|path| file_uri(path)).collect();
    if show_items(&uris).is_ok() {
        return Ok(());
    }
    reveal_one(&paths[0])
}

/// Open a URL in the default browser.
pub(crate) fn open_url(url: &str) -> Result<(), String> {
    std::process::Command::new("xdg-open").arg(url).status().map(|_| ()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #4652: Linux reveal builds `file://` URIs for `FileManager1.ShowItems`, so the file
    /// manager selects the file instead of opening its folder.
    #[test]
    fn file_uris_encode_for_show_items() {
        assert_eq!(file_uri("/photos/IMG_0001.CR3"), "file:///photos/IMG_0001.CR3");
        assert_eq!(file_uri("/home/Me/My Photos/IMG 1.jpg"), "file:///home/Me/My%20Photos/IMG%201.jpg");
        assert_eq!(file_uri("/photos/café-étude.jpg"), "file:///photos/caf%C3%A9-%C3%A9tude.jpg");
        assert_eq!(file_uri("/photos/a#b%c.jpg"), "file:///photos/a%23b%25c.jpg");
    }
}
