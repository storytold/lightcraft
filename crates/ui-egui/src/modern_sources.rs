//! The Modern layout's source picker (fork-owned): Modern has no source panel, so the Library's
//! title ("All Photographs ▾") opens a list of what the grid can show: All Photographs, Picks,
//! Recently Added, Recently Deleted, and every collection under its collection set (Immich's
//! albums under "Immich", the synced devices' collections under theirs). A tap shows it, through
//! the same `library.source` command as Classic's Catalog and Collections panels.
//!
//! Own design.

use egui::{Id, Rect, RichText, Sense, pos2, vec2};
use lcc_catalog::{Album, AlbumId};
use lcc_engine::LibrarySource;
use serde_json::json;

use crate::LccApp;
use crate::theme::Tokens;
use crate::widgets::register;

/// The picker's width (narrower on a phone: the window's less a margin).
const WIDTH: f32 = 320.0;
/// The deepest set nesting followed (sets nest; the catalog is hostile input).
const MAX_DEPTH: usize = 8;

fn open_id() -> Id {
    Id::new("modern-sources-open")
}

/// Whether the picker is open.
pub(crate) fn is_open(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(open_id()).unwrap_or(false))
}

/// Opens or closes the picker, noting the frame (the tap that did it is the picker's).
pub(crate) fn set_open(ctx: &egui::Context, on: bool) {
    let frame = ctx.cumulative_frame_nr();
    ctx.data_mut(|d| {
        d.insert_temp(open_id(), on);
        d.insert_temp(Id::new("modern-sources-toggled"), frame);
    });
}

/// Whether a tap at `p` this frame belongs to the picker: on the grid's header, while the list
/// is open, or the tap that just opened or closed it. Modern's tap-to-open leaves such taps alone.
pub(crate) fn owns_tap(ctx: &egui::Context, p: egui::Pos2) -> bool {
    let (header, toggled) =
        ctx.data(|d| (d.get_temp::<Rect>(Id::new("modern-sources-header")), d.get_temp::<u64>(Id::new("modern-sources-toggled"))));
    is_open(ctx) || toggled == Some(ctx.cumulative_frame_nr()) || header.is_some_and(|h| h.contains(p))
}

/// The grid's title in Modern: tappable, with a ▾ after it; `title_rect` is where the title was
/// painted, `header` the header's rect (the picker opens under it).
pub(crate) fn title(app: &mut LccApp, ui: &mut egui::Ui, title_rect: Rect, header: Rect) {
    let t = Tokens::get(ui.ctx());
    // a small ▾, drawn (the UI font has no such glyph)
    let c = pos2(title_rect.right() + 12.0, header.center().y + 1.0);
    let chevron = Rect::from_center_size(c, vec2(10.0, 6.0));
    ui.painter().add(egui::Shape::convex_polygon(
        vec![pos2(chevron.left(), chevron.top()), pos2(chevron.right(), chevron.top()), pos2(c.x, chevron.bottom())],
        t.text_dim,
        egui::Stroke::NONE,
    ));
    let hit = title_rect.union(chevron).expand2(vec2(8.0, 6.0));
    let r = ui.interact(hit, Id::new("modern-sources-title"), Sense::click()).on_hover_text(crate::i18n::tr("Show a collection"));
    // (Modern's canvas lies over the grid and may take the click: a tap on the title counts either way)
    let tapped = ui.input(|i| i.pointer.primary_clicked() && i.pointer.interact_pos().is_some_and(|p| hit.contains(p)));
    register(ui.ctx(), "button:modernSources", hit);
    ui.ctx().data_mut(|d| {
        d.insert_temp(Id::new("modern-sources-title"), hit);
        d.insert_temp(Id::new("modern-sources-header"), header);
    });
    // once per frame: egui may lay a frame out twice, which would toggle it back
    let frame = ui.ctx().cumulative_frame_nr();
    let last = ui.ctx().data(|d| d.get_temp::<u64>(Id::new("modern-sources-toggled")));
    if (r.clicked() || tapped) && last != Some(frame) {
        set_open(ui.ctx(), !is_open(ui.ctx()));
    }
    if is_open(ui.ctx()) {
        picker(app, ui.ctx(), pos2(header.left() + 12.0, header.bottom()));
    }
}

/// One line of the list: what it shows, its label and how deep it sits.
struct Item {
    kind: Option<AlbumId>,
    fixed: Option<(&'static str, LibrarySource)>,
    label: String,
    depth: usize,
    heading: bool,
}

/// The sets and collections under `parent`, depth first, sets before collections, each by name.
fn tree(albums: &[&Album], parent: Option<AlbumId>, depth: usize, out: &mut Vec<Item>) {
    if depth > MAX_DEPTH {
        return;
    }
    let mut here: Vec<&&Album> = albums.iter().filter(|a| a.parent == parent).collect();
    here.sort_by_key(|a| (!a.folder, a.name.to_lowercase()));
    for a in here {
        let at = out.len();
        out.push(Item { kind: (!a.folder).then_some(a.id), fixed: None, label: a.name.clone(), depth, heading: a.folder });
        if a.folder {
            tree(albums, Some(a.id), depth + 1, out);
            // an empty set says nothing
            if out.len() == at + 1 {
                out.truncate(at);
            }
        }
    }
}

fn items(app: &LccApp) -> Vec<Item> {
    let fixed = [
        ("all", LibrarySource::All, "All Photographs"),
        ("picks", LibrarySource::Picks, "Picks"),
        ("recentlyAdded", LibrarySource::RecentlyAdded, "Recently Added"),
        ("recentlyDeleted", LibrarySource::RecentlyDeleted, "Recently Deleted"),
    ];
    let mut out: Vec<Item> = fixed
        .into_iter()
        .map(|(k, s, label)| Item { kind: None, fixed: Some((k, s)), label: crate::i18n::tr(label).to_string(), depth: 0, heading: false })
        .collect();
    let albums: Vec<&Album> = app.session.catalog.albums().filter(|a| !a.quick).collect();
    let mut tail = Vec::new();
    tree(&albums, None, 0, &mut tail);
    if !tail.is_empty() {
        out.push(Item { kind: None, fixed: None, label: crate::i18n::tr("Collections").to_string(), depth: 0, heading: true });
        out.extend(tail.into_iter().map(|mut i| {
            i.depth += 1;
            i
        }));
    }
    out
}

fn picker(app: &mut LccApp, ctx: &egui::Context, at: egui::Pos2) {
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    let width = WIDTH.min(screen.width() - 24.0).max(160.0);
    let max_h = (screen.bottom() - at.y - 24.0).max(160.0);
    let list = items(app);
    let current = app.session.source;
    let mut chosen: Option<serde_json::Value> = None;
    let area = egui::Area::new(Id::new("modern-sources")).order(egui::Order::Foreground).fixed_pos(at).show(ctx, |ui| {
        egui::Frame::popup(ui.style()).show(ui, |ui| {
            ui.set_width(width);
            egui::ScrollArea::vertical().max_height(max_h).show(ui, |ui| {
                for (n, it) in list.iter().enumerate() {
                    let indent = it.depth as f32 * 14.0;
                    if it.heading {
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.add_space(indent);
                            ui.label(RichText::new(&it.label).size(11.5).strong().color(t.text_label));
                        });
                        continue;
                    }
                    let on = match (it.fixed, it.kind) {
                        (Some((_, s)), _) => current == s,
                        (_, Some(id)) => current == LibrarySource::Album(id),
                        _ => false,
                    };
                    let r = ui
                        .horizontal(|ui| {
                            ui.add_space(indent);
                            // full width, its text at the left (so the nesting shows)
                            ui.with_layout(egui::Layout::top_down_justified(egui::Align::LEFT), |ui| {
                                ui.spacing_mut().button_padding.y = 8.0;
                                ui.selectable_label(on, RichText::new(&it.label).size(15.0))
                            })
                            .inner
                        })
                        .inner;
                    let key = match (it.fixed, it.kind) {
                        (Some((k, _)), _) => k.to_string(),
                        (_, Some(id)) => format!("album{}", id.0),
                        _ => n.to_string(),
                    };
                    register(ui.ctx(), format!("button:modernSource-{key}"), r.rect);
                    if r.clicked() {
                        chosen = Some(match (it.fixed, it.kind) {
                            (Some((k, _)), _) => json!({"kind": k}),
                            (_, Some(id)) => json!({"kind": "album", "id": id.0}),
                            _ => continue,
                        });
                    }
                }
            });
        });
    });
    register(ctx, "panel:modernSources", area.response.rect);
    // a tap outside puts it away (the title's own tap toggles it)
    let outside = ctx.input(|i| i.pointer.any_click() && i.pointer.interact_pos().is_some_and(|p| !area.response.rect.contains(p)));
    let title = ctx.data(|d| d.get_temp::<Rect>(Id::new("modern-sources-title")));
    let on_title = ctx.input(|i| i.pointer.interact_pos()).is_some_and(|p| title.is_some_and(|r| r.contains(p)));
    if let Some(p) = chosen {
        set_open(ctx, false);
        if let Err(e) = app.run("library.source", p) {
            app.toast_error(ctx, e);
        }
    } else if outside && !on_title {
        set_open(ctx, false);
    }
}
