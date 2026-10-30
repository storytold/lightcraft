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
/// has more room than it needs to keep [`PINNED_PAGE_MIN_HEIGHT`] for itself.
const PINNED_PREVIEW_SHARE: f32 = 0.4;

/// What the preview's heading, the one-line hint under the slide, its Photo / Intro / Ending
/// choice and the rule under it take around the slide (at the default spacing, about 82 pt).
const PREVIEW_CHROME: f32 = 82.0;

/// What the same takes once the page has been drawn a second time (measured, at the default
/// spacing: 104 pt). The room a pinned preview leaves for the page is checked against this, not
/// against [`PREVIEW_CHROME`], so a window just tall enough never ends up with a page below
/// its least and a dialog that grows a point on this tab.
const PREVIEW_CHROME_DRAWN: f32 = 104.0;

/// The least room the page keeps under the pinned preview: a few rows, with the filter row above
/// them in the smallest window (900 x 560) that the preview is still pinned in.
const PINNED_PAGE_MIN_HEIGHT: f32 = 100.0;

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
    // The filter, above the tabs it looks through. What is typed is kept from one frame of the
    // window to the next only: Settings opened again starts with it empty.
    let filter_id = egui::Id::unique("settings-filter");
    let pass = ui.ctx().cumulative_pass_nr();
    let kept: Option<Filter> = ui.data(|d| d.get_temp(filter_id));
    let mut filter = kept.filter(|k| k.pass.checked_add(1) == Some(pass)).unwrap_or_default();
    filter.pass = pass;
    let had_text = !filter.text.is_empty();
    let typed = crate::text_field::TextField::singleline("field:settingsFilter", &mut filter.text)
        .hint(crate::i18n::tr("Filter settings"))
        .width(ui.available_width())
        .show(ui);
    let filter_row = ui.cursor().top() - top;
    // Esc with something typed empties the filter and is used up by that, wherever the focus is
    // (in the box, or on a section that was found); the focus goes back to the box.
    let escape = had_text && !crate::widgets::escape_taken(ui.ctx()) && ui.input(|i| i.key_pressed(egui::Key::Escape));
    if typed.cancelled() || escape {
        filter.text.clear();
        if had_text {
            crate::widgets::take_escape(ui.ctx());
            typed.response.request_focus();
        }
    }
    // (an emptied filter forgets what it read: the pages are read afresh the next time it is
    // used, so what is found is what they show then)
    if filter.text.trim().is_empty() {
        filter.sections = None;
    }
    // ↓ in the box goes to the first section found
    let to_first = typed.response.has_focus() && ui.input(|i| i.key_pressed(egui::Key::ArrowDown));
    let open_first = typed.ending == Some(crate::text_field::Ending::Return);
    // (with something typed the page is what was found, of any tab: no tab is the open one)
    let filtering = !filter.text.trim().is_empty();
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
        for (id, label) in TABS {
            if crate::widgets::text_button(ui, &format!("settingsTab-{id}"), label, tab == id && !filtering).clicked() {
                *tab = id.to_string();
                // (a tab opened is its page asked for: the filter gives way)
                filter.text.clear();
            }
        }
    });
    ui.separator();
    // With something typed, the page gives way to the sections found, of whatever tab.
    let words = filter.text.trim().to_lowercase();
    let now = ui.input(|i| i.time);
    let found = (!words.is_empty()).then(|| filter.found(app, &words, now));
    let mut goto = None;
    // (a point less than the room left, so rounding never makes the dialog's own scroll area scroll)
    let room = max_height - (ui.cursor().top() - top) - 1.0;
    let mut height = room.min(PAGE_SIZE.y);
    // The slide preview stays above the page on Slideshow: hovering a template in the Template
    // Browser previews it there, so it has to stay in view however far down the templates are
    // scrolled to. With its heading and choice it takes [`PINNED_PREVIEW_SHARE`] of the room
    // (at least the least tall slide); where that leaves less than [`PINNED_PAGE_MIN_HEIGHT`] for the
    // page, it scrolls with the page instead (as in the short window above). What was found
    // is drawn in its place while the filter has something typed.
    let mut pinned = false;
    if tab == "slideshow" && found.is_none() && room >= PAGE_MIN_HEIGHT {
        // (the filter row is taken off the page, not off the preview: the slide is as large as it was)
        let slide_h = ((room + filter_row).min(PAGE_SIZE.y) * PINNED_PREVIEW_SHARE - PREVIEW_CHROME).max(crate::panels::slide_layout::PREVIEW_MIN_H);
        if height - (slide_h + PREVIEW_CHROME_DRAWN) >= PINNED_PAGE_MIN_HEIGHT {
            let before = ui.cursor().top();
            crate::panels::slide_layout::preview(app, ui, &Tokens::get(ui.ctx()), slide_h);
            ui.add_space(2.0);
            ui.separator();
            height = (height - (ui.cursor().top() - before)).max(PINNED_PAGE_MIN_HEIGHT);
            pinned = true;
        }
    }
    let mut page = |ui: &mut egui::Ui| match &found {
        Some(found) => goto = results(ui, found, &words, open_first, to_first),
        None => {
            // (a section chosen that starts its tab, with no heading of its own: the page goes to its top)
            if ui
                .data(|d| d.get_temp::<(&'static str, String)>(egui::Id::unique(GOTO)))
                .is_some_and(|(to, section)| to == tab.as_str() && section.is_empty())
            {
                ui.scroll_to_cursor(Some(egui::Align::TOP));
            }
            if pinned {
                slideshow_tab(app, ui, &Tokens::get(ui.ctx()), false);
            } else {
                body(app, ui, tab.as_str());
            }
            // (what the page took: wider than the room for it would mean a control is cut off)
            register(ui, "settings:page", ui.min_rect());
        }
    };
    if room < PAGE_MIN_HEIGHT {
        page(ui);
    } else {
        // A page that goes on below shows its scroll handle all the time: without it, a page a
        // line or two too long looks cut off, not scrollable.
        ui.spacing_mut().scroll.dormant_handle_opacity = 0.6;
        let salt = if found.is_some() { "found" } else { tab.as_str() };
        let area = egui::ScrollArea::vertical()
            .id_salt(("settings-page", salt))
            .max_height(height)
            .min_scrolled_height(height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(2.0);
                page(ui);
            });
        register(ui, "settings:pageRoom", area.inner_rect);
    }
    // A section chosen from what was found: its tab opens (on the next frame, as a clicked tab
    // does), the filter is emptied, and the section's heading is scrolled to when it is drawn.
    let goto_id = egui::Id::unique(GOTO);
    if let Some((to, section)) = goto {
        *tab = to.to_string();
        filter.text.clear();
        ui.data_mut(|d| d.insert_temp(goto_id, (to, section)));
        ui.ctx().request_repaint();
    } else if ui.data(|d| d.get_temp::<(&'static str, String)>(goto_id)).is_some_and(|(to, _)| to == tab.as_str()) {
        // (the page it was for has been drawn: its heading has had its chance)
        ui.data_mut(|d| d.remove::<(&'static str, String)>(goto_id));
    }
    ui.data_mut(|d| d.insert_temp(filter_id, filter));
}

/// Where the section to scroll to is kept between the click on a result and its page being drawn.
const GOTO: &str = "settings-goto";

/// A section of Settings as the filter knows it: the tab it is on, its heading's English name
/// (empty for what a tab draws above its first heading) and every piece of text it draws.
#[derive(Clone)]
struct Section {
    tab: &'static str,
    name: String,
    texts: Vec<String>,
}

/// The filter's state between frames: what is typed, and the sections of every tab with their
/// text, read once for the language and library they were read in.
#[derive(Clone, Default)]
struct Filter {
    pass: u64,
    text: String,
    sections: Option<(String, std::sync::Arc<Vec<Section>>)>,
}

impl Filter {
    /// The sections that have every word of `words` (lower case), in the order of the tabs.
    fn found(&mut self, app: &mut LightcraftApp, words: &str, now: f64) -> Vec<Section> {
        let of = format!("{}|{}", app.ui.language.code(), app.session.library.as_ref().map(|l| l.dir.display().to_string()).unwrap_or_default());
        let sections = match &self.sections {
            Some((read_for, sections)) if *read_for == of => sections.clone(),
            _ => {
                let sections = std::sync::Arc::new(sections(app, now));
                self.sections = Some((of, sections.clone()));
                sections
            }
        };
        sections.iter().filter(|section| section.has(words)).cloned().collect()
    }
}

impl Section {
    /// The name of the section's tab, in the language shown.
    fn tab_name(&self) -> &'static str {
        TABS.iter().find(|(id, _)| *id == self.tab).map_or("", |(_, label)| crate::i18n::tr(label))
    }

    /// Whether the section, with the name of its tab, has every word of `words` (lower case).
    fn has(&self, words: &str) -> bool {
        let all = format!("{} {}", self.tab_name(), self.texts.join(" ")).to_lowercase();
        words.split_whitespace().all(|word| all.contains(word))
    }

    /// What the section is called in the list of what was found: its tab, then its heading.
    fn title(&self) -> String {
        let tab = self.tab_name();
        if self.name.is_empty() { tab.to_string() } else { format!("{tab} › {}", crate::i18n::tr(&self.name)) }
    }
}

/// Every section of every tab with the text it draws, read by drawing each tab's page once in a
/// context of its own: nothing of it reaches the screen, the list of widgets or the window's own
/// memory, and whatever a tab draws is found without a list kept by hand. Drawing a page asks
/// what opening its tab asks (the smart previews folder, the installed models): the answers the
/// app keeps (the installed denoise models) are filled as opening the tab fills them, those a
/// page keeps in its own context (the smart previews folder, the face models) are asked again,
/// and the slide preview of Slideshow may be asked to render, as that page does. No setting is
/// changed by it, as nothing is clicked. What a page does not draw is not found: the choices of
/// a closed list, tooltips, and rows scrolled out of a list inside a page.
fn sections(app: &mut LightcraftApp, now: f64) -> Vec<Section> {
    fn texts(shape: &egui::Shape, out: &mut Vec<(f32, String)>) {
        match shape {
            egui::Shape::Text(text) => out.push((text.pos.y, text.galley.job.text.clone())),
            egui::Shape::Vec(shapes) => shapes.iter().for_each(|shape| texts(shape, out)),
            _ => {}
        }
    }
    let scratch = egui::Context::default();
    crate::theme::install_fonts(&scratch);
    // (tall enough that no page is cut off: what is outside the screen is not drawn)
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(PAGE_SIZE.x + 32.0, 20_000.0));
    let mut all = Vec::new();
    for (tab, _) in TABS {
        // (on the window's clock: what a page remembers with the time it asked is not left stamped
        // with another)
        let input = egui::RawInput { screen_rect: Some(screen), time: Some(now), ..Default::default() };
        let mut output = scratch.run_ui(input, |ui| {
            ui.set_width(PAGE_SIZE.x);
            body(app, ui, tab);
        });
        // (the font atlas this context made is for no screen: said so, or dropping it is an error)
        output.textures_delta.clear();
        // the page's headings, top to bottom, and each text under the last heading above it
        let mut headings: Vec<(f32, String)> = crate::widgets::take_registry(&scratch)
            .into_iter()
            .filter_map(|(id, r)| Some((r.top(), id.strip_prefix("heading:")?.to_string())))
            .collect();
        headings.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut drawn = Vec::new();
        output.shapes.iter().for_each(|clipped| texts(&clipped.shape, &mut drawn));
        drawn.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut of_tab: Vec<Section> = std::iter::once(String::new())
            .chain(headings.iter().map(|(_, name)| name.clone()))
            .map(|name| Section { tab, name, texts: Vec::new() })
            .collect();
        for (y, text) in drawn {
            // (a text counts as under a heading from a little above the heading's own top)
            let under = headings.iter().take_while(|(top, _)| *top <= y + 1.0).count();
            if let Some(section) = of_tab.get_mut(under)
                && !text.trim().is_empty()
            {
                section.texts.push(text);
            }
        }
        all.extend(of_tab.into_iter().filter(|section| !section.texts.is_empty()));
    }
    all
}

/// The sections found, each a row to choose (`settingsFound:{tab}:{section}`): its tab and
/// heading, and under them the first texts of it that have a word of `words`. Gives the section
/// chosen, by a click, by Return or Space with the focus on it, or (`open_first`) by Return in
/// the filter, which chooses the first. `focus_first` gives the first row the keyboard focus.
fn results(ui: &mut egui::Ui, found: &[Section], words: &str, open_first: bool, focus_first: bool) -> Option<(&'static str, String)> {
    let t = Tokens::get(ui.ctx());
    if found.is_empty() {
        ui.add_space(8.0);
        ui.label(RichText::new(crate::i18n::tr("No setting matches what is typed.")).color(t.text_label));
        hint(ui, &t, "Try one word, or part of one: the names of tabs and sections, labels and explanations are all looked through.");
        return None;
    }
    let mut chosen = open_first.then(|| found.first().map(|first| (first.tab, first.name.clone()))).flatten();
    for (place, section) in found.iter().enumerate() {
        let fill = ui.painter().add(egui::Shape::Noop);
        let row = ui
            .scope_builder(egui::UiBuilder::new().sense(egui::Sense::click()), |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.add_space(3.0);
                ui.add(egui::Label::new(RichText::new(section.title()).font(t.semibold(13.0)).color(t.text)).selectable(false));
                // (what of it has a word that was typed; its heading is in the line above already)
                let heading = crate::i18n::tr(&section.name);
                let with: Vec<&str> = section
                    .texts
                    .iter()
                    .map(String::as_str)
                    .filter(|text| *text != heading && words.split_whitespace().any(|word| text.to_lowercase().contains(word)))
                    .take(2)
                    .collect();
                for text in with {
                    ui.add(egui::Label::new(RichText::new(text).size(11.0).color(t.text_dim)).selectable(false).truncate());
                }
                ui.add_space(3.0);
            })
            .response;
        if focus_first && place == 0 {
            row.request_focus();
        }
        let around = row.rect.expand2(egui::vec2(6.0, 0.0));
        if row.hovered() || row.has_focus() {
            ui.painter().set(fill, egui::Shape::rect_filled(around, egui::CornerRadius::same(4), t.hover));
        }
        if row.has_focus() {
            ui.painter().rect_stroke(around, egui::CornerRadius::same(4), egui::Stroke::new(2.0, t.text), egui::StrokeKind::Inside);
        }
        row.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, section.title()));
        register(ui, format!("settingsFound:{}:{}", section.tab, section.name), row.rect);
        if row.clicked() {
            chosen = Some((section.tab, section.name.clone()));
        }
    }
    chosen
}
