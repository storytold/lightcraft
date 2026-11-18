//! Project links (Help menu, About dialog), from the brand (`lcc_brand`).

/// This app's web site (brand.toml `website`, else `homepage`).
pub const APP_PAGE: &str = lcc_brand::WEBSITE;
/// This app's source repository.
pub const GITHUB: &str = lcc_brand::REPOSITORY;
/// The user guide (brand.toml `docs`, else the repository).
pub const HELP: &str = lcc_brand::DOCS;
/// The community chat (empty when the brand has none: no link is shown).
pub const DISCORD: &str = lcc_brand::DISCORD;
/// Where feedback and bug reports go.
pub const FEEDBACK: &str = lcc_brand::REPOSITORY;

/// (UI command id, menu label, URL) for each link, in Help-menu order. Labels use the `{app}`
/// placeholder; `crate::i18n::tr` fills it in.
pub const LINKS: &[(&str, &str, &str)] = &[
    ("app.help", "{app} Help", HELP),
    ("app.website", "{app} Website", APP_PAGE),
    ("app.github", "{app} Source Code", GITHUB),
    ("app.feedback", "Send Feedback…", FEEDBACK),
    ("app.discord", "Join the Discord", DISCORD),
];
/// The URL behind a link command id.
pub fn url_of(cmd: &str) -> Option<&'static str> {
    LINKS.iter().find(|(id, _, _)| *id == cmd).map(|(_, _, u)| *u)
}

/// Open `url` in the user's browser (through the host's `open_url` service).
pub fn open(app: &mut crate::LccApp, url: &str) -> Result<serde_json::Value, String> {
    let open = app.services.open_url.as_mut().ok_or("can't open links here")?;
    open(url)?;
    Ok(serde_json::json!({ "url": url }))
}

/// The temporary folder Preview in Browser exports to (one per process).
pub fn web_preview_dir() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("{}-web-preview-{}", lcc_brand::BINARY, std::process::id()))
}

/// A `file://` URL for a local path (`file:///tmp/…` on Unix, `file:///C:/…` on Windows).
pub fn file_url(path: &std::path::Path) -> String {
    let path = path.display().to_string().replace('\\', "/");
    if path.starts_with('/') { format!("file://{path}") } else { format!("file:///{path}") }
}

/// The URLs a host's `open_url` may hand to the system browser: any `https://` link, and the one
/// local page Preview in Browser generates (`<web_preview_dir>/index.html`, once it exists).
/// Every other scheme and every other local file is refused.
pub fn check_browser_url(url: &str) -> Result<(), String> {
    if url.starts_with("https://") {
        return Ok(());
    }
    let index = web_preview_dir().join("index.html");
    if url.starts_with("file://") && url == file_url(&index) && index.is_file() {
        return Ok(());
    }
    Err("only https links are opened".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_policy_allows_https_and_only_the_generated_preview() {
        assert!(check_browser_url("https://example.com/a?b#c").is_ok());
        for bad in ["http://example.com", "javascript:alert(1)", "ftp://x", "file:///etc/passwd", "file:///C:/Windows/win.ini", "", "file://"] {
            assert!(check_browser_url(bad).is_err(), "{bad}");
        }
        let dir = web_preview_dir();
        let index = dir.join("index.html");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(check_browser_url(&file_url(&index)).is_err(), "not generated yet");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&index, "<html></html>").unwrap();
        std::fs::write(dir.join("other.html"), "x").unwrap();
        assert!(check_browser_url(&file_url(&index)).is_ok());
        assert!(check_browser_url(&file_url(&dir.join("other.html"))).is_err());
        assert!(check_browser_url(&format!("{}/../index.html", file_url(&dir))).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
