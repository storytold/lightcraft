//! Identifiers from the app's previous name that are stored in users' files. They are file-format
//! tags, not branding: changing them would stop existing files from opening, so they stay as written.
//! This is the only engine file allowed to spell the previous name.

/// The `format` tag of a preset file (written and read).
pub const PRESET_FORMAT: &str = "lightcraft.preset";
/// The `format` tag of an exported curve-preset file (written and read).
pub const CURVE_PRESETS_FORMAT: &str = "lightcraft.curvePresets";
/// The previous product name as a word in preset folder names, skipped when naming a group.
pub const NAME_WORD: &str = "lightcraft";
/// The previous desktop executable name (lock owner notes).
pub const BINARY: &str = "lightcraft";
