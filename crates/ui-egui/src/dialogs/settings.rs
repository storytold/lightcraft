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
    // (counted in frames, not passes: a pass that draws only a prompt, or one that egui discards,
    // does not draw Settings, and Settings is not shown anew for having missed it)
    let frame = ui.ctx().cumulative_frame_nr();
    let kept: Option<Filter> = ui.data(|d| d.get_temp(filter_id));
    // (Settings shown anew: the filter starts empty, and nothing is marked as landed on)
    let opened = !kept.as_ref().is_some_and(|k| k.frame == frame || k.frame.checked_add(1) == Some(frame));
    if opened {
        ui.data_mut(|d| d.remove::<(String, f64)>(egui::Id::unique(LANDED)));
    }
    let mut filter = kept.filter(|_| !opened).unwrap_or_default();
    filter.frame = frame;
    // (the mark is for arriving at a section from the filter: a tab left, however, and come back
    // to shows none. Arriving sets it again when the heading is drawn, further down this frame.)
    if filter.tab != *tab {
        ui.data_mut(|d| d.remove::<(String, f64)>(egui::Id::unique(LANDED)));
    }
    filter.tab.clone_from(tab);
    let had_text = !filter.text.is_empty();
    let typed = crate::text_field::TextField::singleline("field:settingsFilter", &mut filter.text)
        .hint(crate::i18n::tr("Filter settings"))
        .width(ui.available_width())
        .show(ui);
    let filter_row = ui.cursor().top() - top;
    // The box is ready to type in when Settings opens: the filter reaches every section from
    // wherever one is. (Not where typing brings up a keyboard over the window.)
    if opened && !matches!(ui.ctx().os(), egui::os::OperatingSystem::Android | egui::os::OperatingSystem::IOS) {
        typed.response.request_focus();
    }
    // Esc with something typed empties the filter and is used up by that, wherever the focus is
    // (in the box, or on a section that was found); the focus goes back to the box. Not while the
    // dialog is disabled behind a prompt or the library window: that owns the keys (`dialogs::show`).
    let owns_keys = ui.is_enabled();
    let escape = owns_keys && had_text && !crate::widgets::escape_taken(ui.ctx()) && ui.input(|i| i.key_pressed(egui::Key::Escape));
    if owns_keys && (typed.cancelled() || escape) {
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

/// Where the section landed on from the filter is kept while its heading is marked: its name
/// and when.
const LANDED: &str = "settings-landed";

/// How long the heading of the section landed on stays marked, in seconds.
const LANDED_FOR: f64 = 1.6;

/// Words people type for a section that the section does not say itself: the tab, the heading's
/// English name (empty for what a tab draws above its first heading) and the words, in lower
/// case and in the spelling [`fold`] gives. They are English whatever language is shown (in the
/// others a section is found by what it says), and count as much as a word in an explanation.
pub const ALSO: &[(&str, &str, &str)] = &[
    ("general", "", "locale translation translate"),
    ("general", "Startup", "launch start"),
    ("general", "Culling", "trash bin delete deleted"),
    ("general", "Library", "catalog database"),
    ("performance", "Smart previews", "proxy proxies offline"),
    ("import", "Raw defaults", "preset develop"),
    ("import", "Metadata", "iptc author credit"),
    ("import", "Cameras and Cards", "memory card sd camera connected device"),
    ("import", "Auto Import", "watch tether"),
    ("import", "XMP sidecars", "metadata"),
    ("general", "External Editing", "editor photoshop color space bit depth"),
    ("performance", "Rendering", "graphics card speed resolution"),
    ("performance", "Thumbnail cache", "disk space storage previews"),
    ("display", "Monitor profile", "screen color management calibration calibrate"),
    ("interface", "Appearance", "theme color scheme skin"),
    ("slideshow", "Playback", "presentation autoplay crossfade"),
    ("faces", "Face recognition", "people person"),
    ("denoise", "AI Denoise", "noise reduction"),
];

/// Pairs of spellings of one word, the one [`fold`] gives first.
const SPELLINGS: &[(&str, &str)] =
    &[("color", "colour"), ("gray", "grey"), ("catalog", "catalogue"), ("favorite", "favourite"), ("center", "centre"), ("license", "licence")];

/// `text` as the filter compares it: in lower case, and in one spelling of the words that have
/// two (colour as color), so that either finds the other.
fn fold(text: &str) -> String {
    let mut folded = text.to_lowercase();
    for (one, other) in SPELLINGS {
        if folded.contains(other) {
            folded = folded.replace(other, one);
        }
    }
    folded
}

/// A word typed (in lower case) in the one spelling ([`fold`]), which a word still being typed
/// in the other is brought to as well: "colou" is on the way to "colour" and is compared as
/// "color", "catalogu" as "catalog". Only once it has gone past where the two spellings part,
/// so that "colo" stays what it is.
fn fold_typed(word: &str) -> String {
    let folded = fold(word);
    for (one, other) in SPELLINGS {
        let shared = one.chars().zip(other.chars()).take_while(|(a, b)| a == b).count();
        // (the longest start of the other spelling the word ends in, past what the two share)
        let ends_in = other.char_indices().skip(shared + 1).filter_map(|(at, _)| other.get(..at)).filter(|start| folded.ends_with(start)).last();
        if let Some(stem) = ends_in.and_then(|start| folded.strip_suffix(start)) {
            return format!("{stem}{one}");
        }
    }
    folded
}

/// A text as the filter compares it: in lower case, and in one spelling ([`fold`]).
#[derive(Clone)]
struct Compared {
    lower: String,
    folded: String,
}

impl Compared {
    fn of(text: &str) -> Self {
        let lower = text.to_lowercase();
        Self { folded: fold(&lower), lower }
    }

    /// Whether the text has `word` (in lower case): as typed, or in the other spelling of it. As
    /// typed counts too, so that a word is found all the way while it is typed ("colou" is on
    /// the way to "colour", and is in no text once that is spelt "color").
    fn has(&self, word: &str) -> bool {
        self.lower.contains(word) || self.folded.contains(&fold_typed(word))
    }
}

/// A text a section draws: as drawn, as the filter compares it, and whether it is in the small
/// type of an explanation.
#[derive(Clone)]
struct Drawn {
    text: String,
    compared: Compared,
    small: bool,
}

/// A section of Settings as the filter knows it: the tab it is on, its heading's English name
/// (empty for what a tab draws above its first heading) and every piece of text it draws.
#[derive(Clone)]
struct Section {
    tab: &'static str,
    name: String,
    texts: Vec<Drawn>,
}

/// The filter's state between frames: what is typed, and the sections of every tab with their
/// text, read once for the language, library and display profile they were read in. (What is
/// typed is kept only from one pass to the next: a pass in which Settings is not drawn loses it.)
#[derive(Clone, Default)]
struct Filter {
    frame: u64,
    text: String,
    /// The tab that was open on the frame before.
    tab: String,
    sections: Option<(String, std::sync::Arc<Vec<Section>>)>,
}

impl Filter {
    /// The sections that have every word of `words` (in lower case), the best match first
    /// ([`Section::rank`]) and those that match alike in the order of the tabs.
    fn found(&mut self, app: &mut LightcraftApp, words: &str, now: f64) -> Vec<Section> {
        let of = format!(
            "{}|{}|{:?}",
            app.ui.language.code(),
            app.session.library.as_ref().map(|l| l.dir.display().to_string()).unwrap_or_default(),
            // (the Display page says which profile is in use)
            app.renderer.display_id()
        );
        let sections = match &self.sections {
            Some((read_for, sections)) if *read_for == of => sections.clone(),
            _ => {
                let sections = std::sync::Arc::new(sections(app, now));
                self.sections = Some((of, sections.clone()));
                sections
            }
        };
        let mut found: Vec<(u32, &Section)> = sections.iter().filter_map(|section| Some((section.rank(words)?, section))).collect();
        // (a stable sort: the order of the tabs is kept among equals)
        found.sort_by_key(|(rank, _)| *rank);
        found.into_iter().map(|(_, section)| section.clone()).collect()
    }
}

impl Section {
    /// The name of the section's tab, as shown.
    fn tab_name(&self) -> &'static str {
        TABS.iter().find(|(id, _)| *id == self.tab).map_or("", |(_, label)| crate::i18n::tr(label))
    }

    /// How well the section matches `words` (in lower case), less being better, or `None` when one
    /// of them is nowhere in it. Each word counts by the best place it is found in: 0 in the
    /// name of the section or its tab, 1 in a label, choice or button, 2 in an explanation or
    /// among the words people type for the section ([`ALSO`], from the start of one).
    fn rank(&self, words: &str) -> Option<u32> {
        let names = Compared::of(&format!("{} {}", self.tab_name(), crate::i18n::tr(&self.name)));
        let also = || ALSO.iter().filter(|(tab, name, _)| *tab == self.tab && *name == self.name).flat_map(|(_, _, also)| also.split_whitespace());
        words
            .split_whitespace()
            .map(|word| {
                if names.has(word) {
                    Some(0)
                } else if self.texts.iter().any(|drawn| !drawn.small && drawn.compared.has(word)) {
                    Some(1)
                } else if self.texts.iter().any(|drawn| drawn.compared.has(word)) || also().any(|also| also.starts_with(&fold_typed(word))) {
                    Some(2)
                } else {
                    None
                }
            })
            .sum()
    }

    /// What the section is called in the list of what was found: its tab, then its heading.
    fn title(&self) -> String {
        let tab = self.tab_name();
        if self.name.is_empty() { tab.to_string() } else { format!("{tab} › {}", crate::i18n::tr(&self.name)) }
    }
}

/// Where in `text` a word of `words` (in lower case) stands, as ranges of bytes of `text`, in
/// the order they stand in and none inside another: in either case and either spelling.
fn places(text: &str, words: &str) -> Vec<std::ops::Range<usize>> {
    // each char of the text in lower case, with where it starts (a char that is several in lower
    // case counts by its first: the ranges are of `text`, which `to_lowercase` would not keep)
    let chars: Vec<(usize, char)> = text.char_indices().map(|(at, c)| (at, c.to_lowercase().next().unwrap_or(c))).collect();
    let mut found: Vec<std::ops::Range<usize>> = Vec::new();
    for word in words.split_whitespace() {
        // (as typed, in the one spelling, and in the other)
        let folded = fold_typed(word);
        let other = SPELLINGS.iter().find(|(one, _)| folded.contains(one)).map(|(one, other)| folded.replace(one, other));
        let mut spellings = vec![word.to_string()];
        spellings.extend(std::iter::once(folded).chain(other).filter(|spelt| spelt != word));
        for spelt in spellings {
            let spelt: Vec<char> = spelt.chars().collect();
            if spelt.is_empty() {
                continue;
            }
            for start in 0..chars.len() {
                let Some(run) = chars.get(start..start.saturating_add(spelt.len())) else { break };
                if run.iter().map(|(_, c)| c).eq(spelt.iter()) {
                    let from = run.first().map_or(0, |(at, _)| *at);
                    let to = chars.get(start + spelt.len()).map_or(text.len(), |(at, _)| *at);
                    found.push(from..to);
                }
            }
        }
    }
    found.sort_by_key(|range| (range.start, std::cmp::Reverse(range.end)));
    let mut apart: Vec<std::ops::Range<usize>> = Vec::new();
    for range in found {
        if apart.last().is_none_or(|last| range.start >= last.end) {
            apart.push(range);
        }
    }
    apart
}

/// How far into a line under a result the first word found may stand, in chars, before the
/// line is started later so that the word is in sight.
const SNIPPET_LEAD: usize = 40;

/// The line shown under a result for `text`: `text` itself, or from a word's start shortly
/// before the first word found with `…` before it when that word stands far in (the line is cut
/// at its end by the room there is), and where the words found stand in what is given.
fn snippet(text: &str, words: &str) -> (String, Vec<std::ops::Range<usize>>) {
    let found = places(text, words);
    let Some(first) = found.first().map(|range| range.start) else { return (text.to_string(), found) };
    let before = text.get(..first).unwrap_or("");
    if before.chars().count() <= SNIPPET_LEAD {
        return (text.to_string(), found);
    }
    // (from the start of a word, a few words before the one found)
    let lead = before.char_indices().rev().nth(SNIPPET_LEAD / 2).map_or(0, |(at, _)| at);
    let from = before.get(lead..).and_then(|rest| rest.find(' ')).map_or(lead, |space| lead + space + 1).min(first);
    let Some(shown) = text.get(from..) else { return (text.to_string(), found) };
    let shift = '…'.len_utf8();
    let found = found.into_iter().map(|range| range.start - from + shift..range.end - from + shift).collect();
    (format!("…{shown}"), found)
}

/// The key [`sections`] sets in the context it reads the pages in.
const READING_FOR_FILTER: &str = "settingsReadingForFilter";

/// Whether `ctx` is the context the filter reads the pages in ([`sections`]): a page draws as usual but starts no work
/// (a render, say) for text nobody sees.
pub(crate) fn reading_for_filter(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(egui::Id::unique(READING_FOR_FILTER))).unwrap_or(false)
}

/// Every section of every tab with the text it draws, read by drawing each tab's page once in a
/// context of its own: nothing of it reaches the screen, the list of widgets or the window's own
/// memory, and whatever a tab draws is found without a list kept by hand. Drawing a page asks
/// what opening its tab asks (the smart previews folder, the installed models): the answers the
/// app keeps (the installed denoise models) are filled as opening the tab fills them, and those a
/// page keeps in its own context (the smart previews folder, the face models) are asked again.
/// The pages know they are only being read ([`reading_for_filter`]): the slide preview of
/// Slideshow is not asked to render, as nothing of it is shown. No setting is
/// changed by it, as nothing is clicked. What a page does not draw is not found: the choices of
/// a closed list, tooltips, and rows scrolled out of a list inside a page.
fn sections(app: &mut LightcraftApp, now: f64) -> Vec<Section> {
    fn texts(shape: &egui::Shape, out: &mut Vec<(egui::Pos2, String, bool)>) {
        match shape {
            egui::Shape::Text(text) => {
                // (an explanation is in 11 pt; a label, choice or button in 12 pt or more)
                let small = text.galley.job.sections.first().is_some_and(|section| section.format.font_id.size < 12.0);
                out.push((text.pos, text.galley.job.text.clone(), small));
            }
            egui::Shape::Vec(shapes) => shapes.iter().for_each(|shape| texts(shape, out)),
            _ => {}
        }
    }
    let scratch = egui::Context::default();
    scratch.data_mut(|d| d.insert_temp(egui::Id::unique(READING_FOR_FILTER), true));
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
        let registry = crate::widgets::take_registry(&scratch);
        let mut headings: Vec<(f32, String)> =
            registry.iter().filter_map(|(id, r)| Some((r.top(), id.strip_prefix("heading:")?.to_string()))).collect();
        // (where a folder or file is shown, [`path_label`], and what is in a text box: where a
        // library, a cache or an editor happens to be, or the name someone typed, is not what a
        // setting is called, and is not looked through)
        let paths: Vec<egui::Rect> =
            registry.iter().filter(|(id, _)| id.starts_with("path:") || id.starts_with("field:")).map(|(_, r)| r.expand(1.0)).collect();
        headings.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut drawn = Vec::new();
        output.shapes.iter().for_each(|clipped| texts(&clipped.shape, &mut drawn));
        drawn.sort_by(|a, b| a.0.y.total_cmp(&b.0.y));
        let mut of_tab: Vec<Section> = std::iter::once(String::new())
            .chain(headings.iter().map(|(_, name)| name.clone()))
            .map(|name| Section { tab, name, texts: Vec::new() })
            .collect();
        for (at, text, small) in drawn {
            // (a text counts as under a heading from a little above the heading's own top)
            let under = headings.iter().take_while(|(top, _)| *top <= at.y + 1.0).count();
            if let Some(section) = of_tab.get_mut(under)
                && !text.trim().is_empty()
                && !paths.iter().any(|path| path.contains(at))
            {
                section.texts.push(Drawn { compared: Compared::of(&text), text, small });
            }
        }
        all.extend(of_tab.into_iter().filter(|section| !section.texts.is_empty()));
    }
    all
}

/// Where the chars at `found` (ranges of bytes of the galley's text) are drawn, from the
/// galley's corner: a rectangle for each stretch of them in each row.
fn marks(galley: &egui::Galley, found: &[std::ops::Range<usize>]) -> Vec<egui::Rect> {
    let hit: Vec<bool> = galley.text().char_indices().map(|(at, _)| found.iter().any(|range| range.contains(&at))).collect();
    let mut rects = Vec::new();
    let mut at = 0;
    for row in &galley.rows {
        let mut open: Option<egui::Rect> = None;
        // (glyphs stay in the order of the chars, one for each, whichever way the text runs: a word
        // written from right to left has its glyphs placed from the right, not listed from it)
        for (place, glyph) in (at..).zip(row.glyphs.iter()) {
            if hit.get(place).copied().unwrap_or(false) {
                let left = row.pos.x + glyph.pos.x;
                let rect = egui::Rect::from_x_y_ranges(left..=left + glyph.advance_width, row.min_y()..=row.max_y());
                open = Some(open.map_or(rect, |open| open.union(rect)));
            } else if glyph.advance_width > 0.0 {
                rects.extend(open.take());
            }
        }
        rects.extend(open);
        at += row.glyphs.len() + usize::from(row.ends_with_newline);
    }
    rects
}

/// The line under a result, with the chars at `found` (ranges of bytes of it) told apart by a
/// fill behind them and a line under them (the line for whoever cannot tell the fill's colour from
/// the row's). The line of text is one piece in one format: a piece is shaped by itself, so
/// letters found inside a word, in a font or a colour of their own, would cut the word in three
/// (no kerning across the cuts, and in Arabic the letters either side of them left unjoined).
pub(crate) fn found_line(ui: &mut egui::Ui, t: &Tokens, line: String, found: &[std::ops::Range<usize>]) {
    let fill = t.chrome.lerp_to_gamma(t.accent, 0.3);
    // (legible on the fill a hovered or focused row has, and so on a plain one, and on the fill
    // behind what was found: the line is in one colour)
    let dim = t.legible(t.legible(t.legible(t.text_dim, t.hover, 4.5), t.chrome, 4.5), fill, 4.5);
    let format = egui::TextFormat { font_id: t.font(11.0), color: dim, ..Default::default() };
    let job = egui::text::LayoutJob::single_section(line, format);
    let (at, galley, response) = egui::Label::new(job).selectable(false).truncate().layout_in_ui(ui);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, ui.is_enabled(), galley.text()));
    if ui.is_rect_visible(response.rect) {
        // (the text colour, which stands furthest from every surface: on the fill and on a plain, hovered or focused row)
        let under = egui::Stroke::new(1.0, t.text);
        for rect in marks(&galley, found) {
            let rect = rect.translate(at.to_vec2()).expand2(egui::vec2(1.0, 0.0));
            ui.painter().rect_filled(rect, egui::CornerRadius::same(2), fill);
            ui.painter().hline(rect.x_range(), rect.bottom() - 1.0, under);
        }
        if galley.elided {
            response.on_hover_text(galley.text().to_string());
        }
        ui.painter().add(egui::epaint::TextShape::new(at, galley, dim));
    }
}

/// The sections found, each a row to choose (`settingsFound:{tab}:{section}`): its tab and
/// heading, and under them the first texts of it that have a word of `words` ([`snippet`]). Gives the section
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
                let with = section
                    .texts
                    .iter()
                    .filter(|drawn| drawn.text != heading && words.split_whitespace().any(|word| drawn.compared.has(word)))
                    .take(2);
                for drawn in with {
                    let (line, found) = snippet(&drawn.text, words);
                    found_line(ui, &t, line, &found);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Services;

    /// One pass of the Settings window with the filter holding `text` and Escape pressed, in a ui
    /// that is disabled (`dialogs::show` does that while a prompt or the library window owns input)
    /// or not; what the filter holds afterwards.
    fn text_after_escape(enabled: bool) -> String {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let mut tab = "general".to_string();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 900.0));
        let mut pass = |app: &mut LightcraftApp, events: Vec<egui::Event>, enabled: bool| {
            let input = egui::RawInput { screen_rect: Some(screen), events, ..Default::default() };
            let mut output = ctx.run_ui(input, |ui| {
                if !enabled {
                    ui.disable();
                }
                window(app, ui, &mut tab, 800.0);
            });
            output.textures_delta.clear();
        };
        let filter_id = egui::Id::unique("settings-filter");
        // (a pass draws the window, which keeps its filter for the pass after it)
        pass(&mut app, Vec::new(), true);
        ctx.data_mut(|d| {
            let mut kept: Filter = d.get_temp(filter_id).expect("the window keeps its filter");
            kept.text = "sidecar".into();
            d.insert_temp(filter_id, kept);
        });
        let escape = egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: Some(egui::Key::Escape),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        pass(&mut app, vec![escape], enabled);
        let kept: Filter = ctx.data(|d| d.get_temp(filter_id)).expect("the filter is kept");
        kept.text
    }

    #[test]
    fn escape_empties_the_filter_only_while_the_dialog_owns_the_keys() {
        assert_eq!(text_after_escape(true), "", "Escape empties what was typed");
        assert_eq!(text_after_escape(false), "sidecar", "behind a prompt or the library window it is not the dialog's");
    }
}
