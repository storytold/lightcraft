use super::{HOLD, LightcraftApp, PhotoId, Rect, Tokens, fold_header, json, padded, register, text_button, vec2};

pub(super) fn keywords(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId) {
    // (the box is the selection's: the active photo only says that there is one)
    if app.session.catalog.photo(id).is_none() {
        return;
    }
    let top = ui.cursor().top();
    if fold_header(app, ui, "keywording", "Keywords") {
        keywording(app, ui);
    } else if ui.data(|d| d.get_temp::<EnterEdit>(enter_id())).is_some_and(|e| !e.refused) {
        // a folded box applies the Enter Keywords edit it held, as leaving the box does
        let selection = app.session.selected();
        leave_enter_keywords(app, ui, &selection);
    }
    hold_list_in_place(ui, top);
    crate::panels::keyword_list::show(app, ui);
}

/// The Keywording box: the field that adds keywords, the selection's keywords, the keyword set,
/// the painter and the suggestions.
fn keywording(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    padded(ui, |ui| {
        let selection = app.session.selected();
        let have = crate::panels::keywording::add_field(app, ui, &selection);
        ui.add_space(8.0);
        let view = app.ui.keywording_view;
        crate::panels::keywording::view_switch(app, ui);
        if view == crate::state::KeywordingView::Enter && app.ui.keywording_view != view {
            leave_enter_keywords(app, ui, &selection);
        }
        ui.add_space(4.0);
        crate::panels::keywording::scope(app, ui, &selection);
        match app.ui.keywording_view {
            crate::state::KeywordingView::Enter => enter_keywords(app, ui, &selection),
            crate::state::KeywordingView::Keywords => crate::panels::keywording::chip_row(app, ui, &selection),
            _ => crate::panels::keywording::names_row(app, ui),
        }
        ui.add_space(10.0);
        keyword_set(app, ui);
        ui.add_space(8.0);
        // the painter: click photos in the grid to give them (or take away) a keyword
        ui.horizontal(|ui| {
            let painting = app.ui.keyword_painter.clone();
            let mut k = app.ui.keyword_painter_text.clone();
            // (fixed while painting: Stop first)
            let field = ui.add_enabled_ui(painting.is_none(), |ui| crate::panels::keywording::painter_field(ui, &mut k)).inner;
            app.ui.keyword_painter_text = k.clone();
            let label = if painting.is_some() { "Stop" } else { "Paint" };
            let returned = field && painting.is_none() && !k.trim().is_empty();
            if text_button(ui, "keywordPaint", label, painting.is_some())
                .on_hover_text(crate::i18n::tr("Click photos in the grid to toggle the keyword; Esc stops"))
                .clicked()
                || returned
            {
                let _ = app.run("tool.keywordPainter", json!({"keyword": if painting.is_some() { serde_json::Value::Null } else { json!(k) }}));
            }
        });
        ui.add_space(10.0);
        // suggestions: keywords used together with the selection's keywords, else the most used
        // ones (what matches the text being typed is offered under the box itself)
        let suggestions = (*app.caches.suggestions(&app.session.catalog, &have, "", 12)).clone();
        if !suggestions.is_empty() {
            ui.label(egui::RichText::new(crate::i18n::tr("Suggestions")).color(t.text_dim));
            // grouped: the keyword's own name, its whole path in the tooltip
            let grouped = app.ui.settings.keyword_chips == crate::state::KeywordChips::Grouped;
            let names = if grouped { crate::panels::keywording::suggestion_names(&suggestions) } else { Vec::new() };
            let generation = app.session.library_generation();
            let active = app.session.active();
            ui.horizontal_wrapped(|ui| {
                if grouped {
                    ui.spacing_mut().item_spacing = vec2(6.0, 4.0);
                }
                for (i, k) in suggestions.into_iter().enumerate() {
                    let whole = k.replace('|', " › ");
                    let name = names.get(i).unwrap_or(&whole);
                    // A different library, selection or active photo is a different input owner.
                    let owner = egui::Id::unique(("lc-keyword-suggestion", generation, active, &selection, &k));
                    if suggestion_button(ui, owner, &k, name, &whole, grouped)
                        && app.session.library_generation() == generation
                        && app.session.active() == active
                        && app.session.selected() == selection
                    {
                        let _ = app.run("photo.setMeta", json!({"addKeywords": [k]}));
                    }
                }
            });
        }
    });
}

/// An edit of the Enter Keywords box: its text, and the selection it was started on. It is made
/// only once the box is edited, and the selection-sized parts are shared, so the box's data is not
/// copied on every frame.
#[derive(Clone)]
struct EnterEdit {
    text: String,
    /// The selection's keywords when the edit started (shared with `Caches::keyword_chips`).
    start: std::sync::Arc<Vec<crate::panels::keywording::Chip>>,
    /// The photos the edit is for (`photo.setMeta` `ids`).
    ids: std::sync::Arc<[u64]>,
    /// The selection the edit was started on (`key_of`).
    selection: u64,
    /// [`LightcraftApp::selection_changes`] when the edit started: the same photos selected again
    /// after others are another selection.
    changes: u64,
    /// The library the edit was started in ([`lightcraft_engine::Session::library_generation`]):
    /// photo ids are a library's own, so the same ids in another library are other photos.
    library: u64,
    /// The last try to apply it was refused: the text stays to be put right.
    refused: bool,
}

impl EnterEdit {
    fn new(
        text: String,
        chips: std::sync::Arc<Vec<crate::panels::keywording::Chip>>,
        selection: &[PhotoId],
        key: u64,
        changes: u64,
        library: u64,
    ) -> Self {
        let mut seen = std::collections::HashSet::new();
        EnterEdit {
            text,
            start: chips,
            ids: selection.iter().filter(|id| seen.insert(**id)).map(|id| id.0).collect(),
            selection: key,
            changes,
            library,
            refused: false,
        }
    }

    /// Whether the edit was started on `selection` (`key_of`) in the open library, with no other
    /// selection since.
    fn is_for(&self, app: &mut LightcraftApp, selection: u64) -> bool {
        self.selection == selection && self.changes == app.selection_changes() && self.library == app.session.library_generation()
    }
}

/// The Enter Keywords box's field.
const ENTER_FIELD: &str = "field:enterKeywords";

/// Where the Enter Keywords box keeps its edit between frames.
fn enter_id() -> egui::Id {
    egui::Id::unique("kw-enter")
}

/// Applies the Enter Keywords `edit` to the photos it was started on, as one undo step. Returns
/// whether it was applied (an edit that changes nothing is, and so is one saved in memory but not
/// written to disk); a refusal or a failed write is said.
fn apply_enter(app: &mut LightcraftApp, ctx: &egui::Context, edit: &EnterEdit) -> bool {
    match crate::panels::keywording::entered(&app.session.catalog, &edit.start, &edit.text) {
        Ok(change) if change.is_empty() => true,
        Ok(change) => match app.run_typed("photo.setMeta", json!({"ids": &*edit.ids, "addKeywords": change.add, "removeKeywords": change.remove})) {
            Ok(_) => true,
            Err(e) => {
                let applied = e.kind == crate::ErrorKind::NotSaved;
                app.toast_error(ctx, e.message);
                applied
            }
        },
        Err(name) => {
            app.toast_error(ctx, crate::i18n::tr_format!("A keyword name can't end with an asterisk: “{name}”", name = name));
            false
        }
    }
}

/// Another view chosen while the Enter Keywords box holds an edit: the edit is applied, as leaving
/// the box applies it, and the box gives up the cursor. An edit of another selection, or of another
/// library, is given up. A refused edit is kept, marked refused, and comes back when Enter Keywords
/// is chosen again.
fn leave_enter_keywords(app: &mut LightcraftApp, ui: &mut egui::Ui, selection: &[PhotoId]) {
    let id = enter_id();
    crate::text_field::blur(ui.ctx(), ENTER_FIELD);
    let Some(mut edit) = ui.data(|d| d.get_temp::<EnterEdit>(id)) else { return };
    if edit.is_for(app, crate::key_of(selection)) && !apply_enter(app, ui.ctx(), &edit) {
        edit.refused = true;
        ui.data_mut(|d| d.insert_temp(id, edit));
    } else {
        ui.data_mut(|d| d.remove::<EnterEdit>(id));
    }
}

/// Gives up an Enter Keywords edit, applied or not, and the cursor in its box.
fn give_up_enter_keywords(ctx: &egui::Context) {
    crate::text_field::blur(ctx, ENTER_FIELD);
    ctx.data_mut(|d| d.remove::<EnterEdit>(enter_id()));
}

/// Gives up an Enter Keywords edit once the selection or the library differs from the one it was
/// started on, whether its box is drawn or not: the same photos selected again later do not bring
/// it back.
pub(crate) fn forget_stale_enter_keywords(app: &mut LightcraftApp, ctx: &egui::Context) {
    let Some(edit) = ctx.data(|d| d.get_temp::<EnterEdit>(enter_id())) else { return };
    let selection = crate::key_of(app.session.selected());
    if !edit.is_for(app, selection) {
        give_up_enter_keywords(ctx);
    }
}

/// Enter Keywords: the selection's keywords as text, separated by commas, one only some of the
/// photos have marked with an asterisk. Return, leaving the box, choosing another view or folding
/// the box applies the edit to the photos selected when it started, as one undo step; Esc gives it
/// up. An edit whose box was hidden with its panel is applied when the box is next drawn. Another
/// selection while the box is being edited, or another library opened, gives the edit up too. A
/// refused edit keeps its text and says why.
fn enter_keywords(app: &mut LightcraftApp, ui: &mut egui::Ui, selection: &[PhotoId]) {
    let id = enter_id();
    let key = crate::key_of(selection);
    let changes = app.selection_changes();
    let library = app.session.library_generation();
    let chips = app.caches.keyword_chips(&app.session.catalog, selection);
    let fid = crate::text_field::id(ENTER_FIELD);
    // (was the box drawn in the frame before? a panel that hid it for a frame left its field's cursor behind, and
    // the framework keeps that focus record until the field is drawn again)
    let frame = ui.ctx().cumulative_frame_nr();
    let drawn_key = id.with("drawn-frame");
    let drawn_before = ui.data(|d| d.get_temp::<u64>(drawn_key)).is_some_and(|last| frame <= last.saturating_add(1));
    ui.data_mut(|d| d.insert_temp(drawn_key, frame));
    let kept: Option<EnterEdit> = ui.data(|d| d.get_temp(id));
    if !drawn_before && kept.is_some() {
        crate::text_field::blur_id(ui.ctx(), fid);
    }
    let focused = drawn_before && crate::text_field::has_focus_id(ui.ctx(), fid);
    if kept.is_some() {
        eprintln!("DBG enter kept refused={} drawn_before={drawn_before} focused={focused} frame={frame}", kept.as_ref().is_some_and(|e| e.refused));
    }
    // (an edit of another selection or library is given up, never applied to this one)
    if kept.as_ref().is_some_and(|e| !e.is_for(app, key)) && focused {
        crate::text_field::blur_id(ui.ctx(), fid);
    }
    let kept = kept.filter(|e| e.is_for(app, key));
    // (an edit kept without the cursor lost it while the box was not drawn, its panel hidden)
    let left_undrawn = !focused && kept.as_ref().is_some_and(|e| !e.refused);
    // (no edit held: only the shown text is made, and the edit itself once the box is edited)
    let (kept, mut text) = match kept {
        Some(mut e) => {
            let text = std::mem::take(&mut e.text);
            (Some(e), text)
        }
        None => (None, crate::panels::keywording::entered_text(&chips)),
    };
    // Return applies the edit (Shift+Return or a pasted line break separates keywords as a comma does)
    let returned = focused
        && ui.input_mut(|i| {
            let plain =
                |e: &egui::Event| matches!(e, egui::Event::Key { key: egui::Key::Enter, pressed: true, modifiers, .. } if modifiers.is_none());
            let found = i.events.iter().any(plain);
            i.events.retain(|e| !plain(e));
            found
        });
    let r = crate::text_field::TextField::multiline(ENTER_FIELD, &mut text)
        .hint(crate::i18n::tr("Keywords, separated by commas"))
        .label(crate::i18n::tr("Enter Keywords"))
        .rows(2)
        .width(f32::INFINITY)
        .show(ui);
    r.response.clone().on_hover_text(crate::i18n::tr(
        "Return applies the edit to the selected photos; Esc gives it up. An asterisk marks a keyword only some of them have.",
    ));
    if r.cancelled() {
        ui.data_mut(|d| d.remove::<EnterEdit>(id));
        return;
    }
    // (applied as leaving the box applies it)
    let apply = returned || r.committed() || (left_undrawn && !r.editing);
    if !apply && !r.editing && !returned && !kept.as_ref().is_some_and(|e| e.refused) {
        ui.data_mut(|d| d.remove::<EnterEdit>(id));
        return;
    }
    let mut edit = match kept {
        Some(mut e) => {
            e.text = text;
            e
        }
        None => EnterEdit::new(text, chips, selection, key, changes, library),
    };
    let mut done = false;
    if apply {
        let applied = apply_enter(app, ui.ctx(), &edit);
        edit.refused = !applied;
        done = applied;
        if applied && returned {
            crate::text_field::blur_id(ui.ctx(), fid);
        }
    }
    if !done && (r.editing || returned || edit.refused) {
        ui.data_mut(|d| d.insert_temp(id, edit));
    } else {
        ui.data_mut(|d| d.remove::<EnterEdit>(id));
    }
}

/// Only the suggestion leaf changes: the caller keeps its full-path command and selection.
fn suggestion_button(ui: &mut egui::Ui, owner: egui::Id, keyword: &str, name: &str, whole: &str, grouped: bool) -> bool {
    use craft::ui::contract::{Props as _, Show as _};
    let real = !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx());
    let enabled = ui.is_enabled();
    // The SDK owns the press lifecycle, keyboard activation and focus. Do not bridge keys or
    // interpret egui's cached click snapshot, including a press from a disabled/measurement pass.
    // Cancel this leaf's press lifecycle during measurement too, not merely its returned
    // action: a measuring press must never arm the same owner for a later real release.
    let shown = craft::ui::buttons::Button::new(format!("+ {name}")).id(owner).enabled(real && enabled).show(ui);
    if real {
        crate::widgets::register_response(format!("kwSuggest:{keyword}"), shown.response.rect, &shown.response);
    }
    let eligible = real && enabled && ui.is_visible() && ui.clip_rect().intersects(shown.response.rect);
    let clicked = eligible && shown.clicked();
    if grouped {
        let _ = shown.response.on_hover_text(whole);
    }
    clicked
}

/// Keeps the Keyword List where it is on screen when the Keywords box above it (starting at
/// `top`) grows or shrinks: a keyword ticked or unticked in the list, another selection. Without it
/// a long tree slides away from under the pointer by a row of chips each time. It holds only while
/// the box's top is scrolled out of sight (at the top of the panel the box is watched growing) and
/// while the change does not come from the box itself (the pointer on it, or typing in its field).
fn hold_list_in_place(ui: &mut egui::Ui, top: f32) {
    let height = ui.cursor().top() - top;
    let key = egui::Id::unique("kw-box-height");
    let pass = ui.ctx().cumulative_pass_nr();
    // (the height drawn on the pass before: one from an earlier visit to the panel says nothing)
    let before = ui.data(|d| d.get_temp::<(f32, u64)>(key)).filter(|&(_, drawn)| drawn.checked_add(1) == Some(pass)).map(|(h, _)| h);
    ui.data_mut(|d| d.insert_temp(key, (height, pass)));
    let Some(before) = before else { return };
    let grew = height - before;
    if !grew.is_finite() || grew.abs() < 0.5 || top >= ui.clip_rect().top() - 0.5 {
        return;
    }
    // (only the part of it in sight: the pointer on the top bar above a half-hidden box isn't on it)
    let old_box = Rect::from_x_y_ranges(ui.max_rect().x_range(), top..=top + before).intersect(ui.clip_rect());
    let on_box = ui.input(|i| i.pointer.latest_pos()).is_some_and(|p| old_box.contains(p));
    let typing = crate::panels::keywording::field_focused(ui.ctx()) || crate::text_field::focused(ui.ctx(), ENTER_FIELD);
    if on_box || typing {
        return;
    }
    ui.data_mut(|d| d.insert_temp(egui::Id::unique(HOLD), (grew, pass)));
    // (drawn again at once at the new offset, so the list never shows a frame out of place)
    ui.ctx().request_discard("the Keyword List holds its place");
}

/// The keyword set: pick a set, then nine buttons (⌥1–⌥9) that toggle its keywords on the
/// selected photos; "Save as Set…" keeps the current nine under a name.
fn keyword_set(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    // on: every selected photo has it (⌥1–⌥9 then take it off them); partly on: only some do
    let selection = app.session.selected();
    let chips = app.caches.keyword_chips(&app.session.catalog, &selection);
    let t = Tokens::get(ui.ctx());
    let sets = lightcraft_engine::cmd::keywords::keyword_sets_json(&app.session);
    let current = sets["current"].as_str().unwrap_or_default().to_string();
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(crate::i18n::tr("Keyword Set")).color(t.text_dim));
        let recent = lightcraft_engine::cmd::keywords::RECENT;
        let face = crate::i18n::builtin_label(&current, current == recent).to_owned();
        crate::widgets::dropdown_menu(ui, "keywordSetCombo", crate::i18n::tr("Keyword Set"), &face, None, |ui| {
            for set in sets["sets"].as_array().into_iter().flatten() {
                let name = set["name"].as_str().unwrap_or_default();
                if crate::widgets::menu_row(ui, ("keywordSet", name), None, crate::i18n::builtin_label(name, name == recent), None, name == current, true) {
                    let _ = app.run("keyword.useSet", json!({"name": name}));
                }
            }
            crate::widgets::menu_divider(ui);
            // Edit Set…: the current nine, slot by slot (named, Recent Keywords becomes a set)
            if crate::widgets::menu_row(ui, "keywordSetMenu:edit", Some("keywordSetMenu:edit"), crate::i18n::tr("Edit Set…"), None, false, true) {
                let mut slots: Vec<String> =
                    sets["keywords"].as_array().into_iter().flatten().filter_map(|k| k.as_str().map(str::to_string)).collect();
                slots.resize(9, String::new());
                let named = current != recent;
                app.ui.dialog = Some(crate::state::Dialog::KeywordSet {
                    replaces: named.then(|| current.clone()),
                    name: if named { current.clone() } else { String::new() },
                    slots,
                    as_new: false,
                });
            }
            if crate::widgets::menu_row(ui, "keywordSetMenu:save", None, crate::i18n::tr("Save Current Keywords as Set…"), None, false, true) {
                app.ui.dialog = Some(crate::state::Dialog::TextPrompt {
                    title: "Save Keyword Set".into(),
                    hint: "Set name".into(),
                    value: String::new(),
                    command: "keyword.saveSet".into(),
                    params: json!({}),
                    key: "name".into(),
                });
            }
            if current != recent
                && crate::widgets::menu_row(
                    ui,
                    "keywordSetMenu:delete",
                    None,
                    &crate::i18n::tr_format!("Delete “{current}”", current = current),
                    None,
                    false,
                    true,
                )
            {
                let _ = app.run("keyword.deleteSet", json!({"name": current}));
            }
        });
    });
    let kws: Vec<String> = sets["keywords"].as_array().into_iter().flatten().filter_map(|k| k.as_str().map(str::to_string)).collect();
    if kws.is_empty() {
        ui.label(egui::RichText::new(crate::i18n::tr("Keywords you add appear here; ⌥1–⌥9 apply them.")).color(t.text_dim));
        return;
    }
    let bw = ((ui.available_width() - 8.0) / 3.0).floor().max(40.0);
    egui::Grid::new("kw-set-grid").num_columns(3).spacing([4.0, 4.0]).show(ui, |ui| {
        for (i, k) in kws.iter().enumerate() {
            // an empty slot: an idle button, so the others keep their ⌥ keys
            if k.is_empty() {
                let r = ui.add_enabled_ui(false, |ui| ui.add_sized([bw, 22.0], egui::Button::new(""))).inner;
                register(ui, format!("kwSetEmpty:{}", i + 1), r.rect);
                if i % 3 == 2 {
                    ui.end_row();
                }
                continue;
            }
            let chip = chips.iter().find(|c| lightcraft_catalog::keywords::same(&c.path, &lightcraft_catalog::keywords::clean(k)));
            let on = chip.is_some_and(|c| c.on_all());
            let some = chip.is_some() && !on;
            let short = format!("{}{}", k.rsplit('|').next().unwrap_or(k), if some { " *" } else { "" });
            let r = ui
                .add_sized(
                    [bw, 22.0],
                    egui::Button::new(egui::RichText::new(short).size(11.5).color(if on { t.text } else { t.text_label })).selected(on).truncate(),
                )
                .on_hover_text(format!("{} — ⌥{}", k.replace('|', " › "), i + 1));
            register(ui, format!("kwSet:{}", i + 1), r.rect);
            if on {
                register(ui, format!("kwSetOn:{}", i + 1), r.rect);
            } else if some {
                register(ui, format!("kwSetSome:{}", i + 1), r.rect);
            }
            if r.clicked() {
                let _ = app.run("keyword.toggleFromSet", json!({"index": i + 1}));
            }
            if i % 3 == 2 {
                ui.end_row();
            }
        }
    });
}

#[cfg(test)]
#[path = "keyword_suggestions_flow_tests.rs"]
mod keyword_suggestions_flow_tests;
