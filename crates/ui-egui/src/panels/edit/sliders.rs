use super::{
    Align2, ControlSpec, DevelopSettings, Icon, LightcraftApp, PhotoId, Section, Sense, SliderOut, Tokens, Track, Value, controls, divider, json,
    pos2, register, section_header, slider, vec2,
};

/// Commit a slider interaction: begin → live updates → end, so a drag is one undo step.
pub fn apply_slider_out(
    app: &mut LightcraftApp,
    spec: &ControlSpec,
    out: SliderOut,
    mut set: impl FnMut(&mut LightcraftApp, f64) -> Result<Value, String>,
) {
    if out.drag_started && !out.reset {
        let _ = app.run("develop.beginInteraction", json!({"label": spec.label}));
        app.ui.dragging_control = Some(spec.id.to_string());
    }
    if let Some(v) = out.value {
        let _ = set(app, v);
    }
    if out.drag_stopped || out.reset {
        let _ = app.run("develop.endInteraction", json!({}));
        app.ui.dragging_control = None;
    }
}

pub(crate) fn control(app: &mut LightcraftApp, ui: &mut egui::Ui, d: &DevelopSettings, id: &str, enabled: bool) {
    let Some(spec) = controls::find(id) else { return };
    let v = controls::get(d, id).unwrap_or(spec.default);
    let out = slider(ui, spec, v, enabled, None);
    apply_slider_out(app, spec, out, |app, v| app.run("develop.set", json!({"control": id, "value": v})));
}

/// Relative temperature scale for rendered (non-raw) files: −100..100 ↔ Kelvin via mired shift.
pub(super) const REL_TEMP: ControlSpec = ControlSpec {
    id: "wb.tempRel",
    label: "Temp",
    section: Section::Color,
    min: -100.0,
    max: 100.0,
    default: 0.0,
    step: 1.0,
    decimals: 0,
    track: Track::Temp,
};

pub(super) const REL_TINT: ControlSpec = ControlSpec {
    id: "wb.tintRel",
    label: "Tint",
    section: Section::Color,
    min: -100.0,
    max: 100.0,
    default: 0.0,
    step: 1.0,
    decimals: 0,
    track: Track::Tint,
};

pub(super) fn k_to_rel(k: f64) -> f64 {
    ((1e6 / 6500.0 - 1e6 / k) / 0.8).clamp(-100.0, 100.0)
}

pub(super) fn rel_to_k(r: f64) -> f64 {
    1e6 / (1e6 / 6500.0 - r * 0.8)
}

/// The profile dropdown: Favorites, Recent, one submenu per group, then favourite toggle and
/// Browse…. Like the menu bar's menus, the dropdown and each submenu stay inside the window and
/// scroll when their rows don't fit (issue #2059: a folder of many imported profiles ran past the
/// window's bottom edge). `below` is the dropdown's bottom edge, which the menu hangs from.
pub(super) fn profile_menu(app: &mut LightcraftApp, ui: &mut egui::Ui, d: &DevelopSettings, below: f32) {
    crate::menu_level::level(ui, 1, Some(below), |ui| profile_menu_rows(app, ui, d, below));
}

fn profile_menu_rows(app: &mut LightcraftApp, ui: &mut egui::Ui, d: &DevelopSettings, below: f32) {
    use lightcraft_engine::presets::{PROFILES, profile_groups};
    let t = Tokens::get(ui.ctx());
    ui.set_min_width(200.0);
    let cur = d.profile.id.as_str();
    // (clicked, hovered)
    let mut pick: (Option<String>, Option<String>) = (None, None);
    let item = |ui: &mut egui::Ui, key: &str, id: &str, name: &str, pick: &mut (Option<String>, Option<String>)| {
        let r = ui.selectable_label(id == cur, crate::i18n::profile_label(id, name));
        register(ui, format!("profileMenu:{key}:{id}"), r.rect);
        if r.clicked() {
            pick.0 = Some(id.to_string());
        }
        if r.hovered() {
            pick.1 = Some(id.to_string());
        }
    };
    let heading = |ui: &mut egui::Ui, s: &str| {
        ui.label(egui::RichText::new(crate::i18n::tr(s)).font(t.semibold(11.5)).color(t.text_dim));
    };
    for (title, key, ids) in [("Favorites", "fav", app.session.profile_favorites.clone()), ("Recent", "recent", app.session.profile_recent.clone())] {
        let list: Vec<_> = ids.iter().filter_map(|id| app.session.profile_info(id).map(|(name, _)| (id.as_str(), name))).collect();
        if list.is_empty() {
            continue;
        }
        heading(ui, title);
        for (id, name) in list {
            item(ui, key, id, name, &mut pick);
        }
        ui.separator();
    }
    for g in profile_groups() {
        let r = crate::widgets::menu_button(ui, crate::i18n::tr(g), |ui| {
            crate::menu_level::level(ui, 2, Some(below), |ui| {
                ui.set_min_width(170.0);
                for p in PROFILES.iter().filter(|p| p.group == g) {
                    item(ui, "group", p.id, p.name, &mut pick);
                }
            });
        });
        register(ui, format!("profileMenu:groupMenu:{g}"), r.response.rect);
        if r.inner.is_some() {
            crate::menu_level::set_anchor(ui.ctx(), 2, r.response.rect);
        }
    }
    // imported LUT profile groups
    let mut lut_groups: Vec<String> = app.session.lut_profiles.iter().map(|p| p.group.clone()).collect();
    lut_groups.sort_unstable();
    lut_groups.dedup();
    for g in &lut_groups {
        // a folder's name as it is, and its own widget id even when it matches a built-in group
        let r = crate::widgets::menu_button(ui, g.as_str(), |ui| {
            crate::menu_level::level(ui, 2, Some(below), |ui| {
                ui.set_min_width(170.0);
                for p in app.session.lut_profiles.iter().filter(|p| &p.group == g) {
                    item(ui, "group", &p.id, &p.name, &mut pick);
                }
            });
        });
        register(ui, format!("profileMenu:lutGroupMenu:{g}"), r.response.rect);
        if r.inner.is_some() {
            crate::menu_level::set_anchor(ui.ctx(), 2, r.response.rect);
        }
    }
    ui.separator();
    if let Some((name, _)) = app.session.profile_info(cur) {
        let fav = app.session.profile_favorites.iter().any(|f| f == cur);
        let label = if fav {
            crate::i18n::tr_format!("Remove “{}” from Favorites", crate::i18n::profile_label(cur, name))
        } else {
            crate::i18n::tr_format!("Add “{}” to Favorites", crate::i18n::profile_label(cur, name))
        };
        let r = ui.button(label);
        register(ui, "profileMenu:toggleFavorite", r.rect);
        if r.clicked() {
            let _ = app.run("profile.favorite", json!({"id": cur}));
        }
    }
    let r = ui.button(crate::i18n::tr("Browse…"));
    register(ui, "profileMenu:browse", r.rect);
    if r.clicked() {
        let _ = app.run("panel.profiles", json!({}));
    }
    // resting on a profile previews it in the loupe
    if let Some(hovered_id) = &pick.1
        && hovered_id != cur
        && let Some((name, _)) = app.session.profile_info(hovered_id)
    {
        let mut s = d.clone();
        s.profile.id = hovered_id.clone();
        s.profile.amount = 100.0;
        app.hover_preview =
            Some(crate::HoverPreview { label: crate::i18n::tr_format!("Profile: {}", crate::i18n::profile_label(hovered_id, name)), settings: s });
    }
    if let Some(id) = pick.0 {
        let _ = app.run("develop.profile", json!({"id": id}));
    }
}

pub fn sub_title(ui: &mut egui::Ui, title: &str) {
    let t = Tokens::get(ui.ctx());
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
    ui.painter().text(pos2(r.left() + 24.0, r.center().y + 4.0), Align2::LEFT_CENTER, crate::i18n::tr(title), t.semibold(13.0), t.text_label);
}

pub(super) fn section(
    app: &mut LightcraftApp,
    ui: &mut egui::Ui,
    d: &DevelopSettings,
    id: &str,
    title: &str,
    body: impl FnOnce(&mut LightcraftApp, &mut egui::Ui, &DevelopSettings),
) {
    let open = app.ui.section_open(id);
    let (resp, toggled) = section_header(ui, id, title, open, Some(d.section_enabled(id)));
    if let Some(on) = toggled {
        let _ = app.run("develop.sectionEnabled", json!({"section": id, "enabled": on}));
    } else if resp.clicked() {
        // ⌥-click opens just this section, as in Lightroom Classic
        let solo = ui.input(|i| i.modifiers.alt);
        if let Err(e) = app.run(&format!("section.{id}"), json!({"solo": solo})) {
            app.toast(ui.ctx(), e);
        }
    }
    crate::widgets::context_menu(&resp, |ui| {
        let mut solo = app.ui.single_panel;
        let item = ui.checkbox(&mut solo, crate::i18n::tr("Solo Mode"));
        register(ui, "sectionMenu:solo", item.rect);
        if item.clicked() {
            if let Err(e) = app.run("view.soloSections", json!({"on": solo})) {
                app.toast(ui.ctx(), e);
            }
            ui.close();
        }
    });
    if open {
        body(app, ui, d);
    }
    divider(ui);
}

// ------------------------------------------------------------------------------ HDR

/// The Light panel's HDR row: the HDR switch and Visualize HDR, then (HDR on) the headroom limit.
/// The Light panel's HDR settings while HDR editing is on (the switch itself is the HDR button
/// next to B&W): Visualize HDR range and the headroom limit.
pub(super) fn hdr_row(app: &mut LightcraftApp, ui: &mut egui::Ui, d: &DevelopSettings) {
    if !d.hdr.enabled {
        return;
    }
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 4, bottom: 4 }).show(ui, |ui| {
        let mut v = app.ui.hdr_visualize;
        let r = ui.checkbox(&mut v, "Visualize HDR");
        register(ui, "check:hdrVisualize", r.rect);
        if r.on_hover_text("Grey below SDR white; yellow, orange, red, magenta up to 1, 2, 3 and over 3 stops above it").changed() {
            app.ui.hdr_visualize = v;
        }
    });
    control(app, ui, d, "hdr.maxEv", true);
}

/// Quick Develop (grid with several photos selected): relative steps applied to every selected
/// photo from its own value.
pub(super) fn quick_develop(app: &mut LightcraftApp, ui: &mut egui::Ui, n: usize) {
    let t = Tokens::get(ui.ctx());
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 0, bottom: 10 }).show(ui, |ui| {
        ui.label(egui::RichText::new(crate::i18n::tr_format!("Quick Develop · {n} photos", n = n)).color(t.text_label).size(12.5));
        ui.add_space(4.0);
        let rows: [(&str, &str, f64, f64); 9] = [
            ("Exposure", "light.exposure", 1.0 / 3.0, 1.0),
            ("Contrast", "light.contrast", 5.0, 20.0),
            ("Highlights", "light.highlights", 5.0, 20.0),
            ("Shadows", "light.shadows", 5.0, 20.0),
            ("Whites", "light.whites", 5.0, 20.0),
            ("Blacks", "light.blacks", 5.0, 20.0),
            ("Clarity", "effects.clarity", 5.0, 20.0),
            ("Vibrance", "color.vibrance", 5.0, 20.0),
            ("Temp", "wb.temp", 100.0, 500.0),
        ];
        egui::Grid::new("quick-develop").num_columns(5).spacing([4.0, 3.0]).show(ui, |ui| {
            for (label, ctl, small, big) in rows {
                ui.label(egui::RichText::new(crate::i18n::tr(label)).size(11.5).color(t.text_dim));
                // Keep the existing automation ids while drawing all step arrows as vectors.
                for (txt, icon, d) in [
                    ("◀◀", Icon::ChevronDoubleLeft, -big),
                    ("◀", Icon::ChevronLeft, -small),
                    ("▶", Icon::ChevronRight, small),
                    ("▶▶", Icon::ChevronDoubleRight, big),
                ] {
                    let r = crate::widgets::arrow_button(ui, "", icon, false);
                    r.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::Role::Button,
                            ui.is_enabled(),
                            crate::i18n::tr_format!("{label} {d:+} on every selected photo", d = d, label = crate::i18n::tr(label)),
                        )
                    });
                    crate::widgets::register(ui, format!("button:quick-{ctl}-{txt}"), r.rect);
                    if r.on_hover_text(crate::i18n::tr_format!("{label} {d:+} on every selected photo", d = d, label = crate::i18n::tr(label)))
                        .clicked()
                    {
                        let _ = app.run("develop.quickAdjust", json!({"control": ctl, "delta": d}));
                    }
                }
                ui.end_row();
            }
        });
    });
}

/// The preview-only notice's Reload from Disk: re-read the photo's file (a raw a newer version can
/// decode, whose drive was offline when the library opened) in the background; a toast says what
/// came of it once it's done (`crate::reload`).
pub(crate) fn reload_preview_only(app: &mut LightcraftApp, ctx: &egui::Context, id: PhotoId) {
    match app.run("photo.reload", json!({"ids": [id.0]})) {
        Err(e) => app.toast_error(ctx, e),
        Ok(r) if r["background"] == true => {}
        // nothing to read in the background (a photo without a file): answered in place
        Ok(_) if app.session.catalog.photo(id).is_some_and(|p| p.preview_only.is_none()) => {
            app.toast(ctx, crate::i18n::tr("This raw is now developed from its raw data"))
        }
        Ok(_) => app.toast(ctx, crate::i18n::tr("This raw variant still can't be decoded")),
    }
}

/// Which of the Edit header's buttons were clicked.
#[derive(Default)]
pub(super) struct HeaderClicks {
    pub(super) auto: bool,
    pub(super) bw: bool,
    pub(super) hdr: bool,
    pub(super) reset: bool,
}

/// The Edit header's buttons: Auto, B&W and HDR on the left, Reset on the right. A button that doesn't fit on the
/// row starts the next one (long translations in a narrow panel, such as German *Zurücksetzen* beside HDR), so none
/// covers another; Reset stays right-aligned on whichever row it lands.
pub(super) fn header_buttons(ui: &mut egui::Ui, bw: bool, hdr: bool, reset_tip: &str) -> HeaderClicks {
    const GAP: f32 = 8.0;
    let mut clicks = HeaderClicks::default();
    let avail = ui.available_width();
    let left = [("auto", "Auto", false), ("bw", "B&W", bw), ("hdr", "HDR", hdr)];
    // rows of `left` indices, filled greedily; `used` is the width taken on the current (last) row
    let mut rows: Vec<Vec<usize>> = Vec::new();
    let mut row: Vec<usize> = Vec::new();
    let mut used = 0.0;
    for (i, (_, label, _)) in left.iter().enumerate() {
        let w = crate::widgets::text_button_size(ui, label);
        if row.is_empty() {
            used = w;
        } else if used + GAP + w > avail {
            rows.push(std::mem::take(&mut row));
            used = w;
        } else {
            used += GAP + w;
        }
        row.push(i);
    }
    rows.push(row);
    let reset_on_last = used + GAP + crate::widgets::text_button_size(ui, "Reset") <= avail;
    let reset = |ui: &mut egui::Ui, clicks: &mut HeaderClicks| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            clicks.reset = crate::widgets::text_button(ui, "reset", crate::i18n::tr("Reset"), false).on_hover_text(reset_tip).clicked();
        });
    };
    let last = rows.len() - 1;
    for (r, row) in rows.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            for &i in row {
                let (id, label, active) = left[i];
                let mut resp = crate::widgets::text_button(ui, id, crate::i18n::tr(label), active);
                if id == "hdr" {
                    // HDR editing, next to B&W as Lightroom has it
                    resp = resp.on_hover_text(crate::i18n::tr("Edit in HDR: highlights above SDR white, for HDR exports and displays"));
                }
                let clicked = resp.clicked();
                match id {
                    "auto" => clicks.auto = clicked,
                    "bw" => clicks.bw = clicked,
                    _ => clicks.hdr = clicked,
                }
            }
            if r == last && reset_on_last {
                reset(ui, &mut clicks);
            }
        });
    }
    if !reset_on_last {
        ui.horizontal(|ui| reset(ui, &mut clicks));
    }
    clicks
}

/// The Reset button's tooltip with the reset shortcut as this platform writes it (issue #1751).
pub(super) fn reset_tip(keymap: &crate::shortcuts::Keymap, mac: bool) -> String {
    let base = crate::i18n::tr("Reset all edits");
    match crate::shortcuts::shortcut_of(keymap, "develop.reset") {
        Some(sc) => format!("{base} ({})", crate::menubar::shortcut_text(sc, mac)),
        None => base.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Edit header's buttons never cover each other or run past the panel, in any language: a button that doesn't
    /// fit starts the next row, and Reset stays right-aligned. Before, Reset covered HDR in 11 of the 18 languages at
    /// the default 222 pt (German *Zurücksetzen* among them) and in all but the two Chinese ones at some width. 200 pt
    /// is about the narrowest panel (`RIGHT_WIDTH.min` less the margins).
    #[test]
    fn edit_header_buttons_do_not_overlap_in_any_language() {
        use crate::i18n::Locale;
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        let mut wrapped = Vec::new();
        for &locale in Locale::ALL {
            crate::i18n::set_language(locale);
            for width in [200.0, 222.0, 260.0, 400.0] {
                let mut rects = Vec::new();
                for frame in 0..2 {
                    let raw = crate::headless::HeadlessView::raw_input(vec2(width, 200.0), 1.0, frame as f64 / 60.0, vec![]);
                    let mut out = ctx.run_ui(raw, |ui| {
                        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
                            header_buttons(ui, false, false, "");
                        });
                    });
                    out.textures_delta.clear();
                    rects = crate::widgets::take_registry(&ctx);
                }
                let get = |id: &str| rects.iter().find(|(i, _)| i == id).map(|(_, r)| *r).unwrap();
                let buttons = ["button:auto", "button:bw", "button:hdr", "button:reset"].map(get);
                for (i, a) in buttons.iter().enumerate() {
                    assert!(a.right() <= width + 0.5, "{locale:?} {width}: button {i} past the edge: {a:?}");
                    for b in &buttons[i + 1..] {
                        assert!(!a.intersects(b.shrink(0.5)), "{locale:?} {width}: {a:?} overlaps {b:?}");
                    }
                }
                let [_, _, hdr, reset] = buttons;
                assert!((reset.right() - width).abs() < 0.5, "{locale:?} {width}: Reset stays right-aligned: {reset:?}");
                if reset.top() > hdr.top() {
                    wrapped.push((locale, width));
                }
            }
        }
        crate::i18n::set_language(Locale::En);
        assert!(wrapped.contains(&(Locale::De, 222.0)), "German wraps in the default panel: {wrapped:?}");
        // English keeps its single row from the default width up
        assert!(!wrapped.iter().any(|&(l, w)| l == Locale::En && w >= 222.0), "{wrapped:?}");
    }
}
