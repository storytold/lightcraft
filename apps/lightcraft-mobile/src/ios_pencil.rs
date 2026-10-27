//! Apple Pencil hover and double tap, and the iPad pointer (trackpad, mouse): hovering,
//! scrolling and secondary clicks.
//!
//! winit 0.30 on iOS reports touches only, so a Pencil held above the screen, or the pointer of a
//! Magic Keyboard's trackpad, never moved egui's pointer: no brush outline before painting, no
//! hover highlights or tooltips. A `UIHoverGestureRecognizer` on winit's view reports both; its
//! positions become egui `PointerMoved` events (UIKit points are egui points) and its end a
//! `PointerGone`.
//!
//! Two-finger trackpad scrolls and mouse wheels didn't scroll anything either. A
//! `UIPanGestureRecognizer` that takes scroll events only (no touches) turns them into egui
//! `MouseWheel` events at the pointer; when the fingers lift, the scroll carries on and slows
//! down as UIKit's own scrolling does ([`input`] runs it, frame by frame).
//!
//! winit also reports a secondary click (a two-finger trackpad click, a mouse's right button) as
//! an ordinary touch, so egui saw a primary click and no context menu opened. A
//! `UITapGestureRecognizer` that requires the secondary button, and is only given events that have
//! it pressed (finger touches and primary clicks never reach it, so they aren't delayed), holds
//! those touches back from winit and reports a secondary click at the pointer instead. UIKit tells
//! the buttons apart only because `ios/Info.plist` opts in to indirect input
//! (`UIApplicationSupportsIndirectInputEvents`); without it a trackpad click is a finger touch.
//!
//! With that opt-in a trackpad pinch is no longer two emulated finger touches either: a
//! `UIPinchGestureRecognizer` that takes only those pinches (transform events) turns them into
//! egui `Zoom` events at the pointer, as a desktop trackpad's pinch.
//!
//! A `UIPencilInteraction` reports the double tap on the Pencil's side; when the
//! user's preferred action (Settings ▸ Apple Pencil) is to switch to the eraser or to the previous
//! tool, the app gets [`taken_tap`] and swaps the mask brush between Add and Erase.
//!
//! Drop the hover part once eframe moves to a winit that reports iOS hover (0.31 does).
#![allow(unsafe_code)] // Objective-C class definitions and the window handle's UIView pointer

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{NSObject, ProtocolObject};
use objc2::{ClassType, DefinedClass, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_foundation::NSObjectProtocol;
use objc2_ui_kit::{
    UIEvent, UIEventButtonMask, UIEventType, UIGestureRecognizer, UIGestureRecognizerDelegate, UIGestureRecognizerState, UIHoverGestureRecognizer,
    UIPanGestureRecognizer, UIPencilInteraction, UIPencilInteractionDelegate, UIPencilPreferredAction, UIPinchGestureRecognizer, UIScrollTypeMask,
    UITapGestureRecognizer, UIView,
};

thread_local! {
    // main thread only: UIKit calls the handlers there, and the shell's frames run there
    static QUEUE: RefCell<Vec<egui::Event>> = const { RefCell::new(Vec::new()) };
    static TAPPED: Cell<bool> = const { Cell::new(false) };
    static HANDLER: RefCell<Option<Retained<Handler>>> = const { RefCell::new(None) };
    static CONTEXT: RefCell<Option<egui::Context>> = const { RefCell::new(None) };
    // the scroll gesture's translation so far, and the coasting after it (velocity, pt/s)
    static SCROLLED: Cell<egui::Vec2> = const { Cell::new(egui::Vec2::ZERO) };
    static COAST: Cell<Option<egui::Vec2>> = const { Cell::new(None) };
    static COAST_AT: Cell<Option<f64>> = const { Cell::new(None) };
    // a trackpad pinch's scale so far
    static PINCHED: Cell<f64> = const { Cell::new(1.0) };
}

/// How fast coasting slows down (per second, exponential) and where it stops (pt/s).
const COAST_DECAY: f32 = 4.0;
const COAST_STOP: f32 = 20.0;

/// A secondary-button press or release at `pos`.
fn secondary(pos: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton { pos, button: egui::PointerButton::Secondary, pressed, modifiers: egui::Modifiers::default() }
}

/// A trackpad scroll by `delta`: fingers on the trackpad (`Trackpad`), or the coasting after they
/// lift (`Momentum`).
fn wheel(delta: egui::Vec2, phase: egui::TouchPhase, source: egui::MouseWheelSource) -> egui::Event {
    egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta, phase, source, modifiers: egui::Modifiers::default() }
}

/// The view hovering is reported in (winit's).
struct Ivars {
    view: Retained<UIView>,
}

define_class!(
    /// Target of the hover recognizer and delegate of the Pencil interaction.
    #[unsafe(super(NSObject))]
    #[name = "LCPencilHandler"]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    struct Handler;

    unsafe impl NSObjectProtocol for Handler {}

    impl Handler {
        #[unsafe(method(hover:))]
        fn hover(&self, recognizer: &UIGestureRecognizer) {
            let state = recognizer.state();
            let event = if state == UIGestureRecognizerState::Began || state == UIGestureRecognizerState::Changed {
                let p = recognizer.locationInView(Some(&self.ivars().view));
                egui::Event::PointerMoved(egui::pos2(p.x as f32, p.y as f32))
            } else {
                egui::Event::PointerGone
            };
            QUEUE.with_borrow_mut(|q| q.push(event));
            repaint();
        }

        #[unsafe(method(scroll:))]
        fn scroll(&self, recognizer: &UIPanGestureRecognizer) {
            let view = &self.ivars().view;
            let state = recognizer.state();
            let at = recognizer.locationInView(Some(view));
            let t = recognizer.translationInView(Some(view));
            let t = egui::vec2(t.x as f32, t.y as f32);
            let mut events = vec![egui::Event::PointerMoved(egui::pos2(at.x as f32, at.y as f32))];
            if state == UIGestureRecognizerState::Began {
                COAST.set(None);
                SCROLLED.set(t);
                events.push(wheel(t, egui::TouchPhase::Start, egui::MouseWheelSource::Trackpad));
            } else if state == UIGestureRecognizerState::Changed {
                events.push(wheel(t - SCROLLED.replace(t), egui::TouchPhase::Move, egui::MouseWheelSource::Trackpad));
            } else {
                events.push(wheel(t - SCROLLED.replace(egui::Vec2::ZERO), egui::TouchPhase::End, egui::MouseWheelSource::Trackpad));
                if state == UIGestureRecognizerState::Ended {
                    let v = recognizer.velocityInView(Some(view));
                    let v = egui::vec2(v.x as f32, v.y as f32);
                    COAST.set((v.length() > COAST_STOP).then_some(v));
                    COAST_AT.set(None);
                }
            }
            QUEUE.with_borrow_mut(|q| q.extend(events));
            repaint();
        }

        #[unsafe(method(secondaryClick:))]
        fn secondary_click(&self, recognizer: &UITapGestureRecognizer) {
            if recognizer.state() != UIGestureRecognizerState::Ended {
                return;
            }
            let p = recognizer.locationInView(Some(&self.ivars().view));
            let pos = egui::pos2(p.x as f32, p.y as f32);
            QUEUE.with_borrow_mut(|q| q.extend([egui::Event::PointerMoved(pos), secondary(pos, true), secondary(pos, false)]));
            repaint();
        }

        #[unsafe(method(pinch:))]
        fn pinch(&self, recognizer: &UIPinchGestureRecognizer) {
            let state = recognizer.state();
            if state != UIGestureRecognizerState::Began && state != UIGestureRecognizerState::Changed {
                return;
            }
            let scale = recognizer.scale();
            let before = if state == UIGestureRecognizerState::Began { 1.0 } else { PINCHED.get() };
            PINCHED.set(scale);
            if !(scale > 0.0 && before > 0.0) {
                return;
            }
            let p = recognizer.locationInView(Some(&self.ivars().view));
            let pos = egui::pos2(p.x as f32, p.y as f32);
            QUEUE.with_borrow_mut(|q| q.extend([egui::Event::PointerMoved(pos), egui::Event::Zoom((scale / before) as f32)]));
            repaint();
        }
    }

    unsafe impl UIGestureRecognizerDelegate for Handler {
        // (the delegate of the secondary-click and the trackpad-pinch recognizers)
        #[unsafe(method(gestureRecognizer:shouldReceiveEvent:))]
        fn should_receive_event(&self, recognizer: &UIGestureRecognizer, event: &UIEvent) -> bool {
            if recognizer.isKindOfClass(UIPinchGestureRecognizer::class()) {
                event.r#type() == UIEventType::Transform
            } else {
                event.buttonMask().contains(UIEventButtonMask::Secondary)
            }
        }
    }

    unsafe impl UIPencilInteractionDelegate for Handler {
        #[unsafe(method(pencilInteractionDidTap:))]
        fn pencil_interaction_did_tap(&self, _interaction: &UIPencilInteraction) {
            let action = UIPencilInteraction::preferredTapAction(self.mtm());
            if action == UIPencilPreferredAction::SwitchEraser || action == UIPencilPreferredAction::SwitchPrevious {
                TAPPED.set(true);
                repaint();
            }
        }
    }
);

fn repaint() {
    CONTEXT.with_borrow(|c| {
        if let Some(c) = c {
            c.request_repaint();
        }
    });
}

/// Adds the hover, scroll and secondary-click recognizers and the Pencil interaction to winit's
/// view, once.
pub fn install(handle: raw_window_handle::RawWindowHandle, ctx: &egui::Context) {
    let raw_window_handle::RawWindowHandle::UiKit(h) = handle else { return };
    CONTEXT.with_borrow_mut(|c| *c = Some(ctx.clone()));
    if HANDLER.with_borrow(Option::is_some) {
        return;
    }
    // SAFETY: on iOS the window handle's `ui_view` is winit's UIView, alive while the window is
    let view: &UIView = unsafe { h.ui_view.cast::<UIView>().as_ref() };
    let mtm = view.mtm();
    let handler = Handler::alloc(mtm).set_ivars(Ivars { view: view.retain() });
    // SAFETY: NSObject's designated initializer
    let handler: Retained<Handler> = unsafe { msg_send![super(handler), init] };
    // SAFETY: `hover:` is a method of the target taking the recognizer; the target is kept alive
    // in HANDLER for the app's lifetime (recognizers don't retain their targets)
    let hover = unsafe { UIHoverGestureRecognizer::initWithTarget_action(UIHoverGestureRecognizer::alloc(mtm), Some(&handler), Some(sel!(hover:))) };
    view.addGestureRecognizer(&hover);
    // SAFETY: as above, with `scroll:`
    let scroll = unsafe { UIPanGestureRecognizer::initWithTarget_action(UIPanGestureRecognizer::alloc(mtm), Some(&handler), Some(sel!(scroll:))) };
    // trackpad and mouse-wheel scrolls only: no finger touch starts it (those stay winit's)
    scroll.setAllowedScrollTypesMask(UIScrollTypeMask::All);
    scroll.setMaximumNumberOfTouches(0);
    view.addGestureRecognizer(&scroll);
    // SAFETY: as above, with `secondaryClick:`
    let click =
        unsafe { UITapGestureRecognizer::initWithTarget_action(UITapGestureRecognizer::alloc(mtm), Some(&handler), Some(sel!(secondaryClick:))) };
    click.setButtonMaskRequired(UIEventButtonMask::Secondary);
    // winit gets none of a secondary click's touches (they'd be a primary click there)
    click.setDelaysTouchesBegan(true);
    click.setDelegate(Some(ProtocolObject::from_ref(&*handler)));
    view.addGestureRecognizer(&click);
    // SAFETY: as above, with `pinch:`
    let pinch = unsafe { UIPinchGestureRecognizer::initWithTarget_action(UIPinchGestureRecognizer::alloc(mtm), Some(&handler), Some(sel!(pinch:))) };
    // trackpad pinches only (see the delegate): finger pinches stay winit's touches
    pinch.setCancelsTouchesInView(false);
    pinch.setDelegate(Some(ProtocolObject::from_ref(&*handler)));
    view.addGestureRecognizer(&pinch);
    let pencil = UIPencilInteraction::new(mtm);
    pencil.setDelegate(Some(ProtocolObject::from_ref(&*handler)));
    view.addInteraction(ProtocolObject::from_ref(&*pencil));
    HANDLER.with_borrow_mut(|h| *h = Some(handler));
    let _ = Handler::class();
}

/// Moves the hover positions and scrolls reported since the last frame into `raw`, and the
/// coasting of a scroll whose fingers have lifted. A touch in progress takes the pointer itself
/// (UIKit reports no hover then).
pub fn input(raw: &mut egui::RawInput) {
    QUEUE.with_borrow_mut(|q| raw.events.append(q));
    let (Some(v), Some(now)) = (COAST.get(), raw.time) else { return };
    let dt = COAST_AT.replace(Some(now)).map_or(0.0, |then| (now - then).clamp(0.0, 0.1) as f32);
    let v = v * (-COAST_DECAY * dt).exp();
    if v.length() < COAST_STOP {
        COAST.set(None);
        return;
    }
    COAST.set(Some(v));
    raw.events.push(wheel(v * dt, egui::TouchPhase::Move, egui::MouseWheelSource::Momentum));
    repaint();
}

/// Whether the Pencil was double-tapped (to switch to the eraser) since the last call.
pub fn taken_tap() -> bool {
    TAPPED.replace(false)
}
