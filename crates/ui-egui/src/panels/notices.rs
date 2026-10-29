//! Things the user must not miss (issue #103): settings files that were damaged or unreadable when
//! the library opened (and host warnings such as a damaged `ui.json`), and quitting while changes
//! are only in memory or while tasks that would be cut short are running (issue #345).

use egui::{Align2, RichText, vec2};

use crate::theme::Tokens;
use crate::widgets::WidgetHit;
use crate::{LightcraftApp, QuitPrompt};

/// Collect the library's settings-file warnings (each is shown once), and the no-GPU notice once the GPU device has
/// been tried.
pub fn logic(app: &mut LightcraftApp) {
    let new = app.session.take_library_warnings();
    app.notices.extend(new);
    use lightcraft_engine::gpu;
    if app.gpu_notice && app.gpu_applied == Some(true) && gpu::ready() {
        app.gpu_notice = false;
        if !gpu::available() {
            app.notices.extend(gpu_notice(gpu::unavailable_reason().as_deref(), app.ui.settings.gpu));
        }
    }
}

/// What to tell the user when GPU rendering is on (`enabled`) but the GPU could not be used for `reason`: only when no
/// graphics adapter was found at all ("no GPU adapter among …", e.g. no Vulkan loader on Linux), which makes every
/// render much slower without saying so. Nothing when it was turned off (preference, `LIGHTCRAFT_GPU`), a software
/// adapter was skipped, or it failed later; Settings ▸ Performance and Help ▸ System Info show those.
pub fn gpu_notice(reason: Option<&str>, enabled: bool) -> Option<String> {
    if !enabled || !reason?.starts_with("no GPU adapter") {
        return None;
    }
    let mut text =
        crate::i18n::tr("No graphics processor found: LightCraft renders on the CPU, which is much slower. Details in Help → System Info.")
            .to_string();
    if cfg!(target_os = "linux") {
        text.push(' ');
        text.push_str(crate::i18n::tr("On NixOS, start it from `nix develop`."));
    }
    Some(text)
}

/// The quit prompt's text while `count` cancellable tasks run (`names` lists them).
fn tasks_message(count: usize, names: &str) -> String {
    if count == 1 {
        crate::i18n::tr_format!("1 task is still running: {names}. Quitting stops it.", names = names)
    } else {
        crate::i18n::tr_format!("{n} tasks are still running: {names}. Quitting stops them.", n = count, names = names)
    }
}

/// The quit prompt's text when `n` changes couldn't be written to disk, for the reason `e`.
fn unsaved_message(n: usize, e: &str) -> String {
    let s = crate::i18n::plural_s(n);
    let lost = if n == 1 { "it is" } else { "they are" };
    crate::i18n::tr_format!("{n} change{} couldn't be written to disk: {e}\nIf you quit now, {} lost.", s, lost, n = n, e = e)
}

/// The window may close now (`true`), or the quit prompt is shown (`false`). Running tasks that
/// can be cancelled (an import, an export…) are asked about first; then changes that couldn't be
/// written are retried — the log, then a snapshot of everything in memory.
pub fn may_close(app: &mut LightcraftApp) -> bool {
    if app.quit_confirmed {
        return true;
    }
    // before the library is touched: a Cancel here keeps working with it
    let running = app.session.activity.running_cancellable();
    if !running.is_empty() {
        let names = running.iter().map(|t| crate::i18n::tr(&t.label)).collect::<Vec<_>>().join(", ");
        app.quit_prompt = Some(QuitPrompt::Tasks(tasks_message(running.len(), &names)));
        return false;
    }
    if app.session.unsaved().is_none() {
        return true;
    }
    let _ = app.session.persist();
    if app.session.unsaved().is_some()
        && let Err(e) = app.session.close_library()
    {
        log::error!("quit: library not saved: {e}");
    }
    match app.session.unsaved() {
        None => true,
        Some((n, e)) => {
            app.quit_prompt = Some(QuitPrompt::Unsaved(unsaved_message(n, e)));
            false
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Choice {
    Retry,
    QuitAnyway,
    Cancel,
    Ok,
}

/// The prompt (quit or settings notice) that owns input this frame, if any.
///
/// The dim area is `Order::Foreground`, so a settled egui pass routes pointer input to it before
/// the main view or a `Middle` dialog. The control channel and raw keyboard/event handlers do not
/// go through that hit test, however: they must consult this explicit barrier as well.
pub(crate) fn prompt_layer(app: &LightcraftApp) -> Option<egui::LayerId> {
    let id = match (&app.quit_prompt, app.notices.is_empty()) {
        (Some(QuitPrompt::Tasks(_)), _) => "quit-tasks",
        (Some(QuitPrompt::Unsaved(_)), _) => "quit-unsaved",
        (None, false) => "settings-notice",
        (None, true) => return None,
    };
    Some(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique(id)))
}

/// Whether a prompt is on screen and owns all pointer and keyboard input.
pub fn blocks_input(app: &LightcraftApp) -> bool {
    prompt_layer(app).is_some()
}

/// Whether a registered automation target may receive input. While a prompt is up only its own
/// Foreground window is interactive; the same barrier must hold before the first prompt layout
/// settles, when egui has not yet made the new areas interactable.
pub(crate) fn allows_target(app: &LightcraftApp, hit: Option<WidgetHit>) -> bool {
    match prompt_layer(app) {
        None => true,
        Some(layer) => hit.is_some_and(|hit| hit.layer_id == layer),
    }
}

/// Identity of the prompt whose focus we track. A queued settings notice is a different prompt
/// from the one before it even though both use the same layer and id.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PromptKey {
    layer: egui::LayerId,
    text: String,
    queue_len: usize,
}

/// Identity of the prompt currently owning input: its layer, text and, for the settings-notice
/// queue, the number of notices remaining, including the one shown. `None` when no prompt is up.
fn prompt_key(app: &LightcraftApp) -> Option<PromptKey> {
    let (layer, text, queue_len) = match (&app.quit_prompt, app.notices.first()) {
        (Some(prompt), _) => (prompt_layer(app)?, prompt.text().to_string(), 0),
        (None, Some(text)) => (prompt_layer(app)?, text.clone(), app.notices.len()),
        (None, None) => return None,
    };
    Some(PromptKey { layer, text, queue_len })
}

/// The prompt's keyboard/focus state, isolated from the still-drawn background.
///
/// The first host frame of a new prompt is a shielding pass: pointer/touch/key/accessibility input
/// is removed before panels are drawn, and the frame is discarded so a second pass can lay out the
/// new `Area` without any input. From then on this state routes only the prompt's own keys and
/// AccessKit actions; everything else is dropped before egui or the panels can see it.
#[derive(Default)]
pub(crate) struct PromptInput {
    key: Option<PromptKey>,
    /// The prompt has completed a layout pass; its `Area`s are interactable and its button ids are
    /// known. Before this, input must not reach it or the panels under it.
    laid_out: bool,
    /// The next `pass 0` is the shielding pass for a newly shown prompt.
    transition: bool,
    /// Focused button index; `focus_set` distinguishes "not chosen yet" from index 0.
    focus: usize,
    focus_set: bool,
    /// Enter/Space or an AccessKit Click on a prompt button.
    activate: Option<usize>,
    /// Escape (safe cancel).
    cancel: bool,
    /// Actual egui response ids of the prompt's buttons from its latest draw.
    button_ids: Vec<(String, egui::Id)>,
    /// Pointer state saved before the background draws. Restored only while the prompt itself is
    /// drawn (egui's `Response::clicked` needs `pointer_events`), then hidden again for the
    /// dialogs and overlays drawn after it.
    pointer: Option<egui::PointerState>,
}

impl PromptInput {
    /// Reset the prompt input state for `key`, discarding layout, focus, button ids and pending
    /// activation. A present key arms the shield transition; `None` leaves input unowned.
    fn reset_to(&mut self, key: Option<PromptKey>) {
        let transition = key.is_some();
        self.key = key;
        self.laid_out = false;
        self.transition = transition;
        self.focus = 0;
        self.focus_set = false;
        self.activate = None;
        self.cancel = false;
        self.button_ids.clear();
        self.pointer = None;
    }

    /// The index of the prompt button whose real egui id matches AccessKit's `node`, if any.
    fn button_index(&self, accesskit_node: egui::accesskit::NodeId) -> Option<usize> {
        self.button_ids.iter().position(|(_, id)| id.accesskit_id() == accesskit_node)
    }
}

/// Surrender egui's current keyboard focus before a chosen prompt button disappears.
fn clear_prompt_focus(ctx: &egui::Context) {
    ctx.memory_mut(|memory| {
        if let Some(id) = memory.focused() {
            memory.surrender_focus(id);
        }
    });
}

/// Whether a key press belongs to the prompt: Tab (optionally with Shift) or unmodified Enter,
/// Space or Escape.
fn prompt_key_allowed(key: egui::Key, modifiers: egui::Modifiers) -> bool {
    match key {
        egui::Key::Tab => modifiers.is_none() || modifiers.shift_only(),
        egui::Key::Enter | egui::Key::Space | egui::Key::Escape => modifiers.is_none(),
        _ => false,
    }
}

/// Whether an AccessKit action targets a prompt button on the root tree and is a Focus or Click
/// request.
fn prompt_accesskit_allowed(request: &egui::accesskit::ActionRequest, ids: &[(String, egui::Id)]) -> bool {
    request.target_tree == egui::accesskit::TreeId::ROOT
        && ids.iter().any(|(_, id)| id.accesskit_id() == request.target_node)
        && matches!(request.action, egui::accesskit::Action::Focus | egui::accesskit::Action::Click)
}

/// Drop host and synthetic input that the background must not receive while a prompt is up.
///
/// Key releases and modifier changes always survive: egui updates `keys_down` from them in
/// `begin_pass`, so dropping a release would leave a key held after the prompt goes away. Pointer
/// and accessibility input is kept only once the prompt's first layout has settled. Wheel/zoom/
/// rotate/text/clipboard input has no prompt meaning and is always removed. A touch Start/Move is
/// dropped while the prompt is open; End/Cancel always stays,
/// including the creation/transition frames, because egui's private `touch_states` tracks the
/// finger separately and dropping the release would leave it pressed after the prompt goes away.
fn filter_prompt_events(app: &LightcraftApp, raw: &mut egui::RawInput) {
    raw.events.retain(|event| prompt_allows_event(app, event));
}

/// Whether `event` is the release half of a touch gesture. Its state lives in egui's private
/// `touch_states`, outside the `PointerState` that the prompt's cleanup resets, so it must survive
/// even before the prompt's first layout has settled.
fn touch_release(event: &egui::Event) -> bool {
    matches!(event, egui::Event::Touch { phase: egui::TouchPhase::End | egui::TouchPhase::Cancel, .. })
}

/// Whether an input event may reach egui while a prompt is up. This is the single rule used both
/// for host input before `begin_pass` and for a synthetic batch after `raw_input_hook`; the control
/// channel can therefore reject the exact gestures that the prompt barrier would drop, while an
/// allowed prompt key, pointer action or targeted AccessKit action is still delivered.
pub(crate) fn prompt_allows_event(app: &LightcraftApp, event: &egui::Event) -> bool {
    if app.notice_input.key.is_none() {
        return true;
    }
    let laid_out = app.notice_input.laid_out;
    let ids = &app.notice_input.button_ids;
    match event {
        // Key releases and modifier/window-focus changes always survive: egui updates `keys_down`
        // from them in `begin_pass`, so dropping a release would leave a key held afterwards.
        egui::Event::Key { pressed: false, .. } | egui::Event::ModifiersChanged(_) | egui::Event::WindowFocused(_) => true,
        egui::Event::Key { key, pressed: true, modifiers, .. } => laid_out && prompt_key_allowed(*key, *modifiers),
        egui::Event::AccessKitActionRequest(request) => laid_out && prompt_accesskit_allowed(request, ids),
        egui::Event::Text(_)
        | egui::Event::Ime(_)
        | egui::Event::Cut
        | egui::Event::Copy
        | egui::Event::Paste(_)
        | egui::Event::PasteImage(_)
        | egui::Event::MiddleClickPaste { .. }
        | egui::Event::MouseWheel { .. }
        | egui::Event::Zoom(_)
        | egui::Event::Rotate(_) => false,
        // Pointer input at a settled prompt belongs to its backdrop or buttons. A new touch
        // gesture does not, but its End/Cancel must still clear egui's private touch state even
        // in the prompt's creation/transition frames.
        egui::Event::PointerMoved(_) | egui::Event::PointerButton { .. } | egui::Event::PointerGone => laid_out,
        touch @ egui::Event::Touch { .. } if touch_release(touch) => true,
        egui::Event::Touch { .. } => false,
        _ => true,
    }
}

/// Restore the pointer state saved for the prompt just before it is drawn. The background has
/// already drawn with no pointer; egui's `Response::clicked` still needs the pass's pointer events
/// to turn the interaction snapshot into a click on a prompt button.
pub(crate) fn restore_prompt_pointer(app: &LightcraftApp, ctx: &egui::Context) {
    if let Some(pointer) = &app.notice_input.pointer {
        ctx.input_mut(|input| input.pointer = pointer.clone());
    }
}

/// Hide pointer state again after the prompt has drawn, so a dialog or overlay drawn later in the
/// same pass cannot act on the same release that dismissed the prompt.
pub(crate) fn hide_background_pointer(ctx: &egui::Context) {
    ctx.input_mut(|input| {
        input.pointer = egui::PointerState::default();
        input.smooth_scroll_delta = egui::Vec2::ZERO;
    });
    ctx.stop_dragging();
}

/// Apply the prompt barrier after synthetic events have been appended to `raw`.
pub(crate) fn filter_raw_input(app: &LightcraftApp, raw: &mut egui::RawInput) {
    filter_prompt_events(app, raw);
}

/// Note a prompt transition before egui sees the host input. Returns true when this host frame
/// first sees a new prompt (its synthetic gestures must be canceled and its pointer state reset).
pub(crate) fn prepare_raw_input(app: &mut LightcraftApp, ctx: &egui::Context, raw: &mut egui::RawInput) -> bool {
    let current = prompt_key(app);
    let started = current.is_some() && app.notice_input.key != current;
    if app.notice_input.key != current {
        app.notice_input.reset_to(current);
        clear_prompt_focus(ctx);
    }
    filter_prompt_events(app, raw);
    started
}

/// Apply the same barrier to the live input after `begin_pass`. A prompt can be created by this
/// frame's logic after `raw_input_hook` already ran; this captures its keys before panels read
/// them, drops the rest, and reports whether the prompt was first discovered here (so its
/// pre-existing gesture queue must be canceled). The draw pass itself uses [`shield_pass`].
pub(crate) fn prepare_ui_pass(app: &mut LightcraftApp, ctx: &egui::Context) -> bool {
    let current = prompt_key(app);
    let has_prompt = current.is_some();
    let started = has_prompt && app.notice_input.key != current;
    if app.notice_input.key != current {
        app.notice_input.reset_to(current);
        clear_prompt_focus(ctx);
    }
    if !has_prompt {
        return false;
    }
    let shield = ctx.current_pass_index() == 0 && app.notice_input.transition;
    let laid_out = app.notice_input.laid_out && !shield;
    if laid_out {
        // A screen reader's Focus/Click can target only a button whose actual response id we
        // recorded in a completed prompt pass. Tab/Shift+Tab and Enter/Space/Escape are handled
        // here too, so no background panel sees a key that belongs to the prompt.
        let events = ctx.input(|input| input.events.clone());
        for event in &events {
            match event {
                egui::Event::Key { key, pressed: true, modifiers, .. } if prompt_key_allowed(*key, *modifiers) => match key {
                    egui::Key::Tab => {
                        let len = app.notice_input.button_ids.len();
                        if len > 0 {
                            app.notice_input.focus_set = true;
                            app.notice_input.focus = if modifiers.shift_only() {
                                app.notice_input.focus.checked_add(len - 1).map_or(0, |i| i % len)
                            } else {
                                app.notice_input.focus.checked_add(1).map_or(0, |i| i % len)
                            };
                        }
                    }
                    egui::Key::Enter | egui::Key::Space => {
                        app.notice_input.focus_set = true;
                        app.notice_input.activate = Some(app.notice_input.focus);
                    }
                    egui::Key::Escape => app.notice_input.cancel = true,
                    _ => {}
                },
                egui::Event::AccessKitActionRequest(request) => {
                    if let Some(index) = app.notice_input.button_index(request.target_node) {
                        app.notice_input.focus_set = true;
                        app.notice_input.focus = index;
                        if request.action == egui::accesskit::Action::Click {
                            app.notice_input.activate = Some(index);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    if laid_out {
        app.notice_input.pointer = Some(ctx.input(|input| input.pointer.clone()));
    }
    ctx.input_mut(|input| {
        if shield || !laid_out {
            // The pass is discarded. Clear held keys too: the background must not keep a key down
            // merely because its release arrived in the prompt's creation frame.
            input.keys_down.clear();
        }
        // The interaction snapshot was computed at begin_pass, before this reset: a prompt
        // button's `Response::clicked` still reflects this pass's click. Removing the pointer from
        // the live input stops panels that read `ctx.input().pointer` directly from starting a
        // drag, scrolling or completing a click underneath the prompt.
        input.pointer = egui::PointerState::default();
        input.smooth_scroll_delta = egui::Vec2::ZERO;
        input.events.retain(|event| match event {
            egui::Event::ModifiersChanged(_) | egui::Event::WindowFocused(_) => true,
            egui::Event::Key { .. } | egui::Event::Text(_) | egui::Event::Ime(_) => false,
            egui::Event::Cut
            | egui::Event::Copy
            | egui::Event::Paste(_)
            | egui::Event::PasteImage(_)
            | egui::Event::MiddleClickPaste { .. }
            | egui::Event::MouseWheel { .. }
            | egui::Event::Zoom(_)
            | egui::Event::Rotate(_) => false,
            // The allowed Focus/Click was already dispatched above to the prompt's own button;
            // dropping every AccessKit request here leaves no residual for a covered panel to act
            // on in this pass (including a request that targeted a background node).
            egui::Event::AccessKitActionRequest(_) => false,
            egui::Event::PointerMoved(_) | egui::Event::PointerButton { .. } | egui::Event::PointerGone => false,
            // A touch End/Cancel must reach egui even in this creation pass; Start/Move is
            // background input like a new pointer gesture.
            touch @ egui::Event::Touch { .. } if touch_release(touch) => true,
            egui::Event::Touch { .. } => false,
            _ => true,
        });
    });
    ctx.stop_dragging();
    started
}

/// Whether `draw_window` must render only the new prompt for this first pass. The second pass
/// (after the discard) draws the whole window with empty raw input, so the new prompt's first
/// frame cannot activate a panel underneath it.
pub(crate) fn shield_pass(app: &LightcraftApp, ctx: &egui::Context) -> bool {
    ctx.current_pass_index() == 0 && app.notice_input.transition && prompt_key(app).is_some()
}

/// A completed, nondiscarded pass has laid out and focused the prompt: input may reach it from the
/// next host frame. A changed/dismissed prompt is reset here as well, so a queued next notice is
/// treated as a new transition.
pub(crate) fn finish_ui_pass(app: &mut LightcraftApp, ctx: &egui::Context) {
    if ctx.will_discard() {
        return;
    }
    let current = prompt_key(app);
    if app.notice_input.key != current {
        // A prompt can change during the draw itself (Tasks -> Unsaved, or OK -> next notice).
        // Leave it in its transition state so the next host frame shields the new prompt too.
        app.notice_input.reset_to(current);
        return;
    }
    if current.is_some() {
        app.notice_input.laid_out = true;
        app.notice_input.transition = false;
    }
}

/// Draw one modal prompt and return the index of the chosen button. The dim takes the modal
/// layer and a sizing pass is discarded so the first interactive pass has settled geometry;
/// `input` keeps focus on the safe Cancel/OK button unless a keyboard or AccessKit choice arrives.
fn window(ctx: &egui::Context, id: &str, title: &str, text: &str, buttons: &[(&str, &str, Choice)], input: &mut PromptInput) -> Option<usize> {
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    let last = buttons.len().checked_sub(1)?;
    let cancel_index = buttons.iter().position(|(_, _, choice)| matches!(choice, Choice::Cancel | Choice::Ok)).unwrap_or(last);
    if !input.focus_set {
        input.focus = cancel_index;
        input.focus_set = true;
    }
    input.focus = input.focus.min(last);
    let focus = input.focus;
    let manual = if std::mem::take(&mut input.cancel) { Some(cancel_index) } else { input.activate.take().filter(|index| *index <= last) };
    // The backdrop must cover modal Middle-order dialogs as well as the main view.
    let dim = egui::Area::new(egui::Id::unique((id, "dim"))).order(egui::Order::Foreground).fixed_pos(screen.min).show(ctx, |ui| {
        // The first layout of a newly shown Area is a sizing pass: egui keeps it invisible and
        // `interactable = false`. A release injected before the next host frame would then bypass
        // the layer check and reach the widget below. Finish the real hit area in this same frame.
        if ui.is_sizing_pass() {
            ctx.request_discard("notice dim: settle the modal hit area before input");
        }
        ui.allocate_rect(screen, egui::Sense::click_and_drag());
        ui.painter().rect_filled(screen, 0.0, egui::Color32::from_black_alpha(140));
    });
    // The dim (with its full-screen hit area) is the modal scope; the prompt window below is moved
    // above it. Everything below the dim, including a Middle-order Settings dialog, is then
    // outside egui's modal layer and loses keyboard focus registration as well as pointer hits.
    ctx.memory_mut(|memory| memory.set_modal_layer(dim.response.layer_id));
    let mut chosen = manual;
    let mut seen = Vec::with_capacity(buttons.len());
    let frame = egui::Frame::window(&ctx.global_style()).inner_margin(egui::Margin::symmetric(18, 14));
    let shown = egui::Window::new(crate::i18n::tr(title))
        .id(egui::Id::unique(id))
        .order(egui::Order::Foreground)
        .collapsible(false)
        .resizable(false)
        .frame(frame)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .default_width(440.0)
        .show(ctx, |ui| {
            // The prompt window has the same first-pass sizing rule as the dim above.
            if ui.is_sizing_pass() {
                ctx.request_discard("notice prompt: settle its buttons before input");
            }
            // a fixed width: a wrapped label in an auto-sized, centred window would move it every frame
            ui.set_width(420.0);
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.add(egui::Label::new(RichText::new(text).color(t.text_label)).wrap());
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                for (i, (bid, label, _)) in buttons.iter().enumerate() {
                    let r = ui.add(egui::Button::new(crate::i18n::tr(label)).min_size(vec2(0.0, 26.0)));
                    let widget = format!("button:{bid}");
                    // The real response id is the AccessKit node a screen reader targets. A hover
                    // probe's id would make every Focus/Click action fail to match the button.
                    crate::widgets::register_response(widget.clone(), r.rect, &r);
                    if i == focus {
                        r.request_focus();
                    }
                    seen.push((widget, r.id));
                    if r.clicked() && chosen.is_none() {
                        chosen = Some(i);
                    }
                }
            });
        });
    // A backdrop press can raise its area; keep the prompt buttons above it.
    if let Some(window) = shown {
        ctx.move_to_top(window.response.layer_id);
    }
    input.button_ids = seen;
    if chosen.is_some() {
        // The focused prompt button is about to disappear. Drop its focus now so this pass's
        // `egui_wants_keyboard_input` (and `text_focus`) does not make the next Esc look like it
        // belongs to a text field/dialog still underneath.
        clear_prompt_focus(ctx);
    }
    chosen
}

/// Draw the prompt owning input, if any, and act on its answer: quit-while-tasks cancels jobs
/// and re-checks for unsaved changes; quit-with-unsaved retries, quits or cancels; a settings
/// notice is removed when acknowledged. Returns without drawing when no prompt is up.
pub fn show(app: &mut LightcraftApp, ctx: &egui::Context) {
    if let Some(QuitPrompt::Tasks(text)) = app.quit_prompt.clone() {
        let buttons = [("quitAnyway", "Quit Anyway", Choice::QuitAnyway), ("quitCancel", "Cancel", Choice::Cancel)];
        let Some(i) = window(ctx, "quit-tasks", "Quit while tasks are running?", &text, &buttons, &mut app.notice_input) else {
            return;
        };
        app.quit_prompt = None;
        if buttons.get(i).is_some_and(|(_, _, choice)| *choice == Choice::QuitAnyway) {
            app.session.activity.cancel_all();
            // unsaved changes still get their own question
            if may_close(app) {
                app.ui.quit = true;
            }
        }
        return;
    }
    if let Some(QuitPrompt::Unsaved(text)) = app.quit_prompt.clone() {
        let buttons = [
            ("quitRetry", "Try Saving Again", Choice::Retry),
            ("quitAnyway", "Quit Anyway", Choice::QuitAnyway),
            ("quitCancel", "Cancel", Choice::Cancel),
        ];
        let Some(i) = window(ctx, "quit-unsaved", "Quit with unsaved changes?", &text, &buttons, &mut app.notice_input) else {
            return;
        };
        match buttons.get(i).map(|(_, _, choice)| *choice) {
            Some(Choice::Retry) => {
                app.quit_prompt = None;
                // A retry that fails again recreates the same notice text, so its `PromptKey`
                // would otherwise look unchanged: keep the old layout, focus and button ids from
                // the Retry press. Start a fresh prompt so the next frame shields it and resets
                // focus to the safe Cancel/OK choice.
                app.notice_input.reset_to(None);
                clear_prompt_focus(ctx);
                if may_close(app) {
                    app.ui.quit = true;
                }
            }
            Some(Choice::QuitAnyway) => {
                app.quit_prompt = None;
                app.quit_confirmed = true;
                app.ui.quit = true;
            }
            Some(Choice::Cancel | Choice::Ok) | None => app.quit_prompt = None,
        }
        return;
    }
    if let Some(text) = app.notices.first().cloned() {
        let buttons = [("noticeOk", "OK", Choice::Ok)];
        if window(ctx, "settings-notice", "Settings file problem", &text, &buttons, &mut app.notice_input).is_some() {
            app.notices.remove(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PromptKey, gpu_notice, prepare_ui_pass, prompt_allows_event, prompt_key};

    /// The prompt barrier keeps its own keys and pointer input, drops text/wheel input, and drops
    /// the start/move half of a new touch gesture while keeping End/Cancel to clear egui's private
    /// touch state. No prompt means everything survives.
    #[test]
    fn a_settled_prompt_allows_its_keys_and_pointer_but_not_text_wheel_or_new_touches() {
        let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services::default());
        app.notice_input.key = Some(PromptKey {
            layer: egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("test-prompt")),
            text: "prompt".into(),
            queue_len: 1,
        });
        app.notice_input.laid_out = true;
        let key = |key, modifiers| egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
        assert!(prompt_allows_event(&app, &key(egui::Key::Tab, egui::Modifiers::NONE)));
        assert!(prompt_allows_event(&app, &key(egui::Key::Escape, egui::Modifiers::NONE)));
        assert!(!prompt_allows_event(&app, &key(egui::Key::A, egui::Modifiers::NONE)));
        assert!(!prompt_allows_event(&app, &egui::Event::Text("x".into())));
        assert!(!prompt_allows_event(
            &app,
            &egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, 10.0),
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Wheel,
                modifiers: egui::Modifiers::NONE,
            }
        ));
        let touch =
            |phase| egui::Event::Touch { device_id: egui::TouchDeviceId(1), id: egui::TouchId(1), phase, pos: egui::pos2(10.0, 10.0), force: None };
        assert!(!prompt_allows_event(&app, &touch(egui::TouchPhase::Start)));
        assert!(!prompt_allows_event(&app, &touch(egui::TouchPhase::Move)));
        assert!(prompt_allows_event(&app, &touch(egui::TouchPhase::End)));
        assert!(prompt_allows_event(&app, &touch(egui::TouchPhase::Cancel)));
        assert!(prompt_allows_event(&app, &egui::Event::PointerMoved(egui::pos2(10.0, 10.0))));
        app.notice_input.key = None;
        assert!(prompt_allows_event(&app, &egui::Event::Text("x".into())));
        assert!(prompt_allows_event(&app, &touch(egui::TouchPhase::Start)));
    }

    /// A touch that began before a prompt must still get its End/Cancel while the prompt is in its
    /// creation/transition frames (`laid_out` false). Start/Move remains background input. egui's
    /// private touch state is not cleared by the pointer cleanup, so dropping the release would
    /// leave the finger pressed after the prompt goes away.
    #[test]
    fn a_prompt_in_its_creation_frames_still_releases_touches() {
        let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services::default());
        app.notice_input.key = Some(PromptKey {
            layer: egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("test-prompt")),
            text: "prompt".into(),
            queue_len: 1,
        });
        app.notice_input.laid_out = false;
        let touch =
            |phase| egui::Event::Touch { device_id: egui::TouchDeviceId(1), id: egui::TouchId(1), phase, pos: egui::pos2(10.0, 10.0), force: None };
        assert!(!prompt_allows_event(&app, &touch(egui::TouchPhase::Start)), "a new touch must not start on the background");
        assert!(!prompt_allows_event(&app, &touch(egui::TouchPhase::Move)), "a touch move is still background input");
        assert!(prompt_allows_event(&app, &touch(egui::TouchPhase::End)), "the release must clear egui's touch state");
        assert!(prompt_allows_event(&app, &touch(egui::TouchPhase::Cancel)), "a canceled touch must clear egui's touch state");
        app.notice_input.laid_out = true;
        assert!(!prompt_allows_event(&app, &touch(egui::TouchPhase::Start)), "settling does not let a new touch reach the background");
        assert!(prompt_allows_event(&app, &touch(egui::TouchPhase::End)), "settled prompts still release existing touches");
    }

    /// `prepare_ui_pass` dispatches the prompt's own AccessKit action manually; no AccessKit
    /// request may survive that pass for a covered background widget (or be dispatched twice).
    #[test]
    fn manual_prompt_dispatch_drops_all_accesskit_requests() {
        let ctx = egui::Context::default();
        let mut app = crate::LightcraftApp::new(lightcraft_engine::Session::with_demo(), crate::Services::default());
        app.quit_prompt = Some(crate::QuitPrompt::Tasks("tasks".into()));
        app.notice_input.key = prompt_key(&app);
        app.notice_input.laid_out = true;
        let mut raw = egui::RawInput::default();
        raw.events.push(egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
            action: egui::accesskit::Action::Click,
            target_tree: egui::accesskit::TreeId::ROOT,
            target_node: egui::accesskit::NodeId::from(42_u64),
            data: None,
        }));
        let mut output = ctx.run_ui(raw, |ui| {
            assert!(!prepare_ui_pass(&mut app, ui.ctx()), "the prompt was already known");
            let events = ui.ctx().input(|input| input.events.clone());
            assert!(events.is_empty(), "an AccessKit action survived manual prompt dispatch: {events:?}");
        });
        output.textures_delta.clear();
    }

    #[test]
    fn the_gpu_notice_is_only_for_a_missing_adapter() {
        let none = "no GPU adapter among Backends(VULKAN | GL) (No suitable graphics adapter found)";
        let text = gpu_notice(Some(none), true).expect("no adapter: say so");
        assert!(text.starts_with("No graphics processor found") && text.contains("System Info"), "{text}");
        assert_eq!(text.contains("nix develop"), cfg!(target_os = "linux"), "{text}");
        assert_eq!(gpu_notice(Some(none), false), None, "the preference is off");
        for reason in [
            "disabled by LIGHTCRAFT_GPU=0",
            "disabled by LIGHTCRAFT_GPU_BACKEND=off",
            "disabled by the GPU rendering preference (app.gpu)",
            "software adapter (llvmpipe (LLVM 19.1.7, 256 bits)) skipped: the CPU pipeline is faster",
            "stopped after a GPU failure: device lost",
        ] {
            assert_eq!(gpu_notice(Some(reason), true), None, "{reason}");
        }
        assert_eq!(gpu_notice(None, true), None, "the GPU is in use");
    }
}
