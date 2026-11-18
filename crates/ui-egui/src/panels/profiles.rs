//! The profile browser (the profile menu's "Browse…", or the grid button next to it): every
//! profile as a live thumbnail of the current photo, grouped (Favorites first), with a favourite
//! star, the applied profile's amount, a temporary loupe preview while hovering and apply on click.

use egui::{Align2, Color32, Rect, Sense, Stroke, pos2, vec2};
use lcc_catalog::PhotoId;
use lcc_develop::DevelopSettings;
use serde_json::json;

use super::presets::cover_uv;
use crate::icons::{Icon, paint};
use crate::state::RightPanel;
use crate::theme::Tokens;
use crate::widgets::{divider, icon_button, one_line, register};
use crate::{HoverPreview, LccApp};

/// Long edge of the variant thumbnails (px).
const THUMB_EDGE: usize = 256;
/// Thumbnail columns.
const COLUMNS: usize = 2;

/// The raw's camera profile: its camera's default, the built-in colour, or one of
/// the imported `.dcp` files for its camera, or one of its calibrated looks. Hidden for photos that aren't developed from raw data.
fn camera_profile_row(app: &mut LccApp, ui: &mut egui::Ui, d: &DevelopSettings, id: PhotoId) {
    let Some(photo) = app.session.catalog.photo(id).cloned() else { return };
    if !photo.develops_raw() {
        return;
    }
    let t = Tokens::get(ui.ctx());
    // a DNG of rendered RGB (Photo Merge, Render to DNG): no camera profile describes its pixels, so none is offered
    if app.session.media.profile_facts(id).rendered_rgb {
        ui.horizontal(|ui| {
            ui.add_space(24.0);
            ui.label(egui::RichText::new(crate::i18n::tr("Camera Profile")).color(t.text_dim).font(t.font(13.0)));
            let r = ui.add(
                egui::Label::new(egui::RichText::new(crate::i18n::tr("Not used: this DNG holds rendered RGB")).color(t.text_dim).font(t.font(12.0)))
                    .truncate(),
            );
            register(ui.ctx(), "label:cameraProfileRendered", r.rect);
        });
        ui.add_space(4.0);
        return;
    }
    let camera = photo.meta.camera.clone();
    // (with the camera's Lightroom-matched calibrated looks: Lightroom's "Adobe Color" shown as "Eboda Colour",
    // `lcc_camdb::calibration::display_name`)
    let current = d.profile.camera.clone();
    // the ones the Profile Manager hides are left out (the photo's own stays)
    let options = super::edit::shown_camera_profile_options(&app.session.profile_hidden, &camera, &current);
    let shown = options.iter().find(|(o, _)| *o == current).map(|(_, n)| n.clone()).unwrap_or_else(|| current.clone());
    let mut choice = current.clone();
    ui.horizontal(|ui| {
        ui.add_space(24.0);
        ui.label(egui::RichText::new(crate::i18n::tr("Camera Profile")).color(t.text_dim).font(t.font(13.0)));
        let r = crate::widgets::combo_box("cameraProfile").width((ui.available_width() - 24.0).max(80.0)).selected_text(shown).show_ui(ui, |ui| {
            for (o, n) in &options {
                ui.selectable_value(&mut choice, o.clone(), n);
            }
        });
        register(ui.ctx(), "cameraProfile", r.response.rect);
    });
    if choice != current {
        // a camera profile with a look of its own comes with it ("As profile"); the camera's default keeps the look
        let c = if choice.is_empty() { "develop.cameraProfile" } else { "develop.profile" };
        let _ = app.run(c, json!({"id": choice}));
    }
    // a camera profile chosen for this photo can become its body's own (OneSlips, LR-PROF-BODY)
    if !current.is_empty()
        && let Some(body) = lcc_engine::profile_bodies::key_of(&photo)
    {
        ui.horizontal(|ui| {
            ui.add_space(24.0);
            if app.session.body_profiles.get(&body) == Some(&current) {
                ui.label(egui::RichText::new(crate::i18n::tr("This body's default")).color(t.text_dim).font(t.font(12.0)));
            } else if crate::widgets::text_button(ui, "bodyDefault", crate::i18n::tr("Make default for this body"), false).clicked() {
                let _ = app.run("profile.setBodyDefault", json!({"id": current, "photo": id.0}));
            }
        });
    }
    ui.add_space(4.0);
}

/// The look the "None" choice sets: the default look, which adds nothing of its own, so the camera
/// profile applies alone, with its own look when it has one ("As profile", `DevelopSettings::keeps_camera_look`).
pub const NO_LOOK: &str = lcc_develop::DEFAULT_LOOK;

/// "None": no creative profile, so only the camera profile applies (one click; LR-PROF-MANAGE).
fn none_row(app: &mut LccApp, ui: &mut egui::Ui, d: &DevelopSettings) {
    let t = Tokens::get(ui.ctx());
    let none = d.keeps_camera_look();
    ui.horizontal(|ui| {
        ui.add_space(24.0);
        ui.label(egui::RichText::new(crate::i18n::tr("Look")).color(t.text_dim).font(t.font(13.0)));
        let r = crate::widgets::text_button(ui, "profileNone", crate::i18n::tr("None (camera profile only)"), none)
            .on_hover_text(crate::i18n::tr("No creative profile on top: the photo shows its camera profile alone"));
        if r.clicked() && !none {
            let _ = app.run("develop.profile", json!({"id": NO_LOOK, "amount": 100}));
        }
    });
}

pub fn show(app: &mut LccApp, ui: &mut egui::Ui, id: PhotoId) {
    let t = Tokens::get(ui.ctx());
    let d = (*app.session.develop_of(id).unwrap_or_default()).clone();
    // header: back + title
    let (hr, _) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::hover());
    let mut back = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(hr.min + vec2(12.0, 9.0), vec2(28.0, 28.0))));
    if icon_button(&mut back, "profilesBack", Icon::ChevronLeft, vec2(28.0, 28.0), false, true, "Back to Edit").clicked() {
        app.ui.right = RightPanel::Edit;
    }
    ui.painter().text(pos2(hr.left() + 46.0, hr.center().y + 2.0), Align2::LEFT_CENTER, crate::i18n::tr("Profiles"), t.semibold(15.0), t.text);
    // Manage…: the Profile Manager (hide, delete, rename, import; LR-PROF-MANAGE)
    let manage_rect = Rect::from_min_max(pos2(hr.right() - 130.0, hr.top() + 9.0), pos2(hr.right() - 12.0, hr.bottom() - 9.0));
    let mut manage = ui.new_child(egui::UiBuilder::new().max_rect(manage_rect).layout(egui::Layout::right_to_left(egui::Align::Center)));
    if crate::widgets::text_button(&mut manage, "profilesManage", crate::i18n::tr("Manage…"), false).clicked() {
        let _ = app.run("dialog.profileManager", json!({}));
    }
    divider(ui);
    // the applied profile and its amount
    let (cr, _) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::hover());
    // (a photo developing with its camera profile's own look names the camera profile, "As profile")
    let (name, _) = super::edit::profile_row_names(app, id, &d);
    ui.painter().text(pos2(cr.left() + 12.0, cr.center().y), Align2::LEFT_CENTER, crate::i18n::tr("Profile"), t.font(13.0), t.text_dim);
    // a long name is cut short (with …) at the panel's edge
    let g = one_line(ui.painter(), &name, t.font(14.0), t.text_label, cr.width() - 84.0 - 24.0);
    ui.painter().galley(pos2(cr.left() + 84.0, cr.center().y - g.size().y / 2.0), g, t.text_label);
    if d.profile.id != "lc.color" {
        super::edit::control(app, ui, &d, "profile.amount", true);
        ui.add_space(6.0);
    }
    none_row(app, ui, &d);
    camera_profile_row(app, ui, &d, id);
    divider(ui);
    let shown_groups = app.session.shown_groups();
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
    // every group with the Profile Manager's names, groups and order (hidden and removed ones left out)
    for g in &shown_groups {
        sections.push((g.name.as_str(), "", g.imported, g.profiles.iter().map(|p| (p.id.clone(), p.name.clone(), p.group.clone())).collect()));
    }
    // (a group whose profiles the Profile Manager all hides is left out)
    sections.retain(|s| !s.3.is_empty());
    for (title, tag, imported, list) in sections {
        // an imported folder named like a built-in group still opens and closes on its own
        let key = if imported { format!("lut:{title}") } else { format!("profile:{title}") };
        let holds_current = list.iter().any(|p| p.0 == d.profile.id);
        let open = app.ui.group_is_open(&key, holds_current);
        let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
        crate::widgets::announce(&resp, title.to_string());
        register(ui.ctx(), if imported { format!("profileGroup:lut:{title}") } else { format!("profileGroup:{title}") }, r);
        paint(
            ui.painter(),
            Rect::from_center_size(pos2(r.left() + 22.0, r.center().y), vec2(16.0, 16.0)),
            if open { Icon::ChevronDown } else { Icon::ChevronRight },
            t.text_label,
        );
        ui.painter().text(
            pos2(r.left() + 36.0, r.center().y),
            Align2::LEFT_CENTER,
            crate::i18n::builtin_label(title, !imported),
            t.semibold(13.0),
            t.text_label,
        );
        ui.painter().text(pos2(r.right() - 22.0, r.center().y), Align2::RIGHT_CENTER, list.len().to_string(), t.font(12.0), t.text_dim);
        if resp.clicked() {
            app.ui.group_open.insert(key, !open);
        }
        if !open {
            continue;
        }
        let gap = 8.0;
        let cw = ((ui.available_width() - 44.0 - gap * (COLUMNS as f32 - 1.0)) / COLUMNS as f32).max(40.0);
        let th = (cw * 0.75).round();
        for row in list.chunks(COLUMNS) {
            let (rr, _) = ui.allocate_exact_size(vec2(ui.available_width(), th + 30.0), Sense::hover());
            // only the cells in view ask for their thumbnail: every cell asking re-rendered the
            // photo once per profile at every edit (hundreds of imported profiles, hundreds of
            // renders queued at once)
            let in_view = ui.is_rect_visible(rr.expand(th));
            for (i, p) in row.iter().enumerate() {
                let cell = Rect::from_min_size(pos2(rr.left() + 22.0 + i as f32 * (cw + gap), rr.top()), vec2(cw, th + 24.0));
                cell_ui(app, ui, &d, id, tag, p, cell, th, in_view);
            }
        }
        ui.add_space(6.0);
    }
    ui.add_space(30.0);
    let last_hover_id = egui::Id::new("last-profile-hover");
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

/// `d` with profile `profile_id` (at 100 % unless it is the applied one).
fn with_profile_id(d: &DevelopSettings, profile_id: &str) -> DevelopSettings {
    let mut s = d.clone();
    if s.profile.id != profile_id {
        s.profile.id = profile_id.to_string();
        s.profile.amount = 100.0;
    }
    s
}

#[allow(clippy::too_many_arguments)]
fn cell_ui(
    app: &mut LccApp,
    ui: &mut egui::Ui,
    d: &DevelopSettings,
    id: PhotoId,
    tag: &str,
    p: &(String, String, String),
    cell: Rect,
    th: f32,
    in_view: bool,
) {
    let (p_id, p_name, p_group) = (p.0.as_str(), p.1.as_str(), p.2.as_str());
    let t = Tokens::get(ui.ctx());
    let key = if tag.is_empty() { format!("profileCell:{p_id}") } else { format!("profileCell:{tag}:{p_id}") };
    let resp = ui.interact(cell, egui::Id::new(&key), Sense::click());
    crate::widgets::announce(&resp, p_name.to_string());
    register(ui.ctx(), key, cell);
    let thumb = Rect::from_min_size(cell.min, vec2(cell.width(), th));
    // (the photo's settings with this profile: only for a cell in view or under the pointer)
    let s = (in_view || resp.hovered()).then(|| with_profile_id(d, p_id));
    let painter = ui.painter();
    painter.rect_filled(thumb, 3.0, t.canvas);
    if in_view
        && let Some(s) = &s
        && let Some(job) = app.session.variant_job(id, s, THUMB_EDGE)
        && let Some(tex) = app.renderer.variant(job)
    {
        painter.image(tex.id(), thumb, cover_uv(thumb, tex.size), Color32::WHITE);
    }
    let applied = d.profile.id == p_id;
    if applied {
        painter.rect_stroke(thumb.expand(1.0), 4.0, Stroke::new(2.0, t.pick), egui::StrokeKind::Outside);
    } else if resp.hovered() {
        painter.rect_stroke(thumb, 3.0, Stroke::new(1.0, t.text_dim), egui::StrokeKind::Outside);
    }
    let name_color = if applied { t.text } else { t.text_label };
    // cut short (with …) to the cell, so it never runs into its neighbour; the full name on hover
    let g = one_line(painter, crate::i18n::profile_label(p_id, p_name), t.font(12.0), name_color, cell.width() - 2.0);
    painter.galley(pos2(cell.left() + 2.0, thumb.bottom() + 12.0 - g.size().y / 2.0), g, name_color);
    // favourite star (always shown when set, on hover otherwise)
    let fav = app.session.profile_favorites.iter().any(|f| f == p_id);
    let star = Rect::from_center_size(pos2(thumb.right() - 13.0, thumb.top() + 13.0), vec2(20.0, 20.0));
    register(ui.ctx(), format!("profileStar:{}{}", if tag.is_empty() { "" } else { "fav:" }, p_id), star);
    let over_star = resp.hover_pos().is_some_and(|q| star.contains(q));
    if fav || resp.hovered() {
        painter.circle_filled(star.center(), 10.0, Color32::from_black_alpha(110));
        crate::rating::paint_star(painter, star.shrink(4.0), fav, if fav || over_star { t.star } else { Color32::WHITE });
    }
    if resp.hovered()
        && !applied
        && !over_star
        && let Some(s) = s
    {
        app.hover_preview =
            Some(HoverPreview { label: crate::i18n::tr_format!("Profile: {}", crate::i18n::profile_label(p_id, p_name)), settings: s });
        ui.data_mut(|m| m.insert_temp(egui::Id::new("last-profile-hover"), (id, p_id.to_string())));
    } else if resp.hovered() && applied {
        // resting on the applied profile shows it (no preview), not the last other profile hovered
        ui.data_mut(|m| m.remove::<(PhotoId, String)>(egui::Id::new("last-profile-hover")));
    }
    let resp = resp.on_hover_text(format!(
        "{} ({})",
        crate::i18n::profile_label(p_id, p_name),
        crate::i18n::builtin_label(p_group, lcc_engine::presets::profile(p_id).is_some())
    ));
    if resp.clicked() {
        if resp.interact_pointer_pos().is_some_and(|q| star.contains(q)) {
            let _ = app.run("profile.favorite", json!({"id": p_id}));
        } else if !applied {
            let _ = app.run("develop.profile", json!({"id": p_id, "amount": 100}));
            ui.data_mut(|m| m.remove::<(PhotoId, String)>(egui::Id::new("last-profile-hover")));
        }
    }
}
