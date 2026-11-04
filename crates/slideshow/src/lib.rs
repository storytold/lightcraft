//! Slideshow slides for LightCraft: the settings of Lightroom Classic's Slideshow Layout, Options
//! and Backdrop panels ([`SlideSettings`]) and the CPU composer that draws one slide from a
//! rendered photo ([`compose`]).
//!
//! No UI and no catalog here: playback, the settings preview, the `slideshow.preview` command and
//! (later) slideshow export all call [`compose`], so every one of them draws the same pixels.
//!
//! **Resolution independence:** every length is relative to the slide: margins, the stroke width
//! and the shadow's offset and radius are fractions of the slide's *shorter side* (so linked
//! margins are equal in pixels, and a slide composed at two sizes has the same proportions).
//! Angles are degrees counter-clockwise from the right (−45° points down and to the right).
//!
//! **Never crash:** settings are validated by [`SlideSettings::merged`] (a wrong type is an error)
//! and clamped by [`SlideSettings::sanitized`] (NaN, ±∞, negative and huge values land in range);
//! [`compose`] accepts any size and any photo (an empty one draws the backdrop alone).
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod compose;
mod settings;

pub use compose::{Layout, Rect, compose, compose_with, layout, photo_size};
pub use settings::{Aspect, MARGIN_MAX, MAX_SIDE, SHADOW_OFFSET_MAX, SHADOW_RADIUS_MAX, STROKE_MAX, SlideSettings, hex, parse_color};
