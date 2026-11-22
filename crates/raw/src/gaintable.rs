//! DNG `ProfileGainTableMap` (tag 52525, DNG 1.6) and `ProfileGainTableMap2` (tag 52544, DNG 1.7),
//! backed by `craft_develop::sensor::gaintable`.

pub use craft_develop::sensor::gaintable::{GainEval, GainTableMapApply, SourcePlacement};
pub use craft_raw::gaintable::GainTableMap;
