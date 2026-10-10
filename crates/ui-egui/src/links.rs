//! Project links (Help menu, About dialog), from the brand (`dac_brand`).

/// This app's web page.
pub const APP_PAGE: &str = dac_brand::HOMEPAGE;
/// This app's source repository.
pub const GITHUB: &str = dac_brand::REPOSITORY;
/// The user documentation (docs/ in the repository).
pub const HELP: &str = dac_brand::REPOSITORY;
/// Where feedback and bug reports go.
pub const FEEDBACK: &str = dac_brand::REPOSITORY;

/// (UI command id, menu label, URL) for each link, in Help-menu order. Labels use the `{app}`
/// placeholder; `crate::i18n::tr` fills it in.
pub const LINKS: &[(&str, &str, &str)] = &[
    ("app.help", "{app} Help", HELP),
    ("app.website", "{app} Website", APP_PAGE),
    ("app.github", "{app} Source Code", GITHUB),
    ("app.feedback", "Send Feedback…", FEEDBACK),
];
/// The URL behind a link command id.
pub fn url_of(cmd: &str) -> Option<&'static str> {
    LINKS.iter().find(|(id, _, _)| *id == cmd).map(|(_, _, u)| *u)
}

/// Open `url` in the user's browser (through the host's `open_url` service).
pub fn open(app: &mut crate::DacApp, url: &str) -> Result<serde_json::Value, String> {
    let open = app.services.open_url.as_mut().ok_or("can't open links here")?;
    open(url)?;
    Ok(serde_json::json!({ "url": url }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_are_https_and_come_from_the_brand() {
        for (id, label, url) in LINKS {
            assert!(url.starts_with("https://"), "{id}");
            assert!(!label.is_empty());
            assert_eq!(url_of(id), Some(*url));
        }
        assert_eq!((APP_PAGE, GITHUB), (dac_brand::HOMEPAGE, dac_brand::REPOSITORY));
    }
}
