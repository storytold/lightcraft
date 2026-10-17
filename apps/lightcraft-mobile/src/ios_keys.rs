//! Hardware keyboards on iPadOS (Magic Keyboard, Smart Keyboard, Bluetooth), and text input.
//!
//! winit 0.30's iOS view only implements `UIKeyInput` (typed text and ⌫, for the soft keyboard),
//! so no shortcut, arrow, Escape or modifier ever reached the app, and its clipboard on iOS is
//! the app's own. The shell keeps one of two hidden views of its own as the first responder:
//!
//! - [`KeyView`] while egui edits no text: UIKit hands it every key press (`UIPress.key`: a HID
//!   usage and the ⌘⌥⌃⇧ flags), which become egui key events ([`lightcraft_ui_egui::hid`]) on
//!   the next frame, with ⌘ as egui's command modifier. It takes no text, so no soft keyboard.
//! - [`TextView`] while a text field has focus: it implements `UIKeyInput`, so the soft keyboard
//!   shows and typing arrives as text (with the system's dead keys) as it did through winit, but
//!   ⌘ / ⌃ combinations, arrows, Escape, Tab and the navigation keys become key events, ⌘C / ⌘X /
//!   ⌘V become egui's copy / cut / paste, and pasting reads the system pasteboard. Text egui
//!   copies goes to the system pasteboard too ([`after_frame`]).
//!
//! eframe still makes winit's view the first responder when a text field gains focus
//! (`set_ime_allowed`); the next frame hands it to [`TextView`].
//!
//! Drop this once eframe moves to a winit with iOS key events (0.31 has `pressesBegan`).
#![allow(unsafe_code)] // Objective-C class definitions and the window handle's UIView pointer

use std::cell::RefCell;
use std::collections::HashSet;

use objc2::rc::Retained;
use objc2::runtime::NSObject;
use objc2::{ClassType, MainThreadOnly, define_class, msg_send};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSObjectProtocol, NSSet, NSString};
use objc2_ui_kit::{UIKeyInput, UIPasteboard, UIPress, UIPressesEvent, UIResponder, UITextInputTraits, UIView};

use lightcraft_ui_egui::hid;

thread_local! {
    // main thread only: UIKit calls the views there, and the shell's frames run there
    static KEY_VIEW: RefCell<Option<Retained<KeyView>>> = const { RefCell::new(None) };
    static TEXT_VIEW: RefCell<Option<Retained<TextView>>> = const { RefCell::new(None) };
    static QUEUE: RefCell<Vec<egui::Event>> = const { RefCell::new(Vec::new()) };
    static MODIFIERS: RefCell<egui::Modifiers> = const { RefCell::new(egui::Modifiers::NONE) };
    // keys whose press the text view handled itself: their release is its too
    static TAKEN: RefCell<HashSet<i64>> = RefCell::new(HashSet::new());
    static CONTEXT: RefCell<Option<egui::Context>> = const { RefCell::new(None) };
}

define_class!(
    /// A 1 × 1 pt view that is the first responder while egui edits no text, for the key presses.
    #[unsafe(super(UIView, UIResponder, NSObject))]
    #[name = "LCKeyView"]
    #[thread_kind = MainThreadOnly]
    struct KeyView;

    impl KeyView {
        #[unsafe(method(canBecomeFirstResponder))]
        fn can_become_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(pressesBegan:withEvent:))]
        fn presses_began(&self, presses: &NSSet<UIPress>, _event: Option<&UIPressesEvent>) {
            queue(self.mtm(), presses, true, false);
        }

        #[unsafe(method(pressesEnded:withEvent:))]
        fn presses_ended(&self, presses: &NSSet<UIPress>, _event: Option<&UIPressesEvent>) {
            queue(self.mtm(), presses, false, false);
        }

        #[unsafe(method(pressesCancelled:withEvent:))]
        fn presses_cancelled(&self, presses: &NSSet<UIPress>, _event: Option<&UIPressesEvent>) {
            queue(self.mtm(), presses, false, false);
        }
    }
);

define_class!(
    /// The first responder while egui edits text: typed text through `UIKeyInput`, editing keys
    /// and shortcuts as key presses.
    #[unsafe(super(UIView, UIResponder, NSObject))]
    #[name = "LCTextView"]
    #[thread_kind = MainThreadOnly]
    struct TextView;

    impl TextView {
        #[unsafe(method(canBecomeFirstResponder))]
        fn can_become_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(pressesBegan:withEvent:))]
        fn presses_began(&self, presses: &NSSet<UIPress>, event: Option<&UIPressesEvent>) {
            if !queue(self.mtm(), presses, true, true) {
                // SAFETY: UIResponder's own handling, which types the keys through `insertText:`
                let _: () = unsafe { msg_send![super(self), pressesBegan: presses, withEvent: event] };
            }
        }

        #[unsafe(method(pressesEnded:withEvent:))]
        fn presses_ended(&self, presses: &NSSet<UIPress>, event: Option<&UIPressesEvent>) {
            if !queue(self.mtm(), presses, false, true) {
                // SAFETY: as above
                let _: () = unsafe { msg_send![super(self), pressesEnded: presses, withEvent: event] };
            }
        }

        #[unsafe(method(pressesCancelled:withEvent:))]
        fn presses_cancelled(&self, presses: &NSSet<UIPress>, event: Option<&UIPressesEvent>) {
            if !queue(self.mtm(), presses, false, true) {
                // SAFETY: as above
                let _: () = unsafe { msg_send![super(self), pressesCancelled: presses, withEvent: event] };
            }
        }
    }

    unsafe impl NSObjectProtocol for TextView {}

    unsafe impl UITextInputTraits for TextView {}

    unsafe impl UIKeyInput for TextView {
        #[unsafe(method(hasText))]
        fn has_text(&self) -> bool {
            true
        }

        #[unsafe(method(insertText:))]
        fn insert_text(&self, text: &NSString) {
            insert(&text.to_string());
        }

        #[unsafe(method(deleteBackward))]
        fn delete_backward(&self) {
            tap(egui::Key::Backspace);
        }
    }
);

/// HID usages of the modifier keys themselves (left / right ⌃ ⇧ ⌥ ⌘): their release clears them.
fn modifier_key(usage: i64, m: &mut egui::Modifiers) {
    match usage {
        0xE0 | 0xE4 => m.ctrl = false,
        0xE1 | 0xE5 => m.shift = false,
        0xE2 | 0xE6 => m.alt = false,
        0xE3 | 0xE7 => {
            m.command = false;
            m.mac_cmd = false;
        }
        _ => {}
    }
}

/// Queue egui events for `presses`. `editing`: the text view's, which takes only the editing keys
/// and shortcuts ([`hid::edits_by_key`]). Returns whether every press was taken (else the caller
/// passes them on to UIKit to type).
fn queue(mtm: objc2::MainThreadMarker, presses: &NSSet<UIPress>, pressed: bool, editing: bool) -> bool {
    let mut all_taken = true;
    for press in presses.iter() {
        let Some(key) = press.key(mtm) else {
            all_taken = false;
            continue;
        };
        let usage = key.keyCode().0 as i64;
        let mut modifiers = hid::apple_modifiers(key.modifierFlags().0 as i64);
        if !pressed {
            // a released modifier key may still be in the flags of its own release
            modifier_key(usage, &mut modifiers);
        }
        // egui follows the modifiers from these events
        let changed = MODIFIERS.with_borrow_mut(|m| std::mem::replace(m, modifiers) != modifiers);
        if changed {
            push(egui::Event::ModifiersChanged(modifiers));
        }
        let Some(k) = hid::key(usage) else {
            // modifier keys and keys egui has no name for: UIKit sees them while editing
            all_taken &= !editing;
            continue;
        };
        let taken = !editing
            || if pressed {
                let t = hid::edits_by_key(k, modifiers);
                if t {
                    TAKEN.with_borrow_mut(|s| s.insert(usage));
                }
                t
            } else {
                TAKEN.with_borrow_mut(|s| s.remove(&usage))
            };
        if !taken {
            all_taken = false;
            continue;
        }
        // in a text field ⌘C / ⌘X / ⌘V are egui's clipboard events (egui-winit does the same)
        let clipboard = editing && modifiers.command && !modifiers.alt && !modifiers.ctrl;
        match k {
            egui::Key::C if clipboard => {
                if pressed {
                    push(egui::Event::Copy);
                }
            }
            egui::Key::X if clipboard => {
                if pressed {
                    push(egui::Event::Cut);
                }
            }
            egui::Key::V if clipboard => {
                // SAFETY: reading the general pasteboard's string, on the main thread
                if pressed && let Some(text) = unsafe { UIPasteboard::generalPasteboard().string() } {
                    push(egui::Event::Paste(text.to_string()));
                }
            }
            _ => push(egui::Event::Key { key: k, physical_key: Some(k), pressed, repeat: false, modifiers }),
        }
    }
    repaint();
    all_taken
}

/// Text from `insertText:` (typed on the soft keyboard or a hardware one).
fn insert(text: &str) {
    for part in text.split_inclusive(['\n', '\r', '\t']) {
        let (body, end) = match part.char_indices().last() {
            Some((i, c @ ('\n' | '\r' | '\t'))) => (&part[..i], Some(c)),
            _ => (part, None),
        };
        if !body.is_empty() {
            push(egui::Event::Text(body.to_string()));
        }
        match end {
            Some('\t') => tap(egui::Key::Tab),
            Some(_) => tap(egui::Key::Enter),
            None => {}
        }
    }
    repaint();
}

/// A press and release of `key` with the modifiers held.
fn tap(key: egui::Key) {
    let modifiers = MODIFIERS.with_borrow(|m| *m);
    for pressed in [true, false] {
        push(egui::Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers });
    }
    repaint();
}

fn push(e: egui::Event) {
    QUEUE.with_borrow_mut(|q| q.push(e));
}

fn repaint() {
    CONTEXT.with_borrow(|c| {
        if let Some(c) = c {
            c.request_repaint();
        }
    });
}

/// Adds the two views to winit's view (from eframe's window handle), once.
pub fn install(handle: raw_window_handle::RawWindowHandle, ctx: &egui::Context) {
    let raw_window_handle::RawWindowHandle::UiKit(h) = handle else { return };
    CONTEXT.with_borrow_mut(|c| *c = Some(ctx.clone()));
    if KEY_VIEW.with_borrow(Option::is_some) {
        return;
    }
    // SAFETY: on iOS the window handle's `ui_view` is winit's UIView, alive while the window is
    let parent: &UIView = unsafe { h.ui_view.cast::<UIView>().as_ref() };
    let mtm = parent.mtm();
    let frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1.0, 1.0));
    // SAFETY: `initWithFrame:` is UIView's designated initializer; the classes add no ivars
    let key: Retained<KeyView> = unsafe { msg_send![KeyView::alloc(mtm), initWithFrame: frame] };
    // SAFETY: as above
    let text: Retained<TextView> = unsafe { msg_send![TextView::alloc(mtm), initWithFrame: frame] };
    for v in [&**key as &UIView, &**text] {
        v.setHidden(true);
        parent.addSubview(v);
    }
    KEY_VIEW.with_borrow_mut(|v| *v = Some(key));
    TEXT_VIEW.with_borrow_mut(|v| *v = Some(text));
    let _ = (KeyView::class(), TextView::class());
}

/// Moves the keys and text queued since the last frame into `raw`, and gives the keyboard to the
/// text view while a text field has focus (`editing`), else to the key view.
pub fn input(raw: &mut egui::RawInput, editing: bool) {
    QUEUE.with_borrow_mut(|q| raw.events.append(q));
    let first = |v: &UIView| {
        if !v.isFirstResponder() {
            v.becomeFirstResponder();
        }
    };
    if editing {
        TEXT_VIEW.with_borrow(|v| v.as_deref().map(|v| first(v)));
    } else {
        KEY_VIEW.with_borrow(|v| v.as_deref().map(|v| first(v)));
    }
}

/// Text egui copied (⌘C, ⌘X in a text field) goes to the system pasteboard: call at the end of
/// the frame's UI.
pub fn after_frame(ctx: &egui::Context) {
    let copied: Vec<String> = ctx.output(|o| {
        o.commands
            .iter()
            .filter_map(|c| match c {
                egui::OutputCommand::CopyText(t) => Some(t.clone()),
                _ => None,
            })
            .collect()
    });
    if let Some(text) = copied.last() {
        // SAFETY: writing a string to the general pasteboard, on the main thread
        unsafe { UIPasteboard::generalPasteboard().setString(Some(&NSString::from_str(text))) };
    }
}
