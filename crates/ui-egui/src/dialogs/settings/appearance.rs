use super::widgets::{choices, heading};
use super::{RichText, Tokens, register};

/// Appearance Mode (`button:settingsAppearance-{0,1,2}`: Auto, Dark, Light) above the light and
/// dark theme cards (`radio:theme-{id}`), each with a preview of the chosen theme. The card in
/// use is outlined. Applied on the next frame ([`LightcraftApp::sync_theme`]).
pub(super) fn appearance(ui: &mut egui::Ui, t: &Tokens, s: &mut crate::state::AppSettings, system: Option<egui::Theme>) {
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
        theme_card(
            ui,
            t,
            ThemeCard { title: "Light Theme", active: light_active, options: &light, width },
            &mut s.light_theme,
            &mut s.favorite_themes,
        );
        let dark: Vec<_> = DarkTheme::ALL.iter().map(|d| (*d, d.kind())).collect();
        theme_card(ui, t, ThemeCard { title: "Dark Theme", active: !light_active, options: &dark, width }, &mut s.dark_theme, &mut s.favorite_themes);
    };
    if available < 440.0 {
        ui.vertical(|ui| cards(ui, available));
    } else {
        ui.horizontal_top(|ui| cards(ui, ((available - 12.0) / 2.0).floor()));
    }
    ui.add_space(2.0);
}

/// What a theme family's card shows.
struct ThemeCard<'a, V> {
    title: &'a str,
    /// the family in use
    active: bool,
    options: &'a [(V, crate::theme::ThemeKind)],
    width: f32,
}

/// One family's card: its title (and "Active" when it is the one shown), a preview of the
/// selected theme and a radio button per theme.
fn theme_card<V: PartialEq + Copy>(ui: &mut egui::Ui, t: &Tokens, card: ThemeCard<V>, value: &mut V, favorites: &mut Vec<String>) {
    let ThemeCard { title, active, options, width } = card;
    egui::Frame::new()
        .fill(t.inset)
        .stroke(egui::Stroke::new(1.0, if active { t.accent } else { t.field_border }))
        .corner_radius(6)
        .inner_margin(8)
        .show(ui, |ui| {
            ui.vertical(|ui| {
                let inner = (width - 18.0).clamp(120.0, 240.0);
                ui.set_width(inner);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(crate::i18n::tr(title)).font(t.semibold(12.0)).color(t.text));
                    if active {
                        ui.label(RichText::new(crate::i18n::tr("In Use")).size(11.0).color(t.accent));
                    }
                });
                let shown = options.iter().find(|(v, _)| v == value).or(options.first()).map(|(_, k)| *k).unwrap_or_default();
                theme_preview(ui, shown, inner);
                let real = !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx());
                if !real {
                    // Preserve card/preview geometry, but speculative rows have no live owner or alias.
                    for _ in options {
                        ui.allocate_exact_size(egui::vec2(inner, 24.0), egui::Sense::hover());
                    }
                    return;
                }
                use craft::behavior::{NavShape, Proposal};
                use craft::theme::Variant;
                use craft::ui::buttons::Button;
                use craft::ui::choice::{Radio, RadioGroup};
                use craft::ui::contract::{Bind, Props as _, Show as _, ShowContainer as _};
                use craft::ui::runtime::env::Label;
                // Only radios belong to the family's roving arena. Favorite buttons are siblings,
                // so arrows never select a star and Tab can still acquire each visible action.
                let selected = options.iter().position(|(candidate, _)| candidate == value).unwrap_or(0) as u8;
                let radio_width = (inner - 24.0 - ui.spacing().item_spacing.x).max(24.0);
                let mut rows = Vec::with_capacity(options.len());
                let mut body = |ui: &mut egui::Ui, _: &mut craft::ui::contract::BodyCx<'_>| {
                    ui.set_min_width(radio_width);
                    ui.set_max_width(radio_width);
                    for (index, (_, kind)) in options.iter().enumerate() {
                        ui.allocate_ui_with_layout(egui::vec2(radio_width, 24.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            let response = Radio::new(index as u8)
                                .id_salt(("settings-theme-choice", title, kind.id()))
                                .label(crate::i18n::tr(kind.label()).to_owned())
                                .show(ui);
                            if !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx()) {
                                crate::widgets::register_response(format!("radio:theme-{}", kind.id()), response.interact_rect, &response);
                                rows.push((*kind, response.interact_rect));
                            }
                        });
                    }
                };
                let child = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(egui::Rect::from_min_size(ui.cursor().min, egui::vec2(radio_width, f32::INFINITY)))
                        .sizing_pass()
                        .invisible(),
                );
                let group = || {
                    RadioGroup::new(crate::i18n::tr(title).to_owned(), Bind::given(selected))
                        .id_salt(("settings-theme-family", title))
                        .nav(NavShape::Flat)
                        .enabled(ui.is_enabled())
                };
                let size = group().measure_in(&child, Proposal::Ideal, &mut body);
                let (area, _) = ui.allocate_exact_size(egui::vec2(inner, size.y.max(24.0)), egui::Sense::hover());
                let group_rect = egui::Rect::from_min_size(area.min, egui::vec2(radio_width, area.height()));
                let output = RadioGroup::new(crate::i18n::tr(title).to_owned(), Bind::given(selected))
                    .id_salt(("settings-theme-family", title))
                    .nav(NavShape::Flat)
                    .enabled(ui.is_enabled())
                    .show_at(ui, group_rect, &mut body);
                if let Some(index) = output.output.events.committed().copied().filter(|_| ui.is_enabled()) {
                    if let Some((next, _)) = options.get(index as usize) {
                        if *next != *value {
                            *value = *next;
                        }
                    }
                }
                for (kind, radio_rect) in rows {
                    let row = egui::Rect::from_min_max(
                        egui::pos2(area.left(), radio_rect.center().y - 12.0),
                        egui::pos2(area.right(), radio_rect.center().y + 12.0),
                    );
                    let hovered = ui.ctx().pointer_hover_pos().is_some_and(|point| row.contains(point));
                    let favorite = favorites.iter().any(|id| id == kind.id());
                    let rect = egui::Rect::from_min_size(egui::pos2(area.right() - 24.0, radio_rect.center().y - 12.0), egui::vec2(24.0, 24.0));
                    let alias = format!("button:themeFavorite-{}", kind.id());
                    if favorite || hovered {
                        let action = crate::i18n::tr(if favorite { "Remove from favorites" } else { "Add to favorites" });
                        let name = format!("{}: {}", action, crate::i18n::tr(kind.label()));
                        let out = Button::new(name)
                            .id_salt(("settings-theme-favorite", title, kind.id()))
                            .painted(Label::Empty)
                            .variant(Variant::GHOST)
                            .tooltip(action.to_owned())
                            .show_at(ui, rect);
                        crate::widgets::register_response(alias, out.response.interact_rect, &out.response);
                        crate::rating::paint_star(ui.painter(), rect.shrink(4.0), favorite, if favorite { t.accent } else { t.text_dim });
                        if out.clicked() && ui.is_enabled() {
                            if favorite {
                                favorites.retain(|id| id != kind.id());
                            } else {
                                favorites.push(kind.id().into());
                            }
                        }
                    } else {
                        // Retain the existing hidden slot alias for physical hover acquisition only.
                        register(ui, alias, rect);
                    }
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
            crate::panels::grid::selection_ring(&painter, &p, thumb, lightcraft_engine::SelectionState::Active);
        }
    }
}

#[cfg(test)]
#[path = "appearance_theme_cards_sdk_flow_tests.rs"]
mod theme_cards_sdk_flow_tests;
