//! The Settings dialog (⌘,): General, Import, Performance, Display, Interface, Slideshow, Faces,
//! AI Denoise.
//!
//! Changes apply immediately (no OK/Cancel). Where they are stored:
//! - **app settings** ([`crate::state::AppSettings`]: startup view, delete confirmation, GPU,
//!   preview size, filmstrip/grid badges, last library, monitor profile) and Auto Advance live in
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

/// (id, label) of the tabs, in order.
pub const TABS: &[(&str, &str)] = &[
    ("general", "General"),
    ("library", "Library"),
    ("import", "Import"),
    ("files", "Files"),
    ("performance", "Performance"),
    ("display", "Display"),
    ("interface", "Interface"),
    ("slideshow", "Slideshow"),
    ("faces", "Faces"),
    ("denoise", "AI Denoise"),
];

/// What a tab is called on the tab bar. Two tabs go by another name than [`TABS`] gives them,
/// so that the monitor's colour profile and how LightCraft looks are not taken for each other:
/// `display` is Monitor and `interface` is Appearance. (Said here and not in `TABS`, whose lines
/// after these two are where other work adds its tabs; ids stay as they are.)
pub fn tab_label(id: &str, label: &'static str) -> &'static str {
    match id {
        "display" => "Monitor",
        "interface" => "Appearance",
        _ => label,
    }
}

/// Thumbnail cache sizes offered (MB).
const CACHE_SIZES: [u32; 5] = [512, 1024, 2048, 4096, 8192];

const LABEL_W: f32 = 150.0;

/// The page of `tab` (the tab bar in [`window`] switches `tab`).
pub fn body(app: &mut LightcraftApp, ui: &mut egui::Ui, tab: &str) {
    let t = Tokens::get(ui.ctx());
    match tab {
        "library" => library_tab(app, ui, &t),
        "files" => files_tab(app, ui, &t),
        "import" => import_tab(app, ui, &t),
        "performance" => performance_tab(app, ui, &t),
        "display" => display_tab(app, ui, &t),
        "interface" => interface_tab(app, ui, &t),
        "slideshow" => slideshow_tab(app, ui, &t),
        "faces" => super::faces::settings_tab(app, ui, &t),
        "denoise" => super::denoise::settings_tab(app, ui, &t),
        _ => general_tab(app, ui, &t),
    }
}

/// The widest the Settings window's content gets, and the tallest its pages get: the window is
/// one size on every tab, so it stays where it is as the tabs are gone through.
const PAGE_SIZE: egui::Vec2 = egui::vec2(560.0, 640.0);

/// The least room in which a page scrolls by itself. In a window with less (far below the
/// desktop minimum), the tabs and the page scroll together in the dialog's own scroll area.
const PAGE_MIN_HEIGHT: f32 = 120.0;

/// The Settings window's content: the tab bar, which stays put, above the page of `tab`
/// ([`body`]), which scrolls when it is taller than the room for it, with a scroll handle that
/// shows without the pointer on it. `max_height` is the room the app window leaves for both: the
/// two together never take more, so the scroll area every dialog is drawn in has nothing to
/// scroll (but for a window with less than [`PAGE_MIN_HEIGHT`] for the page, where that area
/// scrolls the two together). Every page gets the same room whatever it holds, and each keeps a
/// scroll position of its own. Nothing here is wider than the window from 640 pt of app window
/// up: a row's controls wrap ([`row`]) and paths are shortened ([`path_label`]).
pub fn window(app: &mut LightcraftApp, ui: &mut egui::Ui, tab: &mut String, max_height: f32) {
    ui.set_width((ui.ctx().content_rect().width() - 64.0).clamp(240.0, PAGE_SIZE.x));
    // The page drawn is the one that was open when the frame began: a tab clicked now opens on
    // the next frame, so a text box on the page being left is drawn once more and saves what was
    // typed in it (the click took the focus from it).
    let shown = tab.clone();
    let top = ui.cursor().top();
    // The filter, above the tabs it looks through. What is typed is kept from one frame of the
    // window to the next only: Settings opened again starts with it empty.
    // What says where a control is, and what a text box says while it is empty, are made legible
    // here for everything in the window: a theme's border is 1.6 to 1.7:1 against the window and
    // its weak text under 4.3:1 in a box (the tokens keep their values for the rest of the app).
    let t = Tokens::get(ui.ctx());
    legible_controls(ui, &t);
    let filter_id = egui::Id::new("settings-filter");
    let pass = ui.ctx().cumulative_pass_nr();
    let kept: Option<Filter> = ui.data(|d| d.get_temp(filter_id));
    // (Settings shown anew: the filter starts empty, and nothing is marked as landed on)
    let opened = !kept.as_ref().is_some_and(|k| k.pass.checked_add(1) == Some(pass));
    if opened {
        ui.data_mut(|d| d.remove::<(String, f64)>(egui::Id::new(LANDED)));
    }
    let mut filter = kept.filter(|_| !opened).unwrap_or_default();
    filter.pass = pass;
    // (the mark is for arriving at a section from the filter: a tab left, however, and come back
    // to shows none. Arriving sets it again when the heading is drawn, further down this frame.)
    if filter.tab != shown {
        ui.data_mut(|d| d.remove::<(String, f64)>(egui::Id::new(LANDED)));
    }
    filter.tab.clone_from(&shown);
    let had_text = !filter.text.is_empty();
    let typed = crate::text_field::TextField::singleline("field:settingsFilter", &mut filter.text)
        .hint(crate::i18n::tr("Filter settings"))
        .width(ui.available_width())
        .show(ui);
    // The box is ready to type in when Settings opens: the filter reaches every section from
    // wherever one is. (Not where typing brings up a keyboard over the window.)
    if opened && !matches!(ui.ctx().os(), egui::os::OperatingSystem::Android | egui::os::OperatingSystem::IOS) {
        typed.response.request_focus();
    }
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
    // Each line of tabs stands on a line of its own the width of the window, so the open tab's
    // line always has the bar's under it: in a narrow window, or a language with long names, the
    // tabs take two lines or more.
    let mut lines: Vec<f32> = Vec::new();
    // (the bar is one stop for Tab, at the open tab; the arrow keys go along it)
    let mut bar = crate::widgets::Roving::new(ui, "settingsTabs", TABS.iter().position(|(id, _)| *id == shown));
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(4.0, 5.0);
        for (id, label) in TABS {
            let r = crate::widgets::tab_button_in(ui, &mut bar, &format!("settingsTab-{id}"), tab_label(id, label), shown == *id && !filtering);
            let bottom = egui::emath::GuiRounding::round_to_pixels(r.rect.bottom(), ui.pixels_per_point());
            if !lines.iter().any(|line| (line - bottom).abs() < 0.5) {
                lines.push(bottom);
            }
            if r.clicked() {
                *tab = id.to_string();
                // (a tab opened is its page asked for: the filter gives way)
                filter.text.clear();
                ui.ctx().request_repaint();
            }
        }
    });
    bar.end(ui);
    let divider = t.legible(t.divider, t.chrome, 3.0);
    for bottom in lines {
        let line = egui::Rect::from_min_max(egui::pos2(ui.min_rect().left(), bottom), egui::pos2(ui.min_rect().right(), bottom + 1.0));
        ui.painter().rect_filled(line, egui::CornerRadius::ZERO, divider);
    }
    ui.add_space(1.0);
    // With something typed, the page gives way to the sections found, of whatever tab.
    let words = filter.text.trim().to_lowercase();
    let now = ui.input(|i| i.time);
    let found = (!words.is_empty()).then(|| filter.found(app, &words, now));
    let mut goto = None;
    // (a point less than the room left, so rounding never makes the dialog's own scroll area scroll)
    let room = max_height - (ui.cursor().top() - top) - 1.0;
    let mut page = |ui: &mut egui::Ui| match &found {
        Some(found) => goto = results(ui, found, &words, open_first, to_first),
        None => {
            body(app, ui, shown.as_str());
            // (what the page took: wider than the room for it would mean a control is cut off)
            register(ui.ctx(), "settings:page", ui.min_rect());
        }
    };
    if room < PAGE_MIN_HEIGHT {
        page(ui);
    } else {
        let height = room.min(PAGE_SIZE.y);
        // A page that goes on below shows its scroll handle all the time: without it, a page a
        // line or two too long looks cut off, not scrollable.
        ui.spacing_mut().scroll.dormant_handle_opacity = 0.6;
        let salt = if found.is_some() { "found" } else { shown.as_str() };
        let area = egui::ScrollArea::vertical()
            .id_salt(("settings-page", salt))
            .max_height(height)
            .min_scrolled_height(height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(2.0);
                page(ui);
            });
        register(ui.ctx(), "settings:pageRoom", area.inner_rect);
    }
    // A section chosen from what was found: its tab opens (on the next frame, as a clicked tab
    // does), the filter is emptied, and the section's heading is scrolled to when it is drawn.
    let goto_id = egui::Id::new(GOTO);
    if let Some((to, section)) = goto {
        *tab = to.to_string();
        filter.text.clear();
        ui.data_mut(|d| d.insert_temp(goto_id, (to, section)));
        ui.ctx().request_repaint();
    } else if ui.data(|d| d.get_temp::<(&'static str, String)>(goto_id)).is_some_and(|(to, _)| to == shown) {
        // (the page it was for has been drawn: its heading has had its chance)
        ui.data_mut(|d| d.remove::<(&'static str, String)>(goto_id));
    }
    ui.data_mut(|d| d.insert_temp(filter_id, filter));
}

/// What says where a control is, and what a text box says while it is empty, made legible for
/// everything drawn in `ui` from here on (the window, and each panel that opens over it, which
/// has a style of its own): the border of a button, checkbox, radio button, list or text box
/// stands 3:1 against the window and against a theme card's fill, a text box in use is
/// outlined in an accent that does too, and placeholder text stands 4.5:1 in its box.
pub(super) fn legible_controls(ui: &mut egui::Ui, t: &Tokens) {
    // (a little over 3:1 as a colour: a radio button's ring is one antialiased pixel wide, and
    // what is drawn of it is fainter than the colour it is drawn in)
    let border = t.legible(t.legible(t.button_border, t.chrome, 3.0), t.inset, 3.6);
    let visuals = ui.visuals_mut();
    visuals.widgets.inactive.bg_stroke.color = border;
    // One look for "the focus is here", whatever the control: a line 2 pt wide in the text
    // colour round it, as round a tab or a choice ([`crate::widgets::focus_ring`]). egui draws a
    // button, checkbox, radio button or list that has the focus (or is being pressed) with its
    // "active" outline, and a text box in use with the selection's.
    visuals.widgets.active.bg_stroke = egui::Stroke::new(2.0, t.text);
    visuals.selection.stroke = egui::Stroke::new(2.0, t.text);
    visuals.weak_text_color = Some(t.legible(t.text_dim, t.field, 4.5));
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
    ("library", "Library", "catalog database"),
    ("library", "Smart previews", "proxy proxies offline"),
    ("import", "Raw defaults", "preset develop"),
    ("import", "Metadata", "iptc author credit"),
    ("import", "Cameras and Cards", "memory card sd camera connected device"),
    ("import", "Auto Import", "watch tether"),
    ("files", "XMP sidecars", "metadata"),
    ("files", "External Editing", "editor photoshop color space bit depth"),
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
/// text, read once for the language and library they were read in.
#[derive(Clone, Default)]
struct Filter {
    pass: u64,
    text: String,
    /// The tab that was open on the frame before.
    tab: String,
    sections: Option<(String, std::sync::Arc<Vec<Section>>)>,
}

impl Filter {
    /// The sections that have every word of `words` (in lower case), the best match first
    /// ([`Section::rank`]) and those that match alike in the order of the tabs.
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
        let mut found: Vec<(u32, &Section)> = sections.iter().filter_map(|section| Some((section.rank(words)?, section))).collect();
        // (a stable sort: the order of the tabs is kept among equals)
        found.sort_by_key(|(rank, _)| *rank);
        found.into_iter().map(|(_, section)| section.clone()).collect()
    }
}

impl Section {
    /// The name of the section's tab, as shown.
    fn tab_name(&self) -> &'static str {
        TABS.iter().find(|(id, _)| *id == self.tab).map_or("", |(id, label)| crate::i18n::tr(tab_label(id, label)))
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

/// Every section of every tab with the text it draws, read by drawing each tab's page once in a
/// context of its own: nothing of it reaches the screen, the list of widgets or the window's own
/// memory, and whatever a tab draws is found without a list kept by hand. Drawing a page asks
/// what opening its tab asks (the smart previews folder, the installed models) and fills the
/// same caches of those answers; no setting is changed by it, as nothing is clicked. What a page
/// does not draw is not found: the choices of a closed list, tooltips, and rows scrolled out of
/// a list inside a page.
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

/// Whether `c` is of a script written from right to left (Hebrew, Arabic and those beside them).
fn right_to_left(c: char) -> bool {
    matches!(u32::from(c), 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF)
}

/// Whether `c` is part of a word written from right to left: its letters and marks, and the joiner
/// and non-joiner between them (Persian writes a non-joiner inside a word), which are shaped with it.
fn in_word_from_the_right(c: char) -> bool {
    right_to_left(c) || matches!(c, '\u{200c}' | '\u{200d}')
}

/// Which char of `chars` each of a row's `glyphs` draws, the row's first being of the char `at`
/// (left at the next row's first). Text written from left to right has a glyph for each char, in
/// their order. A word written from right to left has its glyphs from its last char to its
/// first, a mark before the letter it is on, and after them glyphs of no width that are of no
/// char in particular: there each glyph is placed by the char it says it draws.
///
/// Two things this takes for granted, true of the faces there are today: text from left to right
/// has one glyph a char (a face that draws one char as two glyphs would move the later marks of
/// its row), and two words from right to left in different scripts have something between them
/// (with nothing between them the second is left unmarked).
fn chars_drawn(glyphs: &[egui::epaint::text::Glyph], chars: &[char], at: &mut usize) -> Vec<Option<usize>> {
    let mut drawn = Vec::with_capacity(glyphs.len());
    while let Some(glyph) = glyphs.get(drawn.len()) {
        if !right_to_left(glyph.chr) {
            drawn.push(Some(*at));
            *at = at.saturating_add(1);
            continue;
        }
        let word = chars.get(*at..).map_or(0, |rest| rest.iter().take_while(|c| in_word_from_the_right(**c)).count());
        // (the chars of the word no glyph has been found for yet end before `left`; a mark has no
        // width, and the next glyph may be the letter it is on)
        let (mut left, mut marked) = (*at + word, false);
        while let Some(glyph) = glyphs.get(drawn.len()).filter(|glyph| in_word_from_the_right(glyph.chr)) {
            let before = if marked { left + 1 } else { left }.min(*at + word);
            let place = chars.get(*at..before).and_then(|rest| rest.iter().rposition(|c| *c == glyph.chr)).map(|place| *at + place);
            if let Some(place) = place {
                (left, marked) = (place, glyph.advance_width <= 0.0);
            }
            drawn.push(place);
        }
        *at += word;
    }
    drawn
}

/// Where the chars at `found` (ranges of bytes of the galley's text) are drawn, from the
/// galley's corner: a rectangle for each stretch of them in each row.
fn marks(galley: &egui::Galley, found: &[std::ops::Range<usize>]) -> Vec<egui::Rect> {
    let (chars, hit): (Vec<char>, Vec<bool>) = galley.text().char_indices().map(|(at, c)| (c, found.iter().any(|range| range.contains(&at)))).unzip();
    let mut rects = Vec::new();
    let mut at = 0;
    for row in &galley.rows {
        let mut open: Option<egui::Rect> = None;
        for (glyph, drawn) in row.glyphs.iter().zip(chars_drawn(&row.glyphs, &chars, &mut at)) {
            if drawn.and_then(|place| hit.get(place)).copied().unwrap_or(false) {
                let left = row.pos.x + glyph.pos.x;
                let rect = egui::Rect::from_x_y_ranges(left..=left + glyph.advance_width, row.min_y()..=row.max_y());
                open = Some(open.map_or(rect, |open| open.union(rect)));
            } else if glyph.advance_width > 0.0 {
                rects.extend(open.take());
            }
        }
        rects.extend(open);
        at += usize::from(row.ends_with_newline);
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
    let format = egui::TextFormat { font_id: t.caption(), color: dim, ..Default::default() };
    let job = egui::text::LayoutJob::single_section(line, format);
    let (at, galley, response) = egui::Label::new(job).selectable(false).truncate().layout_in_ui(ui);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, ui.is_enabled(), galley.text()));
    if ui.is_rect_visible(response.rect) {
        // (a line that stands on the fill and on a plain, hovered or focused row)
        let under = egui::Stroke::new(1.0, t.legible(t.legible(t.legible(t.text, t.hover, 3.0), t.chrome, 3.0), fill, 3.0));
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
        row.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, section.title()));
        register(ui.ctx(), format!("settingsFound:{}:{}", section.tab, section.name), row.rect);
        if row.clicked() {
            chosen = Some((section.tab, section.name.clone()));
        }
    }
    chosen
}

/// `path` shortened to fit `max` (as `width` measures it) by dropping leading folders, after
/// either kind of separator, so that its end stays (`…/Pictures/Library`,
/// `…\Pictures\Library`): the end says which folder it is. A last folder too long by itself
/// loses its end ([`crate::widgets::elide_head`]).
fn elide_path(path: &str, max: f32, width: impl Fn(&str) -> f32) -> String {
    if width(path) <= max {
        return path.to_string();
    }
    // (a separator at the very end is not where a name starts: `…/` alone would say nothing)
    let end = path.trim_end_matches(['/', '\\']).len();
    let mut rest = path;
    while let Some(i) = rest.find(['/', '\\']).filter(|i| path.len() - rest.len() + i + 1 < end) {
        let separator = if rest.get(i..).is_some_and(|r| r.starts_with('\\')) { '\\' } else { '/' };
        rest = rest.get(i + 1..).unwrap_or("");
        let shorter = format!("…{separator}{rest}");
        if width(&shorter) <= max {
            return shorter;
        }
    }
    let name = rest.trim_end_matches(['/', '\\']);
    // (nothing but separators: never an empty label)
    if name.is_empty() { "…".to_string() } else { crate::widgets::elide_head(name, max, width) }
}

/// A folder or file path on one line in the room left in its row (`text:{id}`): shortened from
/// the front when it is too long ([`elide_path`]), with all of it in a tooltip then.
fn path_label(ui: &mut egui::Ui, id: &str, path: &str, font: egui::FontId, color: egui::Color32) {
    let shown = elide_path(path, ui.available_width(), |s| ui.painter().layout_no_wrap(s.to_string(), font.clone(), color).size().x);
    // (`truncate` is the last resort, for a room narrower than "…")
    let r = ui.add(egui::Label::new(RichText::new(&shown).font(font).color(color)).truncate().show_tooltip_when_elided(false));
    register(ui.ctx(), format!("text:{id}"), r.rect);
    // (what the filter leaves out of what it looks through; not what stands here in words when
    // there is no folder to show)
    if path.contains(['/', '\\']) {
        register(ui.ctx(), format!("path:{id}"), r.rect);
    }
    if shown != path {
        r.on_hover_text(path);
    }
}

/// What the Smart previews section ([`smart_previews`]) keeps between frames: what is chosen for the previews already
/// built (0 move, 1 leave, 2 delete) and a change of folder that waits for a yes because it would
/// delete them, with the frame, library and folder they were chosen and asked in.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Default)]
struct SmartAsk {
    pass: u64,
    library: String,
    folder: String,
    mode: u8,
    waiting: Option<Value>,
}

/// A text box of the Settings window, shown: Esc in it gives its text back and is used up by
/// that, so Settings stays open (a second Esc, with nothing being edited, closes it).
fn field(ui: &mut egui::Ui, field: crate::text_field::TextField<'_>) -> egui::Response {
    let shown = field.show(ui);
    if shown.cancelled() {
        crate::widgets::take_escape(ui.ctx());
    }
    shown.response
}

/// What [`ask_first`] makes of a button: [`Asked::clicked`] once the person has said yes.
pub(super) struct Asked {
    yes: bool,
}

impl Asked {
    /// The person pressed the button and then said yes to its question.
    pub(super) fn clicked(&self) -> bool {
        self.yes
    }
}

/// A button whose action cannot be taken back, or takes long to make good again, asks before it
/// acts. Pressing `button` (registered by the caller as `id`) opens a small panel under it with
/// `question`, which says what the action costs, and two buttons: `yes` (`{id}:confirm`) and
/// Keep (`{id}:keep`), which has the focus when the panel opens. A click elsewhere or Esc answers no. What comes back stands in for the
/// button: its `clicked` is true on the frame the answer was yes.
pub(super) fn ask_first(t: &Tokens, id: &str, button: egui::Response, question: &str, yes: &str) -> Asked {
    let mut answer = false;
    let opened = button.clicked();
    egui::Popup::from_toggle_button_response(&button).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        ui.set_max_width(280.0);
        legible_controls(ui, t);
        ui.add(egui::Label::new(RichText::new(crate::i18n::tr(question)).color(t.text_label)).wrap());
        ui.horizontal(|ui| {
            let r = ui.button(crate::i18n::tr(yes));
            register(ui.ctx(), format!("{id}:confirm"), r.rect);
            if r.clicked() {
                answer = true;
                ui.close();
                button.request_focus();
            }
            let r = ui.button(crate::i18n::tr("Keep"));
            register(ui.ctx(), format!("{id}:keep"), r.rect);
            // (the question opens with the focus on Keep, so Return or Space alone changes nothing)
            if opened {
                r.request_focus();
            }
            // (answered from the keyboard, the focus goes back to the button the question was about)
            if r.clicked() {
                ui.close();
                button.request_focus();
            }
        });
    });
    Asked { yes: answer }
}

pub(super) fn heading(ui: &mut egui::Ui, t: &Tokens, text: &str) {
    ui.add_space(4.0);
    // (`heading:{its English name}`, whatever the language: where a section starts, for scripts
    // and tests that look for one. Callers pass the name translated or not.)
    let behind = ui.painter().add(egui::Shape::Noop);
    let r = ui.label(RichText::new(crate::i18n::tr(text)).font(t.body_strong()).color(t.text));
    let name = crate::i18n::source_of(text);
    register(ui.ctx(), format!("heading:{name}"), r.rect);
    // (chosen from what the filter found: the section is brought to the top of the page, and its
    // heading is marked for a moment, so that the one chosen is told from its neighbours)
    let now = ui.input(|i| i.time);
    let landed_id = egui::Id::new(LANDED);
    if ui.data(|d| d.get_temp::<(&'static str, String)>(egui::Id::new(GOTO))).is_some_and(|(_, section)| section == name) {
        r.scroll_to_me(Some(egui::Align::TOP));
        ui.data_mut(|d| d.insert_temp(landed_id, (name.to_string(), now)));
    }
    if let Some((_, since)) = ui.data(|d| d.get_temp::<(String, f64)>(landed_id)).filter(|(landed, _)| *landed == name) {
        // (full for the first half of its time, then fading out)
        let left = (1.0 - (now - since) / LANDED_FOR).clamp(0.0, 1.0) as f32;
        if left > 0.0 {
            let strength = (left * 2.0).min(1.0);
            let around = r.rect.expand2(egui::vec2(6.0, 2.0));
            let outline = egui::Stroke::new(1.0, t.legible(t.accent, t.chrome, 3.0).gamma_multiply(strength));
            ui.painter().set(behind, egui::Shape::rect_filled(around, egui::CornerRadius::same(4), t.hover.gamma_multiply(strength)));
            ui.painter().rect_stroke(around, egui::CornerRadius::same(4), outline, egui::StrokeKind::Inside);
            register(ui.ctx(), format!("settingsLanded:{name}"), around);
            ui.ctx().request_repaint();
        } else {
            ui.data_mut(|d| d.remove::<(String, f64)>(landed_id));
        }
    }
}

pub(super) fn row<R>(ui: &mut egui::Ui, t: &Tokens, label: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(egui::vec2(LABEL_W, 24.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_min_width(LABEL_W);
            // (a label longer than its column wraps in it; it does not push the controls along)
            ui.add(egui::Label::new(RichText::new(crate::i18n::tr(label)).color(t.text_label)).wrap());
        });
        // The controls wrap in the room beside the label, each further line starting under the
        // first control, so a row with many choices grows down and never widens the window.
        let wrapped = egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true);
        ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 24.0), wrapped, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            add(ui)
        })
        .inner
    })
    .inner
}

pub(super) fn hint(ui: &mut egui::Ui, t: &Tokens, text: &str) {
    // (an explanation is small text: the dim colour where it reads, else as near it as reads)
    ui.label(RichText::new(crate::i18n::tr(text)).font(t.caption()).color(t.legible(t.text_dim, t.chrome, 4.5)));
}

/// A checkbox addressable as `check:{id}`; true when toggled.
pub(super) fn check(ui: &mut egui::Ui, id: &str, value: &mut bool, label: &str) -> bool {
    let r = ui.checkbox(value, crate::i18n::tr(label));
    register(ui.ctx(), format!("check:{id}"), r.rect);
    // (ringed round the whole of it while it has the focus, as a choice or a tab is)
    crate::widgets::focus_ring(ui, &Tokens::get(ui.ctx()), &r, r.rect.expand2(egui::vec2(3.0, 1.0)));
    r.changed()
}

/// Mutually exclusive buttons (`button:{id}-{index}`).
pub(super) fn choices<V: PartialEq + Copy>(ui: &mut egui::Ui, id: &str, options: &[(V, &str)], value: &mut V) -> bool {
    let mut changed = false;
    ui.spacing_mut().item_spacing.x = 4.0;
    // (the row is one stop for Tab, at the choice that holds; the arrow keys go along it)
    let mut group = crate::widgets::Roving::new(ui, id, options.iter().position(|(v, _)| *v == *value));
    for (i, (v, l)) in options.iter().enumerate() {
        if crate::widgets::choice_button_in(ui, &mut group, &format!("{id}-{i}"), l, *value == *v).clicked() && *value != *v {
            *value = *v;
            changed = true;
        }
    }
    group.end(ui);
    changed
}

// ------------------------------------------------------------------------------------- General

/// Library: the library itself. Where it is and opening another ([`library_section`]), where
/// its smart previews are kept ([`smart_previews`], for a library on disk), and what leaves its
/// catalog by itself ([`local_folders`]).
fn library_tab(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    library_section(app, ui, t);
    #[cfg(not(target_arch = "wasm32"))]
    smart_previews(app, ui, t);
    local_folders(app, ui, t);
    in_memory_note(app, ui, t);
}

/// Files: what LightCraft writes beside the photo files ([`xmp_sidecars`]) and what it opens
/// them in ([`external_editing`]).
fn files_tab(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    xmp_sidecars(app, ui, t);
    in_memory_note(app, ui, t);
    external_editing(app, ui, t);
}

/// What is saved with the library lasts only as long as a library that is not on disk does:
/// said under such settings wherever they are (Import says it at its foot itself).
fn in_memory_note(app: &LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    if app.session.library.is_none() {
        hint(ui, t, "In-memory session: these settings last until LightCraft quits.");
    }
}

fn general_tab(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    row(ui, t, crate::i18n::tr("Language"), |ui| {
        let languages: Vec<_> = crate::i18n::Locale::ALL.iter().map(|language| (*language, language.name())).collect();
        // Thirteen language names do not fit one line in a narrow window: wrap them.
        ui.horizontal_wrapped(|ui| {
            choices(ui, "settingsLanguage", &languages, &mut app.ui.language);
        });
        crate::i18n::set_language(app.ui.language);
    });
    startup_and_culling(app, ui, t);
}

/// Library: where the open library is, and opening or making another.
fn library_section(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    heading(ui, t, crate::i18n::tr("Library"));
    let location = match &app.session.library {
        Some(l) => l.dir.display().to_string(),
        None => crate::i18n::tr("In memory — nothing is saved").to_string(),
    };
    row(ui, t, crate::i18n::tr("Location"), |ui| {
        path_label(ui, "settingsLocation", &location, egui::TextStyle::Body.resolve(ui.style()), t.text);
    });
    row(ui, t, "", |ui| {
        let can = app.services.pick_folder.is_some();
        let r = ui.add_enabled(can, egui::Button::new(crate::i18n::tr("Open Library…")));
        register(ui.ctx(), "button:settingsOpenLibrary", r.rect);
        if r.clicked() {
            let _ = app.run("app.openLibrary", json!({}));
        }
        let r = ui.add_enabled(can, egui::Button::new(crate::i18n::tr("New Library…")));
        register(ui.ctx(), "button:settingsNewLibrary", r.rect);
        if r.clicked() {
            let _ = app.run("app.newLibrary", json!({}));
        }
        if !can {
            hint(ui, t, crate::i18n::tr("not available here"));
        }
    });
}

/// Startup and Culling: what LightCraft opens in, and how rating and deleting behave.
fn startup_and_culling(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    heading(ui, t, crate::i18n::tr("Startup"));
    row(ui, t, crate::i18n::tr("Open in"), |ui| {
        choices(
            ui,
            "settingsStartup",
            &[(StartupView::Last, "Last view"), (StartupView::Grid, "Photo Grid"), (StartupView::Detail, "Detail")],
            &mut app.ui.settings.startup_view,
        );
    });
    heading(ui, t, crate::i18n::tr("Culling"));
    check(ui, "settings.autoAdvance", &mut app.ui.auto_advance, "Auto Advance: move to the next photo after rating or flagging");
    check(ui, "settings.confirmDelete", &mut app.ui.settings.confirm_delete, "Confirm before moving photos to Recently Deleted");
    check(ui, "settings.askBeforeMove", &mut app.ui.settings.ask_before_move, "Confirm before moving photos to another folder on disk");
}

/// External Editing: the editors a photo is opened in, and the copy they are given.
fn external_editing(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    heading(ui, t, crate::i18n::tr("External Editing"));
    row(ui, t, crate::i18n::tr("PhotoCraft"), |ui| {
        field(
            ui,
            crate::text_field::TextField::singleline("field:photocraftPath", &mut app.ui.settings.photocraft_path)
                .hint(crate::i18n::tr("Found automatically"))
                .width(220.0),
        );
    });
    edit_file_rows(ui, t, "photocraftFile", &mut app.ui.settings.photocraft_file);
    hint(
        ui,
        t,
        crate::i18n::tr(
            "Photo > Edit In > Edit in PhotoCraft (⇧⌘E, or right-click a photo) renders a copy with your edits, stacks it with the original and opens it in PhotoCraft. Save there and the copy updates here. As PSD, PhotoCraft saves straight back to it. Leave the location empty unless you use a portable copy.",
        ),
    );
    row(ui, t, crate::i18n::tr("Other editor"), |ui| {
        field(
            ui,
            crate::text_field::TextField::singleline("field:externalEditor", &mut app.ui.settings.external_editor)
                .hint(crate::i18n::tr("System default"))
                .width(220.0),
        );
    });
    edit_file_rows(ui, t, "externalFile", &mut app.ui.settings.external_file);
    hint(ui, t, crate::i18n::tr("Edit in Other Editor (⌥⌘E) opens the copy in this application (an app name on macOS, a program path elsewhere)."));
    hint(
        ui,
        t,
        crate::i18n::tr(
            "To hand this editor the original file itself instead (the raw, without a copy: DxO PhotoLab, for example), right-click a photo → Edit In → Open Original in Other Editor (RAW, no copy).",
        ),
    );
}

/// The file Edit In hands an editor: format, colour space and bit depth (`button:{id}Format-…`).
fn edit_file_rows(ui: &mut egui::Ui, t: &Tokens, id: &str, file: &mut crate::state::EditFile) {
    use crate::state::{EditFormat, EditSpace};
    row(ui, t, "", |ui| {
        choices(ui, &format!("{id}Format"), &[(EditFormat::Psd, "PSD"), (EditFormat::Tiff, "TIFF")], &mut file.format);
        ui.separator();
        choices(ui, &format!("{id}Depth"), &[(16u8, "16-bit"), (8, "8-bit")], &mut file.bit_depth);
    });
    row(ui, t, "", |ui| {
        choices(
            ui,
            &format!("{id}Space"),
            &[(EditSpace::AdobeRgb, "Adobe RGB"), (EditSpace::ProPhoto, "ProPhoto"), (EditSpace::DisplayP3, "P3"), (EditSpace::Srgb, "sRGB")],
            &mut file.color_space,
        );
    });
}

// -------------------------------------------------------------------------------------- Import

/// A preset picker: `None` = `none_label`. Returns the new choice when it changed.
fn preset_combo(app: &LightcraftApp, ui: &mut egui::Ui, id: &str, current: Option<&str>, none_label: &str) -> Option<Option<String>> {
    let name = |pid: &str| {
        app.session
            .presets
            .iter()
            .find(|p| p.id == pid)
            .map(|p| crate::i18n::builtin_label(&p.name, p.builtin).to_string())
            .unwrap_or_else(|| crate::i18n::tr_format!("{pid} (missing)", pid = pid))
    };
    let text = current.map(name).unwrap_or_else(|| crate::i18n::tr(none_label).to_string());
    let mut out = None;
    let r = crate::widgets::combo_box(id).width(240.0).selected_text(text).show_ui(ui, |ui| {
        if ui.selectable_label(current.is_none(), crate::i18n::tr(none_label)).clicked() {
            out = Some(None);
        }
        if let Some(pid) = crate::panels::presets::picker_rows(ui, id, &app.session.presets, current) {
            out = Some(Some(pid));
        }
    });
    register(ui.ctx(), format!("combo:{id}"), r.response.rect);
    out.filter(|v| v.as_deref() != current)
}

/// Cameras of the raws in the library plus those with a stored default, sorted.
fn cameras(app: &LightcraftApp) -> Vec<String> {
    let mut v: Vec<String> = app
        .session
        .catalog
        .photos()
        .filter(|p| p.kind == lightcraft_catalog::MediaKind::Raw && !p.meta.camera.is_empty())
        .map(|p| p.meta.camera.clone())
        .chain(app.session.import_defaults.cameras.iter().map(|c| c.camera.clone()))
        .collect();
    v.sort_by_key(|c| c.to_lowercase());
    v.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    v
}

fn import_tab(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let d = app.session.import_defaults.clone();
    heading(ui, t, crate::i18n::tr("Raw defaults"));
    hint(ui, t, crate::i18n::tr("Settings new raw photos start from. Changing them doesn't touch photos already in the library."));
    row(ui, t, crate::i18n::tr("Raw photos"), |ui| {
        if let Some(v) = preset_combo(app, ui, "settingsRawPreset", d.raw_preset.as_deref(), "LightCraft Default") {
            let _ = app.run("library.preferences", json!({"import": {"rawPreset": v}}));
        }
    });
    let mut per = d.per_camera;
    row(ui, t, "", |ui| {
        if check(ui, "settings.perCamera", &mut per, "Use camera-specific defaults") {
            let _ = app.run("library.preferences", json!({"import": {"perCamera": per}}));
        }
    });
    if per {
        let cams = cameras(app);
        if cams.is_empty() {
            hint(ui, t, crate::i18n::tr("No raw photos yet: cameras appear here once their photos are in the library."));
        }
        egui::ScrollArea::vertical().max_height(150.0).id_salt("settingsCameras").show(ui, |ui| {
            for (i, cam) in cams.iter().enumerate() {
                let entry = d.cameras.iter().find(|c| c.camera.eq_ignore_ascii_case(cam));
                row(ui, t, cam, |ui| {
                    // "Raw default" = no entry; otherwise the entry's preset (None = LightCraft Default)
                    const RAW_DEFAULT: &str = "\u{1}raw";
                    let current = match entry {
                        None => Some(RAW_DEFAULT),
                        Some(e) => e.preset.as_deref(),
                    };
                    let mut pick = None;
                    let label = match current {
                        Some(RAW_DEFAULT) => crate::i18n::tr("Same as raw default").to_string(),
                        None => crate::i18n::tr("LightCraft Default").to_string(),
                        Some(pid) => app
                            .session
                            .presets
                            .iter()
                            .find(|p| p.id == pid)
                            .map(|p| crate::i18n::builtin_label(&p.name, p.builtin).to_string())
                            .unwrap_or_else(|| pid.to_string()),
                    };
                    let id = format!("settingsCamera-{i}");
                    let r = crate::widgets::combo_box(&id).width(240.0).selected_text(label).show_ui(ui, |ui| {
                        if ui.selectable_label(current == Some(RAW_DEFAULT), crate::i18n::tr("Same as raw default")).clicked() {
                            pick = Some(json!({"camera": cam, "remove": true}));
                        }
                        if ui.selectable_label(current.is_none(), crate::i18n::tr("LightCraft Default")).clicked() {
                            pick = Some(json!({"camera": cam, "preset": null}));
                        }
                        if let Some(pid) = crate::panels::presets::picker_rows(ui, &id, &app.session.presets, current) {
                            pick = Some(json!({"camera": cam, "preset": pid}));
                        }
                    });
                    register(ui.ctx(), format!("combo:{id}"), r.response.rect);
                    if let Some(c) = pick {
                        let _ = app.run("library.preferences", json!({"camera": c}));
                    }
                });
            }
        });
    }
    heading(ui, t, crate::i18n::tr("Other images (JPEG, PNG, TIFF, HEIC…)"));
    row(ui, t, crate::i18n::tr("Non-raw photos"), |ui| {
        if let Some(v) = preset_combo(app, ui, "settingsOtherPreset", d.other_preset.as_deref(), "None") {
            let _ = app.run("library.preferences", json!({"import": {"otherPreset": v}}));
        }
    });
    heading(ui, t, crate::i18n::tr("Metadata"));
    hint(ui, t, crate::i18n::tr("Added to photos you import that don't already have it."));
    for (key, label, hint_text, value) in
        [("copyright", "Copyright", "© 2026 Your Name", d.copyright.clone()), ("creator", "Creator", "Your Name", d.creator.clone())]
    {
        row(ui, t, label, |ui| {
            let id = egui::Id::new(("settingsMeta", key));
            let mut text: String = ui.data(|m| m.get_temp(id)).unwrap_or(value.clone());
            // (the project's text field: Esc gives the text back and stays in Settings, and with
            // the text back the line below has nothing to save)
            let widget = format!("field:settings.{key}");
            let r = field(ui, crate::text_field::TextField::singleline(&widget, &mut text).hint(hint_text).width(240.0));
            register(ui.ctx(), format!("field:settings.{key}"), r.rect);
            if r.lost_focus() && text.trim() != value {
                let _ = app.run("library.preferences", json!({"import": {key: text.trim()}}));
            }
            if r.has_focus() {
                ui.data_mut(|m| m.insert_temp(id, text));
            } else {
                ui.data_mut(|m| m.remove::<String>(id));
            }
        });
    }
    if !app.session.metadata_presets.is_empty() {
        row(ui, t, crate::i18n::tr("Metadata preset"), |ui| {
            let cur = d.metadata_preset.clone();
            let mut pick = None;
            let r = crate::widgets::combo_box("settingsMetaPreset")
                .width(240.0)
                .selected_text(cur.clone().unwrap_or_else(|| crate::i18n::tr("None").into()))
                .show_ui(ui, |ui| {
                    if ui.selectable_label(cur.is_none(), crate::i18n::tr("None")).clicked() {
                        pick = Some(String::new());
                    }
                    for m in &app.session.metadata_presets {
                        if ui.selectable_label(cur.as_deref() == Some(m.name.as_str()), &m.name).clicked() {
                            pick = Some(m.name.clone());
                        }
                    }
                });
            register(ui.ctx(), "combo:settingsMetaPreset", r.response.rect);
            if let Some(n) = pick {
                let _ = app.run("library.preferences", json!({"import": {"metadataPreset": n}}));
            }
        });
    }
    auto_import(app, ui, t);
}

/// XMP sidecars: whether changes are written beside the photos by themselves, and how the files
/// are named.
fn xmp_sidecars(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    heading(ui, t, crate::i18n::tr("XMP sidecars"));
    let mut xmp = app.session.xmp;
    if check(ui, "settings.autoWriteXmp", &mut xmp.auto_write, "Automatically write changes into XMP sidecars") {
        let _ = app.run("library.xmpPreferences", json!({"autoWrite": xmp.auto_write}));
    }
    hint(
        ui,
        t,
        "On, every change to a photo's edits or metadata is written to an XMP file beside the photo, for other apps to read. Off, that happens when you choose Save Metadata to File in the Photo menu.",
    );
    row(ui, t, crate::i18n::tr("Sidecar names"), |ui| {
        use lightcraft_engine::sidecar::SidecarNaming as N;
        let mut n = xmp.naming;
        if choices(ui, "settingsXmpNaming", &[(N::Stem, "IMG_1.xmp"), (N::Full, "IMG_1.CR3.xmp")], &mut n) {
            let naming = if n == N::Full { "full" } else { "stem" };
            let _ = app.run("library.xmpPreferences", json!({"naming": naming}));
        }
    });
}

/// Auto Import: the folder watched for new photos, and what is done with what arrives there.
fn auto_import(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let d = app.session.import_defaults.clone();
    if !cfg!(target_arch = "wasm32") {
        heading(ui, t, crate::i18n::tr("Cameras and Cards"));
        let mut on_card = d.show_on_card;
        if check(ui, "settings.importOnCard", &mut on_card, "Show the import review when a camera or card is connected") {
            let _ = app.run("library.preferences", json!({"import": {"showOnCard": on_card}}));
        }
        heading(ui, t, crate::i18n::tr("Auto Import"));
        hint(ui, t, crate::i18n::tr("Photos that arrive in this folder (tethering, a scanner, a sync app) are added as soon as they're complete."));
        row(ui, t, crate::i18n::tr("Watched folder"), |ui| {
            match &d.auto_folder {
                Some(folder) => path_label(ui, "settingsAutoFolder", folder, egui::TextStyle::Body.resolve(ui.style()), t.text),
                None => drop(ui.label(RichText::new(crate::i18n::tr("Off")).color(t.text))),
            }
            let can = app.services.pick_folder.is_some();
            let r = ui.add_enabled(can, egui::Button::new(crate::i18n::tr("Choose…")));
            register(ui.ctx(), "button:settingsAutoFolder", r.rect);
            if r.clicked()
                && let Some(f) = app.services.pick_folder.as_mut().and_then(|f| f())
                && let Err(e) = app.run("library.autoImport", json!({"folder": f}))
            {
                app.toast(ui.ctx(), e);
            }
            if d.auto_folder.is_some() && ui.button(crate::i18n::tr("Turn Off")).clicked() {
                let _ = app.run("library.autoImport", json!({"folder": null}));
            }
        });
        if d.auto_folder.is_some() {
            row(ui, t, "", |ui| {
                let mut copy = d.auto_copy;
                if ui.checkbox(&mut copy, crate::i18n::tr("Copy into the library (else use the files where they are)")).changed() {
                    let _ = app.run("library.autoImport", json!({"copy": copy}));
                }
            });
            row(ui, t, crate::i18n::tr("Album"), |ui| {
                // (per library: a name left here by one library is not another's)
                let id = egui::Id::new(("auto-album", app.session.library.as_ref().map(|l| l.dir.clone())));
                let mut name = ui.data(|m| m.get_temp::<String>(id)).unwrap_or_else(|| d.auto_album.clone().unwrap_or_default());
                let r = field(
                    ui,
                    crate::text_field::TextField::singleline("field:settingsAutoAlbum", &mut name).hint(crate::i18n::tr("None")).width(180.0),
                );
                if r.lost_focus() {
                    let _ = app.run("library.autoImport", json!({"album": name.trim()}));
                }
                ui.data_mut(|m| m.insert_temp(id, name));
            });
        }
    }
    if app.session.library.is_none() {
        hint(ui, t, crate::i18n::tr("In-memory session: these settings last until LightCraft quits."));
    }
}

// --------------------------------------------------------------------------------- Performance

fn performance_tab(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    use lightcraft_engine::gpu;
    heading(ui, t, crate::i18n::tr("Rendering"));
    check(ui, "settings.gpu", &mut app.ui.settings.gpu, "Use the GPU for rendering");
    let status = if !gpu::available() {
        match gpu::unavailable_reason() {
            Some(why) => crate::i18n::tr_format!("Rendering on the CPU: {why}", why = why),
            None => "No usable GPU found: rendering on the CPU".to_string(),
        }
    } else {
        format!("GPU: {}", gpu::adapter_name().unwrap_or_else(|| crate::i18n::tr("starting…").into()))
    };
    // One line, the whole text on hover: the reason can be a long driver message (no adapter
    // among the backends …), and the GPU state is process-wide, so it may change between frames;
    // a hint that wrapped would move every control below it under the pointer.
    ui.add(egui::Label::new(RichText::new(crate::i18n::tr(&status)).font(t.caption()).color(t.text_dim)).truncate()).on_hover_text(&status);
    row(ui, t, crate::i18n::tr("Preview size"), |ui| {
        let opts: Vec<(u32, String)> =
            PREVIEW_LIMITS.iter().map(|e| (*e, if *e == 0 { crate::i18n::tr("Automatic").to_string() } else { format!("{e} px") })).collect();
        let opts: Vec<(u32, &str)> = opts.iter().map(|(e, l)| (*e, l.as_str())).collect();
        choices(ui, "settingsPreview", &opts, &mut app.ui.settings.preview_limit);
    });
    hint(
        ui,
        t,
        crate::i18n::tr(
            "Automatic renders the Detail view at the size it is shown, up to the photo's own pixels. Choose a size to cap it: smaller is faster.",
        ),
    );
    row(ui, t, crate::i18n::tr("Memory for caches"), |ui| {
        let auto = crate::i18n::tr_format!("Automatic ({} MB)", lightcraft_engine::memory::default_budget() >> 20);
        let opts = [(0u32, auto.as_str()), (512, "512 MB"), (1024, "1 GB"), (2048, "2 GB"), (4096, "4 GB")];
        choices(ui, "settingsMemory", &opts, &mut app.ui.settings.memory_mb);
    });
    heading(ui, t, crate::i18n::tr("Thumbnail cache"));
    let cur = (app.session.cache_bytes() >> 20) as u32;
    row(ui, t, crate::i18n::tr("Size limit"), |ui| {
        let opts = [(512u32, "512 MB"), (1024, "1 GB"), (2048, "2 GB"), (4096, "4 GB"), (8192, "8 GB")];
        let mut v = if CACHE_SIZES.contains(&cur) { cur } else { 2048 };
        if choices(ui, "settingsCache", &opts, &mut v) {
            let _ = app.run("library.preferences", json!({"cacheMb": v}));
        }
    });
    let used = app.session.media.rendered.disk().map(|d| d.size());
    row(ui, t, crate::i18n::tr("In use"), |ui| {
        ui.label(
            RichText::new(used.map(|b| format!("{:.1} MB", b as f64 / 1048576.0)).unwrap_or_else(|| crate::i18n::tr("memory only").to_string()))
                .color(t.text),
        );
        let r = ui.button(crate::i18n::tr("Clear Cache"));
        register(ui.ctx(), "button:settingsClearCache", r.rect);
        let r = ask_first(
            t,
            "button:settingsClearCache",
            r,
            "Thumbnails and previews are made again as you browse, which takes a while in a large library. Your photos and edits are not touched.",
            "Clear Cache",
        );
        if r.clicked() {
            let _ = app.run("library.clearPreviews", json!({}));
        }
    });
}

/// Local folders: when photos only seen in Local, never added or changed, leave the catalog.
fn local_folders(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    heading(ui, t, crate::i18n::tr("Local folders"));
    row(ui, t, crate::i18n::tr("Forget unchanged photos"), |ui| {
        let opts = [(0u32, "Never"), (7, "After 7 days"), (30, "After 30 days"), (90, "After 90 days"), (365, "After a year")];
        let mut v = app.session.forget_local_days;
        if choices(ui, "settingsForgetLocal", &opts, &mut v) {
            let _ = app.run("library.preferences", json!({"forgetLocalDays": v}));
        }
    });
    hint(
        ui,
        t,
        crate::i18n::tr(
            "Photos seen in Local but never added or changed leave the catalog when their folder hasn't been browsed for this long (checked when the library opens). Files stay on disk; browsing the folder shows them again.",
        ),
    );
}

/// Where this library keeps its smart previews (the offline-editing proxies, which can be large):
/// the effective folder, what is in it, and choosing another one.
#[cfg(not(target_arch = "wasm32"))]
fn smart_previews(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    // listing a big folder every frame would be slow: refresh every 2 s and after a change
    let cache = egui::Id::new("smart-location");
    let now = ui.input(|i| i.time);
    let loc: Value = match ui.data(|d| d.get_temp::<(f64, Value)>(cache)) {
        Some((at, v)) if now - at < 2.0 => v,
        _ => {
            let v = app.run("library.smartPreviewsLocation", json!({})).unwrap_or(Value::Null);
            ui.data_mut(|d| d.insert_temp(cache, (now, v.clone())));
            v
        }
    };
    if loc.is_null() {
        return;
    }
    heading(ui, t, crate::i18n::tr("Smart previews"));
    let path = loc["path"].as_str().unwrap_or_default().to_string();
    let available = loc["available"].as_bool().unwrap_or(true);
    let custom = loc["custom"].as_bool().unwrap_or(false);
    row(ui, t, crate::i18n::tr("Folder"), |ui| {
        path_label(
            ui,
            "settingsSmartFolder",
            &path,
            egui::TextStyle::Body.resolve(ui.style()),
            if available { t.text } else { t.legible(t.reject, t.chrome, 4.5) },
        );
        if !custom {
            // (the catalogs have it with the two spaces it once followed a path with)
            ui.label(RichText::new(crate::i18n::tr("  (library default)").trim()).color(t.text));
        }
    });
    let (count, bytes) = (loc["count"].as_u64().unwrap_or(0), loc["bytes"].as_u64().unwrap_or(0));
    if available {
        hint(
            ui,
            t,
            &crate::i18n::tr_format!(
                "{count} smart preview{} · {:.1} MB",
                if count == 1 { "" } else { "s" },
                bytes as f64 / 1048576.0,
                count = count
            ),
        );
    } else {
        hint(
            ui,
            t,
            crate::i18n::tr(
                "This folder is not available (drive disconnected?). Smart previews are not built or used until it is back or you choose another folder.",
            ),
        );
    }
    // what happens to the previews already built when the folder changes
    // That choice, and a change waiting for an answer, are kept from one frame of this section to
    // the next only, and only for this library and this folder: with Settings closed, another
    // tab or library opened, or the folder changed some other way, nothing is left chosen or asked.
    let ask_id = egui::Id::new("smart-ask");
    let pass = ui.ctx().cumulative_pass_nr();
    let library = app.session.library.as_ref().map(|l| l.dir.display().to_string()).unwrap_or_default();
    let kept: Option<SmartAsk> = ui.data(|d| d.get_temp(ask_id));
    let mut ask = kept.filter(|k| k.pass.checked_add(1) == Some(pass) && k.library == library && k.folder == path).unwrap_or_default();
    let mut mode = ask.mode;
    if count > 0 {
        row(ui, t, crate::i18n::tr("Existing previews"), |ui| {
            choices(ui, "settingsSmartExisting", &[(0u8, "Move them"), (1, "Leave them"), (2, "Delete them")], &mut mode);
        });
    }
    let mut existing = ["move", "leave", "discard"][mode.min(2) as usize];
    // Deleting the previews already built is asked about once more, when the folder they would
    // be deleted for is known: until then the change (the command's parameters) waits here.
    // (asked for "Delete them" only: choosing otherwise withdraws the question)
    let mut waiting: Option<Value> = ask.waiting.take().filter(|_| existing == "discard");
    let mut change = None;
    ui.horizontal(|ui| {
        ui.add_space(LABEL_W);
        if app.services.pick_folder.is_some() {
            let r = ui.button(crate::i18n::tr("Choose Folder…"));
            register(ui.ctx(), "button:settingsSmartChoose", r.rect);
            if r.clicked()
                && let Some(dir) = app.services.pick_folder.as_mut().and_then(|f| f())
            {
                change = Some(json!({"path": dir, "existing": existing}));
            }
        }
        let r = ui.add_enabled(custom, egui::Button::new(crate::i18n::tr("Use Library Folder")));
        register(ui.ctx(), "button:settingsSmartReset", r.rect);
        if r.clicked() {
            change = Some(json!({"reset": true, "existing": existing}));
        }
    });
    if existing == "discard" && count > 0 && change.is_some() {
        waiting = change.take();
    }
    let mut answered = false;
    if let Some(params) = &waiting {
        // (said with the count line above and two plain sentences, so no new format string is needed)
        let how_many =
            crate::i18n::tr_format!("{count} smart preview{} · {:.1} MB", if count == 1 { "" } else { "s" }, bytes as f64 / 1048576.0, count = count);
        let question = format!(
            "{} {how_many}. {}",
            crate::i18n::tr("Delete the smart previews in the folder used until now?"),
            crate::i18n::tr(
                "Photos whose originals are not connected are shown and edited from their smart previews; new ones can only be made from originals that are connected."
            ),
        );
        ui.add(egui::Label::new(RichText::new(question).color(t.legible(t.caution, t.chrome, 4.5))).wrap());
        ui.horizontal(|ui| {
            let r = ui.button(crate::i18n::tr("Delete and Change Folder"));
            register(ui.ctx(), "button:settingsSmartDelete", r.rect);
            if r.clicked() {
                // (what the section shows is up to two seconds old: the folder is asked for
                // again, and a question about a folder that is no longer the one is dropped)
                let still = app.run("library.smartPreviewsLocation", json!({})).is_ok_and(|now| now["path"].as_str() == Some(path.as_str()));
                if still {
                    change = Some(params.clone());
                    existing = "discard";
                } else {
                    ui.data_mut(|d| d.remove::<(f64, Value)>(cache));
                }
                answered = true;
            }
            let r = ui.button(crate::i18n::tr("Cancel"));
            register(ui.ctx(), "button:settingsSmartCancel", r.rect);
            answered |= r.clicked();
        });
    }
    if answered || change.is_some() {
        // (the choice goes back to "Move them": deleting is never left set for the next time)
        waiting = None;
        mode = 0;
    }
    ui.data_mut(|d| d.insert_temp(ask_id, SmartAsk { pass, library, folder: path.clone(), mode, waiting }));
    let result = change.map(|params| app.run("library.smartPreviewsLocation", params));
    if let Some(r) = result {
        ui.data_mut(|d| d.remove::<(f64, Value)>(cache));
        match r {
            Ok(v) => {
                let failed = v["failed"].as_array().map_or(0, Vec::len);
                let msg = match (existing, v["handled"].as_u64().unwrap_or(0)) {
                    (_, 0) => "Smart previews folder changed".to_string(),
                    ("move", n) => format!("Smart previews folder changed; moved {n}"),
                    ("discard", n) => format!("Smart previews folder changed; deleted {n}"),
                    _ => "Smart previews folder changed".to_string(),
                };
                app.toast(ui.ctx(), if failed > 0 { crate::i18n::tr_format!("{msg} ({failed} failed)", failed = failed, msg = msg) } else { msg });
            }
            Err(e) => app.toast(ui.ctx(), e),
        }
    }
    hint(ui, t, crate::i18n::tr("Keep it on a drive with room: smart previews are about 1 MB per photo. The thumbnail cache stays in the library."));
}

// ---------------------------------------------------------------------------------- Interface

/// The monitor profile (`app.displayProfile`): previews are shown through the display's ICC
/// profile, so wide-gamut and calibrated displays show colours as they are.
fn display_tab(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    heading(ui, t, crate::i18n::tr("Monitor profile"));
    let current = app.renderer.display().cloned();
    let screen = current.as_ref().is_some_and(|d| d.path == crate::SCREEN_PROFILE);
    row(ui, t, crate::i18n::tr("Profile"), |ui| {
        let name = match &current {
            Some(d) => d.profile.description.clone(),
            None => crate::i18n::tr("None (the display is treated as sRGB)").to_string(),
        };
        ui.label(RichText::new(name).color(t.text));
        if screen {
            hint(ui, t, crate::i18n::tr("this screen's profile"));
        }
    });
    row(ui, t, "", |ui| {
        let can = app.services.pick_display_profile.is_some();
        let r = ui.add_enabled(can, egui::Button::new(crate::i18n::tr("Choose Profile…")));
        register(ui.ctx(), "button:settingsDisplayProfile", r.rect);
        if r.clicked()
            && let Some(path) = app.services.pick_display_profile.as_mut().and_then(|pick| pick().into_iter().next())
        {
            app.ui.settings.display_profile = path;
        }
        // with no profile chosen, previews follow the screen's own profile where the host knows it
        let none = if app.services.screen_profile.is_some() { crate::i18n::tr("Use Screen Profile") } else { crate::i18n::tr("Use sRGB") };
        let r = ui.add_enabled(!app.ui.settings.display_profile.is_empty(), egui::Button::new(none));
        register(ui.ctx(), "button:settingsDisplayProfileNone", r.rect);
        if r.clicked() {
            app.ui.settings.display_profile.clear();
        }
        if !can {
            hint(ui, t, crate::i18n::tr("not available here"));
        }
    });
    if let Some(e) = &app.display_error {
        ui.label(RichText::new(e).font(t.caption()).color(t.legible(t.reject, t.chrome, 4.5)));
    }
    if let Some(d) = &current {
        let info = d.describe();
        if !screen {
            row(ui, t, crate::i18n::tr("File"), |ui| {
                path_label(ui, "settingsDisplayFile", &d.path, t.caption(), t.legible(t.text_dim, t.chrome, 4.5));
            });
        }
        row(ui, t, crate::i18n::tr("Type"), |ui| {
            let kind = match d.profile.kind {
                lightcraft_engine::display::DisplayKind::MatrixTrc => crate::i18n::tr("Matrix/TRC"),
                lightcraft_engine::display::DisplayKind::Lut => crate::i18n::tr("LUT-based"),
            };
            ui.label(RichText::new(kind).color(t.text));
        });
        row(ui, t, crate::i18n::tr("Primaries (x, y)"), |ui| {
            let p = &info["primaries"];
            let xy = |(k, l): (&str, &str)| format!("{l} {:.3}, {:.3}", p[k][0].as_f64().unwrap_or(0.0), p[k][1].as_f64().unwrap_or(0.0));
            let text = [("red", "R"), ("green", "G"), ("blue", "B")].map(xy).join("   ");
            ui.label(RichText::new(text).font(t.caption()).color(t.legible(t.text_dim, t.chrome, 4.5)));
        });
    }
    hint(
        ui,
        t,
        crate::i18n::tr(
            "Photos are shown through your monitor's ICC profile, so colours are right on wide-gamut and calibrated displays, and the Detail view uses the display's whole gamut. The histogram, the preview caches and exports are not affected.",
        ),
    );
    #[cfg(target_os = "linux")]
    hint(ui, t, crate::i18n::tr("Profiles assigned in your desktop's colour settings are usually in ~/.local/share/icc."));
}

fn interface_tab(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let system = app.system_theme(ui.ctx());
    appearance(ui, t, &mut app.ui.settings, system);
    heading(ui, t, crate::i18n::tr("Filmstrip"));
    check(ui, "settings.filmNames", &mut app.ui.settings.film_names, "Show file names");
    check(ui, "settings.filmBadges", &mut app.ui.settings.film_badges, "Show ratings, flags and edit badges");
    heading(ui, t, crate::i18n::tr("Grid"));
    row(ui, t, crate::i18n::tr("Ratings & flags"), |ui| {
        choices(
            ui,
            "settingsGridBadges",
            &[(GridBadges::Auto, "When rated or hovered"), (GridBadges::Always, "Always"), (GridBadges::Never, "Never")],
            &mut app.ui.settings.grid_badges,
        );
    });
    check(ui, "settings.showFilenames", &mut app.ui.show_filenames, "Square Grid: show file names and formats");
    heading(ui, t, crate::i18n::tr("Keywords"));
    row(ui, t, crate::i18n::tr("Keywords of the selection"), |ui| {
        use crate::state::KeywordChips as K;
        choices(ui, "settingsKeywordChips", &[(K::Paths, "Whole names"), (K::Grouped, "Grouped by parent")], &mut app.ui.settings.keyword_chips);
    });
    hint(
        ui,
        t,
        crate::i18n::tr("Grouped: keywords inside the same keyword share a line naming it, and long lists and the suggestions are shortened."),
    );
    heading(ui, t, crate::i18n::tr("Detail"));
    check(ui, "settings.navigator", &mut app.ui.navigator, "Show the Navigator while zoomed in");
    row(ui, t, crate::i18n::tr("Info overlay"), |ui| {
        use crate::state::InfoOverlay as I;
        choices(ui, "settingsInfo", &[(I::Off, "Off"), (I::Basic, "File & date"), (I::Exposure, "Exposure")], &mut app.ui.info_overlay);
    });
}

// ----------------------------------------------------------------------------------- Slideshow

/// A playback duration slider (`slider:{id}`). While it is dragged the value changes in memory
/// (the slideshow reads it); letting go saves it through `slideshow.playback`.
fn duration_slider(app: &mut LightcraftApp, ui: &mut egui::Ui, id: &str, key: &str, value: f64, range: std::ops::RangeInclusive<f64>) {
    let mut v = value;
    let r = ui.add(egui::Slider::new(&mut v, range).logarithmic(key == "slideDuration").suffix(" s").max_decimals(1));
    register(ui.ctx(), format!("slider:{id}"), r.rect);
    if r.changed()
        && let Ok(next) = app.session.slideshow.merged(&json!({ key: v }))
    {
        app.session.slideshow = next;
    }
    if r.drag_stopped() || (r.changed() && !r.dragged()) {
        let _ = app.run("slideshow.playback", json!({ key: v }));
    }
}

/// The Slideshow tab: the slide preview, then the settings, in one column the width of the page
/// (the page scrolls; a column beside the preview would be wider than the window).
fn slideshow_tab(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    super::slide_layout::preview(app, ui, t);
    ui.add_space(6.0);
    super::slide_templates::section(app, ui, t);
    ui.add_space(6.0);
    playback_section(app, ui, t);
    ui.add_space(6.0);
    super::slide_layout::sections(app, ui, t);
    ui.add_space(6.0);
    super::slide_overlays::sections(app, ui, t);
    ui.add_space(6.0);
    super::slide_titles::sections(app, ui, t);
    ui.add_space(6.0);
    export_section(app, ui, t);
}

/// Export: PDF, JPEG sequence or video of the slideshow (View ▸ Export Slideshow…).
fn export_section(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    heading(ui, t, crate::i18n::tr("Export"));
    let running = app.session.slideshow_export.as_ref().is_some_and(|r| r.running());
    ui.horizontal_wrapped(|ui| {
        for (id, label, format) in [
            ("slideshowExportPdf", "Export PDF…", "pdf"),
            ("slideshowExportJpeg", "Export JPEG…", "jpeg"),
            ("slideshowExportVideo", "Export Video…", "video"),
        ] {
            let r = ui.add_enabled(!running, egui::Button::new(crate::i18n::tr(label)));
            register(ui.ctx(), format!("button:{id}"), r.rect);
            if r.clicked() {
                let _ = app.run("dialog.slideshowExport", json!({"format": format}));
            }
        }
    });
    hint(ui, t, crate::i18n::tr("The photos in view as slides, with the settings above. Progress shows in the activity stack."));
}

fn playback_section(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    use lightcraft_engine::slideshow::{FADE_MAX, SLIDE_MAX, SLIDE_MIN};
    heading(ui, t, crate::i18n::tr("Playback"));
    let s = app.session.slideshow.clone();
    row(ui, t, crate::i18n::tr("Slide duration"), |ui| {
        duration_slider(app, ui, "slideshowSlide", "slideDuration", s.slide_duration, SLIDE_MIN..=SLIDE_MAX)
    });
    row(ui, t, crate::i18n::tr("Fade duration"), |ui| {
        duration_slider(app, ui, "slideshowFade", "fadeDuration", s.fade_duration, 0.0..=FADE_MAX.min(s.slide_duration))
    });
    hint(ui, t, crate::i18n::tr("A slide stays up for its duration, its fade in included. A fade of 0 cuts."));
    row(ui, t, crate::i18n::tr("Fade"), |ui| {
        #[derive(Clone, Copy, PartialEq)]
        enum Fade {
            Cross,
            Black,
            White,
            Color,
        }
        let mut f = match s.fade_color {
            None => Fade::Cross,
            Some([0, 0, 0]) => Fade::Black,
            Some([255, 255, 255]) => Fade::White,
            Some(_) => Fade::Color,
        };
        let opts = [(Fade::Cross, "Crossfade"), (Fade::Black, "Through black"), (Fade::White, "Through white"), (Fade::Color, "Through a colour")];
        if choices(ui, "settingsSlideshowFade", &opts, &mut f) {
            let c = match f {
                Fade::Cross => Value::Null,
                Fade::Black => json!("#000000"),
                Fade::White => json!("#ffffff"),
                Fade::Color => json!([64, 64, 64]),
            };
            let _ = app.run("slideshow.playback", json!({"fadeColor": c}));
        }
        if f == Fade::Color
            && let Some(mut c) = s.fade_color
        {
            let r = egui::color_picker::color_edit_button_srgb(ui, &mut c);
            register(ui.ctx(), "button:slideshowFadeColor", r.rect);
            if r.changed() {
                let _ = app.run("slideshow.playback", json!({"fadeColor": c}));
            }
        }
    });
    let mut v = s.random_order;
    if check(ui, "settings.slideshowRandom", &mut v, "Random order") {
        let _ = app.run("slideshow.playback", json!({"randomOrder": v}));
    }
    let mut v = s.repeat;
    if check(ui, "settings.slideshowRepeat", &mut v, "Repeat (off: the slideshow ends after the last photo)") {
        let _ = app.run("slideshow.playback", json!({"repeat": v}));
    }
    let mut v = s.prepare_previews;
    if check(ui, "settings.slideshowPrepare", &mut v, "Prepare previews in advance") {
        let _ = app.run("slideshow.playback", json!({"preparePreviews": v}));
    }
    hint(
        ui,
        t,
        crate::i18n::tr(
            "Renders the next slides while the current one is up, so fades never wait. Off: each slide renders when it is due (less memory and work).",
        ),
    );
    hint(
        ui,
        t,
        crate::i18n::tr("Slideshow in the View menu plays the photos in view from the selected one. These settings are saved with the library."),
    );
}

/// Appearance Mode (`button:settingsAppearance-{0,1,2}`: Auto, Dark, Light) above the light and
/// dark theme cards (`radio:theme-{id}`), each with a preview of the chosen theme. The card in
/// use is outlined. Applied on the next frame ([`LightcraftApp::sync_theme`]).
fn appearance(ui: &mut egui::Ui, t: &Tokens, s: &mut crate::state::AppSettings, system: Option<egui::Theme>) {
    use crate::state::{AppearanceMode as M, DarkTheme, LightTheme};
    heading(ui, t, crate::i18n::tr("Appearance"));
    ui.label(RichText::new(crate::i18n::tr("Appearance Mode")).color(t.text_label));
    ui.horizontal_wrapped(|ui| {
        choices(ui, "settingsAppearance", &[(M::Auto, "Sync with System"), (M::Dark, "Dark Mode"), (M::Light, "Light Mode")], &mut s.appearance_mode);
    });
    ui.add_space(4.0);
    let light_active = s.theme(system) == s.light_theme.kind();
    let available = ui.available_width();
    let mut cards = |ui: &mut egui::Ui, width: f32| {
        ui.spacing_mut().item_spacing.x = 12.0;
        let light: Vec<_> = LightTheme::ALL.iter().map(|l| (*l, l.kind())).collect();
        theme_card(ui, t, "Light Theme", light_active, &light, &mut s.light_theme, &mut s.favorite_themes, width);
        let dark: Vec<_> = DarkTheme::ALL.iter().map(|d| (*d, d.kind())).collect();
        theme_card(ui, t, "Dark Theme", !light_active, &dark, &mut s.dark_theme, &mut s.favorite_themes, width);
    };
    if available < 440.0 {
        ui.vertical(|ui| cards(ui, available));
    } else {
        ui.horizontal_top(|ui| cards(ui, ((available - 12.0) / 2.0).floor()));
    }
    ui.add_space(2.0);
}

/// One family's card: its title (and "Active" when it is the one shown), a preview of the
/// selected theme and a radio button per theme.
fn theme_card<V: PartialEq + Copy>(
    ui: &mut egui::Ui,
    t: &Tokens,
    title: &str,
    active: bool,
    options: &[(V, crate::theme::ThemeKind)],
    value: &mut V,
    favorites: &mut Vec<String>,
    width: f32,
) {
    egui::Frame::new()
        .fill(t.inset)
        // (the card in use is outlined in the accent, legible as a line on the window)
        .stroke(egui::Stroke::new(1.0, if active { t.legible(t.accent, t.chrome, 3.0) } else { t.field_border }))
        .corner_radius(6)
        .inner_margin(8)
        .show(ui, |ui| {
            ui.vertical(|ui| {
                let inner = (width - 18.0).clamp(120.0, 240.0);
                ui.set_width(inner);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(crate::i18n::tr(title)).font(t.semibold(12.0)).color(t.text));
                    if active {
                        ui.label(RichText::new(crate::i18n::tr("In Use")).font(t.caption()).color(t.legible(t.accent, t.inset, 4.5)));
                    }
                });
                let shown = options.iter().find(|(v, _)| v == value).or(options.first()).map(|(_, k)| *k).unwrap_or_default();
                theme_preview(ui, shown, inner);
                for (v, kind) in options {
                    let row = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(ui.available_width(), 24.0));
                    let hovered = ui.ctx().pointer_hover_pos().is_some_and(|p| row.contains(p));
                    ui.horizontal(|ui| {
                        let r = ui.radio_value(value, *v, crate::i18n::tr(kind.label()));
                        register(ui.ctx(), format!("radio:theme-{}", kind.id()), r.rect);
                        // (ringed round the whole of it while it has the focus, as a checkbox is)
                        crate::widgets::focus_ring(ui, t, &r, r.rect.expand2(egui::vec2(3.0, 1.0)));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let favorite = favorites.iter().any(|id| id == kind.id());
                            // (always there, so that Tab reaches it; drawn while it is a
                            // favourite, under the mouse or has the focus)
                            let r = ui
                                .add(egui::Button::new("").min_size(egui::vec2(24.0, 24.0)).frame(false))
                                .on_hover_text(crate::i18n::tr(if favorite { "Remove from favorites" } else { "Add to favorites" }));
                            r.widget_info(|| {
                                egui::WidgetInfo::labeled(
                                    egui::WidgetType::Button,
                                    true,
                                    format!(
                                        "{}: {}",
                                        crate::i18n::tr(if favorite { "Remove from favorites" } else { "Add to favorites" }),
                                        crate::i18n::tr(kind.label())
                                    ),
                                )
                            });
                            register(ui.ctx(), format!("button:themeFavorite-{}", kind.id()), r.rect);
                            crate::widgets::focus_ring(ui, t, &r, r.rect);
                            if favorite || hovered || r.has_focus() {
                                crate::icons::paint(
                                    ui.painter(),
                                    r.rect.shrink(4.0),
                                    if favorite { crate::icons::Icon::StarFilled } else { crate::icons::Icon::Star },
                                    if favorite { t.accent } else { t.text_dim },
                                );
                            }
                            if r.clicked() {
                                if favorite {
                                    favorites.retain(|id| id != kind.id());
                                } else {
                                    favorites.push(kind.id().into());
                                }
                            }
                        });
                    });
                }
            });
        });
}

/// A small LightCraft window painted with `kind`'s tokens: the top bar with its search field, the
/// photo with the filmstrip under it, the Edit panel (histogram and sliders), the tool strip and
/// the bottom bar.
fn theme_preview(ui: &mut egui::Ui, kind: crate::theme::ThemeKind, width: f32) {
    use egui::{Color32, Rect, Stroke, StrokeKind, pos2, vec2};
    let p = Tokens::for_kind(kind);
    let (rect, _) = ui.allocate_exact_size(vec2(width, (width * 0.56).round().min(130.0)), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let bar = |y: f32, x: f32, w: f32, c: Color32| painter.rect_filled(Rect::from_min_size(pos2(x, y), vec2(w, 2.0)), 1.0, c);
    painter.rect_filled(rect, 3.0, p.canvas);
    // top bar: sidebar and back icons, the centred search field, icons on the right
    let top = Rect::from_min_size(rect.min, vec2(rect.width(), 13.0));
    painter.rect_filled(top, 0.0, p.chrome);
    for x in [6.0, 14.0, 20.0] {
        painter.rect_filled(Rect::from_min_size(top.min + vec2(x, 4.5), vec2(4.0, 4.0)), 1.0, p.icon);
    }
    let search = Rect::from_center_size(top.center(), vec2(rect.width() * 0.34, 8.0));
    painter.rect(search, 2.0, p.field, Stroke::new(1.0, p.field_border), StrokeKind::Inside);
    bar(search.center().y - 1.0, search.center().x - 9.0, 18.0, p.text_dim);
    for i in 0..4 {
        painter.rect_filled(Rect::from_min_size(pos2(top.right() - 10.0 - i as f32 * 9.0, top.top() + 4.5), vec2(4.0, 4.0)), 2.0, p.icon);
    }
    // bottom bar: view buttons, rating stars, a button
    let bottom = Rect::from_min_max(pos2(rect.left(), rect.bottom() - 12.0), rect.max);
    painter.rect_filled(bottom, 0.0, p.chrome);
    for x in [6.0, 13.0, 20.0] {
        painter.rect_filled(Rect::from_min_size(bottom.min + vec2(x, 4.0), vec2(4.0, 4.0)), 1.0, p.icon);
    }
    let stars_x = bottom.center().x - 30.0;
    for i in 0..5 {
        painter.circle_filled(pos2(stars_x + i as f32 * 5.0, bottom.center().y), 1.4, p.star);
    }
    let copy = Rect::from_min_size(pos2(stars_x + 28.0, bottom.top() + 2.5), vec2(30.0, 7.0));
    painter.rect(copy, 3.0, p.button, Stroke::new(1.0, p.button_border), StrokeKind::Inside);
    // tool strip on the right edge, the first tool (Edit) active
    let strip = Rect::from_min_max(pos2(rect.right() - 12.0, top.bottom()), pos2(rect.right(), bottom.top()));
    painter.rect_filled(strip, 0.0, p.chrome);
    painter.line_segment([strip.left_top(), strip.left_bottom()], Stroke::new(1.0, p.divider));
    for i in 0..5 {
        let r = Rect::from_min_size(pos2(strip.left() + 3.0, strip.top() + 4.0 + i as f32 * 9.0), vec2(6.0, 6.0));
        if i == 0 {
            painter.rect_filled(r.expand(1.5), 1.5, p.tool_active);
        }
        painter.rect_stroke(r, 1.0, Stroke::new(1.0, if i == 0 { p.text } else { p.icon }), StrokeKind::Inside);
    }
    // Edit panel: histogram, then labelled sliders
    let panel = Rect::from_min_max(pos2(strip.left() - (rect.width() * 0.27).round(), top.bottom()), pos2(strip.left(), bottom.top()));
    painter.rect_filled(panel, 0.0, p.chrome);
    painter.line_segment([panel.left_top(), panel.left_bottom()], Stroke::new(1.0, p.divider));
    let hist = Rect::from_min_size(panel.min + vec2(4.0, 4.0), vec2(panel.width() - 8.0, 14.0));
    painter.rect_filled(hist, 1.0, p.inset);
    let curve = [0.2, 0.55, 0.9, 0.7, 0.5, 0.62, 0.4, 0.3, 0.18, 0.1];
    let mut pts = vec![hist.left_bottom()];
    for (i, h) in curve.iter().enumerate() {
        pts.push(pos2(hist.left() + hist.width() * i as f32 / (curve.len() - 1) as f32, hist.bottom() - hist.height() * h));
    }
    pts.push(hist.right_bottom());
    // the area under the curve, a column per stretch between two points
    for pair in pts.windows(2) {
        if let [a, b] = pair {
            let r = Rect::from_min_max(pos2(a.x, a.y.max(b.y)), pos2(b.x, hist.bottom()));
            painter.rect_filled(r, 0.0, p.track.gamma_multiply(0.5));
        }
    }
    painter.add(egui::Shape::line(pts, Stroke::new(1.0, p.text_dim)));
    let sliders = [0.55, 0.62, 0.35, 0.7, 0.6, 0.45];
    let mut y = hist.bottom() + 7.0;
    bar(y, panel.left() + 5.0, 14.0, p.text);
    y += 6.0;
    for v in sliders {
        if y + 7.0 > panel.bottom() {
            break;
        }
        bar(y, panel.left() + 5.0, 12.0, p.text_label);
        bar(y, panel.right() - 10.0, 5.0, p.text_dim);
        let track_y = y + 5.0;
        painter.line_segment([pos2(panel.left() + 5.0, track_y), pos2(panel.right() - 5.0, track_y)], Stroke::new(1.0, p.track));
        painter.circle_filled(pos2(panel.left() + 5.0 + (panel.width() - 10.0) * v, track_y), 1.8, p.thumb);
        y += 10.0;
    }
    // the photo (its own colours: a photo looks the same in every theme) above the filmstrip
    let film = Rect::from_min_max(pos2(rect.left(), bottom.top() - 18.0), pos2(panel.left(), bottom.top()));
    let stage = Rect::from_min_max(pos2(rect.left(), top.bottom()), pos2(panel.left(), film.top()));
    let photo = Rect::from_center_size(stage.center(), vec2(stage.width() * 0.62, stage.height() - 8.0));
    painter.rect_filled(photo, 0.0, Color32::from_rgb(0x6f, 0x8f, 0xb4));
    let horizon = photo.top() + photo.height() * 0.62;
    painter.add(egui::Shape::convex_polygon(
        vec![
            pos2(photo.left(), horizon),
            pos2(photo.left() + photo.width() * 0.3, photo.top() + photo.height() * 0.3),
            pos2(photo.left() + photo.width() * 0.55, horizon),
        ],
        Color32::from_rgb(0x4a, 0x55, 0x66),
        Stroke::NONE,
    ));
    painter.add(egui::Shape::convex_polygon(
        vec![
            pos2(photo.left() + photo.width() * 0.35, horizon),
            pos2(photo.left() + photo.width() * 0.68, photo.top() + photo.height() * 0.2),
            pos2(photo.right(), horizon),
        ],
        Color32::from_rgb(0x5a, 0x63, 0x72),
        Stroke::NONE,
    ));
    painter.rect_filled(Rect::from_min_max(pos2(photo.left(), horizon), photo.max), 0.0, Color32::from_rgb(0x3d, 0x5a, 0x3a));
    painter.line_segment([film.left_top(), film.right_top()], Stroke::new(1.0, p.divider));
    for i in 0..5 {
        let cell = Rect::from_min_size(pos2(film.left() + 4.0 + i as f32 * 17.0, film.top() + 3.0), vec2(15.0, 12.0));
        if cell.right() > film.right() - 2.0 {
            break;
        }
        let selected = i == 1;
        painter.rect_filled(cell, 0.0, if selected { p.cell_selected } else { p.cell });
        let thumb = cell.shrink(2.5);
        painter.rect_filled(thumb, 0.0, Color32::from_rgb(0x6f + 8 * i as u8, 0x80, 0x8f));
        if selected {
            painter.rect_stroke(thumb, 0.0, Stroke::new(1.0, p.pick), StrokeKind::Outside);
        }
    }
}

// ------------------------------------------------------------------------------- Open Library…

/// Switching libraries is refused while an import (or the folder scan before one) is running: its
/// remaining batches would be committed into the new library (issue #1509).
pub(crate) fn can_switch_library(app: &LightcraftApp) -> Result<(), String> {
    if crate::lightroom_import::is_running(app) {
        return Err("wait for Lightroom catalog import to finish before switching libraries".into());
    }
    if app.import.is_some() || app.scan.is_some() {
        return Err("wait for the import to finish before switching libraries".into());
    }
    Ok(())
}

/// `app.openLibrary {path?}`: close the current library and open (or create) the one at `path`,
/// or a folder chosen in a dialog. Remembered as the library to open at launch.
///
/// A Lightroom Classic catalog (its folder, an `.lrdata` / `.lrcat-data` folder or the `.lrcat`
/// itself, see [`lightcraft_engine::lightroom_folder`]) is not opened: the current library stays
/// and a dialog offers Import Lightroom Catalog… instead; returns `{opened: false,
/// lightroomCatalog: <.lrcat path or null>, path}`.
pub fn open_library(app: &mut LightcraftApp, p: &Value) -> Result<Value, String> {
    can_switch_library(app)?;
    let path = match p.get("path").and_then(Value::as_str) {
        Some(x) => x.to_string(),
        None => {
            let req = crate::pick::PickRequest::folder(crate::i18n::tr("Open Library"));
            match crate::pick::ask(app, "app.openLibrary", p, "path", req, |s| s.pick_folder.as_mut().and_then(|f| f()).map(|x| vec![x])) {
                crate::pick::Picked::Now(v) => match v.into_iter().next() {
                    Some(x) => x,
                    None => return Ok(Value::Null),
                },
                crate::pick::Picked::Later => return Ok(Value::Null),
                crate::pick::Picked::Unavailable => return Err("no folder dialog on this platform".into()),
            }
        }
    };
    // A Lightroom catalog folder (or part of one) is not a library: before closing the library
    // open now, offer to import it instead (issue #891). The engine refuses it too.
    if let Some(place) = lightcraft_engine::lightroom_folder::lightroom_catalog_at(std::path::Path::new(&path)) {
        let catalog = place.catalog.map(|c| c.to_string_lossy().into_owned());
        let err = lightcraft_engine::EngineError::LightroomCatalog { path: path.clone().into(), catalog: catalog.clone().map(Into::into) };
        // the library problem window (no library could be opened) shows the error instead
        if app.library_problem.is_some() {
            return Err(err.to_string());
        }
        app.ui.dialog = Some(crate::state::Dialog::LightroomCatalog { folder: path.clone(), catalog: catalog.clone() });
        return Ok(json!({"opened": false, "lightroomCatalog": catalog, "path": path}));
    }
    // a remembered library that is gone: say so (and forget it) instead of creating an empty one there
    if app.ui.settings.recent_libraries.contains(&path) && !std::path::Path::new(&path).exists() {
        app.ui.settings.recent_libraries.retain(|r| r != &path);
        return Err(format!("{path}: the library is no longer there (moved, renamed or on a disconnected drive); removed from Open Recent Library"));
    }
    switch_library(app, &path)
}

/// Close the current library and open the one at `path`. If it can't be opened (locked by
/// another program, unreadable, a file), the error is returned and the current library stays open:
/// the target is checked before anything is closed, and if opening it still fails the previous
/// library is opened again (both errors are reported if that fails too).
fn switch_library(app: &mut LightcraftApp, path: &str) -> Result<Value, String> {
    let dir = app.session.check_library(std::path::Path::new(path)).map_err(|e| format!("{path}: {e}"))?;
    let path = dir.to_string_lossy().into_owned();
    // the library being left goes in the list too (also when it was opened at launch)
    let previous_dir = app.session.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.clone());
    let previous = previous_dir.as_ref().map(|d| d.to_string_lossy().into_owned());
    app.session.close_library().map_err(|e| e.to_string())?;
    let opened = app.session.open_library(&dir, false).map(|_| ()).map_err(|e| format!("{path}: {e}"));
    app.lightroom_last = None;
    app.lightroom_result = None;
    if matches!(app.ui.dialog.as_ref(), Some(crate::state::Dialog::LightroomResult { .. })) {
        app.ui.dialog = None;
    }
    app.renderer.forget_all();
    app.ui.compare = None;
    if let Err(err) = opened {
        let Some(prev) = previous_dir else { return Err(err) };
        return match app.session.open_library(&prev, false) {
            Ok(_) => Err(err),
            Err(e) => Err(format!("{err}; reopening the previous library {} failed too: {e}", prev.display())),
        };
    }
    app.ui.settings.library_path = path.clone();
    if let Some(prev) = previous.filter(|p| p != &path && !app.ui.settings.recent_libraries.contains(p)) {
        remember_library(&mut app.ui.settings.recent_libraries, &prev);
    }
    remember_library(&mut app.ui.settings.recent_libraries, &path);
    Ok(json!({"path": path, "name": library_name(&dir), "photos": app.session.catalog.len()}))
}

/// Put `path` first in the recent list, without duplicates, capped at [`RECENT_LIBRARIES`].
pub fn remember_library(recent: &mut Vec<String>, path: &str) {
    recent.retain(|r| r != path);
    recent.insert(0, path.to_string());
    recent.truncate(crate::state::RECENT_LIBRARIES);
}

/// A library's name as people see it: its folder's name.
pub fn library_name(dir: &std::path::Path) -> String {
    dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| dir.display().to_string())
}

/// The open library's name (`None`: in memory, nothing is saved).
pub fn open_library_name(app: &LightcraftApp) -> Option<String> {
    app.session.library.as_ref().filter(|l| l.on_disk).map(|l| library_name(&l.dir))
}

/// `app.newLibrary {path?, name?}`: create a library and switch to it. With `name`, the library
/// is the folder `name` inside `path` (or inside a location chosen in a dialog); without it,
/// `path` itself, or a new folder named after the default library. The folder must not hold a
/// library already, nor anything else.
pub fn new_library(app: &mut LightcraftApp, p: &Value) -> Result<Value, String> {
    can_switch_library(app)?;
    let name = match p.get("name") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(valid_library_name(s)?),
        Some(_) => return Err("app.newLibrary: name must be a string".into()),
    };
    // from the menu: ask for the name first (default prefilled), then for the location
    if name.is_none() && p.get("path").is_none() && app.services.pick_folder.is_some() {
        crate::panels::dialogs::prompt(
            app,
            "New Library",
            "Library name",
            lightcraft_engine::library::DEFAULT_NAME,
            "app.newLibrary",
            json!({}),
            "name",
        );
        return Ok(Value::Null);
    }
    let (location, picked) = match p.get("path").and_then(Value::as_str) {
        Some(x) => (std::path::PathBuf::from(x), false),
        None => {
            let req = crate::pick::PickRequest::folder(crate::i18n::tr("New Library: Choose Where to Put It"));
            match crate::pick::ask(app, "app.newLibrary", p, "path", req, |s| s.pick_folder.as_mut().and_then(|f| f()).map(|x| vec![x])) {
                crate::pick::Picked::Now(v) => match v.into_iter().next() {
                    Some(x) => (std::path::PathBuf::from(x), true),
                    None => return Ok(Value::Null),
                },
                crate::pick::Picked::Later => return Ok(Value::Null),
                crate::pick::Picked::Unavailable => return Err("no folder dialog on this platform".into()),
            }
        }
    };
    let dir = match name {
        Some(n) => location.join(n),
        // a chosen location gets a new folder in it, named after the default library
        None if picked => free_name(&location, lightcraft_engine::library::DEFAULT_NAME),
        None => location,
    };
    if lightcraft_engine::library::is_library(&dir) {
        return Err(format!("{}: there is a library here already; open it with File ▸ Open Library…", dir.display()));
    }
    if dir.is_file() {
        return Err(format!("{}: is a file; a library is a folder", dir.display()));
    }
    if std::fs::read_dir(&dir).is_ok_and(|mut d| d.next().is_some()) {
        return Err(format!("{}: the folder isn't empty; choose an empty folder or a new name", dir.display()));
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: can't create the library folder: {e}", dir.display()))?;
    switch_library(app, &dir.to_string_lossy())
}

/// Characters a folder name can't hold on Windows (and `/`, which no system allows).
const RESERVED_NAME_CHARS: [char; 9] = ['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

fn valid_library_name(s: &str) -> Result<&str, String> {
    let n = s.trim();
    // Windows drops trailing dots and spaces from a folder name: refuse them rather than
    // creating a folder whose name differs from the one asked for (spaces around the name
    // are trimmed first)
    let bad = n.is_empty() || n == "." || n == ".." || n.ends_with(['.', ' ']) || n.contains(RESERVED_NAME_CHARS) || n.chars().any(char::is_control);
    if bad {
        return Err(format!(
            "app.newLibrary: {s:?} can't be a library name (a folder name without < > : \" / \\ | ? * and not ending in a dot or space)"
        ));
    }
    Ok(n)
}

/// `base` inside `location`, or `base 2`, `base 3`… if that is taken.
fn free_name(location: &std::path::Path, base: &str) -> std::path::PathBuf {
    let first = location.join(base);
    if !first.exists() {
        return first;
    }
    (2..1000u32).map(|i| location.join(format!("{base} {i}"))).find(|p| !p.exists()).unwrap_or(first)
}

/// `app.recentLibraries {clear?: bool}`: the recently opened libraries, newest first, as
/// `[{path, name, exists, open}]` (`exists: false`: moved or on a disconnected drive). `clear`
/// forgets them all.
pub fn recent_libraries(app: &mut LightcraftApp, p: &Value) -> Result<Value, String> {
    if p.get("clear").and_then(Value::as_bool) == Some(true) {
        app.ui.settings.recent_libraries.clear();
    }
    let open = app.session.library.as_ref().filter(|l| l.on_disk).map(|l| l.dir.clone());
    let list: Vec<Value> = app
        .ui
        .settings
        .recent_libraries
        .iter()
        .map(|r| {
            let d = std::path::Path::new(r);
            json!({"path": r, "name": library_name(d), "exists": d.is_dir(), "open": open.as_deref() == Some(d)})
        })
        .collect();
    Ok(Value::Array(list))
}

// ---------------------------------------------------------------------------- Display profile

/// `app.displayProfile {path?: string | null}`: show previews through this monitor ICC profile
/// (`""` or `null`: none, the profile of the screen showing the window where the host knows it
/// (macOS), else the display is treated as sRGB) and keep it as the setting; no `path`: report
/// only. A file that can't be used is an error and changes nothing. Returns the profile in use:
/// `{path, description, kind (matrix|lut), primaries {red, green, blue: [x, y]}}`, with
/// `path: null, screen: true` for the screen's profile.
pub fn display_profile(app: &mut LightcraftApp, p: &Value) -> Result<Value, String> {
    let path = match p.get("path") {
        None => None,
        Some(Value::Null) => Some(String::new()),
        Some(Value::String(s)) => Some(s.trim().to_string()),
        Some(_) => return Err("app.displayProfile: path must be a string or null".into()),
    };
    if let Some(path) = path {
        let d = lightcraft_engine::display::Display::load_opt(&path).map_err(|e| format!("app.displayProfile: {e}"))?;
        app.renderer.set_display(d);
        app.ui.settings.display_profile = path.clone();
        app.display_applied = Some(path);
        app.display_error = None;
        app.screen_profile = None;
        app.screen_profile_read = None;
        if app.ui.settings.display_profile.is_empty() {
            app.apply_screen_profile();
        }
    }
    Ok(match app.renderer.display() {
        Some(d) if d.path == crate::SCREEN_PROFILE => {
            let mut v = d.describe();
            v["path"] = Value::Null;
            v["screen"] = json!(true);
            v
        }
        Some(d) => d.describe(),
        None => json!({"path": null, "description": "sRGB (no display profile)"}),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn app_with_import() -> LightcraftApp {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), crate::Services::default());
        app.import = Some(crate::import::ImportTask::new(vec!["a.jpg".into()], json!({"mode": "add"}), 0, false));
        app
    }

    #[test]
    fn open_library_is_refused_while_an_import_runs() {
        let mut app = app_with_import();
        let err = open_library(&mut app, &json!({"path": "other"})).unwrap_err();
        assert!(err.contains("import to finish"), "{err}");
        assert!(app.import.is_some());
    }

    #[test]
    fn new_library_is_refused_while_an_import_runs() {
        let mut app = app_with_import();
        let err = new_library(&mut app, &json!({"path": "other"})).unwrap_err();
        assert!(err.contains("import to finish"), "{err}");
        assert!(app.import.is_some());
    }

    /// The menu items that switch libraries are greyed out meanwhile, like during a Lightroom import.
    #[test]
    fn library_switching_menu_items_are_disabled_while_an_import_runs() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), crate::Services::default());
        app.services.pick_folder = Some(Box::new(|| None));
        assert!(crate::menus::ui_enabled(&app, "app.openLibrary"));
        assert!(crate::menus::ui_enabled(&app, "app.newLibrary"));
        app.import = Some(crate::import::ImportTask::new(vec!["a.jpg".into()], json!({"mode": "add"}), 0, false));
        assert!(!crate::menus::ui_enabled(&app, "app.openLibrary"));
        assert!(!crate::menus::ui_enabled(&app, "app.newLibrary"));
    }

    /// The folder scan before an import's review counts too: it ends in an import into the open library.
    #[test]
    fn open_library_is_refused_while_an_import_scan_runs() {
        let dir = std::env::temp_dir().join(format!("lc-ui-switchscan-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), crate::Services::default());
        crate::import::open(&mut app, vec![dir.to_string_lossy().into_owned()]).unwrap();
        assert!(app.scan.is_some());
        let err = open_library(&mut app, &json!({"path": "other"})).unwrap_err();
        assert!(err.contains("import to finish"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod path_tests {
    use super::elide_path;

    /// Width = characters, so a limit of 20 is "20 characters".
    fn fit(path: &str, max: usize) -> String {
        elide_path(path, max as f32, |s| s.chars().count() as f32)
    }

    #[test]
    fn a_path_that_fits_is_left_alone() {
        assert_eq!(fit("/Users/me/Pictures", 18), "/Users/me/Pictures");
        assert_eq!(fit("", 5), "");
    }

    #[test]
    fn leading_folders_go_first_and_the_end_stays() {
        assert_eq!(fit("/Users/me/Pictures/Library", 20), "…/Pictures/Library");
        assert_eq!(fit("/Users/me/Pictures/Library", 10), "…/Library");
    }

    /// Windows paths: `Path::display` gives backslashes there.
    #[test]
    fn a_path_with_backslashes_keeps_its_end_too() {
        assert_eq!(fit(r"C:\Users\me\Pictures\Library", 20), r"…\Pictures\Library");
        assert_eq!(fit(r"C:\Users\me/Pictures\Library", 10), r"…\Library");
    }

    #[test]
    fn a_last_folder_too_long_by_itself_loses_its_end() {
        assert_eq!(fit("/Users/me/a-very-long-folder-name", 8), "a-very-…");
        assert_eq!(fit("no-separator-at-all", 8), "no-sepa…");
        assert_eq!(fit("/Users/Фотографии-из-отпуска", 8), "Фотогра…");
    }

    #[test]
    fn a_separator_at_the_end_is_not_where_the_name_starts() {
        assert_eq!(fit("/Users/me/Pictures/Library/", 11), "…/Library/");
        assert_eq!(fit("/Users/me/a-very-long-folder-name/", 8), "a-very-…");
        assert_eq!(fit(r"C:\Users\me\Library\", 11), r"…\Library\");
        assert_eq!(fit("///", 2), "…");
        assert_eq!(fit(r"\\server\share\folder", 15), r"…\share\folder");
    }
}
