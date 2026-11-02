//! Control surfaces: hardware dials, keys and buttons that drive LightCraft.
//!
//! Portions © RAD Films - Roman Alurkoff (from TactileBridge), MIT.
//!
//! This crate is the engine behind Settings ▸ Controls and the `surface.*` control-channel methods
//! (`docs/control-surfaces.md`). It has no UI and no I/O: the host (the egui app) feeds it input
//! events and the current [`Context`], runs the [`Effect`]s it returns through the command
//! registry, and pushes its [`layout::DeviceLayout`] to connected surfaces.
//!
//! - [`devices`]: the controls of each supported device (the Logitech MX Creative Console first;
//!   MIDI and Tangent panels plug in as more device definitions).
//! - [`binding`], [`profile`]: what a control does — a develop value, a command, a page switch, a
//!   modifier — grouped into pages and profiles, with pages that follow the app's context.
//! - [`targets`]: the values a dial can move (develop sliders, mask sliders, brush and view values).
//! - [`motion`]: ticks to value deltas — speed, acceleration curve, fine/coarse modifiers, invert.
//! - [`runtime`]: the live engine — coalescing, one undo step per dial gesture, dial focus, pages.
//! - [`settings`]: everything the user tunes, stored as `control-surfaces.json`.
//! - [`loupedeck`]: the Loupedeck+ console's controls, their defaults, and importing a Loupedeck
//!   profile's layout.
//! - [`tactilebridge`]: imports a TactileBridge `settings.json` and its Logi profiles (`.lp5`).
//! - [`catalog`]: every action in categories (the editor's picker; plugins list them in their app).
//! - [`protocol`]: pairing tokens and the message shapes of the surface protocol.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod binding;
pub mod catalog;
pub mod context;
pub mod defaults;
pub mod devices;
pub mod icons;
pub mod layout;
pub mod loupedeck;
pub mod motion;
pub mod profile;
pub mod protocol;
pub mod runtime;
pub mod settings;
pub mod tactilebridge;
pub mod targets;
pub mod zip;

pub use binding::{Binding, Modifier};
pub use context::Context;
pub use devices::{ControlDef, ControlKind, DeviceDef, PageGroup};
pub use profile::{Page, Profile};
pub use runtime::{Effect, Input, Runtime};
pub use settings::SurfaceSettings;
pub use targets::{TargetKind, TargetSpec};
