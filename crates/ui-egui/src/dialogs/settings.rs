//! The Settings dialog (⌘,): General, Import, Performance, Display, Interface, Slideshow, Faces,
//! AI Denoise.
//!
//! Changes apply immediately (no OK/Cancel). Where they are stored:
//! - **app settings** ([`crate::state::AppSettings`]: startup view, delete confirmation, GPU,
//!   preview size, filmstrip/grid badges, last library, monitor profile, whether Import Lightroom
//!   Catalog… asks where a catalog goes) and Auto Advance live in
//!   the UI state,
//!   saved by the host in its config folder (`ui.json`);
//! - **library settings** (import defaults, XMP sidecars, thumbnail cache size, slideshow
//!   playback) go through the `library.preferences` / `library.xmpPreferences` /
//!   `slideshow.playback` commands into the library's `prefs.json`,
//!   so they travel with the library.

use egui::RichText;
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::state::{GridBadges, PREVIEW_LIMITS, StartupView};
use crate::theme::Tokens;
use crate::widgets::register;

mod appearance;
mod display;
mod general;
mod import;
mod libraries;
mod performance;
mod slideshow;
mod widgets;

pub use display::display_profile;
pub(super) use display::view_options;
use display::{display_tab, interface_tab};
use general::general_tab;
use import::import_tab;
pub(crate) use libraries::{can_switch_library, free_name};
pub use libraries::{forget_library, library_name, new_library, open_library, open_library_name, recent_libraries, remember_library};
use performance::performance_tab;
use slideshow::slideshow_tab;
pub(crate) use widgets::{check, choices, heading, hint, row};

pub mod slide_layout;
pub mod slide_overlays;
pub mod slide_templates;
pub mod slide_titles;

/// (id, label) of the tabs, in order.
pub const TABS: &[(&str, &str)] = &[
    ("general", "General"),
    ("import", "Import"),
    ("performance", "Performance"),
    ("display", "Display"),
    ("interface", "Interface"),
    ("slideshow", "Slideshow"),
    ("faces", "Faces"),
    ("denoise", "AI Denoise"),
];

/// Thumbnail cache sizes offered (MB).
const CACHE_SIZES: [u32; 5] = [512, 1024, 2048, 4096, 8192];

const LABEL_W: f32 = 150.0;

/// The page of `tab` (the tab bar in [`window`] switches `tab`).
pub fn body(app: &mut LightcraftApp, ui: &mut egui::Ui, tab: &str) {
    let t = Tokens::get(ui.ctx());
    match tab {
        "import" => import_tab(app, ui, &t),
        "performance" => performance_tab(app, ui, &t),
        "display" => display_tab(app, ui, &t),
        "interface" => interface_tab(app, ui, &t),
        "slideshow" => slideshow_tab(app, ui, &t, true),
        "faces" => crate::panels::faces::settings_tab(app, ui, &t),
        "denoise" => crate::panels::denoise::settings_tab(app, ui, &t),
        _ => general_tab(app, ui, &t),
    }
}

/// The widest the Settings window's content gets, and the tallest its pages get: the window is
/// one size on every tab, so it stays where it is as the tabs are gone through.
const PAGE_SIZE: egui::Vec2 = egui::vec2(560.0, 640.0);

/// The least room in which a page scrolls by itself. In a window with less (far below the
/// desktop minimum), the tabs and the page scroll together in the dialog's own scroll area.
const PAGE_MIN_HEIGHT: f32 = 120.0;

/// The share of the page's room the pinned slide preview of the Slideshow tab takes, when the page
/// has more room than it needs to keep [`PAGE_MIN_HEIGHT`] for itself.
const PINNED_PREVIEW_SHARE: f32 = 0.4;

/// What the preview's heading, the one-line hint under the slide, its Photo / Intro / Ending
/// choice and the rule under it take around the slide (at the default spacing, about 82 pt).
const PREVIEW_CHROME: f32 = 82.0;

/// The Settings window's content: the tab bar, which stays put, above the page of `tab`
/// ([`body`]), which scrolls when it is taller than the room for it, with a scroll handle that
/// shows without the pointer on it. `max_height` is the room the app window leaves for both: the
/// two together never take more, so the scroll area every dialog is drawn in has nothing to
/// scroll (but for a window with less than [`PAGE_MIN_HEIGHT`] for the page, where that area
/// scrolls the two together). Every page gets the same room whatever it holds (but for the pinned
/// preview of Slideshow), and each keeps a scroll position of its own. Nothing here is wider than the window from 640 pt of app window
/// up: a row's controls wrap ([`row`]) and paths are shortened ([`path_label`]).
/// On Slideshow the slide preview is pinned between the tab bar and the page (where the room
/// allows), and comes off that page's room.
pub fn window(app: &mut LightcraftApp, ui: &mut egui::Ui, tab: &mut String, max_height: f32) {
    ui.set_width((ui.ctx().content_rect().width() - 64.0).clamp(240.0, PAGE_SIZE.x));
    let top = ui.cursor().top();
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
        for (id, label) in TABS {
            if crate::widgets::text_button(ui, &format!("settingsTab-{id}"), label, tab == id).clicked() {
                *tab = id.to_string();
            }
        }
    });
    ui.separator();
    // (a point less than the room left, so rounding never makes the dialog's own scroll area scroll)
    let room = max_height - (ui.cursor().top() - top) - 1.0;
    if room < PAGE_MIN_HEIGHT {
        body(app, ui, tab.as_str());
        register(ui, "settings:page", ui.min_rect());
        return;
    }
    let mut height = room.min(PAGE_SIZE.y);
    // The slide preview stays above the page on Slideshow: hovering a template in the Template
    // Browser previews it there, so it has to stay in view however far down the templates are
    // scrolled to. With its heading and choice it takes [`PINNED_PREVIEW_SHARE`] of the room
    // (at least the least tall slide); where that leaves less than [`PAGE_MIN_HEIGHT`] for the
    // page, it scrolls with the page instead (as in the short window above).
    let mut pinned = false;
    if tab == "slideshow" {
        let slide_h = (height * PINNED_PREVIEW_SHARE - PREVIEW_CHROME).max(crate::panels::slide_layout::PREVIEW_MIN_H);
        if height - (slide_h + PREVIEW_CHROME) >= PAGE_MIN_HEIGHT {
            let before = ui.cursor().top();
            crate::panels::slide_layout::preview(app, ui, &Tokens::get(ui.ctx()), slide_h);
            ui.add_space(2.0);
            ui.separator();
            height = (height - (ui.cursor().top() - before)).max(PAGE_MIN_HEIGHT);
            pinned = true;
        }
    }
    // A page that goes on below shows its scroll handle all the time: without it, a page a line
    // or two too long looks cut off, not scrollable.
    ui.spacing_mut().scroll.dormant_handle_opacity = 0.6;
    let page = egui::ScrollArea::vertical()
        .id_salt(("settings-page", tab.as_str()))
        .max_height(height)
        .min_scrolled_height(height)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add_space(2.0);
            if pinned {
                slideshow_tab(app, ui, &Tokens::get(ui.ctx()), false);
            } else {
                body(app, ui, tab.as_str());
            }
            // (what the page took: wider than the room for it would mean a control is cut off)
            register(ui, "settings:page", ui.min_rect());
        });
    register(ui, "settings:pageRoom", page.inner_rect);
}
