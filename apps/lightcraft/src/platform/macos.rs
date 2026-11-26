//! macOS file reveal and URL opening: Finder selects the file (`open -R`).

/// Reveal one path, selecting it in Finder.
pub(crate) fn reveal_one(path: &str) -> Result<(), String> {
    std::process::Command::new("open")
        .args(["-R", path])
        .status()
        .map_err(|e| e.to_string())
        .and_then(|status| if status.success() { Ok(()) } else { Err(format!("reveal failed: {status}")) })
}

/// Open a URL in the default browser.
pub(crate) fn open_url(url: &str) -> Result<(), String> {
    std::process::Command::new("open").arg(url).status().map(|_| ()).map_err(|e| e.to_string())
}
