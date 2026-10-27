//! UIScene lifecycle for winit 0.30 on iOS.
//!
//! iOS 27 stops apps that don't adopt the scene lifecycle
//! (`_UIApplicationEvaluateRuntimeIssueForNoSceneLifecycleAdoption`, a trap at launch), and winit
//! 0.30 (what eframe 0.36 uses) knows nothing about scenes. So the shell adopts it:
//! `ios/Info.plist` declares a scene configuration whose delegate is [`SceneDelegate`]
//! (`LCSceneDelegate`), and the window winit created is attached to the scene once both exist.
//! winit takes the app's lifecycle from `UIApplication…Notification`s, which UIKit still posts
//! for scene-based apps, so nothing else needs forwarding.
//!
//! The same delegate passes on memory warnings ([`crate::memory::warn`]) and tracks the soft
//! keyboard: winit 0.30 shows it (when egui focuses a text
//! field) but reports nothing about the screen it covers, so [`keyboard_height`] comes from
//! UIKit's keyboard-frame notifications, for the shell to fold into egui's safe area.
//!
//! Drop the scene part once eframe moves to a winit with its own scene support (0.31).
#![allow(unsafe_code)] // Objective-C class definition and the UIView pointer from the window handle

use std::cell::RefCell;
use std::sync::atomic::{AtomicU32, Ordering};

use objc2::rc::Retained;
use objc2::runtime::NSObject;
use objc2::{ClassType, MainThreadOnly, Message, define_class, sel};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSObjectProtocol, NSValue};
use objc2_ui_kit::{
    NSValueUIGeometryExtensions, UIApplicationDidReceiveMemoryWarningNotification, UIKeyboardFrameEndUserInfoKey,
    UIKeyboardWillChangeFrameNotification, UIKeyboardWillHideNotification, UIResponder, UIScene, UISceneConnectionOptions, UISceneDelegate,
    UISceneSession, UIView, UIWindow, UIWindowScene, UIWindowSceneDelegate,
};

/// Points of the window's bottom the soft keyboard covers (f32 bits; 0 when it's down).
static KEYBOARD: AtomicU32 = AtomicU32::new(0);

thread_local! {
    // main thread only: UIKit calls the delegate there, and eframe creates the app there
    static SCENE: RefCell<Option<Retained<UIWindowScene>>> = const { RefCell::new(None) };
    static WINDOW: RefCell<Option<Retained<UIWindow>>> = const { RefCell::new(None) };
    // repainted when the keyboard moves (egui may be idle while it animates in)
    static CONTEXT: RefCell<Option<egui::Context>> = const { RefCell::new(None) };
}

define_class!(
    /// The scene delegate named in `ios/Info.plist` (`UISceneDelegateClassName`).
    #[unsafe(super(UIResponder, NSObject))]
    #[name = "LCSceneDelegate"]
    #[thread_kind = MainThreadOnly]
    struct SceneDelegate;

    unsafe impl NSObjectProtocol for SceneDelegate {}

    unsafe impl UISceneDelegate for SceneDelegate {
        #[unsafe(method(scene:willConnectToSession:options:))]
        fn will_connect(&self, scene: &UIScene, _session: &UISceneSession, _options: &UISceneConnectionOptions) {
            let Some(scene) = scene.downcast_ref::<UIWindowScene>() else { return };
            log::info!("scene connected");
            SCENE.with_borrow_mut(|s| *s = Some(scene.retain()));
            attach();
            let center = NSNotificationCenter::defaultCenter();
            // SAFETY: the selectors are methods of this class taking the notification; UIKit keeps
            // the scene delegate for the scene's lifetime (and observers are weak since iOS 9)
            unsafe {
                center.addObserver_selector_name_object(self, sel!(keyboardWillChangeFrame:), Some(UIKeyboardWillChangeFrameNotification), None);
                center.addObserver_selector_name_object(self, sel!(keyboardWillHide:), Some(UIKeyboardWillHideNotification), None);
                center.addObserver_selector_name_object(self, sel!(memoryWarning:), Some(UIApplicationDidReceiveMemoryWarningNotification), None);
            }
        }
    }

    impl SceneDelegate {
        #[unsafe(method(keyboardWillChangeFrame:))]
        fn keyboard_will_change_frame(&self, note: &NSNotification) {
            set_keyboard(keyboard_overlap(note).unwrap_or(0.0));
        }

        #[unsafe(method(keyboardWillHide:))]
        fn keyboard_will_hide(&self, _note: &NSNotification) {
            set_keyboard(0.0);
        }

        #[unsafe(method(memoryWarning:))]
        fn memory_warning(&self, _note: &NSNotification) {
            crate::memory::warn();
        }
    }

    unsafe impl UIWindowSceneDelegate for SceneDelegate {}
);

/// Registers `LCSceneDelegate` with the Objective-C runtime; before the event loop starts, since
/// UIKit looks the class up by name when it connects the scene.
pub fn register() {
    let _ = SceneDelegate::class();
}

/// How much of the window's bottom the keyboard's final frame (in a keyboard notification)
/// covers, in points.
fn keyboard_overlap(note: &NSNotification) -> Option<f32> {
    let info = note.userInfo()?;
    // SAFETY: a UIKit-provided key constant
    let key = unsafe { UIKeyboardFrameEndUserInfoKey };
    let value = info.objectForKey(key)?.downcast::<NSValue>().ok()?;
    // SAFETY: the end frame is documented as an NSValue holding a CGRect
    let end = unsafe { value.CGRectValue() };
    let window = WINDOW.with_borrow(|w| w.clone())?;
    Some((window.bounds().size.height - end.origin.y).max(0.0) as f32)
}

fn set_keyboard(points: f32) {
    KEYBOARD.store(points.to_bits(), Ordering::Relaxed);
    CONTEXT.with_borrow(|c| {
        if let Some(c) = c {
            c.request_repaint();
        }
    });
}

/// Points of the window's bottom the soft keyboard covers now (0 when it's down).
pub fn keyboard_height() -> f32 {
    f32::from_bits(KEYBOARD.load(Ordering::Relaxed))
}

/// The egui context to repaint when the keyboard moves.
pub fn set_context(ctx: &egui::Context) {
    CONTEXT.with_borrow_mut(|c| *c = Some(ctx.clone()));
}

unsafe extern "C" {
    /// libSystem (iOS 13+): bytes this process may still allocate before the system stops it.
    fn os_proc_available_memory() -> usize;
}

/// How much memory iOS lets this process use: what it may still allocate
/// (`os_proc_available_memory`; nearly all of it at launch). The Simulator reports 0 (no jetsam
/// there): half the machine's memory instead.
pub fn process_memory_limit() -> Option<usize> {
    // SAFETY: a documented libSystem call without arguments
    let available = unsafe { os_proc_available_memory() };
    if available > 0 {
        return Some(available);
    }
    let ram = objc2_foundation::NSProcessInfo::processInfo().physicalMemory();
    usize::try_from(ram / 2).ok()
}

/// The window's root view controller (winit's), to present system UI from.
pub fn root_view_controller() -> Option<Retained<objc2_ui_kit::UIViewController>> {
    WINDOW.with_borrow(|w| w.as_ref().and_then(|w| w.rootViewController()))
}

/// Hands over the window winit created (from the view in eframe's window handle).
pub fn set_window(handle: raw_window_handle::RawWindowHandle) {
    let raw_window_handle::RawWindowHandle::UiKit(h) = handle else { return };
    // SAFETY: on iOS the window handle's `ui_view` is winit's UIView, alive while the window is
    let view: &UIView = unsafe { h.ui_view.cast::<UIView>().as_ref() };
    let Some(window) = view.window() else { return };
    WINDOW.with_borrow_mut(|w| *w = Some(window));
    attach();
}

/// Puts the window on the scene once both exist (in either order) and shows it.
fn attach() {
    SCENE.with_borrow(|scene| {
        WINDOW.with_borrow(|window| {
            if let (Some(scene), Some(window)) = (scene, window) {
                window.setWindowScene(Some(scene));
                window.makeKeyAndVisible();
                log::info!("window attached to scene");
            }
        })
    });
}
