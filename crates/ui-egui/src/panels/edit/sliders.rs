use super::{
    ControlSpec, DevelopSettings, Icon, LightcraftApp, PhotoId, Section, Sense, SliderOut, Tokens, Track, Value, controls, divider, json, pos2,
    register, section_header, vec2,
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
    // (the short name is for the eye; screen readers announce the full one)
    let out = crate::widgets::slider_named(ui, spec, v, enabled, short_name(id), None);
    apply_slider_out(app, spec, out, |app, v| app.run("develop.set", json!({"control": id, "value": v})));
}

/// The short name a slider shows under its group's caption (Grain ▸ Amount, Red Primary ▸ Hue,
/// as Lightroom Classic labels them), so a section's name column stays narrow and its tracks
/// long. The control keeps its full name everywhere else (commands, the console, menus, and what
/// screen readers announce).
pub(crate) fn short_name(id: &str) -> Option<&'static str> {
    Some(match id {
        "vignette.amount" | "grain.amount" | "detail.sharpenAmount" | "optics.defringePurple" | "optics.defringeGreen" => "Amount",
        "detail.nrLuminance" => "Luminance",
        "detail.nrColor" => "Color",
        "optics.profileDistortion" => "Distortion",
        "optics.profileVignetting" => "Vignetting",
        "optics.defringePurpleHueLo" | "optics.defringeGreenHueLo" => "Hue Low",
        "optics.defringePurpleHueHi" | "optics.defringeGreenHueHi" => "Hue High",
        "optics.caRed" => "Red/Cyan",
        "optics.caBlue" => "Blue/Yellow",
        "calibration.shadowsTint" => "Tint",
        "calibration.redHue" | "calibration.greenHue" | "calibration.blueHue" => "Hue",
        "calibration.redSat" | "calibration.greenSat" | "calibration.blueSat" => "Saturation",
        "hdr.sdrBrightness" => "Brightness",
        "hdr.sdrContrast" => "Contrast",
        "hdr.sdrHighlights" => "Highlights",
        "hdr.sdrShadows" => "Shadows",
        "hdr.sdrWhites" => "Whites",
        "hdr.sdrClarity" => "Clarity",
        _ => return None,
    })
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
    shade: lightcraft_develop::Shade::Plain,
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
    shade: lightcraft_develop::Shade::Plain,
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
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
    let left = r.left() + crate::widgets::PAD_L;
    let right = r.right() - crate::widgets::PAD_R;
    let g = crate::widgets::one_line(ui.painter(), crate::i18n::tr(title), t.semibold(11.0), t.text_dim, (right - left).max(1.0));
    let y = r.center().y + 2.0;
    let text_right = left + g.size().x;
    ui.painter().galley(pos2(left, y - g.size().y / 2.0), g, t.text_dim);
    if right > text_right + 12.0 {
        ui.painter().hline((text_right + 6.0)..=right, y.round() + 0.5, egui::Stroke::new(1.0, t.divider.gamma_multiply(0.75)));
    }
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
    egui::Frame::NONE.inner_margin(crate::widgets::row_margin(4, 4)).show(ui, |ui| {
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
    egui::Frame::NONE.inner_margin(crate::widgets::row_margin(0, 4)).show(ui, |ui| {
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
    /// Every short slider name has a translation in every catalog: a missing one would show the
    /// English name under a translated caption.
    #[test]
    fn every_short_name_is_translated() {
        let names: std::collections::BTreeSet<&str> = lightcraft_develop::CONTROLS.iter().filter_map(|c| super::short_name(c.id)).collect();
        assert!(names.contains("Hue Low") && names.contains("Brightness"), "{names:?}");
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("locales");
        let mut catalogs = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if !name.ends_with(".json") || name.ends_with("-formats.json") {
                continue;
            }
            catalogs += 1;
            let catalog: std::collections::BTreeMap<String, String> = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let missing: Vec<&&str> = names.iter().filter(|n| !catalog.contains_key(**n)).collect();
            assert!(missing.is_empty(), "{name} lacks {missing:?}");
        }
        assert!(catalogs > 0);
    }
}
