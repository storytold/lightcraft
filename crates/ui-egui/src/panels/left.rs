//! The left "My Photos" panel: library sources, albums tree, and date groups.

use egui::{Align2, Rect, pos2, vec2};
use lightcraft_catalog::{Album, AlbumId, FolderNode, KeywordNode};
use lightcraft_engine::LibrarySource;
use serde_json::json;

use crate::LightcraftApp;
use crate::icons::Icon;
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
pub(crate) use rows::{INDENT_STEP, MIN_INDENT_STEP, indent_step, step_for};
use rows::{LIBFOLDER_REVEALED, SCROLL_TO_ROW, SideEvent, SideRow, SideSpec, content_width, set_tree_open, settle_layout, side_list};
pub(crate) use sections::PlacedSection;
pub use sections::section_drag_feedback;

/// Where the sidebar's visible part ends this frame (its inner right edge while the sidebar
/// scrolls sideways): the tests read it to say what "at the edge" means.
#[cfg(test)]
pub(crate) fn sidebar_visible_right(ctx: &egui::Context) -> f32 {
    sections::visible_right_of(ctx)
}
#[cfg(test)]
pub(crate) use sections::section_drop_before;
use sections::{SECTION_GAP, missing_count, pinned_header, section, section_drag, sidebar_title};

/// Draw the My Photos sidebar: title and open-library name, the scrollable sources, albums,
/// folders and sections, with the section visibility menu on the title.
pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    // the drags of sections and albums are the framework's: the app's state follows them
    sections::adopt_drags(app, ui);
    // (the dock shows the panel edge to edge: the margin round the lists is the panel's own, inside its border)
    let frame = egui::Frame::NONE.fill(t.chrome).stroke(egui::Stroke::new(1.0, t.divider)).inner_margin(egui::Margin::same(8));
    frame.show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        // the title offers the sections' menu too: where hidden sections come back from
        sidebar_title(app, ui);
        let counts = app.caches.counts(&app.session.catalog);
        let (total, picks, deleted) = (counts.total, counts.picks, counts.deleted);
        egui::ScrollArea::both().id_salt("left-scroll").auto_shrink([false, false]).show_viewport(ui, |ui, viewport| {
            drag_auto_scroll(app, ui);
            // where the visible part of the content starts (screen y), for the pinned header
            let view_top = ui.cursor().top() + viewport.min.y;
            ui.data_mut(|d| d.insert_temp(egui::Id::unique("left-view-top"), view_top));
            // rows are as wide as the deepest one asks (last frame), at least the panel; the sidebar
            // scrolls sideways when that exceeds the panel
            let wide = viewport.width().max(content_width(ui.ctx()));
            ui.set_min_width(wide);
            ui.set_max_width(wide);
            // where the visible part of the content ends (screen x), for what stays at the edge
            ui.data_mut(|d| d.insert_temp(egui::Id::unique("left-visible-right"), ui.cursor().left() + viewport.max.x));
            let src = app.session.source;
            // photos whose files can't be found (checked every few seconds, not every frame)
            let missing = missing_count(app, ui);
            let mut sources = vec![
                SideRow::new("all", "image", crate::i18n::tr("All Photos")).count(Some(total)),
                SideRow::new("recentlyAdded", "clock", crate::i18n::tr("Recently Added")),
                SideRow::new("picks", "flag", crate::i18n::tr("Picks")).count(Some(picks)),
            ];
            if missing > 0 || src == LibrarySource::Missing {
                sources.push(SideRow::new("missing", "folder", crate::i18n::tr("Missing Photos")).count(Some(missing)));
            }
            sources.push(SideRow::new("recentlyDeleted", "trash", crate::i18n::tr("Recently Deleted")).count(Some(deleted)));
            let chosen = match src {
                LibrarySource::All => Some("all"),
                LibrarySource::RecentlyAdded => Some("recentlyAdded"),
                LibrarySource::Picks => Some("picks"),
                LibrarySource::Missing => Some("missing"),
                LibrarySource::RecentlyDeleted => Some("recentlyDeleted"),
                _ => None,
            };
            let spec = SideSpec::new("sources", crate::i18n::tr("Library"), app.ui.show_counts).flat().selected(chosen);
            for event in side_list(ui, spec, sources).events {
                if let SideEvent::Activate(kind) = event {
                    let _ = app.run("library.source", json!({"kind": kind}));
                }
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
                            folder_rows(app, ui, &tree);
                        })
                    }
                    SidebarSection::Keywords => {
                        let tree = app.caches.keyword_tree(&app.session.catalog);
                        section(app, ui, s, tree.len(), |app, ui| keyword_rows(app, ui, &tree))
                    }
                };
                placed.push(at);
            }
            ui.data_mut(|d| d.remove::<String>(egui::Id::unique(SCROLL_TO_ROW)));
            // room below the last section (a section dragged to the end is dropped here)
            ui.add_space(SECTION_GAP * 2.0);
            pinned_header(app, ui, &placed, view_top);
            section_drag(app, ui, &placed);
            settle_layout(ui.ctx(), viewport.width());
            // a drag the lists started this frame is adopted at once
            sections::adopt_drags(app, ui);
        });
    });
}
