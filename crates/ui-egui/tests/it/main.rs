//! The integration tests, one module per file, linked into one test binary.
//! `tests/shutdown.rs` stays a binary of its own: it closes the GPU for the whole process.

mod accessibility;
mod dialog_role;
mod docking_workflows;
mod grid_names;
mod landmarks;
mod local_places;
mod photo_name;
mod sidebar_names;
mod thumb_size_slider;
