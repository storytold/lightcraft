//! The Classic Folders panel's own parts: the header buttons and the volume rows.

use egui::Rect;

use crate::DacApp;

/// The + menu at the right end of the Folders header.
pub fn header_buttons(_app: &mut DacApp, _ui: &mut egui::Ui, _header: Rect, _viewport: Rect) {}

/// One row per disk the library's photos are on, with its free space.
pub fn volumes(_app: &mut DacApp, _ui: &mut egui::Ui) {}
