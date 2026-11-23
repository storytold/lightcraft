//! The profile browser (the profile menu's "Browse…", or the grid button next to it): every
//! profile as a live thumbnail of the current photo, grouped (Favorites first), with a favourite
//! star, the applied profile's amount, a temporary loupe preview while hovering and apply on click.

use egui::{Align2, Color32, Rect, Sense, Stroke, pos2, vec2};
use lightcraft_catalog::PhotoId;
use lightcraft_develop::DevelopSettings;
use lightcraft_engine::presets::{PROFILES, profile_groups};
use serde_json::json;

use super::presets::cover_uv;
use crate::icons::Icon;
use crate::state::RightPanel;
use crate::theme::Tokens;
use crate::widgets::{divider, icon_button, one_line};
use crate::{HoverPreview, LightcraftApp};

/// Long edge of the variant thumbnails (px).
const THUMB_EDGE: usize = 256;
/// Thumbnail columns.
const COLUMNS: usize = 2;

pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId) {
    show_with_thumbnail(app, ui, id, paint_thumbnail);
}

fn show_with_thumbnail(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId, thumbnail: impl Fn(&mut LightcraftApp, &egui::Painter, PhotoId, &DevelopSettings, Rect)) {
    let t = Tokens::get(ui.ctx());
    let d = (*app.session.develop_of(id).unwrap_or_default()).clone();
    // header: back + title
    let (hr, _) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::hover());
    let mut back = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(hr.min + vec2(12.0, 9.0), vec2(28.0, 28.0))));
    if icon_button(&mut back, "profilesBack", Icon::ChevronLeft, vec2(28.0, 28.0), false, true, "Back to Edit").clicked() {
        app.ui.right = RightPanel::Edit;
    }
    ui.painter().text(pos2(hr.left() + 46.0, hr.center().y + 2.0), Align2::LEFT_CENTER, crate::i18n::tr("Profiles"), t.semibold(15.0), t.text);
    divider(ui);
    // the applied profile and its amount
    let (cr, _) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::hover());
    let name = app.session.profile_info(&d.profile.id).map(|(name, _)| name).unwrap_or("Color");
    ui.painter().text(pos2(cr.left() + 24.0, cr.center().y), Align2::LEFT_CENTER, crate::i18n::tr("Profile"), t.font(13.0), t.text_dim);
    // a long name is cut short (with …) at the panel's edge
    let g = one_line(ui.painter(), crate::i18n::profile_label(&d.profile.id, name), t.font(14.0), t.text_label, cr.width() - 84.0 - 24.0);
    ui.painter().galley(pos2(cr.left() + 84.0, cr.center().y - g.size().y / 2.0), g, t.text_label);
    if d.profile.id != "lc.color" {
        super::edit::control(app, ui, &d, "profile.amount", true);
        ui.add_space(6.0);
    }
    divider(ui);
    // (title, cell tag, imported LUT folder, profiles)
    let mut sections: Vec<(&str, &str, bool, Vec<(String, String, String)>)> = Vec::new();
    let favs: Vec<(String, String, String)> = app
        .session
        .profile_favorites
        .iter()
        .filter_map(|f| app.session.profile_info(f).map(|(name, group)| (f.clone(), name.to_string(), group.to_string())))
        .collect();
    if !favs.is_empty() {
        sections.push(("Favorites", "fav", false, favs));
    }
    for g in profile_groups() {
        sections.push((
            g,
            "",
            false,
            PROFILES.iter().filter(|p| p.group == g).map(|p| (p.id.to_string(), p.name.to_string(), p.group.to_string())).collect(),
        ));
    }
    // imported profile groups: LUT folders, then camera profiles
    let imported = app.session.imported_profiles();
    let mut lut_groups: Vec<String> = Vec::new();
    for (_, _, g) in &imported {
        if !lut_groups.contains(g) {
            lut_groups.push(g.clone());
        }
    }
    for g in &lut_groups {
        let profiles = imported.iter().filter(|p| &p.2 == g).cloned().collect();
        sections.push((g.as_str(), "", true, profiles));
    }
    for (title, tag, imported, list) in sections {
        if !profile_group_header(ui, title, imported, list.len()) {
            continue;
        }
        let gap = 8.0;
        let cw = ((ui.available_width() - 44.0 - gap * (COLUMNS as f32 - 1.0)) / COLUMNS as f32).max(40.0);
        let th = (cw * 0.75).round();
        for row in list.chunks(COLUMNS) {
            let (rr, _) = ui.allocate_exact_size(vec2(ui.available_width(), th + 30.0), Sense::hover());
            for (i, p) in row.iter().enumerate() {
                let cell = Rect::from_min_size(pos2(rr.left() + 22.0 + i as f32 * (cw + gap), rr.top()), vec2(cw, th + 24.0));
                cell_ui(app, ui, &d, id, tag, p, cell, th, &thumbnail);
            }
        }
        ui.add_space(6.0);
    }
    ui.add_space(30.0);
    let last_hover_id = egui::Id::unique("last-profile-hover");
    let pointer_in_profiles = ui.ctx().input(|i| i.pointer.hover_pos()).is_some_and(|pos| ui.max_rect().contains(pos));
    if pointer_in_profiles {
        if app.hover_preview.is_none()
            && let Some((saved_id, saved_profile_id)) = ui.data(|m| m.get_temp::<(PhotoId, String)>(last_hover_id))
            && saved_id == id
            && let Some((p_name, _)) = app.session.profile_info(&saved_profile_id)
        {
            let s = with_profile_id(&d, &saved_profile_id);
            app.hover_preview = Some(HoverPreview {
                label: crate::i18n::tr_format!("Profile: {}", crate::i18n::profile_label(&saved_profile_id, p_name)),
                settings: s,
            });
        }
    } else {
        ui.data_mut(|m| m.remove::<(PhotoId, String)>(last_hover_id));
    }
}

/// A full-width profile header. State remains view-only and keyed exactly as before.
fn profile_group_header(ui: &mut egui::Ui, title: &str, imported: bool, count: usize) -> bool {
    use craft::ui::contract::{Bind, Props as _, Show as _};

    let open_id = egui::Id::unique(("profile-group", imported, title));
    let open = ui.data(|m| m.get_temp::<bool>(open_id)).unwrap_or(true);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::hover());
    let out = group_header::ProfileGroupHeader::new(ui, Bind::given(open), title, imported, count)
        .id_salt(("profile-group-header", imported, title))
        .show_at(ui, rect);
    // Both layout engines may run this closure speculatively: never publish or persist those runs.
    if !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx()) {
        let key = if imported { format!("profileGroup:lut:{title}") } else { format!("profileGroup:{title}") };
        crate::widgets::register_response(key, rect, &out.response);
        if let Some(value) = out.value() {
            ui.data_mut(|m| m.insert_temp(open_id, *value));
        }
    }
    // Keep the existing expansion timing: this frame's body follows its starting open state.
    open
}

/// The SDK Disclosure centers its glyph in its hit rectangle. A glyph-sized instance would
/// lose the full-row target; a row-sized one would move the arrow over the title. This app
/// leaf keeps the original face while the public SDK pipeline owns the single hit/focus stop,
/// binding, keyboard/AT activation, cancellation and expanded accessibility state.
mod group_header {
    use std::sync::Arc;
    use craft::behavior::{Metrics, Sense};
    use craft::ui::contract::{Bind, BindMut, Common, Component, Cx, Edit as _, EditResponse, MeasureCx, PaintUi, Pointer as _, Props as _, Role};
    use craft::ui::style::{ComponentStyle, FocusLook, StyleCx};
    use egui::{Color32, Galley, Stroke, Vec2, pos2, vec2};
    use crate::theme::Tokens;

    #[derive(Clone, Debug, PartialEq, ComponentStyle)]
    pub(super) struct HeaderStyle {
        #[style(metrics)]
        metrics: Metrics,
        #[style(focus)]
        focus: FocusLook,
    }

    #[derive(Component)]
    pub(super) struct ProfileGroupHeader<'a> {
        #[common]
        common: Common,
        open: Bind<'a, bool>,
        #[style_key]
        focus: Color32,
        label: Arc<Galley>,
        count: Arc<Galley>,
        ink: Color32,
        dim: Color32,
        opacity: f32,
    }

    impl<'a> ProfileGroupHeader<'a> {
        pub(super) fn new(ui: &egui::Ui, open: Bind<'a, bool>, title: &str, imported: bool, count: usize) -> Self {
            let t = Tokens::get(ui.ctx());
            let label = crate::i18n::builtin_label(title, !imported);
            Self {
                common: Common::new(), open, focus: t.accent,
                label: ui.painter().layout_no_wrap(label.to_string(), t.semibold(13.0), t.text_label),
                count: ui.painter().layout_no_wrap(count.to_string(), t.font(12.0), t.text_dim),
                ink: t.text_label, dim: t.text_dim, opacity: ui.opacity(),
            }.label(label.to_string())
        }
    }

    impl Component for ProfileGroupHeader<'_> {
        const ROLE: Role = Role::BUTTON;
        const PARTS: bool = false;
        const OVERLAYS: bool = false;
        type Style = HeaderStyle;
        type Output = EditResponse<bool>;

        fn style(_: &StyleCx<'_>, key: ProfileGroupHeaderStyleKey) -> HeaderStyle {
            HeaderStyle { metrics: Metrics::new(), focus: FocusLook::new(Stroke::new(2.0, key.focus), -2.0) }
        }
        fn measure(&mut self, _: &MeasureCx<'_, Self>) -> Vec2 { vec2(0.0, 32.0) }
        fn sense(&self) -> Sense { Sense::CLICK | Sense::FOCUS }
        fn bind(&mut self) -> Option<BindMut<'_, bool>> { Some(self.open.reborrow()) }
        fn update(&mut self, cx: &mut Cx<'_, Self>) {
            if cx.activation().is_some() { cx.set(!cx.value().copied().unwrap_or(true)); }
        }
        fn access(&self, access: &mut craft::ui::contract::Access, cx: &Cx<'_, Self>) {
            access.expanded(cx.value().copied().unwrap_or(true));
        }
        fn paint(&mut self, ui: &mut PaintUi<'_>, cx: &Cx<'_, Self>) {
            let r = cx.rect;
            let center = pos2(r.left() + 22.0, r.center().y);
            // Same 16pt chevron path and 1.2pt stroke as icons::paint, without a second widget.
            let offsets = if cx.value().copied().unwrap_or(true) {
                [vec2(-4.0, -2.0), vec2(0.0, 2.0), vec2(4.0, -2.0)]
            } else {
                [vec2(-2.0, -4.0), vec2(2.0, 0.0), vec2(-2.0, 4.0)]
            };
            let label_at = pos2(r.left() + 36.0, r.center().y - self.label.size().y / 2.0);
            let count_at = pos2(r.right() - 22.0 - self.count.size().x, r.center().y - self.count.size().y / 2.0);
            let text = |at, galley, color| egui::epaint::TextShape::new(at, galley, color).with_opacity_factor(self.opacity).into();
            ui.reserve_slot(cx.visible).extend(&cx.response.ctx, vec![
                egui::Shape::line(offsets.map(|offset| center + offset).to_vec(), Stroke::new(1.2, self.ink.gamma_multiply(self.opacity))),
                text(label_at, Arc::clone(&self.label), self.ink), text(count_at, Arc::clone(&self.count), self.dim),
            ]);
        }
        fn finish(self, cx: Cx<'_, Self>) -> EditResponse<bool> { cx.edit_response() }
    }
}

#[cfg(test)]
#[path = "profile_group_headers_external155_tests.rs"]
mod profile_group_headers_external155_tests;

/// `d` with profile `profile_id` (at 100 % unless it is the applied one).
fn with_profile_id(d: &DevelopSettings, profile_id: &str) -> DevelopSettings {
    let mut s = d.clone();
    if s.profile.id != profile_id {
        s.profile.id = profile_id.to_string();
        s.profile.amount = 100.0;
    }
    s
}

fn paint_thumbnail(app: &mut LightcraftApp, painter: &egui::Painter, id: PhotoId, s: &DevelopSettings, thumb: Rect) {
    if let Some(job) = app.session.variant_job(id, s, THUMB_EDGE)
        && let Some(tex) = app.renderer.variant(job)
    {
        painter.image(tex.id(), thumb, cover_uv(thumb, tex.size), Color32::WHITE);
    }
}

#[allow(clippy::too_many_arguments)]
fn cell_ui(
    app: &mut LightcraftApp,
    ui: &mut egui::Ui,
    d: &DevelopSettings,
    id: PhotoId,
    tag: &str,
    p: &(String, String, String),
    cell: Rect,
    th: f32,
    thumbnail: &impl Fn(&mut LightcraftApp, &egui::Painter, PhotoId, &DevelopSettings, Rect),
) {
    let (p_id, p_name, p_group) = (p.0.as_str(), p.1.as_str(), p.2.as_str());
    let t = Tokens::get(ui.ctx());
    let thumb = Rect::from_min_size(cell.min, vec2(cell.width(), th));
    let star = Rect::from_center_size(pos2(thumb.right() - 13.0, thumb.top() + 13.0), vec2(20.0, 20.0));
    let applied = d.profile.id == p_id;
    let fav = app.session.profile_favorites.iter().any(|f| f == p_id);
    // Reserve the unchanged face below the SDK controls so their keyboard focus rings stay visible.
    let face = craft::ui::primitives::PaintSlot::reserve(ui.painter());
    let controls = profile_cell_controls(ui, tag, p_id, p_name, cell, star, applied, fav);
    let painted = craft::ui::primitives::PaintSlot::reserve(ui.painter());
    let hovered = controls.tile.hovered() || controls.star.hovered();
    let over_star = controls.star.hovered();
    let real = !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx());
    let s = with_profile_id(d, p_id);
    let painter = ui.painter();
    painter.rect_filled(thumb, 3.0, t.canvas);
    thumbnail(app, painter, id, &s, thumb);
    if applied {
        painter.rect_stroke(thumb.expand(1.0), 4.0, Stroke::new(2.0, t.pick), egui::StrokeKind::Outside);
    } else if hovered {
        painter.rect_stroke(thumb, 3.0, Stroke::new(1.0, t.text_dim), egui::StrokeKind::Outside);
    }
    let name_color = if applied { t.text } else { t.text_label };
    // cut short (with …) to the cell, so it never runs into its neighbour; the full name on hover
    let g = one_line(painter, crate::i18n::profile_label(p_id, p_name), t.font(12.0), name_color, cell.width() - 2.0);
    painter.galley(pos2(cell.left() + 2.0, thumb.bottom() + 12.0 - g.size().y / 2.0), g, name_color);
    // favourite star (always shown when set, on hover otherwise)
    if fav || hovered || controls.star_focus_visible {
        painter.circle_filled(star.center(), 10.0, Color32::from_black_alpha(110));
        crate::rating::paint_star(painter, star.shrink(4.0), fav, if fav || over_star { t.star } else { Color32::WHITE });
    }
    if real && hovered && !applied && !over_star {
        app.hover_preview =
            Some(HoverPreview { label: crate::i18n::tr_format!("Profile: {}", crate::i18n::profile_label(p_id, p_name)), settings: s.clone() });
        ui.data_mut(|m| m.insert_temp(egui::Id::unique("last-profile-hover"), (id, p_id.to_string())));
    } else if real && hovered && applied {
        // resting on the applied profile shows it (no preview), not the last other profile hovered
        ui.data_mut(|m| m.remove::<(PhotoId, String)>(egui::Id::unique("last-profile-hover")));
    }
    // Move only this cell's original face, not either SDK focus ring, into its reserved layer slot.
    face.extend(ui.ctx(), painted.take_after(ui.ctx()));
    let tooltip = format!(
        "{} ({})",
        crate::i18n::profile_label(p_id, p_name),
        crate::i18n::builtin_label(p_group, lightcraft_engine::presets::profile(p_id).is_some())
    );
    let _ = controls.tile.clone().on_hover_text(tooltip.clone());
    let _ = controls.star.clone().on_hover_text(tooltip);
    profile_cell_apply(app, ui, p_id, controls.intent);
}

/// The two independent owners for one cell. The later star wins the overlapping pointer
/// region in the SDK sensing arena; it is never a coordinate branch of the tile's activation.
struct ProfileCellControls {
    tile: egui::Response,
    star: egui::Response,
    star_focus_visible: bool,
    intent: ProfileCellIntent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProfileCellIntent { None, Favorite, Apply }

#[allow(clippy::too_many_arguments)]
fn profile_cell_controls(ui: &mut egui::Ui, tag: &str, p_id: &str, p_name: &str, cell: Rect, star: Rect, applied: bool, fav: bool) -> ProfileCellControls {
    use craft::ui::contract::{Props as _, Show as _};
    let tile_key = if tag.is_empty() { format!("profileCell:{p_id}") } else { format!("profileCell:{tag}:{p_id}") };
    let star_key = format!("profileStar:{}{}", if tag.is_empty() { "" } else { "fav:" }, p_id);
    let label = crate::i18n::profile_label(p_id, p_name).to_string();
    let tile = cell_action::ProfileCellAction::new(ui)
        .id_salt(("profile-tile", tile_key.as_str())).label(label.clone()).description(crate::i18n::tr(if applied { "On" } else { "Off" }).to_string()).selected(applied).show_at(ui, cell);
    let star_out = cell_action::ProfileCellAction::new(ui)
        .id_salt(("profile-star", star_key.as_str()))
        .label(format!("{}: {}", crate::i18n::tr("Favorite"), label)).description(crate::i18n::tr(if fav { "On" } else { "Off" }).to_string()).selected(fav).show_at(ui, star);
    let real = !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx());
    if real {
        crate::widgets::register_response(tile_key, cell, &tile.response);
        crate::widgets::register_response(star_key, star, &star_out.response);
    }
    // A single command at most, even if an activation source is ever shared by mistake.
    let intent = if !real { ProfileCellIntent::None }
        else if craft::ui::contract::clicked(&star_out.response) { ProfileCellIntent::Favorite }
        else if !applied && craft::ui::contract::clicked(&tile.response) { ProfileCellIntent::Apply }
        else { ProfileCellIntent::None };
    ProfileCellControls { tile: tile.response, star: star_out.response, star_focus_visible: star_out.focus_visible, intent }
}

/// Shared with in-memory tests: the real command boundary, not a model-mutating focus proxy.
fn profile_cell_apply(app: &mut LightcraftApp, ui: &egui::Ui, p_id: &str, intent: ProfileCellIntent) {
    if ui.is_sizing_pass() || craft::ui::runtime::measure::active(ui.ctx()) { return; }
    match intent {
        ProfileCellIntent::Favorite => { let _ = app.run("profile.favorite", json!({"id": p_id})); }
        ProfileCellIntent::Apply => {
            let _ = app.run("develop.profile", json!({"id": p_id, "amount": 100}));
            ui.data_mut(|m| m.remove::<(PhotoId, String)>(egui::Id::unique("last-profile-hover")));
        }
        ProfileCellIntent::None => {}
    }
}

/// The tile and star keep their existing host-painted face. Each is a real SDK leaf with
/// its own accessibility node and Tab stop; the pipeline alone owns activation and focus.
mod cell_action {
    use craft::behavior::{Metrics, Sense};
    use craft::ui::contract::{Common, Component, Cx, Focus as _, NoValue, Outcome, PaintUi, Role};
    use craft::ui::style::{ComponentStyle, FocusLook, StyleCx};
    use egui::{Color32, Stroke};
    #[derive(Clone, Debug, PartialEq, ComponentStyle)]
    pub(super) struct ActionStyle {
        #[style(metrics)]
        metrics: Metrics,
        #[style(focus)]
        focus: FocusLook,
    }
    #[derive(Component)]
    pub(super) struct ProfileCellAction {
        #[common]
        common: Common,
        #[style_key]
        accent: Color32,
    }
    impl ProfileCellAction {
        pub(super) fn new(ui: &egui::Ui) -> Self {
            Self { common: Common::new(), accent: crate::theme::Tokens::get(ui.ctx()).accent }
        }
    }
    pub(super) struct ActionOutput {
        pub(super) response: egui::Response,
        pub(super) focus_visible: bool,
    }
    impl Outcome for ActionOutput {
        type Value = NoValue;
        fn response(&self) -> &egui::Response { &self.response }
    }
    impl Component for ProfileCellAction {
        const ROLE: Role = Role::BUTTON;
        const PARTS: bool = false;
        const OVERLAYS: bool = false;
        type Style = ActionStyle;
        type Output = ActionOutput;
        fn style(_: &StyleCx<'_>, key: ProfileCellActionStyleKey) -> ActionStyle {
            ActionStyle { metrics: Metrics::new(), focus: FocusLook::new(Stroke::new(2.0, key.accent), -2.0) }
        }
        fn sense(&self) -> Sense { Sense::CLICK | Sense::FOCUS }
        fn paint(&mut self, _: &mut PaintUi<'_>, _: &Cx<'_, Self>) {}
        fn finish(self, cx: Cx<'_, Self>) -> ActionOutput {
            ActionOutput { response: cx.response.clone(), focus_visible: cx.focus_visible() }
        }
    }
}

#[cfg(test)]
#[path = "profile_tile_favorite_tests.rs"]
mod profile_tile_favorite_tests;
