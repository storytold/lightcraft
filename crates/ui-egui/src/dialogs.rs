//! Modal dialogs (new album, rename, create preset, choose settings to copy, export, about, shortcuts).

use lightcraft_develop::{ControlSpec, Section, SettingsGroup, Track};
use serde_json::json;

use crate::LightcraftApp;
use crate::state::Dialog;
use crate::theme::Tokens;

mod confirm;
mod cull;
mod export;
mod frame;
mod keyword_set;
mod rename;
mod sam_model;
mod save_effect;

pub(crate) use confirm::album_entries;
pub use confirm::confirm_dialog;
pub use cull::{MAX_BURST_GAP, clamp_cull_burst, cull_params, fmt_gap, parse_gap};
use export::group_checklist;
pub(crate) use export::preset_lzw_note;
pub use frame::{ConfirmStyle, confirm_style, list_height, prompt};
use frame::{
    DIALOG_CHROME_GUESS, DIALOG_MIN_BODY, DIALOG_SCREEN_MARGIN, clicked_backdrop, dialog_field, escaped_field, field, fit_lists, just_opened, only_shows,
    return_confirms,
};
pub(crate) use rename::rename_preview;
use rename::{person_rename_plan, rename_body, rename_keyword_body, rename_person_body};
pub(crate) use sam_model::show_model_folder;
use sam_model::{sam_by_hand, sam_model_body};
use slideshow_export::slideshow_export_body;

pub mod credits;
pub mod faces;
pub mod import;
pub mod keymap;
pub mod lightroom_destination;
pub mod merge;
pub mod print_view;
pub mod rules_editor;
pub mod settings;
pub mod slideshow_export;
pub mod updates_ui;

/// About dialog tabs: (widget id suffix, label). The credits come from `crate::credits`.
pub const ABOUT_TABS: &[(&str, &str)] = &[("about", "About"), ("contributors", "Contributors"), ("models", "Models")];

/// Help ▸ What's New (docs/whats-new.md).
pub const WHATS_NEW: &str = include_str!("../../../docs/whats-new.md");

/// The tour's window title and the labels of its next and last-page buttons: translated in every
/// catalog, with the pages' text (the test that pins it reads these).
pub(crate) const WELCOME_TITLE: &str = "Welcome";
pub(crate) const WELCOME_NEXT: &str = "Next";
pub(crate) const WELCOME_START: &str = "Get Started";

/// The first-run tour's pages: (title, what it does, where to find it). Every menu path names a
/// real command (`file.addPhotos`, `view.photoGrid`, `view.detail`, `view.beforeAfter`,
/// `dialog.cull`, `dialog.export`, `dialog.print`); `›` paints (the menu triangle `▸` does not).
/// The pages that name a shortcut take the command's live binding at render time
/// ([`welcome_body`]) — no key is written here, so no platform is shown a chord its menus do
/// not bind.
pub(crate) const WELCOME_PAGES: [(&str, &str, &str); 6] = [
    (
        "Welcome to LightCraft",
        "A fast, non-destructive photo library and raw developer: your originals stay untouched, and every edit can be revisited or reset.",
        "Bring photos in with File › Import Photos…",
    ),
    (
        "Find and organize",
        "The Photo Grid lays the library out as justified rows: rate, flag and colour-label, filter, and gather photos into albums.",
        "View › Photo Grid.",
    ),
    (
        "Develop a photo",
        "Open a photo to develop it: exposure, colour, detail and masks, all non-destructive. Use View › Before/After Side by Side to compare before and after.",
        "View › Detail View.",
    ),
    (
        "Cull a shoot",
        "Assisted Culling scores focus and exposure, finds bursts, and flags blurry shots as rejects, so the keepers surface fast.",
        "Photo › Assisted Culling…",
    ),
    (
        "Share your work",
        "Export renders JPEG, PNG, TIFF, WebP and more, sized as you like; Print lays a photo out on a page as PDF.",
        "File › Export… · File › Print…",
    ),
    (
        "Put agents to work",
        "Everything in LightCraft is a command, so AI agents can drive the running app over MCP: culling, editing, exporting on your word.",
        "Start the app with --control 7980 (or LIGHTCRAFT_CONTROL_PORT), then lightcraft-cli mcp --connect attaches one to it.",
    ),
];

/// The tour page that names the desktop-only launch flow: an MCP agent attaches over a `--control`
/// port, which only the desktop app listens on. The browser build omits the page rather than
/// telling browser users to launch a desktop binary.
const WELCOME_DESKTOP_ONLY_PAGE: usize = 5;

/// The tour's pages for `web_host` (the browser build) against the desktop's: the browser has no
/// `--control` port for `lightcraft-cli` to attach to, so its tour omits [`WELCOME_DESKTOP_ONLY_PAGE`].
/// The parameter is the pure decision; the running host passes [`welcome_pages_host`]'s answer.
pub(crate) fn welcome_pages(web_host: bool) -> &'static [(&'static str, &'static str, &'static str)] {
    if web_host { &WELCOME_PAGES[..WELCOME_DESKTOP_ONLY_PAGE] } else { &WELCOME_PAGES }
}

/// The pages this build's host shows: the web build (wasm32) hides the desktop-only page.
fn welcome_pages_host() -> &'static [(&'static str, &'static str, &'static str)] {
    welcome_pages(cfg!(target_arch = "wasm32"))
}

/// The command whose shortcut a tour page's opening line names.
fn welcome_page_command(page: usize) -> Option<&'static str> {
    match page {
        0 => Some("file.addPhotos"),
        2 => Some("view.detail"),
        4 => Some("dialog.print"),
        _ => None,
    }
}

/// The tour page that names Print. The browser build's File menu disables Print
/// ([`crate::menus::ui_enabled`]), so that host's tour names Export alone in the opening line
/// ([`welcome_opening`]) instead of a menu item the host greys out.
const WELCOME_PRINT_PAGE: usize = 4;

/// The opening line a tour page paints under its text: the pages that name a shortcut carry the
/// command's live binding (`keys`), the Share page names Export alone on the web host, and the
/// rest paint the page's own sentence.
pub(crate) fn welcome_opening(page: usize, opening: &str, keys: Option<&str>, web_host: bool) -> String {
    match (page, keys, web_host) {
        (WELCOME_PRINT_PAGE, _, true) => crate::i18n::tr("File › Export…").to_string(),
        (0, Some(keys), _) => crate::i18n::tr_format!("Bring photos in with File › Import Photos… ({keys}).", keys = keys),
        (2, Some(keys), _) => crate::i18n::tr_format!("View › Detail View ({keys}).", keys = keys),
        (WELCOME_PRINT_PAGE, Some(keys), _) => crate::i18n::tr_format!("File › Export… · File › Print… ({keys}).", keys = keys),
        _ => crate::i18n::tr(opening).to_string(),
    }
}

/// The first-run tour: one page at a time, Back / Skip / Next under it. `close` is set when the
/// tour is finished (Next on the last page) or skipped.
fn welcome_body(app: &LightcraftApp, ui: &mut egui::Ui, t: &Tokens, page: &mut usize, close: &mut bool) {
    let pages = welcome_pages_host();
    let last = pages.len() - 1;
    *page = (*page).min(last);
    let (title, text, opening) = pages[*page];
    ui.add_space(2.0);
    ui.label(egui::RichText::new(crate::i18n::tr(title)).font(t.semibold(16.0)).color(t.text));
    ui.label(egui::RichText::new(crate::i18n::tr(text)).color(t.text_label));
    ui.add_space(2.0);
    // The pages that name a shortcut show the command's live binding, painted the way the menus
    // paint theirs (`menubar::shortcut_text`: `⌘⇧I` on macOS, `Ctrl+Shift+I` elsewhere) — never
    // a hard-coded key. A command whose binding was removed shows the menu path alone.
    let mac = ui.ctx().os() == egui::os::OperatingSystem::Mac;
    let keys = welcome_page_command(*page)
        .and_then(|id| crate::shortcuts::shortcut_of(&app.ui.settings.keymap, id))
        .map(|sc| crate::menus::menubar::shortcut_text(sc, mac));
    let opening = welcome_opening(*page, opening, keys.as_deref(), cfg!(target_arch = "wasm32"));
    ui.label(egui::RichText::new(opening).color(t.text_dim));
    ui.add_space(8.0);
    // progress dots, centred: the active page's is the accent
    let spacing = 12.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 8.0), egui::Sense::hover());
    let x0 = rect.center().x - spacing * (last as f32) / 2.0;
    for i in 0..pages.len() {
        let c = egui::pos2(x0 + i as f32 * spacing, rect.center().y);
        ui.painter().circle_filled(c, if i == *page { 2.5 } else { 1.5 }, if i == *page { t.accent } else { t.text_dim });
    }
    ui.add_space(8.0);
    use crate::button::{Button, Kind};
    let back = Button::new(crate::i18n::tr("Back")).widget("button:welcomeBack").enabled(*page > 0).min_width(72.0);
    let skip = Button::new(crate::i18n::tr("Skip")).kind(Kind::Ghost).widget("button:welcomeSkip").min_width(72.0);
    let next_label = if *page == last { crate::i18n::tr(WELCOME_START) } else { crate::i18n::tr(WELCOME_NEXT) };
    let next = Button::new(next_label).kind(Kind::Primary).widget("button:welcomeNext").min_width(84.0);
    let gap = ui.spacing().item_spacing.x;
    let row_w = ui.available_width().max(ui.min_rect().width()).max(1.0);
    let (skip_w, next_w) = (skip.desired_size(ui).x, next.desired_size(ui).x);
    ui.allocate_ui_with_layout(egui::vec2(row_w, next.desired_size(ui).y), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        if back.show(ui).clicked() {
            *page -= 1;
        }
        ui.add_space((ui.available_width() - skip_w - next_w - 2.0 * gap).max(0.0));
        if skip.show(ui).clicked() {
            *close = true;
        }
        if next.show(ui).clicked() {
            if *page == last {
                *close = true;
            } else {
                *page += 1;
            }
        }
    });
}

/// The framework dialog of each kind of dialog: its id names its layer, its placement and its footer, so the
/// keyboard's place doesn't carry from a dialog to the one it opens. Each Keyword caller keeps its own, as before.
fn dialog_id(dialog: &Dialog) -> egui::Id {
    let keyword_caller = match dialog {
        Dialog::KeywordTag { editing, parent, .. } => Some((editing.clone(), parent.clone())),
        _ => None,
    };
    egui::Id::unique(("lightcraft.dialog", std::mem::discriminant(dialog), keyword_caller))
}

/// The egui layer the framework shows the dialog `id` on: the overlay host shows a layer container's `Area` under
/// the runtime sub-id "area" of the container's id, in the foreground tier of a modal. The prompt and library
/// windows name it before they are drawn, for the control channel's barrier.
pub(crate) fn framework_dialog_layer(id: egui::Id) -> egui::LayerId {
    egui::LayerId::new(egui::Order::Foreground, craft::ui::runtime::memory::sub(id, "area"))
}

/// Whether a popup (a menu, a dropdown, a list) is open above the open dialog: the layer that takes the keys is not
/// the dialog's own modal layer, as of the last pass. Tests read it where they read "no overlay is open" before the
/// dialog was one.
pub(crate) fn popup_open(ctx: &egui::Context) -> bool {
    use craft::ui::runtime::overlay;
    overlay::top(ctx).and_then(|id| overlay::entry(ctx, id)).is_some_and(|entry| !entry.rank.is_modal())
}

/// A parent `Ui` for a framework dialog, which mounts its own layer: a plain `Ui` over the window on the background
/// layer. Unlike an `egui::Area`, it never has a first-frame sizing pass, so a dialog is real from the frame it
/// opens in.
pub(crate) fn host_ui<R>(ctx: &egui::Context, id: egui::Id, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let mut ui = egui::Ui::new(ctx.clone(), id, egui::UiBuilder::new().layer_id(egui::LayerId::background()).max_rect(ctx.content_rect()));
    add(&mut ui)
}

/// Block the underlying panels from the first frame, before the dialog's modal layer is recorded. Close background
/// menus on entry only; popups subsequently opened inside the dialog stay usable.
pub(crate) fn prepare(app: &LightcraftApp, ui: &mut egui::Ui) {
    let Some(dialog) = &app.ui.dialog else { return };
    ui.disable();
    // The framework owns each dialog's layer. Comparing it to egui's modal layer would close the dialog's own menus
    // on every pass, so the entry is noted by pass instead.
    let key = dialog_id(dialog).with("prepared");
    let pass = ui.ctx().cumulative_pass_nr();
    let previous = ui.data_mut(|data| data.get_temp::<u64>(key));
    if previous.is_none_or(|previous| previous != pass && previous.checked_add(1) != Some(pass)) {
        egui::Popup::close_all(ui.ctx());
        // the framework's menus, dropdowns and lists too: the dialog's own layer is not a popup and stays
        craft::ui::runtime::overlay::close_popups(ui.ctx());
    }
    ui.data_mut(|data| data.insert_temp(key, pass));
}

/// The width a dialog first takes (its content may be wider).
fn dialog_width(dlg: &Dialog) -> f32 {
    match dlg {
        Dialog::Import { .. } => crate::import::DIALOG_SIZE[0],
        Dialog::SmartRules { .. } => 680.0,
        Dialog::AllMetadata { .. } => 620.0,
        Dialog::Print => 1100.0,
        Dialog::FaceModel { .. } => 460.0,
        Dialog::LightroomCatalog { .. } => 440.0,
        Dialog::LightroomDestination { .. } => 480.0,
        Dialog::Welcome { .. } => 460.0,
        Dialog::LensDatabase { .. } => 440.0,
        Dialog::ManageEffects { .. } => 560.0,
        // its format row on one line (#780): 615 wide with the dialog's padding, as before
        Dialog::Export { .. } => 599.2,
        _ => 380.0,
    }
}

/// Draw the open dialog. `blocked` is true when a quit prompt or settings notice was on screen at
/// the start of this frame: the dialog is still drawn (its state is preserved), but it takes no input
/// and none of its closes act. The caller freezes `blocked` for the whole frame so the click or key
/// that dismisses the prompt cannot also reach the dialog drawn after it. The blocking
/// library-problem/launch-list window gets the same treatment while it is actually on screen over
/// this dialog.
pub fn show(app: &mut LightcraftApp, ctx: &egui::Context, blocked: bool) {
    // When the pointer was last released: a dialog that a click opened waits out a double click before its title-bar
    // handle (`title_bar_handle`) takes presses.
    if ctx.input(|i| i.pointer.any_released()) {
        let now = ctx.input(|i| i.time);
        ctx.data_mut(|d| d.insert_temp(egui::Id::unique("lightcraft.dialog.last-release"), now));
    }
    let Some(dlg) = &app.ui.dialog else {
        // the next dialog shown is a new one, whichever it is
        ctx.data_mut(|d| d.remove::<egui::Id>(egui::Id::unique("lightcraft.dialog.shown-id")));
        return;
    };
    if save_effect::active(app, ctx) {
        return save_effect::show(app, ctx, blocked);
    }
    // Only show_root has the enabled root Ui required by the SDK About dialog.
    if matches!(dlg, Dialog::About) {
        return;
    }
    if matches!(dlg, Dialog::KeywordSet { .. }) {
        let raw_blocked = blocked || crate::panels::library_problem::blocks_dialog_input(app);
        keyword_set::show(app, ctx, raw_blocked);
        return;
    }
    // The framework dialog mounts its own modal layer; this area only gives it a parent `Ui` (the root `Ui` is
    // disabled under a dialog by `prepare`).
    host_ui(ctx, egui::Id::unique("lightcraft.dialog.host"), |ui| {
        craft::ui::runtime::overlay::stack_place(ctx, STACK_DIALOG, egui::Id::unique("lightcraft.dialog"), || shell(app, ui, blocked));
    });
}

/// The places on the window's dialog stack, bottom first: the framework stacks the modal layers shown at them in this
/// order, whichever appeared or was pressed last. The app's dialog, then the blocking library window, then a quit
/// prompt or settings notice.
pub(crate) const STACK_DIALOG: usize = 0;
pub(crate) const STACK_LIBRARY: usize = 1;
pub(crate) const STACK_PROMPT: usize = 2;

/// A framework button in a dialog body, registered as `name` with its real response. True when it was clicked in a
/// real run.
fn dialog_button(ui: &mut egui::Ui, name: &str, label: &str) -> bool {
    use craft::ui::buttons::Button;
    use craft::ui::contract::{Props as _, Show as _};
    let shown = Button::new(crate::i18n::tr(label).to_owned()).id_salt(("lc-dialog-button", name)).show(ui);
    if ui.is_sizing_pass() || craft::ui::runtime::measure::active(ui.ctx()) {
        return false;
    }
    crate::widgets::register_response(name, shown.response.rect, &shown.response);
    ui.is_enabled() && shown.clicked()
}

/// A framework checkbox in a dialog body, registered as `name`; a measuring run edits nothing.
fn dialog_check(ui: &mut egui::Ui, name: &str, label: &str, value: &mut bool) {
    use craft::ui::choice::Checkbox;
    use craft::ui::contract::{Bind, Props as _, Show as _};
    let mut draft = *value;
    let shown = Checkbox::new(Bind::bound(&mut draft)).id_salt(("lc-dialog-check", name)).label(crate::i18n::tr(label).to_owned()).show(ui);
    if ui.is_sizing_pass() || craft::ui::runtime::measure::active(ui.ctx()) {
        return;
    }
    crate::widgets::register_response(name, shown.response.interact_rect, &shown.response);
    if ui.is_enabled() {
        *value = draft;
    }
}

/// One button of a framework dialog footer drawn by the app: its shown label, its automation name, whether it can be
/// used and whether it has the accent fill.
pub(crate) struct FooterSpec {
    pub label: String,
    pub name: String,
    pub enabled: bool,
    pub primary: bool,
}

/// Draws `buttons` in a framework dialog's footer at their natural size (their labels are never cut short), in the
/// order given, each registered under its automation name with its real response. Returns the index of the one
/// clicked in a real run.
pub(crate) fn footer_buttons(ui: &mut egui::Ui, salt: impl egui::AsIdSalt, buttons: &[FooterSpec]) -> Option<usize> {
    footer_button_responses(ui, salt, buttons).0
}

/// [`footer_buttons`], with each button's response in a real run (none in a measuring run).
pub(crate) fn footer_button_responses(ui: &mut egui::Ui, salt: impl egui::AsIdSalt, buttons: &[FooterSpec]) -> (Option<usize>, Vec<egui::Response>) {
    use craft::theme::Variant;
    use craft::ui::buttons::Button;
    use craft::ui::contract::{Props as _, Show as _};
    let real = !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx());
    let salt = egui::Id::unique(salt);
    let mut clicked = None;
    let mut responses = Vec::new();
    for (at, button) in buttons.iter().enumerate() {
        let shown = Button::new(button.label.clone())
            .id_salt((salt, button.name.as_str()))
            .enabled(button.enabled)
            .variant(if button.primary { Variant::PRIMARY } else { Variant::SECONDARY })
            .show(ui);
        if real {
            crate::widgets::register_response(button.name.clone(), shown.response.rect, &shown.response);
            if button.enabled && ui.is_enabled() && (shown.clicked() || crate::widgets::same_pass_click(&shown.response)) {
                clicked = Some(at);
            }
            responses.push(shown.response.clone());
        }
    }
    (clicked, responses)
}

/// The footer of the generic dialog: Cancel and the confirm, the confirm on the right with Cancel to its left (macOS
/// and GNOME's order), filled with the accent when it is the dialog's primary action.
struct Footer {
    buttons: Vec<FooterSpec>,
    ok: Option<usize>,
    informational: bool,
    primary: bool,
}

impl Footer {
    /// Cancel (none in a dialog that only shows something) and the confirm (none when it has no label).
    fn of(app: &LightcraftApp, dlg: &Dialog) -> Footer {
        let informational = is_informational(dlg);
        let sam = SamState::of(app);
        let primary = !informational && confirm_style(dlg) == ConfirmStyle::Primary;
        let mut footer = Footer { buttons: Vec::new(), ok: None, informational, primary };
        if !informational {
            let label = crate::i18n::tr(cancel_label(dlg, &sam)).to_string();
            footer.buttons.push(FooterSpec { label, name: "button:dialogCancel".into(), enabled: true, primary: false });
        }
        let ok = ok_label(app, dlg, informational, &sam);
        if !ok.is_empty() {
            let enabled = can_confirm(app, dlg, informational);
            footer.ok = Some(footer.buttons.len());
            footer.buttons.push(FooterSpec {
                label: crate::i18n::tr(&ok).to_string(),
                name: "button:dialogOk".into(),
                enabled,
                primary: primary && enabled,
            });
        }
        footer
    }
}

/// The generic dialog in one framework `Dialog`: the body is `dialog_content`, the footer the Cancel/confirm pair,
/// Escape and Enter are the framework's, and the open state is `app.ui.dialog`.
/// The framework dialog's padding on both sides of its body, together.
const DIALOG_PADDING: f32 = 16.0;

fn shell(app: &mut LightcraftApp, host: &mut egui::Ui, blocked: bool) {
    use std::cell::Cell;

    use craft::ui::contract::{Bind, Props as _, ShowContainer as _};
    use craft::ui::dialog::Dialog as FrameworkDialog;
    use craft::ui::runtime::overlay::CloseReason;

    let ctx = host.ctx().clone();
    // A caller's measuring run of the whole dialog shows nothing the user can reach, and the dialog's id may open only
    // one stage a pass: the real run of the same pass shows it.
    // A press made in such a run is not the dialog's: until it is released, the dialog takes no input, so the release
    // of that press cannot click what lies under it.
    let speculative_id = egui::Id::unique("lightcraft.dialog.speculative-press");
    if craft::ui::runtime::measure::active(&ctx) {
        if ctx.input(|i| i.pointer.any_pressed()) {
            ctx.data_mut(|d| d.insert_temp(speculative_id, true));
        }
        return;
    }
    let speculative = ctx.data(|d| d.get_temp::<bool>(speculative_id)).unwrap_or(false);
    let released = ctx.input(|i| i.pointer.any_released());
    let held = speculative && (released || ctx.input(|i| i.pointer.any_down()));
    if speculative && (released || !held) {
        ctx.data_mut(|d| d.remove_temp::<bool>(speculative_id));
    }
    let Some(mut dlg) = app.ui.dialog.clone() else { return };
    let at_start = dlg.clone();
    let mut raw_blocked = blocked || held || crate::panels::library_problem::blocks_dialog_input(app);
    let real = !host.is_sizing_pass() && !craft::ui::runtime::measure::active(&ctx);
    let t = Tokens::get(&ctx);
    let screen = ctx.content_rect();
    let title_bar = title_bar_band(app);
    let title = dialog_title(app, &dlg);
    // The import review can be resized (its photo grid takes the room).
    let import = matches!(dlg, Dialog::Import { .. });
    // The Print dialog sizes itself to the window and scrolls its settings column on its own.
    let print = matches!(dlg, Dialog::Print);
    // Settings keeps its top edge, and the widest it has been, while tabs of different sizes are shown, so its tab
    // row stays where it is (centred like the others, the row jumped with each tab's height)
    let settings = matches!(dlg, Dialog::Settings { .. });
    let top = (screen.height() * 0.08).clamp(DIALOG_SCREEN_MARGIN, 96.0);
    // Every dialog fits the window (#781): the framework scrolls a body that doesn't. Settings is never taller than
    // `body_max`, so its tab bar stays put and its pages scroll by themselves.
    let id = dialog_id(&dlg);
    // The dialog's title bar, padding and footer as measured last time it was shown (never less than the guess).
    let chrome_id = id.with("chrome");
    let chrome = ctx.data(|d| d.get_temp::<f32>(chrome_id)).unwrap_or(DIALOG_CHROME_GUESS).max(DIALOG_CHROME_GUESS);
    let body_max =
        if settings { screen.height() - top - DIALOG_SCREEN_MARGIN - chrome } else { screen.height() - 2.0 * DIALOG_SCREEN_MARGIN - chrome }.max(DIALOG_MIN_BODY);
    // A press held from before this dialog was shown (on the dialog it replaced, or on the window under it) is not
    // this dialog's either: its release must not land on what this dialog put under the pointer.
    let shown_id = egui::Id::unique("lightcraft.dialog.shown-id");
    let replaced = ctx.data(|d| d.get_temp::<egui::Id>(shown_id)) != Some(id);
    ctx.data_mut(|d| d.insert_temp(shown_id, id));
    let now = ctx.input(|i| i.time);
    let delay = ctx.options(|o| o.input_options.max_double_click_delay);
    if replaced {
        // opened by a click (released just now): the first half of a possible double click
        let clicked = ctx.data(|d| d.get_temp::<f64>(egui::Id::unique("lightcraft.dialog.last-release"))).is_some_and(|at| now - at <= delay);
        ctx.data_mut(|d| d.insert_temp(shown_id.with("at"), if clicked { now } else { f64::NEG_INFINITY }));
    }
    // A click that opened the dialog may be the first of a double click: the bar's handle waits out that double click,
    // so its second half is not taken as a double click on the bar (which zooms the window).
    let settled = now - ctx.data(|d| d.get_temp::<f64>(shown_id.with("at"))).unwrap_or(f64::NEG_INFINITY) > delay;
    if replaced && ctx.input(|i| i.pointer.any_down() && !i.pointer.any_pressed()) {
        ctx.data_mut(|d| d.insert_temp(speculative_id, true));
        raw_blocked = true;
    }
    let widest_id = id.with("widest");
    let widest = if settings { ctx.data(|d| d.get_temp::<f32>(widest_id)).unwrap_or(0.0).min(screen.width() * 0.9) } else { 0.0 };
    // (the body's width: the dialog's padding comes on top of it)
    let room = screen.width() - 2.0 * DIALOG_SCREEN_MARGIN - DIALOG_PADDING;
    let width = dialog_width(&dlg).min(room).max(widest).max(1.0);
    // As the old window did, the dialog starts at its width and grows to what its content needs, never to the
    // window's: a field that fills the width it is given fills the dialog's.
    let grown_id = id.with("grown");
    let grown = ctx.data(|d| d.get_temp::<f32>(grown_id)).unwrap_or(0.0).min(room);
    let max_width = width.max(grown);
    // before the dialog: drawing it closes a dropdown the key or click lands outside of
    let popup_was_open = egui::Popup::is_any_open(&ctx) || popup_open(&ctx);
    let mut footer = Footer::of(app, &dlg);
    let (informational, ok_at) = (footer.informational, footer.ok);
    // The backdrop dims the window under the dialog. Like a macOS sheet, the dialog leaves the integrated title bar
    // lit: the window buttons the OS draws over it do not dim. It is an area below the dialog's layer.
    if real {
        egui::Area::new(egui::Id::unique("dialog-dim")).order(egui::Order::Middle).fixed_pos(screen.min).interactable(false).show(&ctx, |ui| {
            ui.painter().rect_filled(below_title_bar(screen, title_bar), 0.0, egui::Color32::from_black_alpha(140));
        });
    }
    // What the body leaves the confirm this pass: the footer's state follows the draft the body just wrote.
    let can_now: Cell<Option<bool>> = Cell::new(None);
    let shown_layer: Cell<Option<egui::LayerId>> = Cell::new(None);
    let picked: Cell<Option<usize>> = Cell::new(None);
    let mut open = true;
    let mut confirm = false;
    let mut close = false;
    let mut content_w = 0.0f32;
    let mut content_h = 0.0f32;
    let mut dialog = FrameworkDialog::new(crate::i18n::tr(&title).to_owned())
        .id(id)
        .opened(Bind::bound(&mut open))
        .enabled(!raw_blocked)
        .closable(false)
        // The dialog's own name field takes the focus when it opens (`dialog_field`); a dialog without one opens with
        // nothing focused, so the first Tab reaches its first control, as before.
        .initial_focus(craft::ui::runtime::focus::Initial::Keep);
    if !footer.buttons.is_empty() {
        let (buttons, can_now, picked) = (&mut footer.buttons, &can_now, &picked);
        let (ok, primary) = (footer.ok, footer.primary);
        let salt = id;
        dialog = dialog.footer(move |ui, _| {
            // the confirm follows the draft the body just wrote
            if let (Some(at), Some(can)) = (ok, can_now.get())
                && let Some(button) = buttons.get_mut(at)
            {
                button.enabled = can;
                button.primary = primary && can;
            }
            if let Some(at) = footer_buttons(ui, salt, &buttons[..]) {
                picked.set(Some(at));
            }
        });
    }
    // Every dialog may take the window less its margins (#781), as the old window did, rather than the framework's
    // share of it: Settings' pages and the Print layout are as wide as they were.
    let share = craft::ui::contract::overlay::ViewportShare::new(
        (1.0 - 2.0 * DIALOG_SCREEN_MARGIN / screen.width().max(1.0)).clamp(0.5, 1.0),
        (1.0 - 2.0 * DIALOG_SCREEN_MARGIN / screen.height().max(1.0)).clamp(0.5, 1.0),
    );
    dialog = dialog.overlay(|overlay| overlay.fit(craft::ui::contract::overlay::OverlayFit::ContentCapped(share)));
    // Return presses OK in a dialog without a text field of its own (Lightroom's default button); a dialog with a
    // field confirms from the field.
    dialog = dialog.close_on_enter(return_confirms(&dlg));
    if import {
        dialog = dialog.resizable(true).overlay(|overlay| overlay.size(egui::vec2(crate::import::DIALOG_SIZE[0], crate::import::DIALOG_SIZE[1])));
    }
    if settings {
        dialog = dialog.first_place(egui::pos2(screen.center().x, screen.top() + top), egui::Align2::CENTER_TOP);
    }
    // A covered dialog takes no keys: its focused field must not read the Escape or the typing meant for what covers it.
    let withheld: Vec<egui::Event> = if raw_blocked {
        ctx.input_mut(|input| {
            let (keys, rest) = std::mem::take(&mut input.events)
                .into_iter()
                .partition(|event| matches!(event, egui::Event::Key { .. } | egui::Event::Text(_) | egui::Event::Ime(_) | egui::Event::Paste(_)));
            input.events = rest;
            keys
        })
    } else {
        Vec::new()
    };
    let output = dialog
        .show(host, |ui, _| {
            let real = !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx());
            if real {
                shown_layer.set(Some(ui.layer_id()));
            }
            ui.set_min_width(width);
            ui.set_max_width(max_width);
            if import {
                // The review sizes its photo grid to the height it is given: give it the dialog's, not the endless
                // height of a scrolling body.
                ui.set_max_height((crate::import::DIALOG_SIZE[1] - DIALOG_CHROME_GUESS).min(body_max).max(DIALOG_MIN_BODY));
            }
            ui.spacing_mut().item_spacing.y = 8.0;
            // (the import review and Print size their own lists to the dialog)
            fit_lists(ui, !import && !print, body_max, |ui| dialog_content(app, ui, &t, body_max, &mut confirm, &mut close, &mut dlg));
            if import {
                crate::import::note_dialog_bottom(ui);
            }
            if real {
                content_w = ui.min_rect().width();
                content_h = ui.min_rect().height();
            }
            can_now.set(Some(can_confirm(app, &dlg, informational)));
        })
        .output;
    if !withheld.is_empty() {
        ctx.input_mut(|input| input.events.extend(withheld));
    }
    if !real {
        return;
    }
    // The window's hit belongs to the dialog's own layer, where its controls are.
    let layer = shown_layer.get().unwrap_or_else(|| framework_dialog_layer(id));
    // The dialog's own rect is its layer's: the area the overlay host shows it in.
    let window_rect = ctx.memory(|m| m.area_rect(layer.id)).unwrap_or(output.response.rect);
    let mut window = output.response.clone();
    window.layer_id = layer;
    window.rect = window_rect;
    window.interact_rect = window_rect;
    crate::widgets::register_response("dialog:window", window.rect, &window);
    if settings && content_w > widest + 0.5 {
        ctx.data_mut(|d| d.insert_temp(widest_id, content_w));
    }
    // The chrome round a body shown whole (not scrolled by the dialog): what the window takes beyond its body.
    let measured_chrome = window_rect.height() - content_h;
    if content_h > 0.0 && content_h <= body_max && measured_chrome > chrome + 0.5 && measured_chrome < screen.height() / 2.0 {
        ctx.data_mut(|d| d.insert_temp(chrome_id, measured_chrome));
        ctx.request_repaint();
    }
    // Grown once: content that fills whatever width it is given would otherwise widen the dialog every frame.
    if content_w > max_width + 0.5 && ctx.data(|d| d.get_temp::<f32>(grown_id)).is_none() {
        ctx.data_mut(|d| d.insert_temp(grown_id, content_w));
        ctx.request_repaint();
    }
    if !popup_was_open && settled && let Some(band) = title_bar {
        title_bar_handle(&ctx, band, window_rect);
    }
    // The startup dialog is recorded once it was presented, not when the logic tick picked it: a host that runs
    // `logic` and quits before a visible frame does not burn the tour, and a crash while it is up still does not
    // reopen it on every launch. A frame whose input a notice owns is not a presentation: the dialog is drawn but
    // cannot be used, so the recording waits for a pass where the dialog actually takes input.
    if !raw_blocked && matches!(at_start, Dialog::Welcome { .. } | Dialog::WhatsNew) {
        app.ui.record_startup_dialog_seen();
    }
    if raw_blocked {
        confirm = false;
        close = false;
        // A press made under the prompt or library window must not be remembered as a backdrop press for a release
        // that arrives after that covering window goes away.
        ctx.data_mut(|d| d.remove_temp::<bool>(egui::Id::unique("dialog-backdrop-press")));
    } else {
        // (the import review keeps its hand-picked photos however the click lands; the title bar is not the backdrop)
        if app.ui.settings.close_dialogs_outside && !import && clicked_backdrop(&ctx, window_rect, title_bar, popup_was_open) {
            close = true;
        }
        if escaped_field(&ctx) && !popup_was_open {
            close = true;
        }
        match output.closed {
            // Escape that closed a popup in the dialog (a dropdown, a date picker's calendar), or a key the shortcuts
            // editor took while recording, leaves the dialog open
            Some(CloseReason::Escape) if popup_was_open || crate::widgets::escape_taken(&ctx) => {}
            // Return with nothing focused in a dialog whose confirm can't take it
            Some(CloseReason::Enter) => confirm |= can_confirm(app, &dlg, informational),
            Some(_) => close = true,
            None => {}
        }
        match picked.get() {
            Some(at) if Some(at) == ok_at && !informational => confirm = true,
            // Cancel, or the Close of a dialog that only shows something
            Some(_) => close = true,
            None => {}
        }
    }
    if confirm {
        close |= confirmed(app, &ctx, &mut dlg);
    }
    // a dialog opened from inside this one (a button in Settings that shows a licence) takes its place
    if app.ui.dialog.as_ref().is_some_and(|now| *now != at_start) {
        return;
    }
    if !close && !blocked {
        let frame = ctx.cumulative_frame_nr();
        ctx.data_mut(|d| d.insert_temp(egui::Id::unique(BACKDROP_SHOWN), frame));
    }
    if close && matches!(dlg, Dialog::Import { .. }) {
        app.renderer.forget_imports();
    }
    app.ui.dialog = if close { None } else { Some(dlg) };
}

/// The band of the top bar that doubles as the window's title bar, as drawn last frame: `None`
/// without the integrated title bar (Windows, Linux, the web), in macOS full screen, and while the
/// bar isn't on screen (the full-screen photo view).
fn title_bar_band(app: &LightcraftApp) -> Option<egui::Rect> {
    // (the full-screen photo view draws no top bar, so the frame it is entered on still holds last frame's region)
    if !app.integrated_titlebar || app.window_is_fullscreen || app.ui.fullscreen {
        return None;
    }
    app.widgets.iter().find(|(id, _)| id == "region:titlebar").map(|(_, r)| *r)
}

/// `screen` less the title bar band across its top.
fn below_title_bar(screen: egui::Rect, band: Option<egui::Rect>) -> egui::Rect {
    band.map_or(screen, |b| egui::Rect::from_min_max(egui::pos2(screen.min.x, b.max.y.clamp(screen.min.y, screen.max.y)), screen.max))
}

/// The bar's controls are disabled under a dialog, and the modal layer takes the hits of the layers
/// below it: this keeps the bar a window handle (drag to move, double-click to zoom), above the
/// modal like a sheet's title bar. A tall dialog reaching into the band keeps its own clicks: the
/// handle is the band on either side of it, and isn't run while the pointer is on the dialog (each
/// handle records whether the window may move, and one the pointer isn't in would say yes).
fn title_bar_handle(ctx: &egui::Context, band: egui::Rect, dialog: egui::Rect) {
    // (egui lifts an area that wasn't there the frame before above the rest: a handle drawn again as the pointer leaves
    // the dialog would sit over an open dropdown and take the press meant for its item)
    if ctx.pointer_hover_pos().is_some_and(|p| dialog.contains(p) && band.contains(p)) {
        // The pointer is on the dialog where it reaches into the bar: a press there is the dialog's, so the window
        // may not move. (The bar's own empty area cannot tell: a framework modal is not a `Middle` panel over it.)
        let pass = ctx.cumulative_pass_nr();
        ctx.data_mut(|d| d.insert_temp(egui::Id::unique("title_bar_window_movable"), (pass, false)));
    }
    if egui::Popup::is_any_open(ctx) || popup_open(ctx) || ctx.pointer_hover_pos().is_some_and(|p| dialog.contains(p)) {
        return;
    }
    let sides =
        if band.intersects(dialog) { [(band.min.x, dialog.min.x), (dialog.max.x, band.max.x)] } else { [(band.min.x, band.max.x), (0.0, 0.0)] };
    for (i, (from, to)) in sides.into_iter().enumerate().filter(|(_, (from, to))| to - from > 1.0) {
        let part = egui::Rect::from_x_y_ranges(from..=to, band.y_range());
        // (above the order the framework's dialogs are in: it lists its layers above every other area of their order,
        // and egui gives the layers under its modal no hits)
        egui::Area::new(egui::Id::unique("dialog-title-bar").with(i)).order(egui::Order::Tooltip).fixed_pos(part.min).show(ctx, |ui| {
            craft_window::title_bar::empty_area(ui, part);
        });
    }
}

/// The title of a dialog.
fn dialog_title(app: &LightcraftApp, dlg: &Dialog) -> String {
    match dlg {
        Dialog::TextPrompt { title, .. } => title.as_str(),
        Dialog::NewAlbum { folder: true, .. } => "Create Folder",
        Dialog::NewAlbum { .. } => "Create Album",
        Dialog::RenameAlbum { .. } => "Rename Album",
        Dialog::Rename { .. } => "Rename Photos",
        Dialog::Import { .. } => "Import Photos",
        Dialog::LightroomResult { .. } => "Lightroom Catalog Import",
        Dialog::LabelNames { .. } => "Edit Color Label Names",
        Dialog::CaptureTime { .. } => "Edit Capture Time",
        Dialog::RenameKeyword { .. } => "Rename Keyword",
        Dialog::RenamePerson { .. } => "Rename Person",
        Dialog::KeywordTag { editing: None, .. } => "Create Keyword Tag",
        Dialog::KeywordTag { .. } => "Edit Keyword Tag",
        Dialog::DeleteKeyword { .. } => "Delete Keyword",
        Dialog::MoveKeyword { .. } => "Merge Keywords",
        Dialog::KeywordSet { .. } => "Edit Keyword Set",
        Dialog::MergeKeywords { .. } => "Merge Keywords",
        Dialog::NewSmartAlbum { .. } => "Create Smart Album",
        Dialog::AllMetadata { .. } => "All Metadata",
        Dialog::SystemInfo { .. } => "System Info",
        Dialog::DenoiseModel { .. } => "AI Denoise Model",
        Dialog::FaceModel { info, .. } if info["download"].is_string() => "Download Face Model",
        Dialog::FaceModel { .. } => "Add Face Model",
        Dialog::WhatsNew => "What's New",
        Dialog::Welcome { .. } => WELCOME_TITLE,
        Dialog::Cull { .. } => "Assisted Culling",
        Dialog::SmartRules { id: None, .. } => "New Smart Album",
        Dialog::SmartRules { .. } => "Edit Smart Album",
        Dialog::AutoStack { .. } => "Auto-Stack by Capture Time",
        Dialog::CreatePreset { .. } => "Create Preset",
        Dialog::CopySettings { .. } => "Choose Edit Settings to Copy",
        Dialog::PasteSettings { .. } => "Paste Selected Settings",
        Dialog::Export { .. } => "Export",
        Dialog::ContactSheet { .. } => "Contact Sheet PDF",
        Dialog::SlideshowExport { .. } => "Export Slideshow",
        Dialog::Print => "Print",
        Dialog::Merge { opts } => opts.title(),
        Dialog::Settings { .. } => "Settings",
        Dialog::ConfirmDelete { .. } => "Delete Photos",
        Dialog::MovePhotos { .. } => "Move Photos",
        Dialog::LightroomCatalog { .. } => "Lightroom Classic Catalog",
        Dialog::LightroomDestination { .. } => "Import Lightroom Catalog",
        Dialog::LensDatabase { .. } => "Lens Profiles",
        Dialog::SaveEffect { .. } => "New Effect Preset",
        Dialog::ManageEffects { .. } => "Manage Effect Presets",
        Dialog::DeleteEffects { names } if names.len() == 1 => "Delete Effect Preset",
        Dialog::DeleteEffects { .. } => "Delete Effect Presets",
        Dialog::RemoveFolder { disk: true, .. } => "Remove Disk from Library",
        Dialog::RemoveFolder { .. } => "Remove Folder from Library",
        Dialog::SynchronizeFolder { .. } => "Synchronize Folder",
        // (nothing to download from in this build: the dialog explains the manual install)
        Dialog::SamModel { .. } if sam_by_hand(&app.session.segmenter) => "Install the SAM 3 Model",
        Dialog::SamModel { .. } => "Download the SAM 3 Model?",
        Dialog::About => "About LightCraft",
        Dialog::Shortcuts => "Keyboard Shortcuts",
        Dialog::ViewOptions => "View Options",
    }
    .to_string()
}

/// Whether a dialog only shows something: a model file LightCraft cannot use has nothing to confirm, nor has a
/// dialog that only shows something (one Close, no Cancel / OK).
fn is_informational(dlg: &Dialog) -> bool {
    let unusable_model = matches!(dlg, Dialog::DenoiseModel { info, .. } | Dialog::FaceModel { info, .. } if info["kind"] == "unsupported");
    unusable_model || matches!(dlg, Dialog::LightroomResult { first: None, .. }) || only_shows(dlg)
}

/// What the SAM 3 download looks like: installed, running, failed, and whether there is nowhere to download from.
struct SamState {
    installed: bool,
    running: bool,
    failed: bool,
    /// no download location in this build: nothing to offer but the manual install
    by_hand: bool,
}

impl SamState {
    fn of(app: &LightcraftApp) -> Self {
        let sam = &app.session.segmenter;
        SamState {
            installed: sam.installed(),
            running: sam.download_status().running,
            failed: sam.download_status().error.is_some(),
            by_hand: sam_by_hand(sam),
        }
    }
}

/// The label of a dialog's cancel button.
fn cancel_label(dlg: &Dialog, sam: &SamState) -> &'static str {
    match dlg {
        Dialog::SamModel { .. } if sam.running || sam.installed || sam.by_hand => "Close",
        Dialog::SamModel { .. } | Dialog::LensDatabase { .. } => "Not Now",
        Dialog::LightroomResult { .. } => "Close",
        _ => "Cancel",
    }
}

/// The label of a dialog's confirm button (empty: it has none).
fn ok_label(app: &LightcraftApp, dlg: &Dialog, informational: bool, sam: &SamState) -> String {
    match dlg {
        Dialog::Import { opts } => import_ok_label(opts),
        Dialog::Export { .. } => photos_ok_label("Export", app.session.selection.ids.len().max(1)),
        Dialog::SamModel { .. } => sam_ok_label(dlg, sam).to_string(),
        _ => fixed_ok_label(app, dlg, informational).to_string(),
    }
}

/// "Import 3 Photos", "Export 1 Photo": a verb and how many photos it acts on.
fn photos_ok_label(verb: &str, n: usize) -> String {
    crate::i18n::tr_format!("{verb} {n} Photo{}", crate::i18n::plural_s(n), verb = crate::i18n::tr(verb), n = n)
}

/// The import review's confirm: "Import 3 Photos", or with Move "Move 3 Photos". A checked file whose photo is in
/// Recently Deleted is restored, not moved (its files stay where they are), so Move counts those apart:
/// "Move 2 Photos · Restore 1 Photo", or "Restore 1 Photo" when nothing is left to move.
fn import_ok_label(opts: &crate::import::ImportDialog) -> String {
    let n = opts.selected_paths().len();
    if !(opts.copy && opts.move_files) {
        return photos_ok_label("Import", n);
    }
    let restored = opts.restored_count();
    match (n - restored, restored) {
        (_, 0) => photos_ok_label("Move", n),
        (0, r) => photos_ok_label("Restore", r),
        (m, r) => format!("{} · {}", photos_ok_label("Move", m), photos_ok_label("Restore", r)),
    }
}

/// The confirm labels that are one fixed phrase.
fn fixed_ok_label(app: &LightcraftApp, dlg: &Dialog, informational: bool) -> &'static str {
    match dlg {
        // worded with a count or by the SAM state: `ok_label` builds them
        Dialog::Import { .. } | Dialog::Export { .. } | Dialog::SamModel { .. } => "",
        Dialog::LightroomResult { first: Some(_), .. } => "Show in Library",
        Dialog::ContactSheet { .. } => "Choose PDF…",
        Dialog::SlideshowExport { .. } => "Export…",
        Dialog::Print => "Print to PDF…",
        Dialog::Merge { .. } => "Merge",
        Dialog::DenoiseModel { info, .. } if !informational && info["download"].is_string() => "Accept & Download",
        Dialog::DenoiseModel { .. } if !informational => "Install",
        Dialog::FaceModel { info, .. } if !informational && info["download"].is_string() => "Download",
        Dialog::FaceModel { .. } if !informational => "Install",
        Dialog::ConfirmDelete { .. } | Dialog::DeleteKeyword { .. } | Dialog::DeleteEffects { .. } => "Delete",
        Dialog::SaveEffect { .. }
        | Dialog::NewAlbum { .. }
        | Dialog::NewSmartAlbum { .. }
        | Dialog::SmartRules { id: None, .. }
        | Dialog::CreatePreset { .. } => "Create",
        Dialog::SmartRules { .. } | Dialog::LabelNames { .. } => "Save",
        Dialog::RenameKeyword { from, to } if app.session.catalog.has_keyword(to) && !lightcraft_catalog::keywords::same(from, to) => {
            "Merge Keywords"
        }
        Dialog::RenameAlbum { .. } | Dialog::Rename { .. } | Dialog::RenameKeyword { .. } => "Rename",
        Dialog::AutoStack { .. } => "Create Stacks",
        Dialog::CaptureTime { .. } => "Change Time",
        Dialog::Cull { .. } => "Cull",
        Dialog::CopySettings { .. } => "Copy",
        Dialog::PasteSettings { .. } => "Paste",
        Dialog::TextPrompt { command, .. } => prompt_verb(command),
        Dialog::MovePhotos { .. } => "Move",
        // (not "Merge": that is photo merging, HDR / panorama, in some languages)
        Dialog::MoveKeyword { .. } | Dialog::MergeKeywords { .. } => "Merge Keywords",
        Dialog::KeywordSet { .. } => "Save",
        Dialog::KeywordTag { editing: None, .. } => "Create",
        Dialog::KeywordTag { .. } => "Save",
        Dialog::RemoveFolder { .. } => "Remove",
        Dialog::LightroomCatalog { .. } => "Import Lightroom Catalog…",
        d @ Dialog::LightroomDestination { .. } => crate::lightroom_destination::ok_label(d),
        Dialog::Welcome { .. } => "",
        Dialog::LensDatabase { .. } => "Download",
        Dialog::SynchronizeFolder { .. } if informational => "Close",
        Dialog::SynchronizeFolder { .. } => "Synchronize",
        Dialog::RenamePerson { from, to, .. } if person_rename_plan(&app.session.catalog, from, to).needs_merge => "Merge",
        Dialog::RenamePerson { .. } => "Rename",
        // the dialogs that only show something, or a model file that can't be used
        Dialog::About
        | Dialog::Shortcuts
        | Dialog::Settings { .. }
        | Dialog::SystemInfo { .. }
        | Dialog::AllMetadata { .. }
        | Dialog::WhatsNew
        | Dialog::ManageEffects { .. }
        | Dialog::ViewOptions
        | Dialog::LightroomResult { .. }
        | Dialog::DenoiseModel { .. }
        | Dialog::FaceModel { .. } => "Close",
    }
}

/// The label of the SAM 3 dialog's confirm button.
fn sam_ok_label(dlg: &Dialog, sam: &SamState) -> &'static str {
    match dlg {
        Dialog::SamModel { then: Some(_), .. } if sam.installed => "Continue",
        Dialog::SamModel { .. } if sam.installed || sam.running || sam.by_hand => "",
        Dialog::SamModel { error, .. } if error.is_some() || sam.failed => "Try Again",
        _ => "Download",
    }
}

/// Whether the confirm button works: installing waits for the licence to be accepted (and Synchronize waits for the
/// scan), a rename waits for its conflicts to be settled, smart-album rules that can't mean anything wait to be fixed.
fn can_confirm(app: &LightcraftApp, dlg: &Dialog, informational: bool) -> bool {
    let rename_blocked = match dlg {
        Dialog::RenamePerson { from, to, merge } => {
            let plan = person_rename_plan(&app.session.catalog, from, to);
            plan.block.is_some() || (plan.needs_merge && !*merge)
        }
        _ => false,
    };
    let waiting = matches!(
        dlg,
        Dialog::DenoiseModel { accepted: false, .. } | Dialog::FaceModel { accepted: false, .. } | Dialog::SynchronizeFolder { counts: None, .. }
    );
    let bad_rules =
        matches!(dlg, Dialog::SmartRules { id, rules, .. } if !rules.check_for(&app.session.catalog, id.map(lightcraft_catalog::AlbumId)).is_empty());
    !rename_blocked && (informational || (!waiting && !bad_rules))
}

/// Say where a folder made on disk is: the new folder holds no photos yet, so the Folders list may not show it.
/// Shared by the dialog's button and `ui.dialog.confirm`.
pub(crate) fn toast_created_folder(app: &mut LightcraftApp, ctx: &egui::Context, dlg: &Dialog, made: &serde_json::Value) {
    if let Dialog::TextPrompt { command, params, .. } = dlg
        && command == "folder.create"
    {
        let (path, parent) = (made["path"].as_str().unwrap_or_default(), params["parent"].as_str().unwrap_or_default());
        app.toast(ctx, crate::panels::left::created_text(path, parent));
    }
}

/// The dialog's confirm was pressed: do it. Whether the dialog closes.
fn confirmed(app: &mut LightcraftApp, ctx: &egui::Context, dlg: &mut Dialog) -> bool {
    let mut closed = false;
    match confirm_dialog(app, dlg) {
        // the import review stays open on an error (e.g. an unusable folder template), and so
        // does Export (e.g. no folder chosen) so the choices aren't lost, and Edit Capture Time
        // (a time that isn't a date, or a shift it refuses), and the lens database offer (a download that can't
        // start: Download again or Not Now)
        Err(e)
            if matches!(
                dlg,
                Dialog::Import { .. }
                    | Dialog::Export { .. }
                    | Dialog::ContactSheet { .. }
                    | Dialog::SlideshowExport { .. }
                    | Dialog::Print
                    | Dialog::NewAlbum { .. }
                    | Dialog::NewSmartAlbum { .. }
                    | Dialog::SmartRules { .. }
                    | Dialog::KeywordTag { .. }
                    | Dialog::KeywordSet { .. }
                    | Dialog::SynchronizeFolder { .. }
                    | Dialog::SaveEffect { .. }
                    | Dialog::CaptureTime { .. }
                    | Dialog::LensDatabase { .. }
            ) =>
        {
            app.toast(ctx, e)
        }
        // the SAM 3 dialog stays open to show the download (or why it can't start)
        Err(e) if matches!(dlg, Dialog::SamModel { .. }) => {
            if let Dialog::SamModel { error, .. } = dlg {
                *error = Some(e);
            }
        }
        // a one-line prompt that is refused stays open with its text, so the name can be fixed
        Err(e) if matches!(dlg, Dialog::TextPrompt { .. }) => app.toast_error(ctx, e),
        // a folder made on disk says so (the new folder holds no photos yet)
        Ok(r) if matches!(&dlg, Dialog::TextPrompt { command, .. } if command == "folder.create") => {
            toast_created_folder(app, ctx, dlg, &r);
            closed = true;
        }
        Ok(r) if matches!(dlg, Dialog::NewAlbum { .. }) && r.get("skipped").and_then(serde_json::Value::as_u64).is_some_and(|n| n > 0) => {
            // albums hold library photos (issue #684): say which part of the selection stayed out
            crate::panels::grid::toast_skipped(app, ctx, &r);
            closed = true;
        }
        // a move says how it went, or why nothing moved
        Ok(r) if matches!(dlg, Dialog::MovePhotos { .. }) => {
            if let Dialog::MovePhotos { name, .. } = dlg {
                let text = crate::panels::left::moved_text(r["moved"].as_u64().unwrap_or(0), name);
                app.toast(ctx, text);
            }
            closed = true;
        }
        Err(e) if matches!(dlg, Dialog::MovePhotos { .. }) => {
            app.toast(ctx, e);
            closed = true;
        }
        // photos whose capture time doesn't read are left as they were: say so
        Ok(r) if matches!(dlg, Dialog::CaptureTime { .. }) && r["unreadable"].as_u64().is_some_and(|n| n > 0) => {
            app.toast(ctx, crate::i18n::tr("Photos whose capture time can't be read were left as they were"));
            closed = true;
        }
        // where a Lightroom catalog goes: stays open to be corrected (a folder that isn't
        // empty), unless the library it names has gone
        Err(e) if matches!(dlg, Dialog::LightroomDestination { .. }) => {
            closed = !crate::lightroom_destination::keeps_open_on_error(app, dlg);
            app.toast(ctx, e);
        }
        Ok(_) if keeps_open(app, dlg) => {
            if let Dialog::SamModel { error, .. } = dlg {
                *error = None;
            }
        }
        _ => closed = true,
    }

    closed
}

/// The last frame a dialog was drawn and stayed open, for [`clicked_backdrop`].
const BACKDROP_SHOWN: &str = "dialog-backdrop-shown";

/// What a one-field prompt's confirm button says, by the command it runs. A command that isn't
/// listed (one opened by automation) has no verb to name and keeps "OK".
fn prompt_verb(command: &str) -> &'static str {
    match command {
        "album.rename" | "folder.rename" | "version.rename" | "preset.rename" | "mask.renameEffect" => "Rename",
        "folder.create" | "mask.setEffectGroup" | "app.newLibrary" => "Create",
        "preset.move" => "Move",
        "keyword.saveSet" | "filter.savePreset" | "curve.savePreset" | "metadata.savePreset" => "Save",
        "photo.tagFromTracklog" => "Add Locations",
        _ => "OK",
    }
}

/// Whether a dialog stays open after its action succeeded (the SAM 3 dialog while the model
/// downloads).
pub fn keeps_open(app: &LightcraftApp, dlg: &Dialog) -> bool {
    matches!(dlg, Dialog::SamModel { .. }) && !app.session.segmenter.installed()
}

/// The body of the AutoStack dialog.
fn auto_stack_body(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, dlg: &mut Dialog) {
    let Dialog::AutoStack { gap } = dlg else { return };
    ui.label(egui::RichText::new(crate::i18n::tr("Stack photos taken within this time of each other:")).color(t.text_label));
    cull::gap_control(
        ui,
        "slider:autoStackGap",
        "value:autoStackGap",
        crate::i18n::tr("Stack photos taken within this time of each other:"),
        gap,
        1.0..=86400.0,
        false,
    );
    let preview = app.session.execute("stack.auto", &json!({"gap": *gap, "preview": true})).unwrap_or_default();
    let scope = if app.session.selection.ids.len() > 1 { "the selected photos" } else { "the photos in view" };
    ui.label(
        egui::RichText::new({
            let (stacks, photos) = (preview["stacks"].as_u64().unwrap_or(0), preview["photos"].as_u64().unwrap_or(0));
            crate::i18n::tr_format!(
                "Creates {stacks} stack{} from {photos} of {scope}",
                if stacks == 1 { "" } else { "s" },
                stacks = stacks,
                photos = photos,
                scope = crate::i18n::tr(scope)
            )
        })
        .color(t.text_dim),
    );
}

/// The body of the Cull dialog.
fn cull_body(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, dlg: &mut Dialog) {
    cull::body(app, ui, t, dlg);
}

/// The body of the WhatsNew dialog.
fn whats_new_body(ui: &mut egui::Ui, t: &Tokens, dlg: &mut Dialog) {
    let Dialog::WhatsNew = dlg else { return };
    egui::ScrollArea::vertical().max_height(list_height(ui, 460.0)).auto_shrink([false, true]).show(ui, |ui| {
        for line in WHATS_NEW.lines() {
            let l = line.trim_end();
            if let Some(h) = l.strip_prefix("### ") {
                ui.add_space(6.0);
                ui.label(egui::RichText::new(crate::i18n::tr(h)).font(t.semibold(12.5)).color(t.text));
            } else if let Some(h) = l.strip_prefix("## ") {
                ui.add_space(8.0);
                ui.label(egui::RichText::new(crate::i18n::tr(h)).font(t.semibold(14.0)).color(t.text));
            } else if l.starts_with("# ") || l.is_empty() {
            } else if let Some(b) = l.strip_prefix("- ") {
                ui.label(egui::RichText::new(format!("•  {}", crate::i18n::tr(b).replace('`', ""))).color(t.text_label));
            } else {
                ui.label(egui::RichText::new(crate::i18n::tr(l.trim()).replace('`', "")).color(t.text_label));
            }
        }
    });
}

/// The body of the LightroomResult dialog.
fn lightroom_result_body(ui: &mut egui::Ui, dlg: &mut Dialog) {
    let Dialog::LightroomResult { summary, library, details, .. } = dlg else { return };
    ui.label(summary.as_str());
    ui.add_space(8.0);
    if let Some(library) = library {
        ui.label(crate::i18n::tr("Destination library"));
        ui.add(egui::Label::new(library.as_str()).wrap().selectable(true));
        ui.label(crate::i18n::tr("Lightroom catalogs import into the open library; no separate catalog folder is created."));
    } else {
        ui.label(crate::i18n::tr("These photos are only in memory. Open a library on disk and import the catalog again to save them."));
    }
    for detail in details.iter() {
        ui.add(egui::Label::new(detail).wrap().selectable(true));
    }
}

/// The body of the SystemInfo dialog.
fn system_info_body(ui: &mut egui::Ui, t: &Tokens, dlg: &mut Dialog) {
    let Dialog::SystemInfo { rows } = dlg else { return };
    egui::Grid::new("sysinfo").num_columns(2).spacing([16.0, 4.0]).striped(true).show(ui, |ui| {
        for (k, v) in rows.iter() {
            ui.label(egui::RichText::new(crate::i18n::tr(k)).color(t.text_dim));
            ui.add(egui::Label::new(egui::RichText::new(v).color(t.text_label)).wrap());
            ui.end_row();
        }
    });
    if dialog_button(ui, "button:systemInfoCopy", "Copy to Clipboard") {
        let text: String = rows.iter().map(|(k, v)| format!("{k}: {v}\n")).collect();
        ui.ctx().copy_text(text);
    }
}

/// The body of the AllMetadata dialog.
fn all_metadata_body(ui: &mut egui::Ui, t: &Tokens, dlg: &mut Dialog) {
    let Dialog::AllMetadata { title, rows, search } = dlg else { return };
    ui.label(egui::RichText::new(title.as_str()).color(t.text_label));
    crate::text_field::TextField::singleline("field:metadataSearch", search)
        .hint(crate::i18n::tr("Filter fields"))
        .label(crate::i18n::tr("Filter fields"))
        .width(f32::INFINITY)
        .show(ui);
    let q = search.trim().to_lowercase();
    let keep = |n: &str, v: &str| q.is_empty() || n.to_lowercase().contains(&q) || v.to_lowercase().contains(&q);
    let mut groups: Vec<(String, Vec<(String, String)>)> = Vec::new();
    for row in rows["exif"].as_array().into_iter().flatten() {
        let (g, n, v) = (row["group"].as_str().unwrap_or(""), row["name"].as_str().unwrap_or(""), row["value"].as_str().unwrap_or(""));
        if !keep(n, v) {
            continue;
        }
        match groups.iter_mut().find(|(x, _)| x == g) {
            Some((_, list)) => list.push((n.into(), v.into())),
            None => groups.push((g.into(), vec![(n.into(), v.into())])),
        }
    }
    let xmp: Vec<(String, String)> = rows["xmp"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| (r["name"].as_str().unwrap_or("").to_string(), r["value"].as_str().unwrap_or("").to_string()))
        .filter(|(n, v)| keep(n, v))
        .collect();
    if !xmp.is_empty() {
        groups.push(("XMP".into(), xmp));
    }
    egui::ScrollArea::vertical().max_height(list_height(ui, 440.0)).auto_shrink([false, true]).show(ui, |ui| {
        if groups.is_empty() {
            ui.label(egui::RichText::new(crate::i18n::tr(rows["note"].as_str().unwrap_or("No metadata found"))).color(t.text_dim));
        }
        for (g, list) in &groups {
            ui.add_space(6.0);
            ui.label(egui::RichText::new(g).font(t.semibold(12.5)).color(t.text));
            egui::Grid::new(format!("meta-{g}")).num_columns(2).spacing([16.0, 3.0]).striped(true).show(ui, |ui| {
                for (n, v) in list {
                    ui.label(egui::RichText::new(n).color(t.text_dim));
                    ui.add(egui::Label::new(egui::RichText::new(v).color(t.text_label)).wrap());
                    ui.end_row();
                }
            });
        }
    });
}

/// The body of the SmartRules dialog.
fn smart_rules_body(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, close: &mut bool, dlg: &mut Dialog) {
    let Dialog::SmartRules { id, name, rules, .. } = dlg else { return };
    let real = !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx());
    use craft::ui::contract::{Bind, Props as _, Show as _};
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ui.spacing().interact_size.y.max(24.0)), egui::Sense::hover());
    let eligible = real && ui.is_enabled() && ui.is_visible() && ui.clip_rect().intersects(rect);
    let shown = craft::ui::text::TextField::new(crate::i18n::tr("Name").to_owned(), Bind::bound(name))
        .id_salt("field:smartName")
        .placeholder(crate::i18n::tr("Name").to_owned())
        // A covering legacy prompt freezes the draft, including gesture-cancel restores.
        // Disabled presentation alone can still produce an SDK Cancel on the old focus.
        .read_only(!ui.is_enabled())
        .limit(None)
        .show_at(ui, rect);
    if real {
        crate::widgets::register_response("field:smartName", shown.response.rect, &shown.response);
    }
    // Only this plain Name opts into the observed legacy one-Escape whole-draft close.
    // No programmatic draft-revert/completion path is installed here: public `escaped`
    // also covers Escape-reason Revert and must not be treated as proof of a physical key.
    *close |= eligible && shown.escaped;
    ui.add_space(6.0);
    // the folder an album made from a folder view carries is not in the editor, but it counts
    let folder = id
        .and_then(|id| app.session.catalog.album(lightcraft_catalog::AlbumId(id)))
        .and_then(|a| a.smart.as_deref().and_then(|f| f.library_folder.clone()));
    // problems, count and albums: cached until the catalog or the rules change (not per frame)
    let now = (app.session.clock)();
    let view = app.caches.rules_view(&app.session.catalog, rules, *id, folder, &now);
    let env = crate::panels::rules_editor::Env { problems: view.problems.clone(), today: now, albums: view.albums.clone() };
    egui::ScrollArea::vertical().max_height(list_height(ui, 360.0)).auto_shrink([false, true]).show(ui, |ui| {
        crate::panels::rules_editor::edit(ui, rules, "rules", 0, &[], &env);
    });
    let n = view.count;
    ui.add_space(4.0);
    if view.problems.is_empty() {
        ui.label(
            egui::RichText::new(crate::i18n::tr_format!(
                "{n} photo{} match · updates automatically as photos change",
                if n == 1 { "" } else { "s" },
                n = n
            ))
            .color(t.text_dim),
        );
    } else {
        ui.label(egui::RichText::new(crate::i18n::tr("Fix the marked rules to save this album.")).color(t.caution));
    }
}

/// The body of the NewSmartAlbum dialog.
fn new_smart_album_body(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, confirm: &mut bool, dlg: &mut Dialog) {
    let Dialog::NewSmartAlbum { name, .. } = dlg else { return };
    if dialog_field(ui, "field:smartAlbumName", name, "Name") {
        *confirm = true;
    }
    let rules = app.session.view_rules();
    // counted when the library or the rules change (or, for "in the last…" rules, the minute), not every frame
    let now = (app.session.clock)();
    let minute = if rules.depends_on_now() { now.get(..16).unwrap_or(&now) } else { "" };
    let n = app.caches.dialog_count(&app.session.catalog, ("smartAlbum", format!("{rules:?}"), minute), || {
        lightcraft_catalog::rules::set_now(Some(now.clone()));
        app.session.catalog.query(&rules, &Default::default()).len()
    });
    ui.label(
        egui::RichText::new(crate::i18n::tr_format!("Matches: {}", crate::i18n::filter_label(&rules, &app.session.catalog))).color(t.text_label),
    );
    ui.label(
        egui::RichText::new(crate::i18n::tr_format!(
            "{n} photo{} now · updates automatically as photos change",
            if n == 1 { "" } else { "s" },
            n = n
        ))
        .color(t.text_dim),
    );
}

/// A framework number field over a whole number, `suffix` written after it (" d"), registered as `name`.
fn whole_number(ui: &mut egui::Ui, name: &str, label: &str, value: &mut i32, range: std::ops::RangeInclusive<i32>, suffix: &str) {
    use craft::ui::contract::{Bind, Props as _, Show as _};
    use craft::ui::text::NumberField;
    let mut exact = Some(f64::from(*value));
    let (rect, _) = ui.allocate_exact_size(egui::vec2(72.0, 24.0), egui::Sense::hover());
    let shown = NumberField::new(crate::i18n::tr(label).to_owned(), Bind::bound(&mut exact))
        .id_salt(("lc-whole-number", name))
        .range(f64::from(*range.start())..=f64::from(*range.end()))
        .step(1.0)
        .snap_typed(true)
        .commit_decimals(0)
        .fixed_decimals(0)
        .show_at(ui, rect);
    if !suffix.trim().is_empty() {
        ui.label(suffix.trim());
    }
    if ui.is_sizing_pass() || craft::ui::runtime::measure::active(ui.ctx()) {
        return;
    }
    crate::widgets::register_response(name, rect, &shown.response);
    if let Some(v) = exact.filter(|v| v.is_finite()) {
        *value = (v.round() as i64).clamp(i64::from(*range.start()), i64::from(*range.end())) as i32;
    }
}

/// A framework number field over a decimal value with `decimals` places, stepping by `step`, registered as `name`.
fn decimal_number(ui: &mut egui::Ui, name: &str, label: &str, value: &mut f32, range: std::ops::RangeInclusive<f64>, step: f64, decimals: usize) {
    use craft::ui::contract::{Bind, Props as _, Show as _};
    use craft::ui::text::NumberField;
    let mut exact = Some(f64::from(*value));
    let (rect, _) = ui.allocate_exact_size(egui::vec2(80.0, 24.0), egui::Sense::hover());
    let shown = NumberField::new(crate::i18n::tr(label).to_owned(), Bind::bound(&mut exact))
        .id_salt(("lc-decimal-number", name))
        .range(range)
        .step(step)
        .commit_decimals(decimals)
        .fixed_decimals(decimals)
        .show_at(ui, rect);
    if ui.is_sizing_pass() || craft::ui::runtime::measure::active(ui.ctx()) {
        return;
    }
    crate::widgets::register_response(name, rect, &shown.response);
    if let Some(v) = exact.filter(|v| v.is_finite()) {
        *value = v as f32;
    }
}

/// The body of the CaptureTime dialog.
fn capture_time_body(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, dlg: &mut Dialog) {
    let Dialog::CaptureTime { mode, time, days, hours, minutes, zone } = dlg else { return };
    let n = app.session.selected().len();
    field(ui, "Change", |ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (i, (label, m)) in [("Set date & time", "set"), ("Shift by", "shift"), ("Time zone", "zone")].iter().enumerate() {
            if crate::widgets::text_button(ui, &format!("captureMode-{i}"), label, mode == m).clicked() {
                *mode = m.to_string();
            }
        }
    });
    let params = match mode.as_str() {
        "set" => {
            field(ui, "New time", |ui| {
                crate::text_field::TextField::singleline("field:captureTime", time)
                    .hint("2026-09-30 14:05:00")
                    .label(crate::i18n::tr("New time"))
                    .width(f32::INFINITY)
                    .show(ui);
            });
            json!({"time": time})
        }
        "shift" => {
            field(ui, "Shift", |ui| {
                whole_number(ui, "field:captureDays", "Days", days, -36_500..=36_500, " d");
                whole_number(ui, "field:captureHours", "Hours", hours, -100_000..=100_000, " h");
                whole_number(ui, "field:captureMinutes", "Minutes", minutes, -1_000_000..=1_000_000, " min");
            });
            json!({"shift": *days as i64 * 86_400 + *hours as i64 * 3600 + *minutes as i64 * 60})
        }
        _ => {
            field(ui, "Hours", |ui| decimal_number(ui, "field:captureZone", "Hours", zone, -26.0..=26.0, 0.25, 2));
            json!({"hours": zone})
        }
    };
    // preview on the active photo (the others move by the same amount)
    let active = app.session.active().and_then(|id| app.session.catalog.photo(id)).map(|p| (p.file_name.clone(), p.date().to_string()));
    if let Some((name, cur)) = active {
        let delta = match mode.as_str() {
            "set" => lightcraft_catalog::dates::normalize_iso(time)
                .and_then(|t| Some(lightcraft_catalog::dates::iso_seconds(&t)? - lightcraft_catalog::dates::iso_seconds(&cur)?)),
            "shift" => params["shift"].as_i64(),
            _ => Some((*zone as f64 * 3600.0).round() as i64),
        };
        // the photo's own time is the problem (a Lightroom catalog's capture time
        // is copied verbatim), not what was typed
        let unreadable = lightcraft_catalog::dates::shift_iso(&cur, 0).is_none();
        let after = match (mode.as_str(), unreadable && n == 1) {
            // alone, it simply gets the time typed (photo.setCaptureTime repairs it)
            ("set", true) => lightcraft_catalog::dates::normalize_iso(time),
            _ => delta.and_then(|d| lightcraft_catalog::dates::shift_iso(&cur, d)),
        };
        ui.label(
            egui::RichText::new(match after {
                Some(a) => format!("{name}: {} → {}", cur.replace('T', " "), a.replace('T', " ")),
                None if unreadable && !(mode == "set" && n == 1) => crate::i18n::tr("The active photo's capture time can't be read").to_string(),
                None => "Enter a date as YYYY-MM-DD HH:MM:SS".into(),
            })
            .color(t.text_label),
        );
    }
    if n > 1 {
        ui.label(egui::RichText::new(crate::i18n::tr_format!("All {n} selected photos move by the same amount.", n = n)).color(t.text_dim));
    }
}

/// The body of the LabelNames dialog.
fn label_names_body(
    app: &mut LightcraftApp,
    ui: &mut egui::Ui,
    t: &Tokens,
    dlg: &mut Dialog,
) -> bool {
    let Dialog::LabelNames { names, save_as } = dlg else {
        return false;
    };
    // Six local inputs only: speculative/disabled edits never publish to the actual draft.
    let mut candidate_names = names.clone();
    let mut candidate_save_as = save_as.clone();
    candidate_names.resize(5, String::new());
    let mut escaped = false;
    let sets = lightcraft_engine::cmd::manage::label_sets_json(&app.session);
    field(ui, "Set", |ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let mut choices = |ui: &mut egui::Ui| {
            for set in sets["sets"].as_array().into_iter().flatten() {
                let name = set["name"].as_str().unwrap_or_default();
                if crate::widgets::text_button(ui, &format!("labelSet-{name}"), name, false)
                    .clicked()
                {
                    candidate_names = set["names"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|n| n.as_str().unwrap_or_default().to_string())
                        .collect();
                }
            }
        };
        if craft::ui::runtime::measure::active(ui.ctx()) && !ui.is_sizing_pass() {
            // A context-side measuring run alone is not an egui sizing Ui. Use the
            // supported sizing-only presentation so no live SDK press can begin.
            let mut sizing = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(ui.available_rect_before_wrap())
                    .layout(*ui.layout())
                    .sizing_pass()
                    .invisible(),
            );
            choices(&mut sizing);
            ui.advance_cursor_after_rect(sizing.min_rect());
        } else {
            // Actual enabled/disabled parents and an existing sizing Ui stay unchanged.
            choices(ui);
        }
    });
    for (i, l) in lightcraft_catalog::ColorLabel::ALL.iter().enumerate() {
        let colour = format!("{l:?}");
        field(ui, &colour, |ui| {
            let (r, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
            ui.painter()
                .circle_filled(r.center(), 6.0, crate::panels::grid::label_color(*l));
            escaped |= label_name_field(
                ui,
                &format!("field:labelName-{}", colour.to_lowercase()),
                &colour,
                &mut candidate_names[i],
                &colour,
            );
        });
    }
    field(ui, "Save as set", |ui| {
        escaped |= label_name_field(
            ui,
            "field:labelSetName",
            "Save as set",
            &mut candidate_save_as,
            crate::i18n::tr("Optional name"),
        );
    });
    ui.label(egui::RichText::new(crate::i18n::tr(
        "Names appear in the label menu, the filter bar and the Info panel, and are written to XMP. Empty = the colour's name.",
    )).color(t.text_dim));
    if ui.is_enabled() && !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx()) {
        *names = candidate_names;
        *save_as = candidate_save_as;
    }
    escaped
}

fn label_name_field(
    ui: &mut egui::Ui,
    alias: &str,
    label: &str,
    value: &mut String,
    hint: &str,
) -> bool {
    use craft::ui::contract::{Bind, Props as _, Show as _};
    use craft::ui::text::TextField;
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width().max(1.0), 24.0),
        egui::Sense::hover(),
    );
    let eligible = ui.is_enabled()
        && ui.is_visible()
        && ui.clip_rect().intersects(rect)
        && !ui.is_sizing_pass()
        && !craft::ui::runtime::measure::active(ui.ctx());
    // Return belongs to this field, but LabelNames has never confirmed from a raw input.
    // Deliberately do not forward submitted to the generic dialog's confirmation flag.
    let shown = TextField::new(crate::i18n::tr(label).to_owned(), Bind::bound(value))
        .id_salt(alias)
        .placeholder(hint.to_owned())
        .limit(None)
        .submits()
        .show_at(ui, rect);
    if !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx()) {
        crate::widgets::register_response(alias, shown.response.rect, &shown.response);
    }
    // Like the existing Smart Name consumer, this plain field has no programmatic
    // revert or completion path. Preserve its observed one-Escape whole-dialog close.
    eligible && shown.escaped
}


/// The body of the MergeKeywords dialog.
fn merge_keywords_body(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, confirm: &mut bool, dlg: &mut Dialog) {
    let Dialog::MergeKeywords { from, into } = dlg else { return };
    ui.label(
        egui::RichText::new(crate::i18n::tr_format!("Replace {} with:", from.iter().map(|f| format!("“{f}”")).collect::<Vec<_>>().join(", ")))
            .color(t.text_label),
    );
    if dialog_field(ui, "field:keywordInto", into, "Keyword") {
        *confirm = true;
    }
    let options: Vec<String> = app
        .caches
        .suggestions(&app.session.catalog, from, into, 12)
        .iter()
        .filter(|k| !from.iter().any(|f| lightcraft_catalog::keywords::is_under(k, f)))
        .cloned()
        .collect();
    ui.horizontal_wrapped(|ui| {
        for k in options {
            use craft::ui::contract::{Props as _, Show as _};
            // a keyword chip: a small framework button named by the keyword
            let chip = craft::ui::buttons::Button::new(k.clone()).id_salt(("keywordIntoChip", &k)).size(craft::theme::ControlSize::Sm).show(ui);
            if !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx()) {
                crate::widgets::register_response(format!("button:keywordInto-{k}"), chip.response.rect, &chip.response);
                if chip.clicked() {
                    *into = k;
                }
            }
        }
    });
}

/// The body of the NewAlbum dialog.
fn new_album_body(
    app: &mut LightcraftApp,
    ui: &mut egui::Ui,
    confirm: &mut bool,
    dlg: &mut Dialog,
) {
    use craft::ui::choice::Checkbox;
    use craft::ui::contract::{Bind, Props as _, Show as _};
    // Respect an explicit native presentation scope without changing the
    // installed generic dialog's supported-SDK default.
    let Dialog::NewAlbum {
        name,
        folder,
        include_selected,
        ..
    } = dlg
    else {
        return;
    };
    if new_album_name_field(ui, name) {
        *confirm = true;
    }
    if !*folder && !app.session.selected().is_empty() {
        ui.add_space(6.0);
        let mut candidate_include = *include_selected;
        let shown = Checkbox::new(Bind::bound(&mut candidate_include))
            .id_salt("albumIncludeSelected")
            .label(crate::i18n::tr("Include selected photos").to_owned())
            .show(ui);
        if ui.is_enabled() && !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx())
        {
            *include_selected = candidate_include;
        }
        if !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx()) {
            crate::widgets::register_response(
                "check:albumIncludeSelected",
                shown.response.rect,
                &shown.response,
            );
        }
    }
}

// Keep the established single-field dialog contract local to this workflow:
// initial select-all, native/SDK Tab ownership, submit and refusal refocus.
fn new_album_name_field(ui: &mut egui::Ui, value: &mut String) -> bool {
    use craft::ui::contract::{Bind, Props as _, Show as _};
    use craft::ui::runtime::focus::{self, FocusTarget};
    use craft::ui::text::TextField;
    let real = !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width().max(1.0), 24.0),
        egui::Sense::hover(),
    );
    let shown = TextField::new(crate::i18n::tr("Name").to_owned(), Bind::bound(value))
        .id_salt("field:albumName")
        .placeholder(crate::i18n::tr("Name").to_owned())
        .limit(None)
        .select_on_focus()
        .submits()
        .show_at(ui, rect);
    if real {
        crate::widgets::register_response("field:albumName", shown.response.rect, &shown.response);
    }
    if real
        && ui.is_enabled()
        && (just_opened(ui, "field:albumName")
            || (!shown.response.has_focus()
                && !shown.submitted
                && !shown.escaped
                && shown.left_by_tab.is_none()
                && ui.memory(|memory| memory.focused().is_none())))
    {
        focus::request(ui.ctx(), FocusTarget::Id(shown.response.id));
    }
    real && ui.is_enabled() && shown.submitted
}


/// The body of the PasteSettings dialog.
fn paste_settings_body(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, dlg: &mut Dialog) {
    let Dialog::PasteSettings { groups } = dlg else { return };
    let n = app.session.selected().len();
    ui.label(
        egui::RichText::new(crate::i18n::tr_format!(
            "Paste into {n} photo{} — only settings that were copied are pasted",
            if n == 1 { "" } else { "s" },
            n = n
        ))
        .color(t.text_dim),
    );
    group_checklist(ui, "pasteGroup", groups);
}

/// The body of the ContactSheet dialog.
fn contact_sheet_body(app: &mut LightcraftApp, ui: &mut egui::Ui, dlg: &mut Dialog) {
    let Dialog::ContactSheet { options } = dlg else { return };
    options.columns = options.columns.clamp(1, 8);
    options.rows = options.rows.clamp(1, 10);
    let n = app.session.selected().len();
    let pages = n.div_ceil(options.columns * options.rows);
    ui.label(contact_sheet_summary(n, pages));
    field(ui, "Paper", |ui| {
        let entries = [
            craft::ui::contract::Entry::new("a4".to_owned(), "A4".to_owned()),
            craft::ui::contract::Entry::new("letter".to_owned(), crate::i18n::tr("Letter").to_owned()),
        ];
        if let Some(paper) = crate::widgets::select(ui, "combo:contactSheetPaper", crate::i18n::tr("Paper"), options.paper.clone(), &entries, Some(options.paper.clone()), None) {
            options.paper = paper;
        }
    });
    dialog_check(ui, "check:contactSheetLandscape", "Landscape", &mut options.landscape);
    field(ui, "Columns", |ui| {
        let mut columns = options.columns as i32;
        whole_number(ui, "field:contactSheetColumns", "Columns", &mut columns, 1..=8, "");
        options.columns = columns.clamp(1, 8) as usize;
    });
    field(ui, "Rows", |ui| {
        let mut rows = options.rows as i32;
        whole_number(ui, "field:contactSheetRows", "Rows", &mut rows, 1..=10, "");
        options.rows = rows.clamp(1, 10) as usize;
    });
    dialog_check(ui, "check:contactSheetCaptions", "Filename captions", &mut options.captions);
    ui.label(crate::i18n::tr("150 dpi · sRGB · current edits · fit without cropping"));
    ui.label(crate::i18n::tr("Choose a PDF destination after confirming. Print the PDF from your viewer."));
}

/// "N selected photos · M pages".
fn contact_sheet_summary(n: usize, pages: usize) -> String {
    crate::i18n::tr_format!("{n} selected photo{} · {pages} page{}", crate::i18n::plural_s(n), crate::i18n::plural_s(pages), n = n, pages = pages)
}

/// Export keeps the established dialog chrome and confirmation authority.
fn export_body(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, dlg: &mut Dialog) {
    export::export_body(app, ui, t, dlg);
}

/// The body of the KeywordTag dialog.
fn keyword_tag_body(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, confirm: &mut bool, dlg: &mut Dialog) {
    let Dialog::KeywordTag {
        editing,
        name,
        parent,
        inside,
        synonyms,
        include_on_export,
        export_containing,
        export_synonyms,
        person,
        add_to_selected,
        ..
    } = dlg
    else {
        return;
    };

    use craft::behavior::Proposal;
    use craft::ui::choice::Checkbox;
    use craft::ui::contract::{Bind, Props as _, Show as _, ShowContainer as _, Sizing};
    use craft::ui::layout_components::Row;

    // These are dialog drafts; only the existing parent confirmation dispatches a command.
    fn field(ui: &mut egui::Ui, caller: (Option<&str>, Option<&str>), alias: &str, label: &str, hint: &str, value: &mut String, first: bool) -> bool {
        use craft::ui::text::TextField;
        let real = !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx());
        let item = || {
            let field = TextField::new(crate::i18n::tr(label).to_owned(), Bind::given(value.clone()))
                .id_salt(("keyword-tag-fields", caller, alias))
                .placeholder(crate::i18n::tr(hint).to_owned())
                .limit(None)
                .submits();
            if first { field.select_on_focus() } else { field }
        };
        if !real {
            let child = ui.new_child(egui::UiBuilder::new().max_rect(ui.available_rect_before_wrap()).sizing_pass().invisible());
            let size = Row::new().measure_in(&child, Proposal::Ideal, |ui, _| item().show(ui));
            ui.allocate_exact_size(egui::vec2(ui.available_width().max(1.0), size.y), egui::Sense::hover());
            return false;
        }
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width().max(1.0), 24.0), egui::Sense::hover());
        let shown = item().show_at(ui, rect);
        crate::widgets::register_response(alias, shown.response.rect, &shown.response);
        if !ui.is_enabled() {
            return false;
        }
        // Public landed events include typing and cancellation's restored draft, not just Return commits.
        if let Some(next) = shown.events.landed() {
            *value = next.clone();
        }
        if first && just_opened(ui, "keyword-tag") {
            craft::ui::runtime::focus::request(ui.ctx(), craft::ui::runtime::focus::FocusTarget::Id(shown.response.id));
        }
        shown.submitted
    }

    fn option(ui: &mut egui::Ui, caller: (Option<&str>, Option<&str>), alias: &str, label: String, value: &mut bool, enabled: bool) {
        let current = *value;
        let enabled = enabled && ui.is_enabled();
        let item = || {
            Checkbox::new(Bind::given(current))
                .id_salt(("keyword-tag-options", caller, alias))
                .label(label.clone())
                .enabled(enabled)
                .sizing(Sizing::FillWidth)
        };
        if ui.is_sizing_pass() || craft::ui::runtime::measure::active(ui.ctx()) {
            // Use the public labelled flow's ideal size without creating a live control or publishing a target.
            let child = ui.new_child(egui::UiBuilder::new().max_rect(ui.available_rect_before_wrap()).sizing_pass().invisible());
            let size = Row::new().measure_in(&child, Proposal::Ideal, |ui, _| item().show(ui));
            ui.allocate_exact_size(size, egui::Sense::hover());
            return;
        }
        let out = item().show(ui);
        crate::widgets::register_response(alias, out.response.rect, &out.response);
        #[cfg(test)]
        keyword_tag_options_flow_tests::observe(ui, alias, &out.response);
        if let Some(&next) = out.events.committed()
            && enabled
            && next != current
        {
            *value = next;
        }
    }
    // Only the actual edit target or creation parent identifies the caller; text, flags and selection are drafts.
    let caller = (editing.as_deref(), parent.as_deref());

    ui.label(egui::RichText::new(crate::i18n::tr("Keyword Name")).color(t.text_dim));
    if field(ui, caller, "field:keywordTagName", "Keyword Name", "Keyword Name", name, true) {
        *confirm = true;
    }
    ui.add_space(4.0);
    ui.label(egui::RichText::new(crate::i18n::tr("Synonyms")).color(t.text_dim));
    if field(ui, caller, "field:keywordSynonyms", "Synonyms", "Separated by commas", synonyms, false) {
        *confirm = true;
    }
    ui.add_space(6.0);

    if editing.is_none() {
        if let Some(p) = parent.as_ref() {
            option(ui, caller, "check:keywordInside", crate::i18n::tr_format!("Put inside “{parent}”", parent = p.replace('|', " › ")), inside, true);
        }
        option(
            ui,
            caller,
            "check:keywordAddToSelected",
            crate::i18n::tr("Add to Selected Photos").to_owned(),
            add_to_selected,
            !app.session.selection.ids.is_empty(),
        );
        ui.add_space(4.0);
    }
    ui.label(egui::RichText::new(crate::i18n::tr("Keyword Tag Options")).color(t.text_dim));
    for (on, label, alias) in [
        (include_on_export, "Include on Export", "check:keywordIncludeOnExport"),
        (export_containing, "Export Containing Keywords", "check:keywordExportContaining"),
        (export_synonyms, "Export Synonyms", "check:keywordExportSynonyms"),
        (person, "Person", "check:keywordPerson"),
    ] {
        option(ui, caller, alias, crate::i18n::tr(label).to_owned(), on, true);
    }
}

/// The body of the MoveKeyword dialog.
fn move_keyword_body(ui: &mut egui::Ui, t: &Tokens, dlg: &mut Dialog) {
    let Dialog::MoveKeyword { keyword, parent } = dlg else { return };
    let name = keyword.rsplit('|').next().unwrap_or(keyword);
    match parent {
        Some(p) => {
            ui.label(crate::i18n::tr_format!("“{parent}” has a “{name}” already. Merge the two?", parent = p.replace('|', " › "), name = name))
        }
        None => ui.label(crate::i18n::tr_format!("There is a “{name}” at the top level already. Merge the two?", name = name)),
    };
    ui.label(egui::RichText::new(crate::i18n::tr("Their photos and the keywords below them come together under one keyword.")).color(t.text_dim));
}

/// The body of the DeleteKeyword dialog.
fn delete_keyword_body(ui: &mut egui::Ui, t: &Tokens, dlg: &mut Dialog) {
    let Dialog::DeleteKeyword { keyword, count } = dlg else { return };
    let name = keyword.replace('|', " › ");
    ui.label(crate::i18n::tr_format!("Delete the keyword “{name}”?", name = name));
    ui.label(
        egui::RichText::new(if *count == 0 {
            crate::i18n::tr("No photo has it; the keywords below it go too.").to_string()
        } else {
            crate::i18n::tr_format!(
                "It is taken off {count} photo{}, with the keywords below it. Undo brings it back.",
                if *count == 1 { "" } else { "s" },
                count = count
            )
        })
        .color(t.text_dim),
    );
}

/// The body of the DeleteEffects dialog.
fn delete_effects_body(ui: &mut egui::Ui, t: &Tokens, dlg: &mut Dialog) {
    let Dialog::DeleteEffects { names } = dlg else { return };
    let what = match names.as_slice() {
        [one] => crate::i18n::tr_format!("Delete the effect preset “{name}”?", name = one),
        _ => crate::i18n::tr_format!("Delete these {n} effect presets?", n = names.len()),
    };
    ui.label(what);
    ui.label(egui::RichText::new(crate::i18n::tr("Masks keep their sliders. Export presets first to keep a copy.")).color(t.text_dim));
}

/// The body of the MovePhotos dialog.
fn move_photos_body(ui: &mut egui::Ui, t: &Tokens, dlg: &mut Dialog) {
    let Dialog::MovePhotos { ids, name, dont_ask, .. } = dlg else { return };
    let n = ids.len();
    ui.label(crate::i18n::tr_format!("Move {n} photo{} to “{}”?", if n == 1 { "" } else { "s" }, name, n = n));
    ui.label(egui::RichText::new(crate::i18n::tr("Their files and XMP sidecars move on disk. Edit → Undo moves them back.")).color(t.text_dim));
    ui.add_space(4.0);
    use craft::ui::choice::Checkbox;
    use craft::ui::contract::{Bind, Props as _, Show as _};
    let real = !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx());
    // A measuring or disabled run edits a copy only; the draft takes the real run's answer.
    let mut candidate = *dont_ask;
    let hint = crate::i18n::tr("Settings → Confirm before moving photos to another folder on disk turns it back on");
    let shown = Checkbox::new(Bind::bound(&mut candidate))
        .id_salt("moveDontAsk")
        .label(crate::i18n::tr("Don't ask again").to_owned())
        .tooltip(hint.to_owned())
        .show(ui);
    if real && ui.is_enabled() {
        *dont_ask = candidate;
    }
    if real {
        crate::widgets::register_response("check:moveDontAsk", shown.response.rect, &shown.response);
    }
}

/// The body of the LightroomCatalog dialog.
fn lightroom_catalog_body(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, dlg: &mut Dialog) {
    let Dialog::LightroomCatalog { folder, catalog } = dlg else { return };
    ui.label(crate::i18n::tr("This is a Lightroom catalog, not a LightCraft library. Import it into a LightCraft library instead?"));
    ui.label(egui::RichText::new(catalog.as_deref().unwrap_or(folder.as_str())).color(t.text_dim));
    // (the import then asks where the catalog goes, unless Settings says to merge, #2672)
    let asks = app.ui.settings.lightroom_import == crate::state::LightroomImport::Ask && app.session.library.as_ref().is_some_and(|l| l.on_disk);
    let what = if asks {
        "The import reads the catalog without changing it. Next, choose whether its photos, edits, keywords and collections join the library open now or go into a new library."
    } else {
        "The import reads the catalog without changing it and adds its photos, edits, keywords and collections to the library open now."
    };
    ui.label(egui::RichText::new(crate::i18n::tr(what)).color(t.text_dim));
}

/// The body of the RemoveFolder dialog.
fn remove_folder_body(ui: &mut egui::Ui, t: &Tokens, dlg: &mut Dialog) {
    let Dialog::RemoveFolder { name, count, path, disk } = dlg else { return };
    ui.label(crate::i18n::tr_format!(
        "Remove “{name}” and its {count} photo{} from the library?",
        if *count == 1 { "" } else { "s" },
        name = name,
        count = count
    ));
    ui.label(egui::RichText::new(path.as_str()).color(t.text_dim));
    let scope = if *disk { "All the photos imported from this disk are included." } else { "That includes the photos in the folders inside it." };
    ui.label(egui::RichText::new(crate::i18n::tr(scope)).color(t.text_dim));
    ui.label(
        egui::RichText::new(crate::i18n::tr("They move to Recently Deleted and can be restored; no file on disk is touched.")).color(t.text_dim),
    );
}

/// About owns its SDK layer. Never put it inside the legacy modal window.
pub(crate) fn show_root(app: &mut LightcraftApp, root: &mut egui::Ui, blocked: bool) {
    if save_effect::active(app, root.ctx()) {
        return save_effect::show_root(app, root, blocked);
    }
    if !matches!(app.ui.dialog, Some(Dialog::About)) {
        return show(app, root.ctx(), blocked);
    }
    // A higher-priority native prompt owns this pass, including its dismissal event.
    if blocked || crate::panels::library_problem::blocks_dialog_input(app) {
        return;
    }
    use craft::ui::patterns::form_dialogs::{AboutDialog, AboutInfo, AboutLink};
    let info = AboutInfo::new("LightCraft", crate::i18n::tr("A clean-room, pure-Rust photo library and raw developer."), env!("CARGO_PKG_VERSION"))
        .community(AboutLink::new(crate::i18n::tr("Join the ArtCraft Discord"), crate::links::DISCORD))
        .links([
            AboutLink::new(crate::i18n::tr("LightCraft website"), crate::links::APP_PAGE),
            AboutLink::new(crate::i18n::tr("Source code on GitHub"), crate::links::GITHUB),
            AboutLink::new(crate::i18n::tr("ArtCraft — more creative apps"), crate::links::WEBSITE),
        ])
        .built_with(crate::i18n::tr_format!("MIT OR Apache-2.0. Fonts: {} (OFL). Icons: original.", crate::theme::font_credits(app.chinese_font.is_some())));
    let dialog = AboutDialog::new(&info, credits::framework_credits());
    let ctx = root.ctx().clone();
    let (out, record) = craft::ui::runtime::inspect::record(root, egui::Id::unique("about-app-contract"), |ui| {
        craft::ui::runtime::overlay::stack_place(&ctx, STACK_DIALOG, egui::Id::unique("lightcraft.dialog"), || dialog.show(ui))
    });
    // The framework dialog's window and its OK under the names every dialog has, with their real responses.
    if !root.is_sizing_pass() && !craft::ui::runtime::measure::active(root.ctx()) {
        crate::widgets::register_response("dialog:window", out.response.rect, &out.response);
        let ok = [crate::i18n::tr("OK"), crate::i18n::tr("Close")];
        if let Some(element) = record.iter().flat_map(|record| record.elements.iter()).find(|element| {
            element.component == "Button" && element.part.is_none() && element.facts.as_ref().and_then(|facts| facts.name.as_deref()).is_some_and(|name| ok.contains(&name))
        }) && let Some(response) = root.ctx().read_response(element.id)
        {
            crate::widgets::register_response("button:dialogOk", element.rect, &response);
        }
    }
    if let Some(url) = out.link {
        let _ = crate::links::open(app, &url);
    }
    if out.closed.is_some() && matches!(app.ui.dialog, Some(Dialog::About)) {
        app.ui.dialog = None;
    }
}

/// The body of the open dialog: what each kind of dialog shows between its title and its buttons. `confirm` is set
/// by a dialog that is confirmed from its body (a text field's Return).
fn dialog_content(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, body_max: f32, confirm: &mut bool, close: &mut bool, dlg: &mut Dialog) {
    match dlg {
        Dialog::AutoStack { .. } => auto_stack_body(app, ui, t, dlg),
        Dialog::Cull { .. } => cull_body(app, ui, t, dlg),
        Dialog::WhatsNew => whats_new_body(ui, t, dlg),
        Dialog::Welcome { page } => welcome_body(app, ui, t, page, close),
        Dialog::DenoiseModel { info, accepted } => crate::panels::denoise::model_dialog(app, ui, t, info, accepted),
        Dialog::FaceModel { info, accepted, .. } => crate::panels::faces::model_dialog(app, ui, t, info, accepted),
        Dialog::LightroomResult { .. } => lightroom_result_body(ui, dlg),
        Dialog::SystemInfo { .. } => system_info_body(ui, t, dlg),
        Dialog::AllMetadata { .. } => all_metadata_body(ui, t, dlg),
        Dialog::SmartRules { .. } => smart_rules_body(app, ui, t, close, dlg),
        Dialog::NewSmartAlbum { .. } => new_smart_album_body(app, ui, t, confirm, dlg),
        Dialog::CaptureTime { .. } => capture_time_body(app, ui, t, dlg),
        Dialog::LabelNames { .. } => { *close |= label_names_body(app, ui, t, dlg); },
        Dialog::Rename { .. } => rename_body(app, ui, t, dlg),
        Dialog::RenamePerson { .. } => rename_person_body(app, ui, t, confirm, dlg),
        Dialog::RenameKeyword { .. } => rename_keyword_body(app, ui, t, confirm, dlg),
        Dialog::MergeKeywords { .. } => merge_keywords_body(app, ui, t, confirm, dlg),
        Dialog::TextPrompt { value, hint, .. } => {
            if dialog_field(ui, "field:prompt", value, hint) {
                *confirm = true;
            }
        }
        Dialog::NewAlbum { .. } => new_album_body(app, ui, confirm, dlg),
        Dialog::RenameAlbum { name, .. } => {
            if dialog_field(ui, "field:albumName", name, "Name") {
                *confirm = true;
            }
        }
        Dialog::CreatePreset { name, group, groups } => {
            if dialog_field(ui, "field:presetName", name, "Preset name") {
                *confirm = true;
            }
            crate::text_field::TextField::singleline("field:presetGroup", group)
                .hint(crate::i18n::tr("Group"))
                .label(crate::i18n::tr("Group"))
                .width(f32::INFINITY)
                .show(ui);
            ui.label(egui::RichText::new(crate::i18n::tr("Settings to include")).color(t.text_dim));
            group_checklist(ui, "presetInclude", groups);
        }
        Dialog::CopySettings { groups } => group_checklist(ui, "copyGroup", groups),
        Dialog::PasteSettings { .. } => paste_settings_body(app, ui, t, dlg),
        Dialog::SlideshowExport { settings } => slideshow_export_body(app, ui, settings),
        Dialog::ContactSheet { .. } => contact_sheet_body(app, ui, dlg),
        Dialog::Export { .. } => export_body(app, ui, t, dlg),
        Dialog::Merge { opts } => crate::merge::body(app, ui, opts),
        Dialog::Import { opts } => crate::import::body(app, ui, opts),
        // (Settings is never taller than `body_max`, so the scroll area round it never
        // scrolls: its tab bar stays put and its pages scroll by themselves)
        Dialog::Settings { tab } => crate::panels::settings::window(app, ui, tab, body_max),
        Dialog::SamModel { error, .. } => sam_model_body(app, ui, error.as_deref()),
        Dialog::KeywordTag { .. } => keyword_tag_body(app, ui, t, confirm, dlg),
        // Edit Keyword Set owns its framework dialog above, not this legacy body dispatcher.
        Dialog::KeywordSet { .. } => {}
        Dialog::MoveKeyword { .. } => move_keyword_body(ui, t, dlg),
        Dialog::DeleteKeyword { .. } => delete_keyword_body(ui, t, dlg),
        Dialog::SaveEffect { name, group, fields, amount, brush } => {
            if crate::panels::mask_effects::save_body(app, ui, name, group, fields, amount, brush) {
                *confirm = true;
            }
        }
        Dialog::ManageEffects { search, selected, rename, confirm_delete, new_group } => {
            crate::panels::mask_effects::manage_body(app, ui, search, selected, rename, confirm_delete, new_group)
        }
        Dialog::DeleteEffects { .. } => delete_effects_body(ui, t, dlg),
        Dialog::ConfirmDelete { count } => {
            let what =
                if *count == 1 { crate::i18n::tr("this photo").to_string() } else { crate::i18n::tr_format!("these {count} photos", count = count) };
            ui.label(crate::i18n::tr_format!("Move {what} to Recently Deleted?", what = what));
            ui.label(egui::RichText::new(crate::i18n::tr("They can be restored from Recently Deleted until it is emptied.")).color(t.text_dim));
        }
        Dialog::MovePhotos { .. } => move_photos_body(ui, t, dlg),
        d @ Dialog::SynchronizeFolder { .. } => crate::sync::body(app, ui, d),
        Dialog::LightroomCatalog { .. } => lightroom_catalog_body(app, ui, t, dlg),
        d @ Dialog::LightroomDestination { .. } => crate::lightroom_destination::body(app, ui, d),
        Dialog::LensDatabase { .. } => crate::lens_db::offer(ui),
        Dialog::RemoveFolder { .. } => remove_folder_body(ui, t, dlg),
        Dialog::About => unreachable!("About is mounted on the root UI"),
        Dialog::Shortcuts => crate::panels::keymap::body(app, ui, t),
        Dialog::ViewOptions => crate::panels::settings::view_options(app, ui, t),
        Dialog::Print => *close |= crate::print_view::body(app, ui, t),
    }
}

#[cfg(test)]
#[path = "dialogs/smart_rules_flow_tests.rs"]
mod smart_rules_flow_tests;

#[cfg(test)]
#[path = "dialogs/new_album_flow_tests.rs"]
mod new_album_port_tests;

#[cfg(test)]
#[path = "dialogs/new_album_sdk_route_tests.rs"]
mod new_album_sdk_route_tests;

#[cfg(test)]
#[path = "dialogs/label_names_flow_tests.rs"]
mod label_names_port_tests;
#[cfg(test)]
#[path = "dialogs/keyword_tag_options_flow_tests.rs"]
mod keyword_tag_options_flow_tests;
