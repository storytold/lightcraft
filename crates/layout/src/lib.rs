//! Page layout shared by the Print, Book, Slideshow and Web modules.
//!
//! - [`Page`]: trim size, margins, bleed and background, in **points** (1/72 inch, the PDF unit),
//!   origin at the top-left of the trim box, y down.
//! - [`Cell`]: a rectangle on a page holding a photo ([`PhotoCell`]: fit/fill, zoom, pan, rotate,
//!   stroke), text ([`TextCell`]: a token template such as `{Title} — {Date}`) or a graphic
//!   ([`GraphicCell`]: filled/stroked rectangle or ellipse).
//! - [`grid`]: contact-sheet grids; [`package`]: picture packages (fixed cell sizes, auto layout
//!   over as many pages as needed).
//! - [`snap`]: guides and snapping of moved cells to page edges, margins, centre lines, guides and
//!   other cells.
//! - [`Document`]: one or more page layouts; a [`Template`] is a document without photos, saved
//!   as JSON (user templates) or built in ([`builtin_templates`], our own designs);
//!   [`Template::instantiate`] fills its photo cells from a list of photos, repeating its pages.
//! - [`tokens`]: token text, the export naming tokens plus layout ones (`{Caption}`,
//!   `{Exposure}`, `{Page}`…).
//! - [`render`]: a raster renderer of a page. Photos come from a [`render::PhotoSource`] (the
//!   engine plugs its render/export pipeline in), text from a [`render::TextRenderer`] (a
//!   placeholder until the `text` crate's shaper is plugged in).
//!
//! Every input (template JSON, agent arguments) is validated: sizes are finite and bounded, counts
//! are capped, and nothing panics on hostile values.
//!
//! Sources: own design (layout model after the observable behaviour of print/book layout tools;
//! shelf packing for picture packages is the textbook "next-fit decreasing height" heuristic).

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod grid;
mod model;
pub mod package;
pub mod render;
pub mod snap;
mod template;
pub mod tokens;

pub use model::*;
pub use template::{Template, builtin_templates};

/// Errors from validating or rendering a layout.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum LayoutError {
    #[error("invalid layout: {0}")]
    Invalid(String),
    #[error("layout JSON: {0}")]
    Json(String),
    #[error("render: {0}")]
    Render(String),
    #[error("item {0} does not fit on the page")]
    DoesNotFit(usize),
}

pub type Result<T> = std::result::Result<T, LayoutError>;

/// Largest page side accepted (points): 200 inches, far beyond any printer or book.
pub const MAX_PAGE_PT: f32 = 14_400.0;
/// Most cells on one page.
pub const MAX_CELLS: usize = 2_000;
/// Most pages in one document.
pub const MAX_PAGES: usize = 2_000;
/// Points per inch.
pub const PT_PER_INCH: f32 = 72.0;
/// Points per millimetre.
pub const PT_PER_MM: f32 = 72.0 / 25.4;
