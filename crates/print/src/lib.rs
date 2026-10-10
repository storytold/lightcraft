//! The Print module's back end.
//!
//! * [`job`]: print settings ([`PrintSettings`]: page setup, layout style, image settings, page
//!   options, print job) and [`PrintSettings::build`], which turns them and a list of photos into
//!   a `dac-layout` [`dac_layout::Document`]; built-in print templates ([`job::builtin_templates`]).
//! * [`text`]: [`text::ShapedText`], the `dac-layout` text renderer over the `dac-text` shaper,
//!   also used for vector PDF text.
//! * [`output`]: PDF (photos, shaped text with embedded subset fonts, graphics, crop marks, an
//!   optional ICC OutputIntent) and JPEG output.
//! * [`ipp`]: IPP/1.1 printing to CUPS (and any IPP Everywhere printer) in pure Rust.
//!
//! Photos come in through `dac_layout::render::PhotoSource`, which the engine implements with
//! its export renderer, so prints match exports. Print sharpening, 16-bit output and soft-proof
//! colour management are applied by that renderer (shared pipeline), not here.
//!
//! Sources: own design; IPP after RFC 8010/8011 (see [`ipp`]).

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod ipp;
pub mod job;
pub mod output;
pub mod text;

pub use job::{Destination, ImageSettings, LayoutStyle, PackageSpec, PageOptions, PrintSettings};

/// Errors from building, rendering or sending a print.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum PrintError {
    #[error("print settings: {0}")]
    Invalid(String),
    #[error(transparent)]
    Layout(#[from] dac_layout::LayoutError),
    #[error("print output: {0}")]
    Output(String),
    #[error("printer: {0}")]
    Ipp(String),
    #[error("print cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, PrintError>;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_robust;
