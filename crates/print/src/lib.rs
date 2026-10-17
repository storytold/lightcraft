//! The Print module's back end.
//!
//! * [`job`]: print settings ([`PrintSettings`]: page setup, layout style, image settings, page
//!   options, print job) and [`PrintSettings::build`], which turns them and a list of photos into
//!   a `lcc-layout` [`lcc_layout::Document`]; built-in print templates ([`job::builtin_templates`]).
//! * [`text`]: [`text::ShapedText`], the `lcc-layout` text renderer over the `lcc-text` shaper,
//!   also used for vector PDF text.
//! * [`output`]: PDF (photos, shaped text with embedded subset fonts, graphics, crop marks, an
//!   optional ICC OutputIntent) and JPEG output.
//! * [`color`]: Print Adjustment and the conversion into the printer profile (8 and 16 bit).
//! * [`ipp`]: IPP/1.1 printing to CUPS (and any IPP Everywhere printer) in pure Rust, over
//!   `ipp://` or `ipps://` (TLS through `lcc-net`, with pinned or trust-on-first-use certificates),
//!   with the printer's own options (paper type, tray, quality, two-sided).
//! * [`send`]: render, submit (PDF, else JPEG pages) with copies, and follow the job state.
//! * [`testprinter`]: a virtual IPP printer on localhost for tests and CI.
//!
//! Photos come in through `lcc_layout::render::PhotoSource`, which the engine implements with
//! its export renderer, so prints match exports. Print sharpening ([`job::PrintSharpening`]) is
//! applied by the engine to the rendered photos at their printed size, not here.
//!
//! Sources: own design; IPP after RFC 8010/8011 (see [`ipp`]).

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod color;
pub mod ipp;
pub mod job;
pub mod output;
pub mod send;
pub mod testprinter;
pub mod text;

pub use job::{
    Destination, ImageSettings, LayoutStyle, PackageSpec, PageOptions, PrintColor, PrintIntent, PrintMedia, PrintSettings, PrintSharpening,
};

/// Errors from building, rendering or sending a print.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum PrintError {
    #[error("print settings: {0}")]
    Invalid(String),
    #[error(transparent)]
    Layout(#[from] lcc_layout::LayoutError),
    #[error("print output: {0}")]
    Output(String),
    #[error("printer: {0}")]
    Ipp(String),
    #[error("print cancelled")]
    Cancelled,
    /// An `ipps://` printer's certificate is not trusted (printers usually sign their own). The
    /// fingerprint is what the user compares with the printer's status page before trusting it.
    #[error(
        "the printer {printer} has a certificate this computer doesn't trust (printers usually make their own). Its SHA-256 fingerprint is {fingerprint}. Compare it with the one on the printer's status or web page, then choose Trust Certificate in the Print Job panel (or run print.trustCertificate), or turn on trusting printer certificates on first use"
    )]
    UntrustedCertificate { printer: String, fingerprint: String },
}

pub type Result<T> = std::result::Result<T, PrintError>;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_ipps;
#[cfg(test)]
mod tests_robust;
