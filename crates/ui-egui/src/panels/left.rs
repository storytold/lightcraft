//! The left "My Photos" panel: library sources, albums tree, and date groups.

use egui::{Align2, Rect, Sense, pos2, vec2};
use lightcraft_catalog::{Album, AlbumId, FolderNode, KeywordNode};
use lightcraft_engine::LibrarySource;
use serde_json::json;

use crate::LightcraftApp;
use crate::icons::{Icon, paint};
use crate::sidebar::SidebarSection;
use crate::theme::Tokens;
use crate::widgets::{icon_button, register};

mod albums;
mod dates;
mod folders;
mod keywords;
mod local;
mod rows;
mod sections;

pub use albums::{album_drag_feedback, auto_scroll_speed};
use albums::{albums_rows, drag_auto_scroll};
pub(crate) use dates::browse_all_photos;
use dates::date_rows;
pub(crate) use folders::{created_text, folder_row_of, move_photos, moved_text};
use folders::{folder_rows, reveal_chosen};
use keywords::keyword_rows;
pub(crate) use local::{fs_cached, fs_cached_running, same_folder};
use local::{local_now, local_rows};
pub(crate) use rows::disclosure_name;
#[cfg(test)]
pub(crate) use rows::{INDENT_STEP, MIN_INDENT_STEP};
use rows::{LIBFOLDER_REVEALED, Row, SCROLL_TO_ROW, set_tree_open};
pub(crate) use rows::{content_width, indent_step, step_for};
pub(crate) use sections::PlacedSection;
pub use sections::section_drag_feedback;
#[cfg(test)]
pub(crate) use sections::section_drop_before;
use sections::{SECTION_GAP, missing_count, pinned_header, section, section_drag, sections_menu};

/// Draw the My Photos sidebar: title and open-library name, the scrollable sources, albums,
/// folders and sections, with the section visibility menu on the title.
pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let frame = egui::Frame::NONE.fill(t.chrome).stroke(egui::Stroke::new(1.0, t.divider));
    frame.show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        // the title offers the sections' menu too: where hidden sections come back from
        let (hr, title) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::click());
        crate::widgets::announce(&title, crate::i18n::tr("My Photos"));
        register(ui, "sidebarTitle", hr);
        crate::widgets::context_menu(&title, |ui| sections_menu(app, ui, None));
        // the open library's name under the title, so nobody edits in the wrong one (issue #902)
        let library = crate::panels::settings::open_library_name(app);
        let title_y = if library.is_some() { hr.center().y - 7.0 } else { hr.center().y };
        ui.painter().text(pos2(hr.left() + 18.0, title_y), Align2::LEFT_CENTER, crate::i18n::tr("My Photos"), t.semibold(15.0), t.text);
        if let Some(name) = library {
            ui.painter().with_clip_rect(hr.shrink2(vec2(18.0, 0.0))).text(
                pos2(hr.left() + 18.0, hr.center().y + 10.0),
                Align2::LEFT_CENTER,
                name,
                t.font(11.0),
                t.text_dim,
            );
            crate::widgets::register(ui, "label:libraryName", hr);
            if let Some(l) = app.session.library.as_ref() {
                title.on_hover_text(l.dir.display().to_string());
            }
        }
        let counts = app.caches.counts(&app.session.catalog);
        let (total, picks, deleted) = (counts.total, counts.picks, counts.deleted);
        egui::ScrollArea::both().id_salt("left-scroll").auto_shrink([false, false]).show_viewport(ui, |ui, viewport| {
            drag_auto_scroll(app, ui);
            // where the visible part of the content starts (screen y), for the pinned header
            let view_top = ui.cursor().top() + viewport.min.y;
            ui.data_mut(|d| d.insert_temp(egui::Id::unique("left-view-top"), view_top));
            // rows are as wide as the widest one needs (last frame), at least the panel
            let wide = viewport.width().max(content_width(ui.ctx()));
            ui.set_min_width(wide);
            ui.set_max_width(wide);
            ui.data_mut(|d| {
                d.insert_temp(egui::Id::unique("left-content-width-next"), 0.0f32);
                d.insert_temp(egui::Id::unique("left-deepest-indent-next"), 0.0f32);
                // where the visible part of the content ends (screen x), for what stays at the edge
                d.insert_temp(egui::Id::unique("left-visible-right"), ui.cursor().left() + viewport.max.x);
            });
            let src = app.session.source;
            for (id, icon, label, count, s) in [
                ("all", Icon::Photos, "All Photos", Some(total), LibrarySource::All),
                ("recentlyAdded", Icon::Clock, "Recently Added", None, LibrarySource::RecentlyAdded),
                ("picks", Icon::FlagPick, "Picks", Some(picks), LibrarySource::Picks),
            ] {
                if Row::new(id, icon, label).count(count).selected(src == s).show(app, ui).clicked() {
                    let _ = app.run("library.source", json!({"kind": id}));
                }
            }
            // photos whose files can't be found (checked every few seconds, not every frame)
            let missing = missing_count(app, ui);
            if (missing > 0 || src == LibrarySource::Missing)
                && Row::new("missing", Icon::Folder, "Missing Photos")
                    .count(Some(missing))
                    .selected(src == LibrarySource::Missing)
                    .show(app, ui)
                    .clicked()
            {
                let _ = app.run("library.source", json!({"kind": "missing"}));
            }
            if Row::new("recentlyDeleted", Icon::Trash, "Recently Deleted")
                .count(Some(deleted))
                .selected(src == LibrarySource::RecentlyDeleted)
                .show(app, ui)
                .clicked()
            {
                let _ = app.run("library.source", json!({"kind": "recentlyDeleted"}));
            }
            // the album just made: the folders down to it open, once (also when the section is shut)
            let reveal = app.ui.reveal_album.take();
            if reveal.is_some() {
                // …and Albums itself is shown and opened, so the new album can be seen
                app.ui.sidebar.reveal(SidebarSection::Albums);
            }
            // the keyword just made from Keywords' header: the section and the keywords above it open
            if let Some(path) = app.ui.reveal_keyword.take() {
                app.ui.sidebar.reveal(SidebarSection::Keywords);
                let mut above = String::new();
                let mut names = path.split('|').peekable();
                while let Some(name) = names.next().filter(|_| names.peek().is_some()) {
                    if !above.is_empty() {
                        above.push('|');
                    }
                    above.push_str(name);
                    set_tree_open(ui, egui::Id::unique(("kw-open", above.to_lowercase())), true);
                }
                // …and the sidebar scrolls to it
                ui.data_mut(|d| d.insert_temp(egui::Id::unique(SCROLL_TO_ROW), format!("keyword:{path}").to_lowercase()));
            }
            // the folder Show in Folders went to: Folders is shown and opened, and so are the
            // folders above it, also when it was already the one shown and one was folded by hand…
            if let Some(path) = app.ui.reveal_folder.take() {
                app.ui.sidebar.reveal(SidebarSection::Folders);
                ui.data_mut(|d| d.remove::<Option<String>>(egui::Id::unique(LIBFOLDER_REVEALED)));
                // …and the sidebar scrolls to it
                ui.data_mut(|d| d.insert_temp(egui::Id::unique(SCROLL_TO_ROW), format!("libfolder:{path}").to_lowercase()));
            }
            // Local keeps track of the folder being browsed whether or not it is drawn
            let local = local_now(app, ui);
            let shown: Vec<SidebarSection> = app.ui.sidebar.shown().collect();
            let mut placed: Vec<PlacedSection> = Vec::with_capacity(shown.len());
            for s in shown {
                let at = match s {
                    SidebarSection::Albums => {
                        let rows = app.session.catalog.albums().filter(|a| a.parent.is_none()).count();
                        section(app, ui, s, rows, |app, ui| albums_rows(app, ui, reveal))
                    }
                    SidebarSection::Local => {
                        let Some(local) = &local else { continue };
                        section(app, ui, s, local.places.places.len(), |app, ui| local_rows(app, ui, local))
                    }
                    SidebarSection::ByDate => {
                        let rows = app.caches.date_groups(&app.session.catalog).len();
                        section(app, ui, s, rows, date_rows)
                    }
                    SidebarSection::Folders => {
                        let tree = app.caches.folder_tree(&app.session.catalog);
                        section(app, ui, s, tree.len(), |app, ui| {
                            reveal_chosen(app, ui, &tree);
                            folder_rows(app, ui, &tree, 0.0);
                        })
                    }
                    SidebarSection::Keywords => {
                        let tree = app.caches.keyword_tree(&app.session.catalog);
                        section(app, ui, s, tree.len(), |app, ui| keyword_rows(app, ui, &tree, 0.0))
                    }
                };
                placed.push(at);
            }
            ui.data_mut(|d| d.remove::<String>(egui::Id::unique(SCROLL_TO_ROW)));
            // room below the last section (a section dragged to the end is dropped here)
            ui.add_space(SECTION_GAP * 2.0);
            pinned_header(app, ui, &placed, view_top);
            section_drag(app, ui, &placed);
            // what the rows asked for becomes next frame's width
            let next = ui.data(|d| d.get_temp::<f32>(egui::Id::unique("left-content-width-next"))).unwrap_or(0.0);
            if (next - content_width(ui.ctx())).abs() > 0.5 {
                ui.data_mut(|d| d.insert_temp(egui::Id::unique("left-content-width"), next));
                ui.ctx().request_repaint();
            }
            // …and the deepest row drawn (or passed over out of sight) sets next frame's step
            let deepest = ui.data(|d| d.get_temp::<f32>(egui::Id::unique("left-deepest-indent-next"))).unwrap_or(0.0);
            let step = step_for(viewport.width(), deepest);
            if step != indent_step(ui.ctx()) {
                ui.data_mut(|d| d.insert_temp(egui::Id::unique("left-indent-step"), step));
                ui.ctx().request_repaint();
            }
        });
    });
}
