//! Text in photos: PaddleOCR's PP-OCRv6 (Apache-2.0) on candle, in pure Rust. A text detector finds the lines, each
//! line is cut out and straightened, a small classifier turns upside-down lines, and the recogniser reads them, in
//! about 50 languages (Chinese, Japanese and Latin scripts included) with one alphabet.
//!
//! The graphs are PaddlePaddle's own ONNX exports, run by a small executor for the operators they use ([`exec`],
//! reading ONNX with [`onnx`]); pre- and post-processing follow PaddleOCR 3's pipeline ([`reader`], [`db`], [`ctc`], [`image`]).
//! The model sets and their pinned files are in [`known`], with the licence a user accepts before the download.
//!
//! The pieces that need no model (images, boxes, decoding, the model list) build everywhere; the models only run
//! natively (the web build reads no text).
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod architecture;
pub mod ctc;
pub mod db;
pub mod image;
pub mod known;

#[cfg(not(target_arch = "wasm32"))]
pub mod exec;
#[cfg(not(target_arch = "wasm32"))]
pub mod onnx;
#[cfg(not(target_arch = "wasm32"))]
pub mod reader;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The model's files are missing, damaged or not the model expected.
    #[error("text model: {0}")]
    Model(String),
    /// A download was asked for without the user having accepted the licence (named here).
    #[error("the {0} licence of the text model has not been accepted")]
    NotAcknowledged(String),
    /// A model file could not be downloaded.
    #[error("downloading the text model: {0}")]
    Download(String),
    /// Bad input (an empty image, a buffer of the wrong size).
    #[error("{0}")]
    Input(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(not(target_arch = "wasm32"))]
impl From<candle_core::Error> for Error {
    fn from(e: candle_core::Error) -> Self {
        Error::Model(e.to_string())
    }
}
