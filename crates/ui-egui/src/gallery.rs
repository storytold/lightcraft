//! The component gallery: every atom of the iPad UI drawn from the real widget code on one screen, so a reviewer
//! (or a screenshot test) can judge each button, chip, switch, field and icon on its own before judging the
//! panels they sit in. Open it with `ui.set {"gallery": true}`; it is a review tool, not part of the app.

use egui::{Align2, Color32, Rect, Sense, Ui, pos2, vec2};

use crate::LightcraftApp;
use crate::icons::{Icon, paint};
use crate::theme::Tokens;

const ICONS: &[(&str, Icon)] = &[
    ("Sidebar", Icon::Sidebar),
    ("Back", Icon::Back),
    ("Forward", Icon::Forward),
    ("Search", Icon::Search),
    ("Filter", Icon::Filter),
    ("Bell", Icon::Bell),
    ("Share", Icon::Share),
    ("Help", Icon::Help),
    ("Cloud", Icon::Cloud),
    ("Sliders", Icon::Sliders),
    ("Crop", Icon::Crop),
    ("Eraser", Icon::Eraser),
    ("Mask", Icon::Mask),
    ("Eye", Icon::Eye),
    ("EyeOff", Icon::EyeOff),
    ("Presets", Icon::Presets),
    ("Versions", Icon::Versions),
    ("Activity", Icon::Activity),
    ("More", Icon::More),
    ("Tag", Icon::Tag),
    ("Info", Icon::Info),
    ("GridPhoto", Icon::GridPhoto),
    ("GridSquare", Icon::GridSquare),
    ("Single", Icon::Single),
    ("Compare", Icon::Compare),
    ("Survey", Icon::Survey),
    ("Sort", Icon::Sort),
    ("Star", Icon::Star),
    ("StarFilled", Icon::StarFilled),
    ("Circle", Icon::Circle),
    ("FlagPick", Icon::FlagPick),
    ("FlagReject", Icon::FlagReject),
    ("Gear", Icon::Gear),
    ("Filmstrip", Icon::Filmstrip),
    ("BeforeAfter", Icon::BeforeAfter),
    ("ChevronDown", Icon::ChevronDown),
    ("ChevronRight", Icon::ChevronRight),
    ("ChevronLeft", Icon::ChevronLeft),
    ("Plus", Icon::Plus),
    ("Minus", Icon::Minus),
    ("Album", Icon::Album),
    ("SmartAlbum", Icon::SmartAlbum),
    ("Stack", Icon::Stack),
    ("Folder", Icon::Folder),
    ("Pencil", Icon::Pencil),
    ("Photos", Icon::Photos),
    ("Clock", Icon::Clock),
    ("Calendar", Icon::Calendar),
    ("Trash", Icon::Trash),
    ("Curve", Icon::Curve),
    ("ProfileGrid", Icon::ProfileGrid),
    ("Close", Icon::Close),
    ("Check", Icon::Check),
    ("Undo", Icon::Undo),
    ("Redo", Icon::Redo),
    ("Brush", Icon::Brush),
    ("Linear", Icon::Linear),
    ("Radial", Icon::Radial),
    ("Sky", Icon::Sky),
    ("Subject", Icon::Subject),
    ("Picker", Icon::Picker),
    ("Rotate", Icon::Rotate),
    ("RotateDrag", Icon::RotateDrag),
    ("Flip", Icon::Flip),
    ("Invert", Icon::Invert),
    ("Dots", Icon::Dots),
    ("Video", Icon::Video),
    ("Heart", Icon::Heart),
    ("Edited", Icon::Edited),
    ("Target", Icon::Target),
    ("Chat", Icon::Chat),
    ("FaceBox", Icon::FaceBox),
    ("GradeAll", Icon::GradeAll),
    ("GradeShadows", Icon::GradeShadows),
    ("GradeMidtones", Icon::GradeMidtones),
    ("GradeHighlights", Icon::GradeHighlights),
    ("Monitor", Icon::Monitor),
    ("Sun", Icon::Sun),
    ("Moon", Icon::Moon),
];

fn caption(ui: &mut Ui, text: &str) {
    let t = Tokens::get(ui.ctx());
    ui.add_space(6.0);
    ui.label(egui::RichText::new(text).font(t.semibold(12.0)).color(t.text_dim));
    ui.add_space(2.0);
}

/// Draws the gallery over the whole window while `app.ui.gallery` is set.
pub fn show(app: &mut LightcraftApp, ctx: &egui::Context) {
    if !app.ui.gallery {
        return;
    }
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    egui::Area::new(egui::Id::unique("component-gallery")).order(egui::Order::Foreground).fixed_pos(screen.min).show(ctx, |ui| {
        ui.set_clip_rect(screen);
        ui.painter().rect_filled(screen, 0.0, t.canvas);
        let inner = screen.shrink2(vec2(16.0, 10.0));
        let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(egui::Align::Min)));
        ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
        ui.label(egui::RichText::new("Component gallery").font(t.semibold(17.0)).color(t.text));
        let mut dummy_on = true;
        let mut dummy_off = false;
        ui.columns(2, |cols| {
            // ---- left column: buttons, chips, switches, fields
            let ui = &mut cols[0];
            caption(ui, "BUTTONS: primary / secondary / disabled / destructive");
            ui.horizontal_wrapped(|ui| {
                ui.add(
                    egui::Button::new(egui::RichText::new("Export 24 Photos").color(Color32::WHITE).font(t.semibold(14.0)))
                        .fill(t.accent)
                        .corner_radius(21.0)
                        .min_size(vec2(150.0, 42.0)),
                );
                ui.add(egui::Button::new("Cancel").corner_radius(21.0).min_size(vec2(90.0, 42.0)));
                ui.add_enabled(false, egui::Button::new("Disabled").corner_radius(21.0).min_size(vec2(90.0, 42.0)));
                ui.add(egui::Button::new(egui::RichText::new("Delete").color(t.reject)).corner_radius(21.0).min_size(vec2(90.0, 42.0)));
            });
            caption(ui, "TEXT CHIPS: unselected / selected");
            ui.horizontal_wrapped(|ui| {
                crate::widgets::text_button(ui, "gal-chip-a", "JPEG", false);
                crate::widgets::text_button(ui, "gal-chip-b", "PNG", true);
                crate::widgets::text_button(ui, "gal-chip-c", "Web & Social (2048px)", false);
            });
            caption(ui, "SEGMENTED CONTROL");
            crate::widgets::segmented(ui, "gal-seg", &[("a", "Thirds"), ("b", "Grid"), ("c", "Golden")], Some(1), 3);
            caption(ui, "SWITCHES: on / off");
            crate::widgets::toggle(ui, &mut dummy_on, "Confirm before moving photos to Recently Deleted");
            crate::widgets::toggle(ui, &mut dummy_off, "HDR output");
            caption(ui, "RATING PILL, ZOOM DROPDOWN");
            ui.horizontal(|ui| {
                crate::rating::Rating::new("gal-stars", 3).show(ui);
                ui.add_space(12.0);
                crate::widgets::dropdown(ui, "gal-dd", "Fit", t.font(13.0), t.text_label);
            });
            caption(ui, "TEXT FIELD (empty, filled)");
            let mut a = String::new();
            let mut b = String::from("Wedding 2026");
            ui.add(egui::TextEdit::singleline(&mut a).hint_text("Name").desired_width(300.0));
            ui.add(egui::TextEdit::singleline(&mut b).desired_width(300.0));
            caption(ui, "GLASS SHELL with a floating group");
            crate::glass::shell(egui::Margin::symmetric(14, 8), egui::Margin::ZERO).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add(egui::Button::new("Deselect All (3)").corner_radius(18.0).min_size(vec2(100.0, 36.0)));
                    ui.add(
                        egui::Button::new(egui::RichText::new("Export (3)").color(Color32::WHITE))
                            .fill(t.accent)
                            .corner_radius(18.0)
                            .min_size(vec2(90.0, 36.0)),
                    );
                });
            });
            caption(ui, "BADGES AND NOTICES");
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(44.0, 32.0), Sense::hover());
                ui.painter().rect_filled(r, 16.0, Color32::from_rgba_unmultiplied(10, 132, 255, 40));
                ui.painter().text(r.center(), Align2::CENTER_CENTER, "\u{2713} 24", t.semibold(11.5), t.accent);
                let (r, _) = ui.allocate_exact_size(vec2(330.0, 28.0), Sense::hover());
                ui.painter().rect_filled(r, 14.0, Color32::from_black_alpha(200));
                ui.painter().text(
                    r.center(),
                    Align2::CENTER_CENTER,
                    "Original file not found \u{b7} import it again from Files",
                    t.font(12.0),
                    t.text,
                );
            });
            // ---- right column: the icon set
            let ui = &mut cols[1];
            caption(ui, "ICONS (as drawn in bars and rails: 22pt on a 40pt target)");
            let per_row = 6;
            for chunk in ICONS.chunks(per_row) {
                ui.horizontal(|ui| {
                    for (name, icon) in chunk {
                        let (r, _) = ui.allocate_exact_size(vec2(72.0, 58.0), Sense::hover());
                        ui.painter().rect_filled(r, 10.0, Color32::from_white_alpha(10));
                        paint(ui.painter(), Rect::from_center_size(pos2(r.center().x, r.top() + 20.0), vec2(22.0, 22.0)), *icon, t.icon);
                        ui.painter().text(pos2(r.center().x, r.bottom() - 10.0), Align2::CENTER_CENTER, *name, t.font(8.5), t.text_dim);
                    }
                });
            }
        });
    });
}
