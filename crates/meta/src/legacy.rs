//! Format identifiers inherited from the project's previous name. They are written into users'
//! XMP sidecars, so they never change: renaming them would orphan every existing develop setting.

/// The develop-settings XMP namespace URI (prefix `lc`), as found in existing sidecars.
pub const LC_NS: &str = "http://ns.lightcraft.app/lc/1.0/";
