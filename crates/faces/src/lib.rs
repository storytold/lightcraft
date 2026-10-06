//! Face models for LightCraft: what a model is ([`ModelManifest`]), whether to trust a description of one
//! ([`manifest::validate`]), the models we know by hash ([`known`]), and a probe that reads an unknown
//! `.onnx` file's input and output shapes without running it ([`onnx`]), so installing a model of your
//! own can be one drop and one licence prompt.
//!
//! Everything here treats model files and manifests as hostile input: sizes are capped, numbers must be
//! finite and sane, and a malformed file is an error, never a panic. Running a model (the `tract` runtime)
//! is not part of this crate yet.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod graph;
pub mod hash;
pub mod known;
pub mod manifest;
pub mod net;
pub mod onnx;
#[cfg(feature = "tract")]
pub mod runtime;
pub mod suggest;
pub mod synthetic;
pub mod yunet;

pub use manifest::{Colour, Commercial, InputSpec, Licence, ManifestError, ModelManifest, OutputSpec, Resize, Role, Thresholds};
