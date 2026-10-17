//! Names the browser build stored under its previous product name. They name data already in
//! users' browsers, so they never follow the brand: changing them would orphan saved libraries.

/// The IndexedDB database holding the library when OPFS is unavailable.
pub const IDB_NAME: &str = "lightcraft";
/// The Web Locks name that keeps a library to one tab; shared with tabs still running an older build.
pub const TAB_LOCK: &str = "lightcraft-library";
