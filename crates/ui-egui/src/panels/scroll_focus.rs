//! Keeps the keyboard focus in view in the app's own `egui::ScrollArea`s.
//!
//! The framework reveals a stop that Tab or a focus request moves to by asking its scroll owner, a framework scrolling
//! body, to show it. The side panels scroll in an `egui::ScrollArea`, which is no such owner: without this a Tab walk
//! went on below the panel's bottom edge, to controls no one could see.

/// Notes the key presses that walk the keyboard focus, from the host's input before the framework's key stage takes
/// them (installed ahead of the runtime).
pub(crate) struct FocusWalkKeys;

impl egui::Plugin for FocusWalkKeys {
    fn debug_name(&self) -> &'static str {
        "lightcraft focus walk keys"
    }

    fn input_hook(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        note_navigation_keys(ctx, input);
    }
}

/// Notes a key press that walks the keyboard focus (Tab, the arrows, Home / End, Page Up / Down): [`reveal_focus`]
/// follows only those walks, not a focus a panel's opening or a restore moves.
fn note_navigation_keys(ctx: &egui::Context, raw: &egui::RawInput) {
    use egui::Key;
    let walks = raw.events.iter().any(|e| {
        matches!(e, egui::Event::Key { key, pressed: true, .. } if matches!(key, Key::Tab | Key::ArrowUp | Key::ArrowDown | Key::ArrowLeft | Key::ArrowRight | Key::Home | Key::End | Key::PageUp | Key::PageDown))
    });
    if walks {
        let pass = ctx.cumulative_pass_nr();
        ctx.data_mut(|d| d.insert_temp(navigation_key(), pass));
    }
}

fn navigation_key() -> egui::Id {
    egui::Id::unique("lc-focus-walk-key")
}

/// Whether a key that walks the focus came in for this pass (or the one just before, which a reveal draws again).
fn walked(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<u64>(navigation_key())).is_some_and(|noted| noted.saturating_add(3) >= ctx.cumulative_pass_nr())
}

/// Scrolls the `egui::ScrollArea` being shown (call it last in the area's body) so that a framework stop the keyboard
/// focus has just moved to, with no pointer press (a click focuses what is under the pointer, on screen already), is in
/// view. `key` keeps the stop last seen focused, one per area. The pass is drawn again at once, so what it shows (and
/// tells a screen reader) is the scrolled view.
pub(crate) fn reveal_focus(ui: &mut egui::Ui, key: egui::Id) {
    let ctx = ui.ctx().clone();
    if ui.is_sizing_pass() || craft::ui::runtime::measure::active(&ctx) {
        return;
    }
    let focused = craft::ui::runtime::focus::focused_rec(&ctx);
    // the focused stop's key, and its own widget and arena root
    let now = focused.as_ref().map(|rec| (egui::Id::unique((rec.stop.id, rec.stop.part)), rec.stop.id, rec.stop.root));
    let before = ui.data_mut(|d| d.get_temp::<Option<(egui::Id, egui::Id, egui::Id)>>(key)).flatten();
    ui.data_mut(|d| d.insert_temp(key, now));
    let Some(rec) = focused else { return };
    if now.map(|n| n.0) == before.map(|b| b.0) || !walked(&ctx) || ui.input(|i| i.pointer.any_pressed() || i.pointer.any_down()) {
        return;
    }
    // Only a walk inside this area is followed: the stop before was drawn here too. A focus that appears from nowhere
    // or arrives from elsewhere (a panel opening and taking its first focus, a restore) leaves the scroll where it is.
    let clip = ui.clip_rect();
    let laid_out = ui.min_rect();
    let here = |response: &egui::Response| {
        response.layer_id == ui.layer_id() && (clip.x_range().contains(response.rect.center().x) || laid_out.contains(response.rect.center()))
    };
    let Some((_, before_id, before_root)) = before else { return };
    if !ctx.read_response(before_id).or_else(|| ctx.read_response(before_root)).is_some_and(|response| here(&response)) {
        return;
    }
    // the stop's own widget, or the root of the arena it is a part of
    let Some(response) = ctx.read_response(rec.stop.id).or_else(|| ctx.read_response(rec.stop.root)) else { return };
    let rect = response.rect;
    // only a stop drawn inside this area: same layer, and within its width (a side panel) or inside what its body laid
    // out (a sideways strip, whose stops run past its edges)
    let across = rect.left() >= clip.left() - 1.0 && rect.right() <= clip.right() + 1.0;
    if response.layer_id != ui.layer_id() || !rect.is_finite() || !(across || ui.min_rect().expand(1.0).contains_rect(rect)) {
        return;
    }
    if clip.contains_rect(rect) {
        return;
    }
    ui.scroll_to_rect(rect, None);
    ctx.request_discard("the keyboard focus moved below the panel's edge");
}
