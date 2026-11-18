//! The shared backend the AI features run their models through (issue #491): semantic search embeddings (#487),
//! aesthetic scores (#488), culling suggestions (#489) and orientation detection (#490).
//!
//! - [`device`]: which device the jobs run on — the GPU when a usable one exists and the preference allows it, else
//!   the CPU, with the reason kept.
//! - [`queue`]: one [`Queue`] with a worker thread of its own, so a model never runs on the UI thread. Jobs are taken
//!   in priority order (someone waiting first, import-time work after), can be cancelled one by one or all at once,
//!   and fall back to the CPU when the GPU fails. A batch row ([`Rows`]) shows the work in the activity stack.
//! - [`manifest`] and [`known`]: self-describing `manifest.json` (version, role, SHA-256 per file, SPDX licence) and
//!   the catalogue of known open-weights checkpoints (`laion/CLIP-ViT-B-32-laion2B-s34B-b79K`).
//! - [`encoder`], [`tokenizer`], [`weights`] and [`clip`]: clean-room MIT CLIP ViT + BPE text transformer encoder on
//!   `candle-core`.
//! - [`store`]: versioned binary embedding cache (`ml-embeddings-<id>.bin`) with atomic writes and torn-tail recovery.
//!
//! It is pure `std` (no `unsafe`, no C), and never panics: a job that panics ends as [`Outcome::Failed`], and the
//! worker keeps going.
//!
//! See `docs/ml-backend.md`.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod accel;
#[cfg(not(target_arch = "wasm32"))]
pub mod clip;
pub mod device;
pub mod encoder;
pub mod known;
pub mod manifest;
pub mod queue;
pub mod store;
pub mod tokenizer;
#[cfg(not(target_arch = "wasm32"))]
pub mod weights;

pub use accel::{AccelFactory, Accelerator, BlockWeights, TextTowerWeights, TowerWeights, VisionTowerWeights};
#[cfg(not(target_arch = "wasm32"))]
pub use clip::{Clip, ClipError};
pub use device::{Device, Kind, Preference, Probe, select};
pub use encoder::{Encoder, RgbImage, preprocess_image};
pub use known::{CLIP_VIT_B32_ID, Download, DownloadFile, clip_vit_b32};
pub use manifest::{
    Commercial, InputSpec, Licence, MANIFEST_FILE, MANIFEST_VERSION, ManifestError, ModelFile, ModelManifest, Role, VerifyError, sha256_file,
    valid_file_name, valid_id,
};
pub use queue::{Config, Ctx, Finished, Job, JobError, Outcome, Priority, Queue, Row, Rows, Status, SubmitError, Work};
pub use store::{Hit, Record, Store, StoreError, cosine, fingerprint, l2_normalize, store_file_name};
pub use tokenizer::{EncodedText, Tokenizer, TokenizerError};

#[cfg(test)]
mod tests;
