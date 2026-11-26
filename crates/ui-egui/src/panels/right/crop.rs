use super::{LightcraftApp, PhotoId, Tokens, divider, header, json, padded, register, slider, text_button};

use craft::ui::choice::Checkbox;
use craft::ui::contract::{Bind, Props as _, Show as _};

/// Publish only the actual SDK response, never a speculative layout pass's target or edit.
fn crop_checkbox(ui: &mut egui::Ui, id: &str, label: &str, value: &mut bool) -> bool {
    let measuring = ui.is_sizing_pass() || craft::ui::runtime::measure::active(ui.ctx());
    let before = *value;
    let shown = Checkbox::new(Bind::bound(value)).id_salt(id).label(crate::i18n::tr(label).to_owned()).show(ui);
    if measuring {
        *value = before;
        return false;
    }
    // As in lens_blur_apply, the SDK adds the label to this same owner after
    // rendering the square. Publish its final real row, not the square-only rect.
    let response = ui.ctx().read_response(shown.response.id).unwrap_or_else(|| shown.response.clone());
    crate::widgets::register_response(id, response.rect, &response);
    shown.value().is_some() && *value != before
}

/// The aspect presets of the Crop panel: (menu label, `crop.aspect` value).
const ASPECT_PRESETS: [(&str, &str); 10] = [
    ("Free", "free"),
    ("Original", "original"),
    ("1 × 1", "1x1"),
    ("4 × 5 / 8 × 10", "4x5"),
    ("8.5 × 11", "8.5x11"),
    ("5 × 7", "5x7"),
    ("2 × 3 / 4 × 6", "2x3"),
    ("4 × 3", "4x3"),
    ("16 × 9", "16x9"),
    ("16 × 10", "16x10"),
];

/// What the aspect button says for the stored lock (`aspect` = width × 100, height × 100):
/// a preset's name when the ratio matches one in either orientation, "Original" for the photo's own
/// shape, otherwise the ratio as `1.37 : 1`.
pub(super) fn aspect_label(aspect: Option<(u32, u32)>, original: Option<f64>) -> String {
    let Some((w, h)) = aspect else { return "Free".into() };
    if w == 0 || h == 0 {
        return "Free".into();
    }
    let r = w as f64 / h as f64;
    let is = |q: f64| q.is_finite() && q > 0.0 && ((r - q).abs() / q < 0.004 || (1.0 / r - q).abs() / q < 0.004);
    if original.is_some_and(is) {
        return "Original".into();
    }
    for (label, key) in ASPECT_PRESETS {
        if let Some((x, y)) = key.split_once('x')
            && let (Ok(x), Ok(y)) = (x.parse::<f64>(), y.parse::<f64>())
            && is(x / y)
        {
            return label.to_string();
        }
    }
    format!("{:.2} : 1", r.max(1.0 / r))
}

fn aspect_real(ui: &egui::Ui) -> bool {
    !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx())
}

/// The custom fields keep their public locators; the SDK owners are stable for this photo.
/// Return/Enter submits the pair; Tab/blur only edits the local menu draft.
fn aspect_field(ui: &mut egui::Ui, owner: PhotoId, widget: &str, label: &str, value: &mut String) -> bool {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(46.0, 24.0), egui::Sense::hover());
    let out = craft::ui::text::TextField::new(crate::i18n::tr(label).to_owned(), Bind::bound(value))
        .id_salt(("crop-custom-field", owner.0, widget))
        .limit(None)
        .submits()
        .show_at(ui, rect);
    if aspect_real(ui) {
        crate::widgets::register_response(widget, out.response.rect, &out.response);
        if out.escaped {
            crate::widgets::take_escape(ui.ctx());
        }
    }
    aspect_real(ui) && ui.is_enabled() && out.submitted
}

/// "Custom" row of the aspect menu: two number fields and an Apply button (`crop.aspect` `[w, h]`).
/// The app-specific parser and limits are deliberately unchanged; typing never edits a photo.
fn custom_aspect(ui: &mut egui::Ui, owner: PhotoId) -> Option<serde_json::Value> {
    let key = egui::Id::unique(("crop-custom-aspect", owner.0));
    let (mut w, mut h): (String, String) = ui.data(|d| d.get_temp(key)).unwrap_or_else(|| ("3".into(), "2".into()));
    let mut apply = false;
    ui.horizontal(|ui| {
        ui.label(crate::i18n::tr("Custom"));
        apply |= aspect_field(ui, owner, "field:cropCustomW", "Width", &mut w);
        ui.label("×");
        apply |= aspect_field(ui, owner, "field:cropCustomH", "Height", &mut h);
        let go = craft::ui::buttons::Button::new(crate::i18n::tr("Apply")).id_salt(("crop-custom-apply", owner.0)).show(ui);
        if aspect_real(ui) {
            crate::widgets::register_response("button:cropCustomApply", go.response.rect, &go.response);
            apply |= go.clicked();
        }
    
    });
    if !aspect_real(ui) || !ui.is_enabled() {
        return None;
    }
    ui.data_mut(|d| d.insert_temp(key, (w.clone(), h.clone())));
    if apply {
        // Accept `3`, `2.5` and `3,5`; anything else (zero, negative, text, huge) is ignored.
        let num = |t: &str| t.trim().replace(',', ".").parse::<f64>().ok().filter(|v| v.is_finite() && *v > 0.0 && *v <= 1000.0);
        if let (Some(x), Some(y)) = (num(&w), num(&h))
            && (1.0 / lightcraft_geom::MAX_RATIO..=lightcraft_geom::MAX_RATIO).contains(&(x / y))
        {
            return Some(json!({"aspect": [x, y]}));
        }
    }
    None
}

/// The actual preset/custom popup used by the Crop panel. All commands retain their active
/// photo semantics; a menu belonging to an old/absent active photo cannot retarget another one.
fn aspect_picker(app: &mut LightcraftApp, ui: &mut egui::Ui, owner: PhotoId, original: Option<f64>) {
    use craft::ui::buttons::Button;
    use craft::ui::contract::ShowContainer as _;
    use craft::ui::contract::overlay::{Dismiss, Outside};
    use craft::ui::controls::disclosure::DisclosureGlyph;
    use craft::ui::menu::{Menu, MenuItem};
    use craft::ui::popover::Popover;

    let current = aspect_label(app.crop_tool_aspect(), original);
    let enabled = app.session.active() == Some(owner);
    let mut picked = None;
    let face = Button::new(crate::i18n::tr("Aspect Ratio"))
        .painted(crate::i18n::tr(&current).to_owned())
        .trailing_icon(DisclosureGlyph::dropdown())
        .id_salt(("crop-aspect", owner.0))
        .enabled(enabled)
        .show(ui);
    if !aspect_real(ui) {
        return;
    }
    crate::widgets::register_response("dropdown:cropAspect", face.response.rect, &face.response);
    let pass = ui.ctx().cumulative_pass_nr();
    let slot = face.response.id.with("menu-open");
    let mut open = ui
        .data(|d| d.get_temp::<(u64, bool)>(slot))
        .filter(|(last, _)| *last == pass || last.checked_add(1) == Some(pass))
        .is_some_and(|(_, open)| open)
        && enabled
        && ui.is_enabled();
    if enabled && ui.is_enabled() && face.clicked() {
        open = !open;
    }
    // This popup includes a validating form, not just menu actions. Menu's default
    // on-activate dismissal would close it even when Apply rejects invalid text.
    // Keep preset roving in an inline Menu; only a successful app-level pick closes
    // the form. The SDK's normal popup focus/escape/outside lifecycle stays in charge.
    // Exit presence is paint-only. In particular, a retained text root must not
    // receive typing or write the saved menu draft after the open binding closes.
    let body_interactive = open;
    Popover::new(crate::i18n::tr("Aspect Ratio"), Bind::bound(&mut open))
        .id_salt(("crop-aspect-menu", owner.0))
        .opened_by(&face.response)
        .dismiss(Dismiss::new().outside(Outside::Absorb))
        // Inline Menu supplies the roving group, but its host must supply the menu
        // scope used by keyboard_in_menu and menu-level key routing.
        .overlay(|overlay| overlay.focus(craft::ui::runtime::focus::FocusPolicy::MENU))
        .show(ui, |ui, cx| {
            if !body_interactive {
                ui.disable();
            }
            Menu::inline(crate::i18n::tr("Aspect Ratio")).id_salt(("crop-aspect-presets", owner.0)).values().show(ui, |ui, _| {
                for (label, value) in ASPECT_PRESETS {
                    let item = MenuItem::new(crate::i18n::tr(label).replace('&', "&&"))
                        .id_salt(("crop-aspect-preset", owner.0, value))
                        .entry_checked(current == label)
                        .keep_open(true)
                        .show(ui);
                    if aspect_real(ui) {
                        crate::widgets::register_response(format!("button:cropAspect-{value}"), item.response.rect, &item.response);
                        if ui.is_enabled() && item.ran {
                            picked = Some(json!({"aspect": value}));
                        }
                    }
                }
            });
            ui.separator();
            if let Some(custom) = custom_aspect(ui, owner) {
                picked = Some(custom);
            }
            if picked.is_some() {
                cx.request_close();
            }
        });
    ui.data_mut(|d| d.insert_temp(slot, (pass, open)));
    if ui.is_enabled()
        && enabled
        && app.session.active() == Some(owner)
        && let Some(payload) = picked
    {
        let _ = app.run("crop.aspect", payload);
    }
}

/// The Crop and Geometry panels for photo `id`: the aspect ratio and its lock, the angle, the
/// Overlay dropdown (with Flip Overlay for Triangle and Spiral) and the Upright dropdown, where
/// Guided starts drawing guides on the photo and any other mode stops it.
pub(super) fn crop(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId) {
    let d = app.session.develop_of(id).unwrap_or_default();
    header(ui, "Crop");
    padded(ui, |ui| {
        let original = app.session.catalog.photo(id).map(|p| {
            let (w, h) = (p.width.max(1) as f64, p.height.max(1) as f64);
            if d.orientation.swaps_axes() { h / w } else { w / h }
        });
        // wrapped: the label, the ratio and Lock are wider than a narrow panel, and a row wider than the panel would
        // widen the whole column (the sliders' values and the buttons below ran past its edge)
        ui.horizontal_wrapped(|ui| {
            ui.label(crate::i18n::tr("Aspect Ratio"));
            aspect_picker(app, ui, id, original);
            let locked = app.crop_tool_aspect().is_some();
            if text_button(ui, "cropLock", if locked { "Locked" } else { "Lock" }, locked).clicked() {
                let _ = app.run("crop.aspect", json!({"aspect": "toggle"}));
            }
        });
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            if text_button(ui, "cropRotateLeft", crate::i18n::tr("Rotate Left"), false).clicked() {
                let _ = app.run("photo.rotateLeft", json!({}));
            }
            if text_button(ui, "cropRotateRight", crate::i18n::tr("Rotate Right"), false).clicked() {
                let _ = app.run("photo.rotateRight", json!({}));
            }
        });
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            if text_button(ui, "cropFlipH", crate::i18n::tr("Flip H"), d.crop.flip_h).clicked() {
                let _ = app.run("photo.flipHorizontal", json!({}));
            }
            if text_button(ui, "cropFlipV", crate::i18n::tr("Flip V"), d.crop.flip_v).clicked() {
                let _ = app.run("photo.flipVertical", json!({}));
            }
            if text_button(ui, "cropSwapAspect", crate::i18n::tr("Swap Aspect (X)"), false).clicked() {
                let _ = app.run("crop.rotateAspect", json!({}));
            }
            if text_button(ui, "cropReset", crate::i18n::tr("Reset"), false).clicked() {
                let _ = app.run("crop.reset", json!({}));
            }
        });
    });
    let ang = lightcraft_develop::controls::find("crop.angle").copied();
    if let Some(spec) = ang {
        let out = slider(ui, &spec, d.crop.geometry.angle, true, Some("Straighten"));
        crate::panels::edit::apply_slider_out(app, &spec, out, |app, v| app.run("crop.straighten", json!({"angle": v})));
        // an exact angle, typed (issue #534): a framework number field of its own, besides the slider's value
        padded(ui, |ui| {
            ui.horizontal(|ui| {
                use craft::behavior::{EditEvent, NumberCodec, Reader};
                use craft::ui::text::NumberField;
                ui.label(crate::i18n::tr("Angle"));
                let real = !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx());
                let number_spec = spec;
                let shown = NumberField::new(crate::i18n::tr("Angle").to_owned(), Bind::given(Some(d.crop.geometry.angle)).reset(Some(spec.default)))
                    .id_salt(("cropAngleField", id))
                    .range(spec.min..=spec.max)
                    .step(spec.step)
                    // a drag on the field moves 0.05° a point, as before
                    .scrub_step(0.05)
                    .commit_decimals(2)
                    .fixed_decimals(2)
                    .unit("°".to_owned())
                    // read as the Straighten value reads a typed one: "-3,25" too
                    .codec(
                        NumberCodec::new(Reader::new(), 2)
                            .fixed()
                            // a turned angle carries its sign, as the readout writes it ("+2.50")
                            .explicit_plus()
                            .parse_with(move |text| crate::widgets::typed_value(&number_spec, text.trim().trim_end_matches('°'))),
                    )
                    .show(ui);
                if real {
                    crate::widgets::register_response("cropAngleField", shown.response.rect, &shown.response);
                }
                if real && ui.is_enabled() {
                    const OWNER: &str = "cropAngleField";
                    let angle = |value: f64| (value.clamp(spec.min, spec.max) * 100.0).round() / 100.0;
                    // a scrub of the field (the pointer held) is live and one undo step; typing is not: a typed angle
                    // applies on Return or on leaving the field, no half-typed angles (-3° on the way to -3.25°)
                    let held = ui.input(|input| input.pointer.primary_down());
                    for event in &shown.events {
                        let owned = app.ui.dragging_control.as_deref() == Some(OWNER);
                        match event {
                            EditEvent::Begin { .. } if held => {
                                if app.run("develop.beginInteraction", json!({"label": "Straighten"})).is_ok() {
                                    app.ui.dragging_control = Some(OWNER.into());
                                }
                            }
                            EditEvent::Update { value: Some(v), .. } if v.is_finite() && owned => {
                                let _ = app.run("crop.straighten", json!({"angle": angle(*v)}));
                            }
                            EditEvent::Commit { after, .. } | EditEvent::Set { after, .. } => {
                                if let Some(v) = after.filter(|v| v.is_finite())
                                    && (owned || angle(v) != d.crop.geometry.angle)
                                {
                                    let _ = app.run("crop.straighten", json!({"angle": angle(v)}));
                                }
                                if owned {
                                    let _ = app.run("develop.endInteraction", json!({}));
                                    app.ui.dragging_control = None;
                                }
                            }
                            // Esc keeps the old angle and gives the field up, as the old one did, doing nothing else: the
                            // next Esc is the crop tool's
                            EditEvent::Cancel { .. } => {
                                if owned {
                                    let _ = app.run("develop.cancelInteraction", json!({}));
                                    app.ui.dragging_control = None;
                                }
                                crate::widgets::take_escape(ui.ctx());
                                if let Some(rec) = craft::ui::runtime::focus::focused_rec(ui.ctx()).filter(|rec| rec.stop.id == shown.response.id) {
                                    ui.ctx().memory_mut(|memory| memory.surrender_focus(rec.stop.root));
                                }
                            }
                            _ => {}
                        }
                    }
                }
            });
        });
    }
    padded(ui, |ui| {
        ui.horizontal(|ui| {
            let on = app.ui.tool == "straighten";
            if text_button(ui, "straightenTool", crate::i18n::tr("Straighten Tool"), on)
                .on_hover_text(crate::i18n::tr("Drag along the horizon; double-click for Auto"))
                .clicked()
            {
                app.ui.tool = if on { String::new() } else { "straighten".into() };
            }
            if text_button(ui, "straightenAuto", crate::i18n::tr("Auto"), false).clicked() {
                app.auto_straighten(ui.ctx());
            }
        });
    });
    // Crop's composition overlay is a view preference, not a develop edit or a canvas action. Layout probes must
    // neither publish speculative choices nor activate the retained Flip action.
    let real = !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx());
    let interactive = real && ui.is_enabled() && app.session.active() == Some(id);
    padded(ui, |ui| {
        if !interactive {
            ui.disable();
        }
        use crate::state::CropOverlay as O;
        let overlays = [
            (O::Thirds, "Thirds", "thirds"),
            (O::Grid, "Grid", "grid"),
            (O::Golden, "Golden Ratio", "golden"),
            (O::Diagonal, "Diagonal", "diagonal"),
            (O::Triangle, "Triangle", "triangle"),
            (O::Spiral, "Spiral", "spiral"),
            (O::None, "None", "none"),
        ];
        // scoped to the photo: a menu opened (or a row held) for one photo never picks for another
        let picked = ui.push_id(("crop-overlay", id), |ui| crate::widgets::labeled_dropdown(ui, "cropOverlay", "Overlay", "", &overlays, app.ui.crop_overlay)).inner;
        if let Some(o) = picked
            && interactive
        {
            app.ui.crop_overlay = o;
        }
        if matches!(app.ui.crop_overlay, O::Triangle | O::Spiral)
            && text_button(ui, "cropOverlayOrient", crate::i18n::tr("Flip Overlay (⇧O)"), false).clicked()
            && interactive
        {
            let _ = app.run("view.cropOverlayOrientation", json!({}));
        }
    });
    divider(ui);
    header(ui, "Geometry");
    padded(ui, |ui| {
        use lightcraft_develop::Upright;
        let modes = [
            (Upright::Off, "Off", "off"),
            (Upright::Auto, "Auto", "auto"),
            (Upright::Guided, "Guided", "guided"),
            (Upright::Level, "Level", "level"),
            (Upright::Vertical, "Vertical", "vertical"),
            (Upright::Full, "Full", "full"),
        ];
        if let Some(mode) = crate::widgets::labeled_dropdown(ui, "upright", "Perspective Correction", "", &modes, d.geometry.upright) {
            let _ = app.run("geometry.upright", json!({"mode": mode}));
            // Guided starts drawing guides on the photo; any other mode stops it
            app.ui.tool = if mode == Upright::Guided { "guidedUpright".into() } else { String::new() };
        }
        let guided = d.geometry.upright == lightcraft_develop::Upright::Guided;
        ui.horizontal_wrapped(|ui| {
            if !guided
                && d.geometry.upright != lightcraft_develop::Upright::Off
                && text_button(ui, "uprightUpdate", crate::i18n::tr("Update"), false).clicked()
            {
                let mode = serde_json::to_value(d.geometry.upright).unwrap_or_default();
                let _ = app.run("geometry.upright", json!({"mode": mode}));
            }
            if guided {
                let drawing = app.ui.tool == "guidedUpright";
                if text_button(ui, "uprightDraw", crate::i18n::tr("Draw Guides"), drawing).clicked() {
                    app.ui.tool = if drawing { String::new() } else { "guidedUpright".into() };
                }
                if !d.geometry.guides.is_empty() && text_button(ui, "uprightClear", crate::i18n::tr("Clear Guides"), false).clicked() {
                    let _ = app.run("geometry.guides", json!({"guides": []}));
                }
            }
        });
        if guided {
            ui.label(
                egui::RichText::new(crate::i18n::tr_format!(
                    "{} of 4 guides — drag along lines that should be vertical or horizontal.",
                    d.geometry.guides.len()
                ))
                .size(11.0)
                .color(Tokens::get(ui.ctx()).text_dim),
            );
        }
        ui.add_space(4.0);
        let mut c = d.geometry.constrain_crop;
        if crop_checkbox(ui, "check:crop.constrain", "Constrain Crop", &mut c) {
            let _ = app.run("develop.merge", json!({"settings": {"geometry": {"constrain_crop": c}}, "label": "Constrain Crop"}));
        }
    });
    for spec in lightcraft_develop::controls::in_section(lightcraft_develop::Section::Geometry).filter(|c| c.id != "crop.angle") {
        let v = lightcraft_develop::controls::get(&d, spec.id).unwrap_or(spec.default);
        let out = slider(ui, spec, v, true, None);
        crate::panels::edit::apply_slider_out(app, spec, out, |app, v| app.run("develop.set", json!({"control": spec.id, "value": v})));
    }
}

/// The Remove panel for photo `id`: the Mode dropdown, where None (shown while no brush is chosen)
/// lets spots be selected and moved without painting new ones, Find Dust Spots, the brush settings
/// (which edit the selected spot too) and the Tool Overlay choice.
pub(super) fn remove(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId) {
    let real = !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx());
    let interactive = real && ui.is_enabled() && app.session.active() == Some(id);
    let gesture_key = egui::Id::unique("remove-brush-interaction-owner");
    // Commands use the selected spot. Do not let an old captured gesture become a
    // live edit of a new photo/spot, or leave its engine interaction open on disable.
    if real
        && let Some((photo, spot, control)) = ui.data(|d| d.get_temp::<(PhotoId, usize, String)>(gesture_key))
        && (!interactive || photo != id || app.session.active_spot != Some(spot))
    {
        if app.ui.dragging_control.as_deref() == Some(control.as_str()) {
            let _ = app.run("develop.cancelInteraction", json!({}));
            app.ui.dragging_control = None;
        }
        ui.data_mut(|d| d.remove::<(PhotoId, usize, String)>(gesture_key));
    }
    if !interactive {
        ui.disable();
    }
    let d = app.session.develop_of(id).unwrap_or_default();
    let selected = app.session.active_spot.filter(|index| d.spots.get(*index).is_some());
    // The common text_button and crop_checkbox already use SDK components. Keep
    // those adapters, including text_button's native route, rather than duplicate them.
    header(ui, "Remove");
    padded(ui, |ui| {
        ui.push_id(("remove-header", id), |ui| {
            // None, the first item, is what shows while no brush is chosen: the photo then takes no new
            // spots (`retouch.rs`), and existing ones can still be selected and moved
            let modes = [
                ("", "None", "none"),
                ("remove", "Remove", "remove"),
                ("fill", "Content-Aware", "fill"),
                ("heal", "Heal", "heal"),
                ("clone", "Clone", "clone"),
            ];
            let current = modes.iter().find(|(tool, _, _)| app.ui.tool == *tool).map_or("", |m| m.0);
            if let Some(tool) = crate::widgets::labeled_dropdown(ui, "removeMode", "Mode", "", &modes, current)
                && interactive
            {
                app.ui.tool = tool.into();
            }
            ui.add_space(8.0);
            ui.label(crate::i18n::tr_format!("{} spot(s) on this photo", d.spots.len()));
            ui.add_space(4.0);
            if text_button(ui, "findDust", crate::i18n::tr("Find Dust Spots"), false)
                .on_hover_text(crate::i18n::tr("Heal the small, soft dark spots sensor dust leaves on smooth areas"))
                .clicked()
                && interactive
            {
                match app.run("spot.findDust", json!({})) {
                    Ok(r) => {
                        let n = r["added"].as_u64().unwrap_or(0);
                        app.toast(
                            ui.ctx(),
                            if n == 0 {
                                crate::i18n::tr("No dust spots found").to_string()
                            } else {
                                crate::i18n::tr_format!("Healed {n} dust spot{}", if n == 1 { "" } else { "s" }, n = n)
                            },
                        );
                    }
                    Err(e) => app.toast(ui.ctx(), e),
                }
            }
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(crate::i18n::tr("Paint over a distraction on the photo to remove it.")).color(Tokens::get(ui.ctx()).text_dim),
            );
        });
    });
    // Brush settings; with a spot selected they edit exactly that spot too.
    let sel = selected.and_then(|index| d.spots.get(index).map(|spot| (index, spot.clone())));
    if let Some((i, sp)) = &sel {
        if interactive {
            (app.ui.remove_size, app.ui.remove_feather, app.ui.remove_opacity) = (sp.size as f32, sp.feather as f32, sp.opacity as f32);
        }
        divider(ui);
        let mode = match sp.mode {
            lightcraft_develop::SpotMode::Heal => "Heal",
            lightcraft_develop::SpotMode::Clone => "Clone",
            lightcraft_develop::SpotMode::Remove => "Remove",
            lightcraft_develop::SpotMode::Fill => "Content-Aware",
        };
        crate::panels::edit::sub_title(ui, &crate::i18n::tr_format!("{mode} spot {} of {}", i + 1, d.spots.len(), mode = crate::i18n::tr(mode)));
    }
    let plain = |id: &'static str, label: &'static str, min: f64, max: f64, default: f64| lightcraft_develop::ControlSpec {
        id,
        label,
        section: lightcraft_develop::Section::Detail,
        min,
        max,
        default,
        step: 1.0,
        decimals: 0,
        track: lightcraft_develop::Track::Plain,
    };
    // Local model mapping only: the SDK owns editing/gestures and the existing
    // apply_slider_out adapter owns engine begin/update/end and Undo grouping.
    let field = |ui: &mut egui::Ui, spec: &lightcraft_develop::ControlSpec, value: f64, enabled: bool| {
        // a draft or gesture belongs to the photo and spot it began on
        let out = crate::widgets::slider_salted(ui, spec, value, interactive && enabled, None, egui::Id::new(("remove-field", id, selected)));
        (out, out.cancelled)
    };
    let sliders = [
        (plain("ui.removeSize", "Size", 1.0, 250.0, 20.0), "size", sel.as_ref().map_or(app.ui.remove_size as f64, |(_, sp)| sp.size) * 1000.0),
        (plain("ui.removeFeather", "Feather", 0.0, 100.0, 50.0), "feather", sel.as_ref().map_or(app.ui.remove_feather as f64, |(_, sp)| sp.feather)),
        (plain("ui.removeOpacity", "Opacity", 0.0, 100.0, 100.0), "opacity", sel.as_ref().map_or(app.ui.remove_opacity as f64, |(_, sp)| sp.opacity)),
    ];
    for (spec, key, v) in sliders {
        let (out, cancelled) = field(ui, &spec, v, true);
        let scale = if key == "size" { 1000.0 } else { 1.0 };
        if interactive && let Some(v) = out.value {
            match key {
                "size" => app.ui.remove_size = (v / scale) as f32,
                "feather" => app.ui.remove_feather = v as f32,
                _ => app.ui.remove_opacity = v as f32,
            }
        }
        if cancelled {
            if ui
                .data(|d| d.get_temp::<(PhotoId, usize, String)>(gesture_key))
                .is_some_and(|(photo, spot, control)| photo == id && Some(spot) == selected && control == spec.id)
            {
                if app.ui.dragging_control.as_deref() == Some(spec.id) {
                    let _ = app.run("develop.cancelInteraction", json!({}));
                    app.ui.dragging_control = None;
                }
                ui.data_mut(|d| d.remove::<(PhotoId, usize, String)>(gesture_key));
            }
        } else if interactive && let Some(index) = selected {
            if out.drag_started {
                ui.data_mut(|d| d.insert_temp(gesture_key, (id, index, spec.id.to_owned())));
            }
            crate::panels::edit::apply_slider_out(app, &spec, out, |app, v| app.run("spot.update", json!({key: v / scale})));
            if out.drag_stopped {
                ui.data_mut(|d| d.remove::<(PhotoId, usize, String)>(gesture_key));
            }
        }
    }
    if sel.is_some() {
        padded(ui, |ui| {
            ui.push_id(("remove-selected-actions", id, selected), |ui| {
                ui.horizontal(|ui| {
                    if text_button(ui, "spotRefresh", crate::i18n::tr("Refresh Source (/)"), false).clicked() && interactive {
                        let _ = app.run("spot.refreshSource", json!({}));
                    }
                    if text_button(ui, "spotDelete", crate::i18n::tr("Delete (⌫)"), false).clicked() && interactive {
                        let _ = app.run("spot.delete", json!({}));
                    }
                });
            });
        });
    }
    padded(ui, |ui| {
        let mut v = app.ui.visualize_spots;
        if crop_checkbox(ui, "check:remove.visualizeSpots", "Visualize Spots (A)", &mut v) && interactive {
            app.ui.visualize_spots = v;
        }
    });
    let spec = plain("ui.spotsThreshold", "Threshold", 0.0, 100.0, 50.0);
    let (out, _) = field(ui, &spec, app.ui.spots_threshold as f64, app.ui.visualize_spots);
    if interactive
        && app.ui.visualize_spots
        && let Some(v) = out.value
    {
        app.ui.spots_threshold = v as f32;
    }
    padded(ui, |ui| {
        tool_overlay_options(app, ui);
        ui.add_space(4.0);
        ui.push_id(("remove-clear", id), |ui| {
            if !d.spots.is_empty() && text_button(ui, "removeClear", crate::i18n::tr("Delete all spots"), false).clicked() && interactive {
                // Preserve the original reverse indexed commands and their Undo steps.
                for i in (0..d.spots.len()).rev() {
                    let _ = app.run("spot.delete", json!({"index": i}));
                }
            }
        });
    });
}

/// Tool Overlay: when the spot and mask pins show on the photo (Auto / Always / Selected / Never;
/// Shift+H steps through them). Remove and Masking panels.
pub(crate) fn tool_overlay_options(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    use crate::state::{RightPanel, ToolOverlay};
    use craft::ui::buttons::Button;
    use craft::ui::contract::{Bind, Props as _, Show as _, ShowContainer as _, Trigger};
    use craft::ui::controls::disclosure::DisclosureGlyph;
    use craft::ui::menu::{Menu, MenuItem};

    let real = !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx());
    let owner = app.session.active();
    let masking = app.ui.right == RightPanel::Masking;
    let eligible = owner.is_some() && (!masking || app.ui.mask_pins);
    let current = app.ui.tool_overlay;
    let t = Tokens::get(ui.ctx());
    let mut picked = None;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(crate::i18n::tr("Tool Overlay")).color(t.text_dim));
        let face = Button::new(crate::i18n::tr("Tool Overlay"))
            .painted(crate::i18n::tr(current.label()).to_owned())
            .trailing_icon(DisclosureGlyph::dropdown())
            .tooltip(crate::i18n::tr("When pins show on the photo (Shift+H cycles)"))
            .id_salt(("tool-overlay", owner, masking))
            .enabled(real && eligible)
            .show(ui);
        if !real {
            return;
        }
        crate::widgets::register_response("dropdown:toolOverlay", face.response.rect, &face.response);
        // As in curve_footer, the real SDK button activation opens for pointer,
        // Enter and Space. The SDK owns menu focus, arrows, Tab and dismissal.
        let pass = ui.ctx().cumulative_pass_nr();
        let slot = face.response.id.with("menu-open");
        let mut open = ui
            .data(|data| data.get_temp::<(u64, bool)>(slot))
            .filter(|(last, _)| *last == pass || last.checked_add(1) == Some(pass))
            .is_some_and(|(_, open)| open)
            && eligible
            && ui.is_enabled();
        if eligible && ui.is_enabled() && face.clicked() {
            open = !open;
        }
        let body_interactive = open;
        Menu::new(crate::i18n::tr("Tool Overlay"))
            .id_salt(("tool-overlay-menu", owner, masking))
            .values()
            .tab_exits()
            .opened_by(&face.response, Trigger::Manual)
            .open(Bind::bound(&mut open))
            .show(ui, |ui, _| {
                // Menu measures this body inside the same parent pass. The outer
                // real flag cannot authorize aliases or actions in those callbacks.
                let body_real = !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx());
                // A closing layer may still paint; it must not keep live rows or
                // expose aliases after Escape, a pick, or parent disablement.
                if !body_interactive {
                    ui.disable();
                }
                for mode in ToolOverlay::ALL {
                    let row = MenuItem::new(crate::i18n::tr(mode.label()))
                        .id_salt(("tool-overlay-mode", owner, masking, mode.name()))
                        .entry_checked(mode == current);
                    let row = if mode == current { row.default_stop() } else { row };
                    let shown = row.show(ui);
                    if body_interactive && body_real {
                        crate::widgets::register_response(format!("toolOverlay:{}", mode.name()), shown.response.rect, &shown.response);
                        if ui.is_enabled() && shown.ran {
                            picked = Some(mode);
                        }
                    }
                }
            });
        ui.data_mut(|data| data.insert_temp(slot, (pass, open)));
    });
    if real
        && eligible
        && ui.is_enabled()
        && app.session.active() == owner
        && (app.ui.right == RightPanel::Masking) == masking
        && (!masking || app.ui.mask_pins)
        && let Some(mode) = picked
    {
        let _ = app.run("view.toolOverlay", json!({"mode": mode.name()}));
    }
}

#[cfg(test)]
#[path = "shared_tool_overlay_flow_tests.rs"]
mod shared_tool_overlay_flow_tests;

pub(super) fn red_eye(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId) {
    let d = app.session.develop_of(id).unwrap_or_default();

    // Numeric ownership belongs to the actual bound interaction. The earlier
    // mode/delete controls and commands only gain real-pass action eligibility.
    type Owner = (PhotoId, usize, usize, [u64; 4], bool);
    #[derive(Clone)]
    struct Gesture {
        owner: Owner,
        control: String,
        label: String,
        original: std::sync::Arc<lightcraft_develop::DevelopSettings>,
    }
    let real = !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx());
    let interactive = real && ui.is_enabled() && app.session.active() == Some(id);
    let selected = d.red_eye.get(app.ui.eye.min(d.red_eye.len().saturating_sub(1))).map(|eye| {
        (
            id,
            app.ui.eye.min(d.red_eye.len().saturating_sub(1)),
            d.red_eye.len(),
            [eye.center.x.to_bits(), eye.center.y.to_bits(), eye.rx.to_bits(), eye.ry.to_bits()],
            eye.pet,
        )
    });
    let gesture_key = egui::Id::unique("red-eye-numeric-interaction-owner");
    let previous_key = egui::Id::unique("red-eye-numeric-parent-pass");
    let gesture = ui.data(|data| data.get_temp::<Gesture>(gesture_key));
    let owns = |app: &LightcraftApp, gesture: &Gesture| {
        app.ui.dragging_control.as_deref() == Some(gesture.control.as_str())
            && app.session.interaction.as_ref().is_some_and(|interaction| {
                interaction.photo == gesture.owner.0
                    && interaction.label == gesture.label
                    && std::sync::Arc::ptr_eq(&interaction.original, &gesture.original)
            })
    };
    let pass = ui.ctx().cumulative_pass_nr();
    let previous = ui.data(|data| data.get_temp::<(Option<Owner>, bool, u64, u64)>(previous_key));
    let gap = previous.is_some_and(|(_, _, seen, _)| seen.saturating_add(1) < pass);
    let rebound = previous.is_some_and(|(owner, enabled, _, _)| owner != selected || enabled != interactive);
    let lost = gesture.as_ref().is_some_and(|gesture| !owns(app, gesture));
    let epoch = previous.map_or(0, |(_, _, _, epoch)| epoch.saturating_add(u64::from(gap || rebound || lost)));
    if real {
        if (gap || rebound || lost || !interactive || gesture.as_ref().is_some_and(|gesture| Some(gesture.owner) != selected))
            && let Some(gesture) = &gesture
        {
            // Cancel only the exact engine interaction this adapter began. A
            // foreign interaction with the same control name is not ours. The engine
            // keeps an edit committed after the drag's snapshot (Reset Edits run
            // mid-drag) instead of restoring over it.
            if owns(app, gesture) {
                let _ = app.run("develop.cancelInteraction", json!({}));
                app.ui.dragging_control = None;
            } else if app.session.interaction.is_none() && app.ui.dragging_control.as_deref() == Some(gesture.control.as_str()) {
                // A genuine panel/selection command may already have committed it.
                app.ui.dragging_control = None;
            }
            ui.data_mut(|data| data.remove::<Gesture>(gesture_key));
        }
        ui.data_mut(|data| data.insert_temp(previous_key, (selected, interactive, pass, epoch)));
    }
    header(ui, "Red Eye");
    padded(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            for (label, pet) in [("Red Eye", false), ("Pet Eye", true)] {
                if text_button(ui, &format!("eyeMode-{}", if pet { "pet" } else { "red" }), label, app.ui.eye_pet == pet).clicked() && interactive {
                    app.ui.eye_pet = pet;
                }
            }
        });
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(crate::i18n::tr("Drag over an eye on the photo; the pupil inside is found automatically."))
                .color(Tokens::get(ui.ctx()).text_dim),
        );
    });
    let n = d.red_eye.len();
    if n == 0 {
        return;
    }
    if app.ui.eye >= n {
        app.ui.eye = n - 1;
    }
    let i = app.ui.eye;
    let eye = d.red_eye[i];
    divider(ui);
    crate::panels::edit::sub_title(
        ui,
        &crate::i18n::tr_format!(
            "{label} {index} of {n}",
            label = crate::i18n::tr(if eye.pet { "Pet Eye" } else { "Red Eye" }),
            index = i + 1,
            n = n
        ),
    );
    // find(indexed id) returns a TEMPLATE spec: retain the old unindexed
    // sliderLabel:/slider:/sliderValue: aliases; only the command payload is indexed.
    let field = |ui: &mut egui::Ui, spec: &lightcraft_develop::ControlSpec, value: f64| {
        // the record key callers read the row's parts under
        let row = crate::widgets::SliderRow {
            spec,
            value,
            enabled: interactive,
            label: None,
            salt: egui::Id::new(("red-eye-field", selected, epoch)),
            record: Some(egui::Id::unique(("red-eye-field-record", id, i, spec.id))),
        };
        let out = crate::widgets::slider_row(ui, &row);
        (out, out.cancelled)
    };
    for k in ["pupilSize", "darken"] {
        let cid = format!("redEye.{i}.{k}");
        let Some(spec) = lightcraft_develop::controls::find(&cid) else { continue };
        let v = lightcraft_develop::controls::get(&d, &cid).unwrap_or(spec.default);
        let (out, cancelled) = field(ui, spec, v);
        if cancelled {
            if let Some(gesture) = ui.data(|data| data.get_temp::<Gesture>(gesture_key))
                && gesture.control == spec.id
                && Some(gesture.owner) == selected
            {
                if owns(app, &gesture) {
                    let _ = app.run("develop.cancelInteraction", json!({}));
                    app.ui.dragging_control = None;
                }
                ui.data_mut(|data| data.remove::<Gesture>(gesture_key));
            }
        } else if interactive {
            crate::panels::edit::apply_slider_out(app, spec, out, |app, v| app.run("develop.set", json!({"control": cid, "value": v})));
            if out.drag_started
                && let (Some(owner), Some(interaction)) = (selected, app.session.interaction.as_ref())
            {
                ui.data_mut(|data| {
                    data.insert_temp(
                        gesture_key,
                        Gesture { owner, control: spec.id.to_owned(), label: interaction.label.clone(), original: interaction.original.clone() },
                    )
                });
            }
            if out.drag_stopped {
                ui.data_mut(|data| data.remove::<Gesture>(gesture_key));
            }
        }
    }
    padded(ui, |ui| {
        if eye.pet {
            let mut on = eye.catchlight.is_some();
            if crop_checkbox(ui, "check:redeye.catchlight", "Add Catchlight", &mut on) {
                let _ = app.run("redeye.catchlight", json!({"index": i, "on": on}));
            }
            ui.add_space(4.0);
        }
        ui.horizontal(|ui| {
            if text_button(ui, "eyeDelete", crate::i18n::tr("Delete"), false).clicked() && interactive {
                let _ = app.run("redeye.delete", json!({"index": i}));
            }
            if n > 1 && text_button(ui, "eyeDeleteAll", crate::i18n::tr("Delete all"), false).clicked() && interactive {
                for k in (0..n).rev() {
                    let _ = app.run("redeye.delete", json!({"index": k}));
                }
            }
        });
    });
}

#[cfg(test)]
#[path = "crop_checkboxes_external155_tests.rs"]
mod crop_checkboxes_external155_tests;

#[cfg(test)]
#[path = "crop_aspect_flow_tests.rs"]
mod crop_aspect_flow_tests;

#[cfg(test)]
#[path = "remove_controls_flow_tests.rs"]
mod remove_controls_flow_tests;

#[cfg(test)]
#[path = "red_eye_controls_flow_tests.rs"]
mod red_eye_controls_flow_tests;

#[cfg(test)]
#[path = "crop_overlay_flow_tests.rs"]
mod crop_overlay_flow_tests;
