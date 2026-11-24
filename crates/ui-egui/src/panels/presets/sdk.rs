//! SDK faces for the real Presets collection. The host keeps grouping, virtualization and commands.
use craft::ui::contract::{Props as _, Show as _, ShowContainer as _};
use craft::ui::list::{ListRow, RowInk, RowOutput, ThumbAspect, ThumbSize};

pub(super) fn group_row(ui: &mut egui::Ui, rect: egui::Rect, favorites: bool, group: &str, label: &str, open: bool) -> RowOutput {
    ListRow::new(label.to_owned()).id_salt(("preset-group-row", favorites, group)).strong(true).depth(0).group(open).show_at(ui, rect)
}

/// A row remains available for Favorite/Export without a photo, as on main. `preset.apply`
/// retains its engine has_selection predicate; a refused apply creates no mark or undo step.
pub(super) struct PresetRow<'a> {
    pub favorites: bool,
    pub preset: &'a str,
    pub label: &'a str,
    pub selected: bool,
    pub favorite: bool,
    pub thumbnails: bool,
    pub texture: Option<(egui::TextureId, [usize; 2])>,
}

impl PresetRow<'_> {
    pub fn show_at(self, ui: &mut egui::Ui, rect: egui::Rect) -> egui::Response {
        // Under its group: indented past the group's disclosure, in the muted ink the names had.
        let mut row = ListRow::new(self.label.to_owned())
            .id_salt(("preset-apply-row", self.favorites, self.preset))
            .selected(self.selected)
            .depth(1)
            .ink(RowInk::Muted);
        if self.favorite {
            row = row.trailing_named("★", crate::i18n::tr("Favorites"));
        }
        if self.thumbnails {
            row = row.thumb_aspect(ThumbSize::Medium, ThumbAspect::new(3, 2), move |painter, rect| {
                if let Some((texture, size)) = self.texture {
                    painter.picture(texture, rect, super::cover_uv(rect, size), 2.0);
                }
            });
        }
        row.show_at(ui, rect).response
    }
}

/// Framework secondary-click and Shift+F10 routing, without the egui CLICKED flag bridge.
/// Carrying a photo suppresses these menus just as LightCraft's shared menu helper does.
pub(super) fn context_menu(ui: &mut egui::Ui, response: &egui::Response, label: &str, mut body: impl FnMut(&mut egui::Ui)) {
    if craft::ui::runtime::measure::active(ui.ctx()) || crate::widgets::carrying(ui.ctx()) {
        return;
    }
    craft::ui::menu::Menu::context(label.to_owned(), response).id_salt(("preset-context-menu", response.id)).show(ui, |ui, _| body(ui));
}
