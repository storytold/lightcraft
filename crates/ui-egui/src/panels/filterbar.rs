//! The library filter bar above the grid: rating (≥ / = / ≤), flags, colour labels, kind,
//! edited state and camera / lens / keyword pickers (several values each, any of which matches) — every
//! control drives `library.filter`.
//! "Save as Smart Collection…" keeps the current view as a live album; "Clear" resets the filter.
//! The Metadata columns under it (issue #218, `view.filterColumns`) list each field's values among the photos in
//! view with counts (`library.facets`); a click narrows the view (`library.facetPick`) and the columns cascade.
#![allow(clippy::indexing_slicing, reason = "not yet converted to checked indexing: tracked in docs/gaps.md (indexing lint)")]

use egui::{Color32, Rect, Sense, Stroke, StrokeKind, vec2};
use lcc_catalog::{ColorLabel, Flag, MediaKind, RatingOp};
use serde_json::{Value, json};

use crate::LccApp;
use crate::icons::{Icon, paint};
use crate::theme::Tokens;
use crate::widgets::register;

pub const HEIGHT: f32 = 40.0;
/// Bar width (points) below which the controls take two rows.
const WIDE: f32 = 1300.0;

/// Colour-label swatches (UI colours, our own choice).
pub use crate::theme::label_color;

fn filter(app: &mut LccApp, patch: Value) {
    let _ = app.run("library.filter", patch);
}

/// A small label before a group of controls.
fn caption(ui: &mut egui::Ui, text: &str) {
    let t = Tokens::get(ui.ctx());
    ui.label(egui::RichText::new(crate::i18n::tr(text)).font(t.font(12.0)).color(t.text_dim));
}

/// A square toggle with an icon or a colour swatch.
fn toggle(ui: &mut egui::Ui, id: &str, on: bool, tip: &str, draw: impl FnOnce(&egui::Painter, Rect, Color32)) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    let (r, resp) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::click());
    crate::widgets::announce_selected(&resp, crate::i18n::tr(tip), on);
    register(ui.ctx(), format!("filter:{id}"), r);
    let p = ui.painter();
    if on {
        p.rect_filled(r, 5.0, t.tool_active);
        p.rect_stroke(r, 5.0, Stroke::new(1.0, t.accent), StrokeKind::Inside);
    } else if resp.hovered() {
        p.rect_filled(r, 5.0, t.hover);
    }
    draw(
        p,
        r.shrink(5.0),
        if on {
            t.text
        } else if resp.hovered() {
            t.text_label
        } else {
            t.icon
        },
    );
    resp.on_hover_text(crate::i18n::tr(tip))
}

/// A dropdown showing `current`; `items` lists its (label, filter patch, selected) rows. It runs only while the
/// dropdown is open: a metadata picker lists every camera, lens or keyword of the library, and building those rows
/// on every frame made scrolling the grid under the bar stutter in a large library (issue #538).
fn picker(app: &mut LccApp, ui: &mut egui::Ui, id: &str, current: &str, active: bool, items: impl FnOnce(&mut LccApp) -> Vec<(String, Value, bool)>) {
    let t = Tokens::get(ui.ctx());
    let r = crate::widgets::dropdown(
        ui,
        &format!("filter-{id}"),
        if active && matches!(id, "camera" | "lens" | "keyword") { current } else { crate::i18n::tr(current) },
        t.font(12.5),
        if active { t.text } else { t.text_label },
    );
    if active {
        ui.painter().rect_stroke(r.rect.expand(2.0), 4.0, Stroke::new(1.0, t.accent.gamma_multiply(0.7)), StrokeKind::Outside);
    }
    let mut chosen = None;
    let searchable = matches!(id, "camera" | "lens" | "keyword");
    // the metadata pickers take several values (issue #213): a click toggles one and the list stays open
    let multi = searchable;
    // the search text of this picker alone; `None` until the dropdown opens, cleared when it closes
    let key = egui::Id::new(("filterSearch", id));
    let close = if multi { egui::PopupCloseBehavior::CloseOnClickOutside } else { egui::PopupCloseBehavior::CloseOnClick };
    // a stable id: the button's own comes from its place, which moves when the chips row appears after a pick
    let opened = egui::Popup::menu(&r).id(egui::Id::new(("filterPicker", id))).close_behavior(close).show(|ui| {
        let mut query = String::new();
        if searchable {
            let previous: Option<String> = ui.data(|d| d.get_temp(key));
            let just_opened = previous.is_none();
            query = previous.unwrap_or_default();
            let widget = format!("filterSearch:{id}");
            let field = crate::text_field::TextField::singleline(&widget, &mut query)
                .hint(crate::i18n::tr("Search…"))
                .width(220.0)
                .begin_edit(just_opened)
                .show(ui);
            // the click that opened the dropdown can take the focus away on its release: while
            // nothing else has it, the box takes it back so typing goes straight in
            if !field.response.has_focus() && ui.memory(|m| m.focused().is_none()) {
                field.response.request_focus();
            }
            ui.data_mut(|d| d.insert_temp(key, query.clone()));
            ui.separator();
        }
        let needle = query.to_lowercase();
        let items = items(app);
        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
            for (n, (label, patch, sel)) in items.iter().enumerate() {
                // the "Any …" row (a patch that only clears: nulls and empty lists) always stays, and so does the
                // column's operator toggle (a patch with an `…Op` key)
                // (its operator, if any, back to OR; a row that only sets the operator is not it)
                let any = patch.as_object().is_none_or(|o| {
                    o.keys().any(|k| !k.ends_with("Op"))
                        && o.iter().all(|(k, v)| v.is_null() || v.as_array().is_some_and(Vec::is_empty) || (k.ends_with("Op") && v == "any"))
                });
                let op_row = !any && patch.as_object().is_some_and(|o| o.keys().any(|k| k.ends_with("Op")));
                if searchable && !any && !op_row && !needle.is_empty() && !label.to_lowercase().contains(&needle) {
                    continue;
                }
                let resp = ui.selectable_label(*sel, if searchable && !any { label.as_str() } else { crate::i18n::tr(label) });
                register(ui.ctx(), format!("filterItem:{id}:{n}"), resp.rect);
                if resp.clicked() {
                    chosen = Some(patch.clone());
                    // "Any …" clears the choice: nothing more to pick
                    if multi && any {
                        ui.close();
                    }
                }
            }
        });
    });
    if opened.is_none() {
        ui.data_mut(|d| d.remove::<String>(key));
    }
    if let Some(p) = chosen {
        filter(app, p);
    }
}

/// The values a metadata picker has chosen: its multi-select list, and the single value (set from elsewhere)
/// first when the list doesn't hold it.
fn chosen_values(single: Option<&str>, list: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for v in single.into_iter().chain(list.iter().map(String::as_str)) {
        if !out.iter().any(|x| x.eq_ignore_ascii_case(v)) {
            out.push(v.to_string());
        }
    }
    out
}

/// The view's culling counts ([`lcc_engine::Session::tally`]), kept until the catalog, the source or the filter
/// changes (a slider drag or a frame with nothing new reuses them).
fn tally(app: &LccApp, ctx: &egui::Context) -> lcc_catalog::Tally {
    let key = format!("{}|{:?}|{}", app.session.catalog.revision, app.session.source, serde_json::to_string(&app.session.filter).unwrap_or_default());
    let id = egui::Id::new("filter-tally");
    if let Some((k, t)) = ctx.data(|d| d.get_temp::<(String, lcc_catalog::Tally)>(id))
        && k == key
    {
        return t;
    }
    let t = app.session.tally();
    ctx.data_mut(|d| d.insert_temp(id, (key, t.clone())));
    t
}

/// Whether any photo in the catalog carries a quality score (issue #1313), kept until the catalog
/// changes: the scanner's picker appears once a scan has scored something.
fn quality_scanned(app: &LccApp, ctx: &egui::Context) -> bool {
    let revision = app.session.catalog.revision;
    let id = egui::Id::new("filter-quality-scanned");
    if let Some((r, yes)) = ctx.data(|d| d.get_temp::<(u64, bool)>(id))
        && r == revision
    {
        return yes;
    }
    let yes = app.session.catalog.any_quality_scanned();
    ctx.data_mut(|d| d.insert_temp(id, (revision, yes)));
    yes
}

/// A live count after a flag or label toggle, small and dim.
fn count(ui: &mut egui::Ui, id: &str, n: usize) {
    let t = Tokens::get(ui.ctx());
    let r = ui.add(egui::Label::new(egui::RichText::new(super::grid::group_digits(n)).font(t.font(10.5)).color(t.text_dim)).selectable(false));
    register(ui.ctx(), id, r.rect);
    ui.add_space(4.0);
}

pub fn show(app: &mut LccApp, outer: &mut egui::Ui) {
    let ui = &mut *outer;
    let t = Tokens::get(ui.ctx());
    // one row when there is room, else the metadata pickers and actions go to a second row
    let two_rows = ui.available_width() < WIDE;
    let rows_height = if two_rows { 2.0 * HEIGHT - 8.0 } else { HEIGHT };
    // the Metadata columns (issue #218) go under the rows
    let extra = if app.ui.filter_columns.is_empty() { 0.0 } else { COLUMNS_HEIGHT };
    let (bar, _) = ui.allocate_exact_size(vec2(ui.available_width(), rows_height + extra), Sense::hover());
    let columns_area = (extra > 0.0).then(|| Rect::from_min_max(bar.min + vec2(16.0, rows_height), bar.max - vec2(16.0, 8.0)));
    ui.painter().rect_filled(bar, 0.0, t.chrome);
    ui.painter().line_segment([bar.left_bottom(), bar.right_bottom()], Stroke::new(1.0, t.divider));
    register(ui.ctx(), "filterbar", bar);
    let f = app.session.filter.clone();
    let row = |ui: &mut egui::Ui, i: f32| {
        let r = Rect::from_min_size(bar.min + vec2(16.0, 4.0 + i * (HEIGHT - 8.0)), vec2(bar.width() - 32.0, HEIGHT - 8.0));
        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(r).layout(egui::Layout::left_to_right(egui::Align::Center)));
        c.spacing_mut().item_spacing.x = 4.0;
        c
    };
    let mut row1 = row(ui, 0.0);
    let mut row2 = row(ui, 1.0);
    let ui = &mut row1;

    // live counts: the view's photos by mark, without the marks' own filter (worked out when the catalog,
    // source or filter changes)
    let tally = tally(app, ui.ctx());
    // rating: operator + stars
    caption(ui, "Rating");
    let op_label = match f.rating_op {
        RatingOp::AtLeast => "≥",
        RatingOp::Exactly => "=",
        RatingOp::AtMost => "≤",
    };
    picker(app, ui, "ratingOp", op_label, false, |_| {
        [("≥  at least", "atLeast", RatingOp::AtLeast), ("=  exactly", "exactly", RatingOp::Exactly), ("≤  at most", "atMost", RatingOp::AtMost)]
            .into_iter()
            .map(|(l, k, op)| (l.to_string(), json!({"ratingOp": k}), f.rating_op == op))
            .collect()
    });
    if let Some(r) = crate::rating::Rating::new("filter", f.rating).automation_ids("filter:star").show(ui) {
        filter(app, json!({"rating": r}));
    }
    ui.add_space(14.0);

    // flags (one at a time; click again to clear)
    caption(ui, "Flag");
    for (id, icon, flag, tip) in [
        ("pick", Icon::FlagPick, Flag::Pick, "Picked"),
        ("reject", Icon::FlagReject, Flag::Reject, "Rejected"),
        ("none", Icon::Circle, Flag::None, "Unflagged"),
    ] {
        let on = f.flag == Some(flag);
        let color = match flag {
            Flag::Pick if on => t.pick,
            Flag::Reject if on => t.reject,
            _ => Color32::TRANSPARENT,
        };
        let icon = if on && flag == Flag::Pick { Icon::FlagPickFilled } else { icon };
        let resp = toggle(ui, id, on, tip, |p, r, c| paint(p, r, icon, if color == Color32::TRANSPARENT { c } else { color }));
        count(ui, &format!("filter:count:{id}"), tally.flag(flag));
        if resp.clicked() {
            filter(app, json!({"flag": if on { Value::Null } else { json!(id) }}));
        }
    }
    ui.add_space(14.0);

    // colour labels
    caption(ui, "Label");
    // several labels can be on at once (any of them matches); lit are those `label` and `labels` both let through
    let chosen: Vec<ColorLabel> = f.labels_shown().unwrap_or_default();
    for l in ColorLabel::ALL {
        let on = chosen.contains(&l);
        let name = format!("{l:?}").to_lowercase();
        let tip = crate::i18n::tr_format!("{} label (click more labels to show any of them)", crate::i18n::color_label(&app.session.catalog, l));
        let resp = toggle(ui, &format!("label-{name}"), on, &tip, |p, r, _| {
            p.circle_filled(r.center(), 5.5, label_color(l));
        });
        count(ui, &format!("filter:count:{name}"), tally.label(l));
        if resp.clicked() {
            let mut next = chosen.clone();
            if on {
                next.retain(|x| *x != l);
            } else {
                next.push(l);
            }
            next.sort();
            let names: Vec<String> = next.iter().map(|x| format!("{x:?}").to_lowercase()).collect();
            filter(app, json!({"label": Value::Null, "labels": names}));
        }
    }
    // photos without a label: the hollow swatch; ANDs with the colours above
    let none_on = f.labels_none;
    let resp = toggle(ui, "label-none", none_on, "Photos without a colour label", |p, r, c| {
        p.circle_stroke(r.center(), 5.5, Stroke::new(1.5, c));
    });
    count(ui, "filter:count:labelnone", tally.unlabeled);
    if resp.clicked() {
        filter(app, json!({"labelsNone": !none_on}));
    }
    ui.add_space(14.0);

    // kind + edited
    let kind_label = match f.kind {
        None => "All types",
        Some(MediaKind::Image) => "Photos",
        Some(MediaKind::Raw) => "Raw",
        Some(MediaKind::Video) => "Videos",
    };
    let kind_label = match f.merged.as_deref() {
        Some("hdr") => "HDR",
        Some("panorama") => "Panoramas",
        Some("hdrPanorama") => "HDR Panoramas",
        Some("focusStack") => "Focus Stacks",
        Some(_) => "Merged",
        None => kind_label,
    };
    picker(app, ui, "kind", kind_label, f.kind.is_some() || f.merged.is_some(), |_| {
        [
            ("All types", Value::Null, None),
            ("Photos", json!("image"), Some(MediaKind::Image)),
            ("Raw", json!("raw"), Some(MediaKind::Raw)),
            ("Videos", json!("video"), Some(MediaKind::Video)),
        ]
        .into_iter()
        .map(|(l, v, k)| (l.to_string(), json!({"kind": v, "merged": null}), f.kind == k && f.merged.is_none()))
        .chain(
            [("HDR", "hdr"), ("Panoramas", "panorama"), ("HDR Panoramas", "hdrPanorama"), ("Focus Stacks", "focusStack")]
                .into_iter()
                .map(|(l, m)| (l.to_string(), json!({"kind": null, "merged": m}), f.merged.as_deref() == Some(m))),
        )
        .collect()
    });
    let edited_label = match f.edited {
        None => "Edited or not",
        Some(true) => "Edited",
        Some(false) => "Unedited",
    };
    picker(app, ui, "edited", edited_label, f.edited.is_some(), |_| {
        [("Edited or not", Value::Null, None), ("Edited", json!(true), Some(true)), ("Unedited", json!(false), Some(false))]
            .into_iter()
            .map(|(l, v, e)| (l.to_string(), json!({"edited": v}), f.edited == e))
            .collect()
    });
    if f.immich.is_some() || !app.session.catalog.remote_links().is_empty() {
        let current = match f.immich.as_deref() {
            Some("linked") => "In Immich",
            Some("probable") => "Immich: probable",
            Some(_) => "Not in Immich",
            None => "Immich: any",
        };
        picker(app, ui, "immich", current, f.immich.is_some(), |_| {
            [
                ("Immich: any", Value::Null),
                ("In Immich", json!("linked")),
                ("Not in Immich", json!("notLinked")),
                ("Immich: probable", json!("probable")),
            ]
            .into_iter()
            .map(|(l, v)| (l.to_string(), json!({"immich": v.clone()}), f.immich.as_deref() == v.as_str()))
            .collect()
        });
        #[cfg(not(target_arch = "wasm32"))]
        super::connections::smart_search_box(app, ui);
    }
    // Vignetting Calibration references: only while the catalog holds a map (or the filter is set)
    if f.vignetting_reference.is_some() || app.session.catalog.vignetting_maps().next().is_some() {
        let options = [
            ("Vignetting reference: any photo", Value::Null),
            ("Vignetting reference", json!("reference")),
            ("Not a vignetting reference", json!("none")),
        ];
        let want = f.vignetting_reference.as_deref();
        let current = options.iter().find(|(_, v)| v.as_str() == want).map_or("Vignetting reference: any photo", |(l, _)| *l);
        picker(app, ui, "vignettingReference", current, want.is_some(), |_| {
            options.iter().map(|(l, v)| (l.to_string(), json!({"vignettingReference": v.clone()}), want == v.as_str())).collect()
        });
    }

    // The Image Quality Scanner's verdict (issue #1313): any, accepted, to review (pending),
    // rejected, or the photos no scan has scored. Shown once a scan has scored something (or the
    // filter is set), so a catalog no scan reached is not cluttered with a choice of nothing.
    if f.quality.is_some() || f.quality_min.is_some() || f.quality_max.is_some() || quality_scanned(app, ui.ctx()) {
        let options = [
            ("Quality: any photo", Value::Null),
            ("Quality: accepted", json!("accepted")),
            ("Quality: to review", json!("pending")),
            ("Quality: rejected", json!("rejected")),
            ("Quality: not scanned", json!("unscanned")),
        ];
        let want = f.quality.as_deref();
        let current = options.iter().find(|(_, v)| v.as_str() == want).map_or("Quality: any photo", |(l, _)| *l);
        picker(app, ui, "quality", current, want.is_some(), |_| {
            options.iter().map(|(l, v)| (l.to_string(), json!({"quality": v.clone()}), want == v.as_str())).collect()
        });
        // the score's floor: a slider in place of its button, and the button to clear it (the
        // slider is drawn only while a floor is set, so the bar stays as wide as the verdict
        // picker needs; a ceiling set elsewhere — a rule, another window — is left as it is)
        if f.quality_min.is_some() {
            let mut min = f.quality_min.unwrap_or(0.0);
            let r =
                ui.add(egui::Slider::new(&mut min, 0.0..=100.0).text(crate::i18n::tr("Quality score at least")).fixed_decimals(0).show_value(false));
            register(ui.ctx(), "slider:qualityMin", r.rect);
            if r.changed() {
                filter(app, json!({"qualityMin": min}));
            }
            let clear = crate::widgets::text_button(ui, "qualityMinClear", "×", true);
            register(ui.ctx(), "button:qualityMinClear", clear.rect);
            if clear.clicked() {
                filter(app, json!({"qualityMin": Value::Null}));
            }
        } else {
            // (the label is translated in the button)
            let floor = crate::widgets::text_button(ui, "qualityMinSet", "Quality score at least", false);
            register(ui.ctx(), "button:qualityMinSet", floor.rect);
            if floor.clicked() {
                filter(app, json!({"qualityMin": 50.0}));
            }
        }
    }

    // metadata pickers
    let ui: &mut egui::Ui = if two_rows { &mut row2 } else { &mut row1 };
    // several values of one picker show any of them (issue #213); a value set elsewhere (the Keyword List
    // in the sidebar sets one keyword) counts as chosen, and the first toggle here moves it into the list
    for (id, key, list_key, all_label, current, list) in [
        ("camera", "camera", "cameras", "Camera", f.camera.as_deref(), f.cameras.as_slice()),
        ("lens", "lens", "lenses", "Lens", f.lens.as_deref(), f.lenses.as_slice()),
        ("keyword", "keyword", "keywords", "Keyword", f.keyword.as_deref(), f.keywords.as_slice()),
    ] {
        let chosen = chosen_values(current, list);
        let shown = match chosen.as_slice() {
            [] => all_label.to_string(),
            [one] => one.clone(),
            [first, rest @ ..] => format!("{first} +{}", rest.len()),
        };
        let shown = if shown.chars().count() > 22 { format!("{}…", shown.chars().take(21).collect::<String>()) } else { shown };
        let active = !chosen.is_empty();
        let items = move |app: &mut LccApp| {
            // the library's values, asked for only while the menu is open: listing them again after a metadata change
            // goes through every photo
            let values = app.caches.filter_values(&app.session.catalog);
            let all = match id {
                "camera" => &values.cameras,
                "lens" => &values.lenses,
                _ => &values.keywords,
            };
            // a photo has one camera, so only lenses and keywords combine with AND (issue #683)
            let op_key = format!("{key}Op");
            let op = match id {
                "camera" => None,
                "lens" => Some(f.lens_op),
                _ => Some(f.keyword_op),
            };
            let is_on = |v: &str| chosen.iter().any(|c| c.eq_ignore_ascii_case(v));
            // "Any …" clears the values and puts the operator back to OR, so the next pick doesn't start in AND
            let mut clear = json!({key: Value::Null, list_key: []});
            if op.is_some()
                && let Some(o) = clear.as_object_mut()
            {
                o.insert(op_key.clone(), json!("any"));
            }
            let mut items = vec![(format!("Any {}", all_label.to_lowercase()), clear, chosen.is_empty())];
            items.extend(all.iter().map(|v| {
                let on = is_on(v);
                let next: Vec<&String> =
                    if on { chosen.iter().filter(|c| !c.eq_ignore_ascii_case(v)).collect() } else { chosen.iter().chain([v]).collect() };
                (v.clone(), json!({key: Value::Null, list_key: next}), on)
            }));
            // the boolean operator (issue #683): OR matches photos with any chosen value, AND only those with all
            if let Some(op) = op {
                items.push((crate::i18n::tr("Match any (OR)").to_string(), json!({op_key.clone(): "any"}), op == lcc_catalog::MatchOp::Any));
                items.push((crate::i18n::tr("Match all (AND)").to_string(), json!({op_key: "all"}), op == lcc_catalog::MatchOp::All));
            }
            items
        };
        picker(app, ui, id, &shown, active, items);
    }

    // faces picker: detected face boxes, named or not (`faces` in `library.filter`)
    const FACE_OPTIONS: &[(&str, &str)] =
        &[("No faces", "0"), ("1 face", "1"), ("2 faces", "2"), ("1+ faces", "1+"), ("3+ faces", "3+"), ("5+ faces", "5+"), ("10+ faces", "10+")];
    let current_faces = f.faces.as_deref();
    let preset = current_faces.and_then(|v| FACE_OPTIONS.iter().find(|(_, val)| *val == v));
    let mut face_items = vec![("Any faces".to_string(), json!({"faces": Value::Null}), current_faces.is_none())];
    face_items.extend(FACE_OPTIONS.iter().map(|(label, val)| (label.to_string(), json!({"faces": val}), current_faces == Some(*val))));
    let custom = current_faces.filter(|_| preset.is_none()).map(|spec| crate::i18n::tr_format!("Faces: {spec}", spec = spec));
    if let (Some(label), Some(spec)) = (&custom, current_faces) {
        face_items.push((label.clone(), json!({"faces": spec}), true));
    }
    let shown_faces = match (preset, custom) {
        (Some((label, _)), _) => label.to_string(),
        (None, Some(label)) => label,
        (None, None) => "Faces".to_string(),
    };
    picker(app, ui, "faces", &shown_faces, f.faces.is_some(), move |_| face_items);

    // the Metadata columns' switch (issue #218)
    let on = !app.ui.filter_columns.is_empty();
    let resp = toggle(ui, "columns", on, "Metadata columns: click a value to narrow the view", |p, r, c| {
        let w = r.width() / 3.0;
        for i in 0..3 {
            let col = Rect::from_min_size(r.min + vec2(i as f32 * w + 1.0, 1.0), vec2(w - 2.0, r.height() - 2.0));
            p.rect_stroke(col, 1.0, Stroke::new(1.2, c), StrokeKind::Inside);
        }
    });
    register(ui.ctx(), "button:filterColumns", resp.rect);
    if resp.clicked() {
        let _ = app.run("view.filterColumns", json!({"show": !on}));
    }

    // save / clear
    ui.add_space(14.0);
    let filtering = app.session.filter != Default::default();
    let save = ui.add_enabled(filtering, egui::Button::new(egui::RichText::new(crate::i18n::tr("Save as Smart Collection…")).font(t.font(12.5))));
    register(ui.ctx(), "button:filterSave", save.rect);
    if save.clicked() {
        let _ = app.run("dialog.newSmartAlbum", json!({}));
    }
    let clear = ui.add_enabled(filtering, egui::Button::new(egui::RichText::new(crate::i18n::tr("Clear")).font(t.font(12.5))));
    register(ui.ctx(), "button:filterClear", clear.rect);
    if clear.clicked() {
        app.ui.search.clear();
        let _ = app.run("library.clearFilter", json!({}));
    }
    // filter presets: apply one, save the current filter, delete
    let presets = crate::widgets::dropdown(ui, "filterPresets", crate::i18n::tr("Presets"), t.font(12.5), t.text);
    register(ui.ctx(), "button:filterPresets", presets.rect);
    egui::Popup::menu(&presets).show(|ui| {
        let names: Vec<String> = app.session.filter_presets.iter().map(|f| f.name.clone()).collect();
        for n in &names {
            let r = ui.button(n);
            if r.clicked() {
                let _ = app.run("filter.applyPreset", json!({"name": n}));
            }
            crate::widgets::context_menu(&r, |ui| {
                if ui.button(crate::i18n::tr("Delete Preset")).clicked() {
                    let _ = app.run("filter.deletePreset", json!({"name": n}));
                }
            });
        }
        if names.is_empty() {
            ui.label(egui::RichText::new(crate::i18n::tr("No filter presets yet")).color(t.text_dim));
        }
        ui.separator();
        if ui.add_enabled(filtering, egui::Button::new(crate::i18n::tr("Save Current Filter as Preset…"))).clicked() {
            crate::panels::dialogs::prompt(app, "Save Filter Preset", "Preset name", "", "filter.savePreset", json!({}), "name");
        }
    });
    // the Metadata columns, under the rows
    if !app.ui.filter_columns.is_empty()
        && let Some(area) = columns_area
    {
        columns(app, outer, area);
    }
}

/// The Metadata columns' height (points), under the filter bar's rows: a column's field, its search box and its values.
pub const COLUMNS_HEIGHT: f32 = 198.0;
/// The fields a Metadata column can list (`library.facetPick`'s fields), with their headers.
pub const COLUMN_FIELDS: [(&str, &str); 25] = [
    ("year", "Date"),
    ("camera", "Camera"),
    ("lens", "Lens"),
    ("keyword", "Keyword"),
    ("person", "People"),
    ("label", "Label"),
    ("flag", "Flag"),
    ("rating", "Rating"),
    ("kind", "File Type"),
    ("format", "File Format"),
    ("iso", "ISO"),
    ("focal", "Focal Length"),
    ("aperture", "Aperture"),
    ("shutter", "Shutter Speed"),
    ("aspect", "Aspect Ratio"),
    ("title", "Title"),
    ("headline", "Headline"),
    ("caption", "Caption"),
    ("creator", "Creator"),
    ("copyright", "Copyright"),
    ("location", "Location"),
    ("city", "City"),
    ("country", "Country"),
    ("preset", "Develop Preset"),
    ("ai", "AI Tools"),
];
/// The columns first shown, as Lightroom Classic's Metadata filter starts.
const DEFAULT_COLUMNS: [&str; 4] = ["year", "camera", "lens", "label"];
/// Most columns side by side.
const MAX_COLUMNS: usize = 6;

/// The filter bar's commands (`menus_fork`).
pub const COMMANDS: &[crate::menus::UiCommand] = &[("view.filterColumns", "Metadata Filter Columns", None, "")];

/// `view.filterColumns {show?: bool, column?: index, field?: name, count?: 1..6, toggle?: name}` → `{columns}`: shows or
/// hides the filter bar's Metadata columns (no argument toggles), sets the field one column lists, or the number of
/// columns; `toggle` adds the field's column (the columns appear if hidden) or removes it (the last one hides them all).
pub fn run(app: &mut LccApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    if id != "view.filterColumns" {
        return None;
    }
    Some(filter_columns(app, p))
}

fn filter_columns(app: &mut LccApp, p: &Value) -> Result<Value, String> {
    const C: &str = "view.filterColumns";
    let index = |key: &str, range: std::ops::RangeInclusive<usize>| -> Result<Option<usize>, String> {
        match p.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => v
                .as_u64()
                .and_then(|c| usize::try_from(c).ok())
                .filter(|c| range.contains(c))
                .map(Some)
                .ok_or_else(|| format!("{C}: {key} is {} to {}", range.start(), range.end())),
        }
    };
    let field = match p.get("field") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_str()
                .filter(|f| COLUMN_FIELDS.iter().any(|(k, _)| k == f))
                .ok_or_else(|| format!("{C}: field is one of {}", COLUMN_FIELDS.map(|(k, _)| k).join(", ")))?,
        ),
    };
    let toggle = match p.get("toggle") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_str()
                .filter(|f| COLUMN_FIELDS.iter().any(|(k, _)| k == f))
                .ok_or_else(|| format!("{C}: toggle is one of {}", COLUMN_FIELDS.map(|(k, _)| k).join(", ")))?,
        ),
    };
    if let Some(f) = toggle {
        let columns = &mut app.ui.filter_columns;
        if let Some(i) = columns.iter().position(|c| c == f) {
            columns.remove(i);
            if i < app.ui.filter_column_search.len() {
                app.ui.filter_column_search.remove(i);
            }
            if columns.is_empty() {
                app.ui.filter_column_search.clear();
                return Ok(json!({"columns": []}));
            }
        } else if columns.len() >= MAX_COLUMNS {
            return Err(format!("{C}: at most {MAX_COLUMNS} columns"));
        } else {
            columns.push(f.to_string());
        }
        app.ui.filter_bar = true;
        if !matches!(app.ui.view, crate::state::ViewMode::PhotoGrid | crate::state::ViewMode::SquareGrid) {
            app.ui.view = crate::state::ViewMode::PhotoGrid;
        }
        return Ok(json!({"columns": app.ui.filter_columns}));
    }
    let column = index("column", 0..=MAX_COLUMNS - 1)?;
    let count = index("count", 1..=MAX_COLUMNS)?;
    let show = match p.get("show") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_bool().ok_or_else(|| format!("{C}: show is true or false"))?),
    };
    if field.is_some() != column.is_some() {
        return Err(format!("{C}: column and field go together"));
    }
    let shown = !app.ui.filter_columns.is_empty();
    // nothing but a toggle: show ⇄ hide
    let show = show.unwrap_or(field.is_some() || count.is_some() || !shown);
    if !show {
        app.ui.filter_columns.clear();
        // the columns' search boxes go with them
        app.ui.filter_column_search.clear();
        return Ok(json!({"columns": []}));
    }
    if app.ui.filter_columns.is_empty() {
        app.ui.filter_columns = DEFAULT_COLUMNS.iter().map(|f| f.to_string()).collect();
    }
    if let Some(n) = count {
        let spare: Vec<String> = COLUMN_FIELDS.iter().map(|(k, _)| k.to_string()).filter(|k| !app.ui.filter_columns.contains(k)).collect();
        let mut spare = spare.into_iter();
        app.ui.filter_columns.truncate(n);
        while app.ui.filter_columns.len() < n {
            app.ui.filter_columns.push(spare.next().unwrap_or_else(|| "camera".to_string()));
        }
    }
    if let (Some(c), Some(f)) = (column, field) {
        let n = app.ui.filter_columns.len();
        let Some(slot) = app.ui.filter_columns.get_mut(c) else {
            return Err(format!("{C}: there are {n} columns"));
        };
        // another field lists other values: the column's search starts empty
        if let Some(search) = app.ui.filter_column_search.get_mut(c) {
            search.clear();
        }
        *slot = f.to_string();
    }
    // the columns live in the filter bar, over the grid
    app.ui.filter_bar = true;
    if !matches!(app.ui.view, crate::state::ViewMode::PhotoGrid | crate::state::ViewMode::SquareGrid) {
        app.ui.view = crate::state::ViewMode::PhotoGrid;
    }
    Ok(json!({"columns": app.ui.filter_columns}))
}

/// `library.facets` for the columns, computed again only when the library, the source or the filter changed.
fn facets(app: &mut LccApp, ctx: &egui::Context, fields: &[String]) -> std::sync::Arc<Value> {
    let key = format!(
        "{}|{:?}|{}|{}",
        app.session.catalog.revision,
        app.session.source,
        serde_json::to_string(&app.session.filter).unwrap_or_default(),
        fields.join(",")
    );
    let id = egui::Id::new("filterColumnsFacets");
    if let Some((k, v)) = ctx.data(|d| d.get_temp::<(String, std::sync::Arc<Value>)>(id))
        && k == key
    {
        return v;
    }
    let v = std::sync::Arc::new(app.session.execute("library.facets", &json!({"fields": fields})).unwrap_or(Value::Null));
    ctx.data_mut(|d| d.insert_temp(id, (key, v.clone())));
    v
}

/// How a column shows a value `library.facets` lists.
fn value_label(field: &str, value: &str) -> String {
    let tr = |s: &str| crate::i18n::tr(s).to_string();
    match (field, value) {
        ("flag", "pick") => tr("Picked"),
        ("flag", "reject") => tr("Rejected"),
        ("flag", "none") => tr("Unflagged"),
        ("kind", "image") => tr("Photos"),
        ("kind", "raw") => tr("Raw"),
        ("kind", "video") => tr("Videos"),
        ("rating", "0") => tr("Unrated"),
        ("rating", n) => n.parse::<usize>().map(|n| "★".repeat(n.min(5))).unwrap_or_else(|_| n.to_string()),
        _ => value.to_string(),
    }
}

/// Whether what a Metadata column's search box holds lets `text` (a value as the column shows it) through.
/// An empty box lets everything through; case is ignored.
fn matches(query: &str, text: &str) -> bool {
    let needle = query.trim().to_lowercase();
    needle.is_empty() || text.to_lowercase().contains(&needle)
}

/// One row of a Metadata column: the value, its count at the right. Returns whether it was clicked.
fn value_row(ui: &mut egui::Ui, widget: String, text: String, count: u64, on: bool, clickable: bool) -> bool {
    let t = Tokens::get(ui.ctx());
    let (rr, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), if clickable { Sense::click() } else { Sense::hover() });
    register(ui.ctx(), widget, rr);
    crate::widgets::announce_selected(&resp, text.clone(), on);
    if on {
        ui.painter().rect_filled(rr, 3.0, t.tool_active);
    } else if clickable && resp.hovered() {
        ui.painter().rect_filled(rr, 3.0, t.hover);
    }
    let color = if !clickable {
        t.text_dim
    } else if on {
        t.text
    } else {
        t.text_label
    };
    let cg = ui.painter().layout_no_wrap(count.to_string(), t.font(11.5), t.text_dim);
    let room = (rr.width() - cg.size().x - 14.0).max(10.0);
    let mut job = egui::text::LayoutJob::simple_singleline(text, t.font(12.0), color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(room);
    let g = ui.painter().layout_job(job);
    ui.painter().galley(egui::pos2(rr.left() + 4.0, rr.center().y - g.size().y / 2.0), g, color);
    ui.painter().galley(egui::pos2(rr.right() - 4.0 - cg.size().x, rr.center().y - cg.size().y / 2.0), cg, t.text_dim);
    clickable && resp.clicked()
}

/// The Metadata columns (issue #218): each lists one field's values among the photos in view with how many photos
/// have each; a click narrows the view (`library.facetPick`), and the columns after it count only what is left.
fn columns(app: &mut LccApp, ui: &mut egui::Ui, area: Rect) {
    let t = Tokens::get(ui.ctx());
    let f = app.session.filter.clone();
    let fields: Vec<String> = app.ui.filter_columns.iter().take(MAX_COLUMNS).cloned().collect();
    let n = fields.len().max(1);
    let facets = facets(app, ui.ctx(), &fields);
    let gap = 8.0;
    let w = ((area.width() - gap * (n as f32 - 1.0)) / n as f32).max(60.0);
    register(ui.ctx(), "filterColumns", area);
    // one search box per column, kept from frame to frame (a search survives the faceted counts changing with it)
    app.ui.filter_column_search.resize(fields.len(), String::new());
    let mut picked: Option<Value> = None;
    let mut op_pick: Option<(String, Value)> = None;
    let mut retitle: Option<(usize, &'static str)> = None;
    for (i, field) in fields.iter().enumerate() {
        let r = Rect::from_min_size(area.min + vec2(i as f32 * (w + gap), 0.0), vec2(w, area.height()));
        ui.painter().rect_stroke(r, 4.0, Stroke::new(1.0, t.divider), StrokeKind::Inside);
        let mut c = ui.new_child(egui::UiBuilder::new().max_rect(r.shrink(4.0)).layout(egui::Layout::top_down(egui::Align::Min)));
        c.spacing_mut().item_spacing.y = 1.0;
        let title = COLUMN_FIELDS.iter().find(|(k, _)| k == field).map_or("Camera", |(_, l)| *l);
        let head = crate::widgets::dropdown_within(&mut c, &format!("filterColumn{i}"), crate::i18n::tr(title), t.semibold(12.5), t.text, w - 8.0);
        egui::Popup::menu(&head).id(egui::Id::new(("filterColumnField", i))).show(|ui| {
            for (k, label) in COLUMN_FIELDS {
                let resp = ui.selectable_label(k == field, crate::i18n::tr(label));
                register(ui.ctx(), format!("filterColumnField:{i}:{k}"), resp.rect);
                if resp.clicked() {
                    retitle = Some((i, k));
                }
            }
        });
        c.separator();
        // the sticky search box: it stays at the top while the values below scroll, and narrows them as it is
        // typed (issue #218); an emptied box shows every value again
        let mut query = app.ui.filter_column_search.get(i).cloned().unwrap_or_default();
        crate::text_field::TextField::singleline(&format!("field:filterColumnSearch:{i}"), &mut query)
            .hint(crate::i18n::tr("Search…"))
            .width((w - 12.0).max(24.0))
            .font(t.font(12.0))
            .margin(egui::Margin::symmetric(6, 2))
            .show(&mut c);
        if let Some(slot) = app.ui.filter_column_search.get_mut(i) {
            *slot = query.clone();
        }
        // the column's boolean operator for the multi-value fields (issue #683): any chosen value matches (OR) or
        // all of them must (AND); a photo has one camera, and the single-value fields (label, flag, rating, kind,
        // year) have nothing to combine
        let has_op = matches!(field.as_str(), "lens" | "keyword" | "person");
        if has_op {
            let op_key = format!("{field}Op");
            let op = match field.as_str() {
                "lens" => f.lens_op,
                "keyword" => f.keyword_op,
                _ => f.person_op,
            };
            c.horizontal(|c| {
                c.spacing_mut().item_spacing.x = 4.0;
                let buttons = [
                    ("Any", lcc_catalog::MatchOp::Any, "Match any of the chosen values (OR)"),
                    ("All", lcc_catalog::MatchOp::All, "Match all of the chosen values (AND)"),
                ];
                for (label, want, tip) in buttons {
                    let resp = c.selectable_label(op == want, egui::RichText::new(crate::i18n::tr(label)).font(t.font(11.5)));
                    register(ui.ctx(), format!("filterColumnOp:{i}:{label}"), resp.rect);
                    if resp.clicked() && op != want {
                        op_pick = Some((op_key.clone(), json!(if want == lcc_catalog::MatchOp::All { "all" } else { "any" })));
                    }
                    resp.on_hover_text(crate::i18n::tr(tip));
                }
            });
            c.separator();
        }
        let data = &facets[field.as_str()];
        let chosen: Vec<&str> = data["chosen"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
        let none_on = data["noneChosen"].as_bool().unwrap_or(false);
        let all = format!("{} ({})", crate::i18n::tr("All"), data["distinct"].as_u64().unwrap_or(0));
        if value_row(&mut c, format!("filterValue:{i}:0"), all, data["photos"].as_u64().unwrap_or(0), chosen.is_empty() && !none_on, true) {
            picked = Some(json!({"field": field, "value": null}));
            // clearing the column also puts its operator back to OR, so the next pick doesn't start in AND
            if has_op {
                op_pick = Some((format!("{field}Op"), json!("any")));
            }
        }
        egui::ScrollArea::vertical().id_salt(("filterColumnRows", i)).auto_shrink([false, false]).show(&mut c, |ui| {
            let values = data["values"].as_array().map(Vec::as_slice).unwrap_or(&[]);
            // the search box lets through only the rows whose text holds what is typed; a row keeps its place in
            // the full list, so clicking one picks the same value whatever is typed
            let mut shown = 0;
            for (n, v) in values.iter().enumerate() {
                let (Some(value), Some(count)) = (v["value"].as_str(), v["count"].as_u64()) else { continue };
                let label = value_label(field, value);
                if !matches(&query, &label) {
                    continue;
                }
                shown += 1;
                let on = chosen.iter().any(|c| c.eq_ignore_ascii_case(value));
                if value_row(ui, format!("filterValue:{i}:{}", n + 1), label, count, on, true) {
                    picked = Some(json!({"field": field, "value": value}));
                }
            }
            if shown == 0 && !query.trim().is_empty() {
                ui.add(egui::Label::new(egui::RichText::new(crate::i18n::tr("No matches")).font(t.font(12.0)).color(t.text_dim)));
            }
            let none = data["none"].as_u64().unwrap_or(0);
            // photos without a value (no keyword, no capture date, no title…) can be picked too (issue #777)
            if (none > 0 || none_on)
                && value_row(ui, format!("filterValue:{i}:{}", values.len() + 1), crate::i18n::tr("Unknown").to_string(), none, none_on, true)
            {
                picked = Some(json!({"field": field, "none": true}));
            }
        });
    }
    if let Some(args) = picked {
        let _ = app.run("library.facetPick", args);
    }
    if let Some((key, value)) = op_pick {
        filter(app, json!({key: value}));
    }
    if let Some((column, field)) = retitle {
        let _ = app.run("view.filterColumns", json!({"column": column, "field": field}));
    }
}
