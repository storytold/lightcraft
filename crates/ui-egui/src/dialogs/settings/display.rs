use super::appearance::appearance;
use super::widgets::{check, choices, heading, hint, path_label, row};
use super::{GridBadges, LightcraftApp, RichText, Tokens, Value, json, register};

// ---------------------------------------------------------------------------------- Interface

/// The monitor profile (`app.displayProfile`): previews are shown through the display's ICC
/// profile, so wide-gamut and calibrated displays show colours as they are.
pub(super) fn display_tab(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    monitor_profile_sdk(app, ui, t);
}

fn monitor_profile_sdk(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    use craft::ui::contract::{Props as _, Show as _};
    let real = !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx());
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
        let r =
            craft::ui::buttons::Button::new(crate::i18n::tr("Choose Profile…").to_owned()).id_salt("settingsDisplayProfile").enabled(can).show(ui);
        if real {
            crate::widgets::register_response("button:settingsDisplayProfile", r.response.rect, &r.response);
        }
        if real
            && ui.is_enabled()
            && can
            && r.clicked()
            && let Some(path) = app.services.pick_display_profile.as_mut().and_then(|pick| pick().into_iter().next())
        {
            app.ui.settings.display_profile = path;
        }
        // with no profile chosen, previews follow the screen's own profile where the host knows it
        let none = if app.services.screen_profile.is_some() { crate::i18n::tr("Use Screen Profile") } else { crate::i18n::tr("Use sRGB") };
        let enabled = !app.ui.settings.display_profile.is_empty();
        let r = craft::ui::buttons::Button::new(none.to_owned()).id_salt("settingsDisplayProfileNone").enabled(enabled).show(ui);
        if real {
            crate::widgets::register_response("button:settingsDisplayProfileNone", r.response.rect, &r.response);
        }
        if real && ui.is_enabled() && enabled && r.clicked() {
            app.ui.settings.display_profile.clear();
        }
        if !can {
            hint(ui, t, crate::i18n::tr("not available here"));
        }
    });
    if let Some(e) = &app.display_error {
        ui.label(RichText::new(e).size(11.0).color(t.reject));
    }
    if let Some(d) = &current {
        let info = d.describe();
        if !screen {
            row(ui, t, crate::i18n::tr("File"), |ui| {
                path_label(ui, "settingsDisplayFile", &d.path, egui::FontId::proportional(11.0), t.text_dim, 0.0);
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
            ui.label(RichText::new(text).size(11.0).color(t.text_dim));
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

pub(super) fn interface_tab(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
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
    heading(ui, t, crate::i18n::tr("Detail View"));
    check(ui, "settings.navigator", &mut app.ui.navigator, "Show the Navigator while zoomed in");
    row(ui, t, crate::i18n::tr("Info overlay"), |ui| {
        use crate::state::InfoOverlay as I;
        choices(ui, "settingsInfo", &[(I::Off, "Off"), (I::Basic, "Info 1"), (I::Exposure, "Info 2")], &mut app.ui.info_overlay);
    });
    hint(ui, t, crate::i18n::tr("View → View Options chooses the lines Info 1 and Info 2 show."));
}

/// View ▸ View Options (⌘J), Lightroom Classic's Loupe View tab: whether the info overlay shows and
/// which, the three lines each of Info 1 and Info 2 shows, which of them shows briefly when the
/// photo changes, and whether the loupe shows its loading and rendering message
/// (`view.viewOptions`). Changes apply as they are made.
pub(in super::super) fn view_options(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens) {
    use crate::state::{InfoOverlay as I, LoupeInfo, LoupeInfoField as F};
    heading(ui, t, crate::i18n::tr("Loupe View"));
    let mut show = app.ui.info_overlay != I::Off;
    if check(ui, "viewOptions.showInfo", &mut show, "Show Info Overlay") {
        app.ui.info_overlay = if show { I::Basic } else { I::Off };
    }
    ui.add_enabled_ui(show, |ui| {
        ui.horizontal(|ui| {
            choices(ui, "viewOptionsInfo", &[(I::Basic, "Info 1"), (I::Exposure, "Info 2")], &mut app.ui.info_overlay);
        });
    });
    for (n, title, defaults) in [(1, "Loupe Info 1", LoupeInfo::INFO1), (2, "Loupe Info 2", LoupeInfo::INFO2)] {
        heading(ui, t, crate::i18n::tr(title));
        let lines = if n == 1 { &mut app.ui.settings.loupe_info.info1 } else { &mut app.ui.settings.loupe_info.info2 };
        for (i, line) in lines.iter_mut().enumerate() {
            let id = format!("viewOptionsInfo{n}Line{}", i + 1);
            let name = crate::i18n::tr_format!("{title}: {line}", title = crate::i18n::tr(title), line = crate::i18n::tr(line.label()));
            use craft::ui::buttons::Button;
            use craft::ui::contract::{Bind, Props as _, Show as _, ShowContainer as _, Trigger};
            use craft::ui::controls::disclosure::DisclosureGlyph;
            use craft::ui::menu::{Menu, MenuItem};

            let (rect, _) = ui.allocate_exact_size(egui::vec2(220.0, 24.0), egui::Sense::hover());
            if ui.is_sizing_pass() || craft::ui::runtime::measure::active(ui.ctx()) {
                continue;
            }
            let face = Button::new(name.clone())
                .painted(crate::i18n::tr(line.label()).to_owned())
                .trailing_icon(DisclosureGlyph::dropdown())
                .id_salt(("view-options-line", &id))
                .show_at(ui, rect);
            crate::widgets::register_response(format!("combo:{id}"), rect, &face.response);
            // A value menu retains the existing semantic option locators, unlike indexed
            // Select rows. Only activation commits; navigating or dismissing edits nothing.
            let mut picked = None;
            // The SDK's external Click opener is pointer-press-only. Let the production
            // button's actual activation (pointer, keyboard or AT) own the toggle instead.
            // Stale state expires when this field leaves the presented subtree.
            let pass = ui.ctx().cumulative_pass_nr();
            let slot = face.response.id.with("menu-open");
            let mut open = ui
                .data(|data| data.get_temp::<(u64, bool)>(slot))
                .filter(|(last, _)| *last == pass || last.saturating_add(1) == pass)
                .is_some_and(|(_, open)| open);
            if ui.is_enabled() && face.clicked() {
                open = !open;
            }
            Menu::new(name)
                .id_salt(("view-options-menu", &id))
                .values()
                .opened_by(&face.response, Trigger::Manual)
                .open(Bind::bound(&mut open))
                .show(ui, |ui, _| {
                    for f in F::ALL {
                        let key = serde_json::to_value(f).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
                        let measuring = ui.is_sizing_pass() || craft::ui::runtime::measure::active(ui.ctx());
                        // These are literal translated captions, not mnemonic-marked menu text.
                        let caption = crate::i18n::tr(f.label()).replace('&', "&&");
                        let row = MenuItem::new(caption).id_salt((&id, &key)).entry_checked(line == f);
                        let row = if line == f { row.default_stop() } else { row };
                        let row = row.show(ui);
                        if !measuring {
                            crate::widgets::register_response(format!("button:{id}-{key}"), row.response.rect, &row.response);
                            if ui.is_enabled() && row.ran {
                                picked = Some(*f);
                            }
                        }
                    }
                });
            ui.data_mut(|data| data.insert_temp(slot, (pass, open)));
            if ui.is_enabled()
                && let Some(field) = picked
            {
                *line = field;
            }
        
        }
        use craft::ui::buttons::Button;
        use craft::ui::contract::{Props as _, Show as _};
        let r =
            Button::new(crate::i18n::tr("Use Defaults").to_owned()).id_salt(("view-options-defaults", n)).enabled(*lines != defaults).show(ui);
        if !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx()) {
            crate::widgets::register_response(format!("button:viewOptionsDefaults{n}"), r.response.rect, &r.response);
            if ui.is_enabled() && *lines != defaults && r.clicked() {
                *lines = defaults;
            }
        }
    
        // one Info at most shows briefly: ticking one unticks the other
        let info = if n == 1 { I::Basic } else { I::Exposure };
        let mut briefly = app.ui.settings.loupe_info.briefly == info;
        if check(ui, &format!("viewOptions.briefly{n}"), &mut briefly, "Show briefly when photo changes") {
            app.ui.settings.loupe_info.briefly = if briefly { info } else { I::Off };
        }
    }
    heading(ui, t, crate::i18n::tr("General"));
    check(ui, "viewOptions.loupeMessages", &mut app.ui.settings.loupe_messages, "Show message when loading or rendering photos");
}

#[cfg(test)]
#[path = "loupe_view_options_flow_tests.rs"]
mod loupe_view_options_flow_tests;

// ----------------------------------------------------------------------------------- Slideshow

/// A playback duration slider (`slider:{id}`). While it is dragged the value changes in memory
/// (the slideshow reads it); letting go saves it through `slideshow.playback`.
pub(super) fn duration_slider(app: &mut LightcraftApp, ui: &mut egui::Ui, id: &str, key: &str, value: f64, range: std::ops::RangeInclusive<f64>) {
    let mut v = value;
    let r = ui.add(egui::Slider::new(&mut v, range).logarithmic(key == "slideDuration").suffix(" s").max_decimals(1));
    register(ui, format!("slider:{id}"), r.rect);
    if r.changed()
        && let Ok(next) = app.session.slideshow.merged(&json!({ key: v }))
    {
        app.session.slideshow = next;
    }
    if r.drag_stopped() || (r.changed() && !r.dragged()) {
        let _ = app.run("slideshow.playback", json!({ key: v }));
    }
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
#[path = "display_profile_flow_tests.rs"]
mod display_profile_flow_tests;
