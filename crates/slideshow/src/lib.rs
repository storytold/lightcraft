//! The Classic Slideshow module's logic, independent of the UI: settings and templates
//! ([`settings`]), text tokens ([`tokens`]), timing and order ([`timeline`]), the slide compositor
//! ([`compose`]), music ([`music`]) and the JPEG-sequence export ([`export`]).
//!
//! The UI (`crates/ui-egui/src/slideshow_ui.rs`) previews and plays slides with these; the full
//! screen show of View ▸ Slideshow keeps working as the impromptu slideshow.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod compose;
pub mod export;
pub mod music;
pub mod settings;
pub mod timeline;
pub mod tokens;

pub use settings::{SavedSlideshow, Settings, Template, builtin_templates};
pub use timeline::{Frame, Plan, Segment};
pub use tokens::SlideInfo;
