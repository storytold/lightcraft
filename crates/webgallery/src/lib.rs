//! Web gallery generator (L1): the Web module's static sites.
//!
//! - [`settings`]: everything the Web module's panels edit (template, site info, colour palette,
//!   appearance, image info tokens, output settings), serialisable for saved galleries.
//! - [`tokens`]: `{title}` / `{caption}` / `{filename}` … caption templates.
//! - [`site`]: turns settings + a list of photos into the files of a static site (HTML, CSS and a
//!   small script; no external CDN, no web fonts). The images themselves are rendered by the caller
//!   (the engine's export) at the sizes [`site::Site::images`] asks for.
//! - `sftp` (feature `sftp`, native only): upload a generated site over SFTP (pure Rust, russh).
//!
//! The templates are our own work (Sources: own design). They are responsive (CSS grid and
//! `srcset`-free fixed sizes chosen by the settings), keyboard navigable (arrow keys, Home/End,
//! Escape in the viewer) and every `<img>` carries alt text from the caption.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod settings;
pub mod site;
pub mod tokens;

#[cfg(all(feature = "sftp", not(target_arch = "wasm32")))]
pub mod sftp;

pub use settings::{Appearance, GallerySettings, ImageInfo, MetadataMode, Output, Palette, Server, SiteInfo, Template};
pub use site::{GalleryPhoto, ImageRequest, Site, SiteFile, generate};

/// HTML-escapes text for element content and attribute values.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn escapes_markup() {
        assert_eq!(super::escape(r#"<a href="x">&'</a>"#), "&lt;a href=&quot;x&quot;&gt;&amp;&#39;&lt;/a&gt;");
    }
}
