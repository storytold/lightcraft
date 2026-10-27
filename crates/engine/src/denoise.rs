//! AI denoise, the session's side (`docs/denoise.md`): which photos have a denoised picture, making the missing ones
//! in the background, and keeping the cache within its size.
//!
//! The denoised picture of a raw photo is *cached data*, never a file in the library: it is made from the untouched
//! original by the model the user chose, kept in `<library>/denoise/<key>.lcdn`, and thrown away whenever space is
//! needed (it is made again when it is next wanted). Its name is a hash of the model, the algorithm and the photo's
//! content, so a changed file or another model simply does not find it. The Denoise *amount* is an ordinary develop
//! setting (`enhance.denoise`): a render mixes the plain and the denoised picture by it ([`crate::media`]).
//!
//! What runs where: the session thread only decides (which photos, which model) and keeps an index of what is ready
//! ([`crate::media::DenoiseIndex`], memory only). One background thread at a time decodes a photo, runs the model on
//! its mosaic tile by tile and writes the product ([`make_product`]); an export that finds its photo's product missing
//! makes it itself, on its own thread, so it never comes out without the denoise it was asked for.
//!
//! Failure is a message, never a panic: a file that is not a Bayer raw, a model that does not load, a full disk, a
//! panic inside the runtime and a cancelled job all end as an [`Outcome`] the pump records.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::time::{Duration, SystemTime};

use lightcraft_catalog::{Photo, PhotoId, Source};
use lightcraft_denoise::bayer::Layout;
use lightcraft_denoise::manifest::DenoiserManifest;
use lightcraft_denoise::run::{Control, Params, TileRunner, denoise_bayer};
use lightcraft_denoise::{Error as RunError, product};
use lightcraft_develop::DevelopSettings;
use lightcraft_preview::Hasher128;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::media::{DenoiseSpec, MakeProduct, PairLoader};
use crate::merge::ByteReader;
use crate::model_download::Downloads;
use crate::{Selection, Session};

mod gpu;
mod model;
mod products;
mod session;
mod settings;
mod state;

#[cfg(test)]
pub(crate) use gpu::GPU_SETUP_MARKER;
#[cfg(all(feature = "denoise", test))]
use gpu::{GPU_CRASHED, GPU_MAX_FALLBACKS, GPU_PARALLEL, SETTING_UP, card_is_faster, set_up_card};
pub use model::RunOn;
#[cfg(all(feature = "denoise", test))]
pub(crate) use model::default_loader;
#[cfg(all(feature = "denoise", test))]
use model::{GpuSide, Network, RemoteSide, network, worth_retrying};
pub(crate) use model::{Loader, Model};
#[cfg(feature = "denoise")]
pub use model::{Serving, serving};
pub(crate) use products::eligible;
#[cfg(test)]
pub(crate) use products::{JobSpec, MakeError, Progress, make_product};
pub use settings::{DEFAULT_CACHE_GB, DIR, EXT};
pub(crate) use settings::{Settings, forget_gpu_set_ups, installed_models, read_capped, read_settings, write_atomic, write_settings};
pub(crate) use state::State;
pub use state::{Pace, PhotoState};

#[cfg(all(test, feature = "denoise"))]
mod gpu_tests;
