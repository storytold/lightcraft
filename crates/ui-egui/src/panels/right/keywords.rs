use super::{HOLD, LightcraftApp, PhotoId, Rect, Tokens, fold_header, json, padded, register, text_button, vec2};

pub(super) fn keywords(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId) {
    // (the box is the selection's: the active photo only says that there is one)
    if app.session.catalog.photo(id).is_none() {
        return;
    }
    let top = ui.cursor().top();
    if fold_header(app, ui, "keywording", "Keywords") {
        keywording(app, ui);
    }
    hold_list_in_place(ui, top);
    crate::panels::keyword_list::show(app, ui);
}

/// The Keywording box: the field that adds keywords, the selection's keywords, the keyword set,
/// the painter and the suggestions.
fn keywording(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    padded(ui, |ui| {
        let kid = egui::Id::unique("kw-input");
        let mut text = ui.data_mut(|d| d.get_temp::<String>(kid).unwrap_or_default());
        // the keywords every selected photo has: those only some have are still offered and suggested
        let selection = app.session.selected();
        let have: Vec<String> =
            app.caches.keyword_chips(&app.session.catalog, &selection).iter().filter(|c| c.on_all()).map(|c| c.path.clone()).collect();
        // while the box has the cursor it offers the library's keywords for what is being typed,
        // right under itself; it stays for a click on one of them, which takes the cursor away
        let field = crate::text_field::id("field:keyword");
        let oid = egui::Id::unique("kw-offers");
        let pass = ui.ctx().cumulative_pass_nr();
        // (remembered from the last frame: the text the highlight was for and the keyword it was
        // on, where the offers were drawn, and whether the box was in sight)
        let (typed_then, at_then, drawn, drawn_pass, hidden): (String, Option<String>, Rect, u64, bool) =
            ui.data(|d| d.get_temp(oid)).unwrap_or((String::new(), None, Rect::NOTHING, 0, false));
        let last_frame = drawn_pass.checked_add(1) == Some(pass);
        let clicking = last_frame
            && ui.input(|i| (i.pointer.any_down() || i.pointer.any_released()) && i.pointer.interact_pos().is_some_and(|p| drawn.contains(p)));
        let typed = crate::panels::keywording::typing(&text).to_string();
        // (none while the box is scrolled out of the panel: they would float over what is there)
        let offers = ((ui.memory(|m| m.has_focus(field)) || clicking) && !(last_frame && hidden))
            .then(|| app.caches.completions(&app.session.catalog, &have, &typed))
            .filter(|o| !o.keywords.is_empty() || o.create.is_some());
        // ↓ / ↑ move through the offers (none is highlighted at first, and ↑ from the first goes
        // back to none: Return then gives what was typed, as without offers); Return gives the
        // highlighted one. The highlight is on a keyword, not a line: it stays with it when the
        // library changes under it, and goes when the keyword does. With Shift or Alt the keys
        // are the text's own.
        let mut at = at_then.filter(|k| typed_then == typed && offers.as_ref().is_some_and(|o| o.keywords.contains(k)));
        let mut picked = None;
        if let Some(o) = offers.as_ref().filter(|o| !o.keywords.is_empty()) {
            let pressed = |ui: &mut egui::Ui, key: egui::Key| {
                ui.input_mut(|i| {
                    let plain =
                        |e: &egui::Event| matches!(e, egui::Event::Key { key: k, pressed: true, modifiers, .. } if *k == key && modifiers.is_none());
                    let found = i.events.iter().any(plain);
                    i.events.retain(|e| !plain(e));
                    found
                })
            };
            let line = at.as_ref().and_then(|k| o.keywords.iter().position(|x| x == k));
            if pressed(ui, egui::Key::ArrowDown) {
                let next = line.map_or(0, |a| a.saturating_add(1)).min(o.keywords.len().saturating_sub(1));
                at = o.keywords.get(next).cloned();
            } else if pressed(ui, egui::Key::ArrowUp) {
                at = line.and_then(|a| a.checked_sub(1)).and_then(|a| o.keywords.get(a).cloned());
            }
            if let Some(k) = &at
                && pressed(ui, egui::Key::Enter)
            {
                picked = Some(crate::panels::keywording::Picked::Keyword(k.clone()));
            }
        }
        // (after an offer was taken the box starts a new edit, empty: Esc has nothing to bring back)
        // (asked for on the frame before this one only: a box that wasn't drawn then, its panel
        // switched away, doesn't claim the cursor whenever it is next drawn)
        let bid = egui::Id::unique("kw-begin");
        let begin = ui.data_mut(|d| d.remove_temp::<u64>(bid)).is_some_and(|asked| asked.checked_add(1) == Some(pass));
        // Add Keywords (⌘K) asks for the cursor too, and for the box to be scrolled into sight
        let asked = app.ui.focus_keywords.take().is_some_and(|at| pass <= at.saturating_add(crate::state::FOCUS_KEYWORDS_PASSES));
        let r = crate::text_field::TextField::singleline("field:keyword", &mut text)
            .hint(crate::i18n::tr("Add keyword"))
            .width(f32::INFINITY)
            .begin_edit(begin || asked)
            .show(ui);
        if asked {
            r.response.scroll_to_me(None);
        }
        let in_sight = ui.clip_rect().contains(r.response.rect.center());
        let mut drawn = Rect::NOTHING;
        // (what isn't drawn can't be taken: Return on the frame the box scrolls out gives nothing)
        if !in_sight {
            picked = None;
        }
        if offers.is_none() {
            // (not over the offers)
            r.response.clone().on_hover_text(crate::i18n::tr("Separate several keywords with commas. Return gives them to the selected photos."));
        }
        if let Some(o) = offers.as_ref().filter(|_| in_sight) {
            let (clicked, rect) = crate::panels::keywording::offers_below(ui, r.response.rect, o, at.as_deref());
            drawn = rect;
            picked = picked.or(clicked);
        }
        ui.data_mut(|d| d.insert_temp(oid, (typed.clone(), at, drawn, pass, !in_sight)));
        // Return gives the keywords to the selected photos; Esc gives back what was there before
        // this edit (the shared field does)
        let returned = r.ending == Some(crate::text_field::Ending::Return) && !text.trim().is_empty();
        // a new name goes inside the default parent (Put New Keywords Inside This Keyword)
        let as_typed = |app: &LightcraftApp, text: &str| -> Vec<String> {
            text.split(',').map(|s| app.session.catalog.typed_keyword(s)).filter(|s| !s.is_empty()).collect()
        };
        let adding: Option<Vec<String>> = match picked {
            // an offer: it, and the keywords typed before the last comma
            Some(crate::panels::keywording::Picked::Keyword(k)) => {
                let mut kws = crate::panels::keywording::typed_before(&app.session.catalog, &text);
                kws.push(k);
                Some(kws)
            }
            Some(crate::panels::keywording::Picked::Typed) => Some(as_typed(app, &text)),
            None if returned => Some(as_typed(app, &text)),
            None => None,
        };
        if let Some(kws) = adding {
            // (refused, the text stays to be put right, and the refusal is said)
            match app.run("photo.setMeta", json!({"addKeywords": kws})) {
                Ok(_) => {
                    text.clear();
                    // an offer taken leaves the cursor in the box, for the next keyword
                    if !returned {
                        ui.data_mut(|d| d.insert_temp(bid, pass));
                    }
                }
                Err(e) => app.toast_error(ui.ctx(), e),
            }
        }
        ui.data_mut(|d| d.insert_temp(kid, text));
        ui.add_space(8.0);
        crate::panels::keywording::view_switch(app, ui);
        ui.add_space(4.0);
        crate::panels::keywording::scope(app, ui, &selection);
        if app.ui.keywording_view == crate::state::KeywordingView::Keywords {
            crate::panels::keywording::chip_row(app, ui, &selection);
        } else {
            crate::panels::keywording::names_row(app, ui);
        }
        ui.add_space(10.0);
        keyword_set(app, ui);
        ui.add_space(8.0);
        // the painter: click photos in the grid to give them (or take away) a keyword
        ui.horizontal(|ui| {
            let pid = egui::Id::unique("kw-painter");
            let painting = app.ui.keyword_painter.clone();
            let mut k: String = ui.data(|d| d.get_temp(pid)).unwrap_or_else(|| painting.clone().unwrap_or_default());
            // (fixed while painting: Stop first)
            let field = ui
                .add_enabled_ui(painting.is_none(), |ui| {
                    crate::text_field::TextField::singleline("field:keywordPainter", &mut k)
                        .hint(crate::i18n::tr("Keyword to paint"))
                        .width(130.0)
                        .show(ui)
                })
                .inner;
            ui.data_mut(|d| d.insert_temp(pid, k.clone()));
            let label = if painting.is_some() { "Stop" } else { "Paint" };
            let returned = field.ending == Some(crate::text_field::Ending::Return) && painting.is_none() && !k.trim().is_empty();
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
            ui.horizontal_wrapped(|ui| {
                if grouped {
                    ui.spacing_mut().item_spacing = vec2(6.0, 4.0);
                }
                for (i, k) in suggestions.into_iter().enumerate() {
                    let whole = k.replace('|', " › ");
                    let name = names.get(i).unwrap_or(&whole);
                    let r = ui.add(egui::Button::new(egui::RichText::new(format!("+ {name}")).color(t.text_label)).corner_radius(10.0));
                    let r = if grouped { r.on_hover_text(&whole) } else { r };
                    register(ui, format!("kwSuggest:{k}"), r.rect);
                    if r.clicked() {
                        let _ = app.run("photo.setMeta", json!({"addKeywords": [k]}));
                    }
                }
            });
        }
    });
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
    let typing = ui.memory(|m| m.has_focus(crate::text_field::id("field:keyword")));
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
        let combo = crate::widgets::combo_box("kw-set")
            .selected_text(crate::i18n::builtin_label(&current, current == lightcraft_engine::cmd::keywords::RECENT))
            .show_ui(ui, |ui| {
                for set in sets["sets"].as_array().into_iter().flatten() {
                    let name = set["name"].as_str().unwrap_or_default();
                    if ui
                        .selectable_label(name == current, crate::i18n::builtin_label(name, name == lightcraft_engine::cmd::keywords::RECENT))
                        .clicked()
                    {
                        let _ = app.run("keyword.useSet", json!({"name": name}));
                    }
                }
                ui.separator();
                // Edit Set…: the current nine, slot by slot (named, Recent Keywords becomes a set)
                let edit = ui.button(crate::i18n::tr("Edit Set…"));
                register(ui, "keywordSetMenu:edit", edit.rect);
                if edit.clicked() {
                    let mut slots: Vec<String> =
                        sets["keywords"].as_array().into_iter().flatten().filter_map(|k| k.as_str().map(str::to_string)).collect();
                    slots.resize(9, String::new());
                    let named = current != lightcraft_engine::cmd::keywords::RECENT;
                    app.ui.dialog = Some(crate::state::Dialog::KeywordSet {
                        replaces: named.then(|| current.clone()),
                        name: if named { current.clone() } else { String::new() },
                        slots,
                        as_new: false,
                    });
                }
                if ui.button(crate::i18n::tr("Save Current Keywords as Set…")).clicked() {
                    app.ui.dialog = Some(crate::state::Dialog::TextPrompt {
                        title: "Save Keyword Set".into(),
                        hint: "Set name".into(),
                        value: String::new(),
                        command: "keyword.saveSet".into(),
                        params: json!({}),
                        key: "name".into(),
                    });
                }
                if current != lightcraft_engine::cmd::keywords::RECENT
                    && ui.button(crate::i18n::tr_format!("Delete “{current}”", current = current)).clicked()
                {
                    let _ = app.run("keyword.deleteSet", json!({"name": current}));
                }
            });
        crate::widgets::name_combo(&combo.response, crate::i18n::tr("Keyword Set"));
        register(ui, "keywordSetCombo", combo.response.rect);
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
