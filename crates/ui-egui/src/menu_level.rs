//! Menus bounded by the window (#189). egui keeps a popup inside the window: a menu or submenu
//! taller than the room below the menu bar was slid up over the bar, which hid the menu titles,
//! and the pointer that had rested on the title then rested on the menu's first row and opened
//! its submenu by itself. Here every level of a menu gets only the room it has below the bar (a
//! submenu: from its row down, or up to the bar when egui opens it upward) and scrolls inside
//! it: wheel, dragging, and ▲ / ▼ strips that scroll while the pointer rests on them, like
//! native menus on a small display. The same approach as PhotoCraft's menu bar (its #319).

use egui::{Id, Rect, Sense, Ui, pos2, vec2};

/// Height of a scroll-arrow strip.
pub const ARROW: f32 = 16.0;
/// Gap kept between a menu and the window edge.
const EDGE: f32 = 6.0;
/// How fast resting on a scroll arrow scrolls, in points per second.
const SPEED: f32 = 600.0;

fn anchor_id(depth: usize) -> Id {
    Id::new(("lc-menu-anchor", depth))
}

/// Remember the row the submenu at `depth` (2 = a top-level menu's submenu) hangs from. It is
/// read on the next frame: a submenu is drawn while its row is, before the row's rect is known.
pub fn set_anchor(ctx: &egui::Context, depth: usize, row: Rect) {
    let pass = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| d.insert_temp(anchor_id(depth), (row, pass)));
}

/// The row the submenu at `depth` hung from on the previous frame (none when it has just opened,
/// or when a stored row belongs to a submenu that has closed since).
fn anchor(ctx: &egui::Context, depth: usize) -> Option<Rect> {
    anchor_at(ctx.data(|d| d.get_temp(anchor_id(depth))), ctx.cumulative_pass_nr())
}

/// The anchor `stored` as `(row, pass it was set on)`, if that was the pass before `pass`.
fn anchor_at(stored: Option<(Rect, u64)>, pass: u64) -> Option<Rect> {
    stored.filter(|(_, at)| at.saturating_add(1) == pass).map(|(row, _)| row)
}

/// The height a menu level's rows may take. Every level stays below the menu bar: a top-level
/// menu hangs from it, and a submenu that is too tall for the window between its row and the
/// bottom, or (opening upward, as egui does when that fits) between the bar and its row, scrolls
/// instead of egui sliding it up over the bar. `anchor` is the submenu's row (unknown on its
/// first frame), `rows` the level's rows' height when known (the popup shrinks to it), and
/// `frame` the popup's margins.
pub fn level_room(screen: Rect, bar_bottom: Option<f32>, depth: usize, anchor: Option<Rect>, rows: Option<f32>, frame: f32) -> f32 {
    let top = bar_bottom.unwrap_or(screen.top()).max(screen.top()) + EDGE;
    let bottom = screen.bottom() - EDGE;
    let below_bar = bottom - top;
    let room = match anchor.filter(|_| depth > 1) {
        Some(row) => {
            // Downward from the row, or upward from it, whichever leaves more room. egui opens a
            // submenu upward only when the downward one doesn't fit the window (it hangs from the
            // row's top less half the frame margin).
            let (down, up) = (bottom - row.top(), row.bottom() - top);
            let opens_up = |h: f32| row.top() - (frame - 2.0) / 2.0 + h + frame > screen.bottom();
            let want = rows.unwrap_or(f32::INFINITY);
            if want > down && up > down && opens_up(want.min(up)) { up } else { down }.min(below_bar)
        }
        None => below_bar,
    } - frame;
    if room.is_finite() { room.max(4.0 * ARROW) } else { 4.0 * ARROW }
}

/// Is the pointer over `rect` on this ui's layer (not under a submenu drawn on top)?
fn pointer_on(ui: &Ui, rect: Rect) -> bool {
    ui.ctx().pointer_hover_pos().is_some_and(|p| rect.contains(p) && ui.ctx().layer_id_at(p) == Some(ui.layer_id()))
}

/// Draw one level of a menu, bounded by the window: when its rows don't fit the room below the
/// menu bar they scroll, with scroll arrows at the top and bottom. `depth` is 1 for a top-level
/// menu; `bar_bottom` is where the menu bar ends. The level's rect is registered as
/// `menu-level:<depth>` for the control channel and tests.
pub fn level(ui: &mut Ui, depth: usize, bar_bottom: Option<f32>, rows: impl FnOnce(&mut Ui)) {
    let ctx = ui.ctx().clone();
    let screen = ctx.content_rect();
    // The menu frame's margin and stroke around the rows.
    let frame = ui.spacing().menu_margin.sum().y + 2.0;
    let key = ui.id().with(("lc-menu-level", depth));
    // Rows' height from the last frame: does this level overflow?
    let content: Option<f32> = ctx.data(|d| d.get_temp(key));
    let room = level_room(screen, bar_bottom, depth, anchor(&ctx, depth), content, frame);
    let over = content.is_some_and(|h| h > room + 0.5);
    let up = over.then(|| ui.allocate_exact_size(vec2(0.0, ARROW), Sense::hover()).0);
    let height = if over { room - 2.0 * ARROW } else { room };
    // The popup's `Ui` is only as tall as the popup was last frame (egui's default area size on
    // the first), and a scroll area never grows past that. Ask for the room the window has;
    // auto-shrink then fits the area to its rows, so a menu scrolls only when they don't fit.
    // No scrollbar: the ▲ / ▼ strips, the wheel and dragging scroll a menu, like native menus.
    // egui's floating bar is drawn over the rows' right edge, where the shortcut text ends, and in
    // its wide (hovered) state its background hides the last character (issue #691).
    let out = egui::ScrollArea::vertical()
        .id_salt(("menu-level", depth))
        .max_height(height)
        .min_scrolled_height(height)
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
        .scroll_source(egui::containers::scroll_area::ScrollSource::ALL)
        .show(ui, rows);
    let down = over.then(|| ui.allocate_exact_size(vec2(0.0, ARROW), Sense::hover()).0);
    ctx.data_mut(|d| d.insert_temp(key, out.content_size.y));
    crate::widgets::register(&ctx, format!("menu-level:{depth}"), ui.min_rect());
    let (Some(up), Some(down)) = (up, down) else { return };
    let t = crate::theme::Tokens::get(&ctx);
    let x = ui.min_rect().x_range();
    let max = (out.content_size.y - out.inner_rect.height()).max(0.0);
    let offset = out.state.offset.y;
    let dt = ctx.input(|i| i.stable_dt).clamp(0.0, 0.1);
    let mut target = offset;
    for (strip, dir, live) in [(up, -1.0, offset > 0.5), (down, 1.0, offset < max - 0.5)] {
        let strip = Rect::from_x_y_ranges(x, strip.y_range());
        let c = strip.center();
        let (h, w) = (4.5, 6.0);
        let tri = if dir < 0.0 {
            vec![pos2(c.x - w, c.y + h * 0.5), pos2(c.x + w, c.y + h * 0.5), pos2(c.x, c.y - h * 0.5)]
        } else {
            vec![pos2(c.x - w, c.y - h * 0.5), pos2(c.x + w, c.y - h * 0.5), pos2(c.x, c.y + h * 0.5)]
        };
        ui.painter().add(egui::Shape::convex_polygon(tri, if live { t.text } else { t.text_disabled }, egui::Stroke::NONE));
        let id = key.with(dir < 0.0);
        let r = ui.interact(strip, id, Sense::hover());
        r.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, live, crate::i18n::tr(if dir < 0.0 { "Scroll menu up" } else { "Scroll menu down" }))
        });
        if live && pointer_on(ui, strip) {
            target = (offset + dir * SPEED * dt.max(1.0 / 120.0)).clamp(0.0, max);
            ctx.request_repaint();
        }
        // The wheel scrolls over the arrows too.
        if pointer_on(ui, strip) {
            let wheel = ctx.input(|i| i.smooth_scroll_delta.y);
            if wheel != 0.0 {
                target = (target - wheel).clamp(0.0, max);
            }
        }
    }
    if target != offset {
        let mut st = out.state;
        st.offset.y = target;
        st.store(&ctx, out.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_room_keeps_every_level_below_the_bar() {
        let screen = Rect::from_min_size(egui::Pos2::ZERO, vec2(1280.0, 703.0));
        let (bar, frame) = (Some(42.0), 14.0);
        let below = 703.0 - EDGE - (42.0 + EDGE) - frame;
        assert_eq!(level_room(screen, bar, 1, None, None, frame), below);
        // A submenu from a row near the top opens downward with the room under it…
        let row = Rect::from_min_size(pos2(100.0, 80.0), vec2(200.0, 30.0));
        assert_eq!(level_room(screen, bar, 2, Some(row), None, frame), 703.0 - EDGE - 80.0 - frame);
        // …and from a row near the bottom, upward to the bar.
        let low = Rect::from_min_size(pos2(100.0, 600.0), vec2(200.0, 30.0));
        assert_eq!(level_room(screen, bar, 2, Some(low), None, frame), 630.0 - (42.0 + EDGE) - frame);
        // A short submenu from a low row still fits downward: it keeps that room.
        assert_eq!(level_room(screen, bar, 2, Some(low), Some(50.0), frame), 703.0 - EDGE - 600.0 - frame);
        // Never more than the space under the bar, never less than the scroll arrows need.
        let odd = Rect::from_min_size(pos2(100.0, -50.0), vec2(200.0, 30.0));
        assert!(level_room(screen, bar, 2, Some(odd), None, frame) <= below);
        assert_eq!(level_room(Rect::from_min_size(egui::Pos2::ZERO, vec2(100.0, 40.0)), bar, 1, None, None, frame), 4.0 * ARROW);
        assert_eq!(level_room(screen, Some(f32::NAN), 1, None, None, frame), level_room(screen, None, 1, None, None, frame));
        assert_eq!(level_room(Rect::NOTHING, bar, 2, Some(row), None, frame), 4.0 * ARROW);
        // A top-level menu ignores a stale anchor.
        assert_eq!(level_room(screen, bar, 1, Some(low), None, frame), below);
    }

    #[test]
    fn an_anchor_is_read_on_the_next_frame_only() {
        let row = Rect::from_min_size(pos2(10.0, 20.0), vec2(200.0, 24.0));
        // set on pass 7: the submenu's room is measured from it on pass 8…
        assert_eq!(anchor_at(Some((row, 7)), 8), Some(row));
        // …not on the pass it was set (the row is drawn after its submenu), nor once the
        // submenu has closed and stopped refreshing it
        assert_eq!(anchor_at(Some((row, 7)), 7), None);
        assert_eq!(anchor_at(Some((row, 7)), 9), None);
        assert_eq!(anchor_at(None, 8), None);
        assert_eq!(anchor_at(Some((row, u64::MAX)), 0), None);
    }
}
