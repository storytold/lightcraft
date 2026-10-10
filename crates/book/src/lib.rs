//! Photo books: the Book module's document and everything it does that isn't drawing the UI.
//!
//! - [`Book`]: settings (type PDF/JPEG, size, cover, paper, JPEG quality, resolution,
//!   sharpening), the cover and the pages, guides, page numbers, backgrounds, favourite templates
//!   and text style presets. Saved as JSON ([`Book::to_json`] / [`Book::from_json`]).
//! - Pages hold [`BookCell`]s in **normalized** trim coordinates (0–1), so changing the book size
//!   refits every page; padding, text sizes and offsets are in points.
//! - [`templates`]: built-in page templates by photo count (1–4+, with or without text), our own
//!   designs.
//! - [`auto`]: Auto Layout presets (one photo per page, left blank, with text, fill) and
//!   Clear Layout.
//! - [`ops`]: every edit as a JSON operation (`book.addPage`, `book.cell`, …) so the UI, the
//!   control channel, MCP and the CLI share one implementation.
//! - [`render`]: a page as pixels (preview, JPEG export) through a
//!   [`dac_layout::render::PhotoSource`], text shaped with `dac-text`.
//! - [`export`]: a PDF (cover plus pages, spreads as single pages, photos as JPEG with an sRGB
//!   ICC profile, text as embedded subset fonts) or one JPEG per page.
//!
//! Every input is validated; nothing panics on hostile values.
//!
//! Sources: own design (after the observable behaviour of photo-book layout tools).

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod auto;
pub mod export;
mod model;
pub mod ops;
pub mod render;
pub mod templates;
pub mod text;

pub use model::*;

/// Errors from editing, validating or exporting a book.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum BookError {
    #[error("invalid book: {0}")]
    Invalid(String),
    #[error("book JSON: {0}")]
    Json(String),
    #[error("{0}")]
    Bad(String),
    #[error("render: {0}")]
    Render(String),
    #[error("export: {0}")]
    Export(String),
}

pub type Result<T> = std::result::Result<T, BookError>;

impl From<dac_layout::LayoutError> for BookError {
    fn from(e: dac_layout::LayoutError) -> Self {
        BookError::Render(e.to_string())
    }
}

/// Most pages in a book (covers excluded).
pub const MAX_PAGES: usize = 500;
/// Most cells on one page.
pub const MAX_CELLS: usize = 64;
