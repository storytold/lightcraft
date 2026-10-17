//! Printing for LightCraft: from the pages the Print module composes to a device.
//!
//! The Print module (`lightcraft-engine`'s `print`) composes pages and writes them as a print-ready
//! PDF. This crate is what carries that to a printer, and it is the part that does not need egui:
//!
//! * [`spool`]: the system spooler (craft-print's list of printers and its `submit`), for a PDF.
//! * [`sheets`]: the composed pages as 8-bit sRGB [`sheets::Sheet`]s and the [`sheets::SheetConverter`]
//!   that makes PWG raster or JPEG of them for a printer that lists no PDF. A photo app has the pixels
//!   already, so the converter needs no PDF renderer: it is registered with craft-print's registry, which
//!   decides *whether* to convert (the printer does not list the document's format) and *at what
//!   resolution* (the job's own, else the listed one nearest 300 dpi).
//! * [`network`] (feature `network-print`, not on wasm32): one printer reached over WS-Print or IPP
//!   ([`network::Station`]) behind a private craft-print registry, and the tracked print job on it. Everything
//!   in it except the converter is generic across the Craft apps (same shape as `pdfcraft-print`'s
//!   `network` module) and is the part to share from craft-libs later.
//!
//! Safety rules this crate keeps: no printer is opened, queried or sent anything until the app asks; a
//! job needs a `UserConfirmed` (the ticked box); a document is sent once, and an unknown outcome is shown,
//! not retried.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod sheets;
pub mod spool;

#[cfg(all(feature = "network-print", not(target_arch = "wasm32")))]
pub mod network;
