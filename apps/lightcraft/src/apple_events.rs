//! macOS open-documents and quit Apple events.
//!
//! Finder double-clicks, Open With, drops on the Dock icon and `open -a LightCraft photo.jpg` don't
//! pass paths on the command line: Launch Services sends the running (or just-launched) app a
//! `kAEOpenDocuments` ('odoc') Apple event. winit 0.30 doesn't handle it and owns the
//! `NSApplicationDelegate`, and handling it ourselves needs Objective-C class declarations, i.e.
//! `unsafe`, which this workspace forbids outside `crates/sysmem`. The audited `fmv-macos-events`
//! crate, which PhotoCraft and FilmCraft use for the same purpose, wraps exactly that (an
//! `NSAppleEventManager` handler registered before Finder's launch event, leaving winit's
//! delegate alone) behind a safe main-thread API; this module adapts it to the
//! [`OsEvent`](lightcraft_ui_egui::OsEvent)s the UI polls every frame. Without a handler the
//! event reaches only AppKit, which answers "LightCraft cannot open files in the “JPEG image”
//! format", and the photo never arrives.
//!
//! On the other platforms files arrive as arguments (see `launch_imports`) and [`Source`] is inert.

use lightcraft_ui_egui::OsEventsFn;

/// Keeps the Apple-event handlers registered; hold it on the main thread until the event loop
/// returns.
pub struct Guard {
    #[cfg(target_os = "macos")]
    _registration: fmv_macos_events::Registration,
}

/// Where the UI reads the OS requests from.
#[derive(Clone, Default)]
pub struct Source {
    #[cfg(target_os = "macos")]
    inbox: Option<fmv_macos_events::Inbox>,
}

/// Register the handlers. Call on the main thread before the event loop starts, so the event that
/// launched the app (a Finder double-click) is caught too.
pub fn install() -> (Guard, Source) {
    #[cfg(target_os = "macos")]
    {
        let (registration, inbox) = fmv_macos_events::Registration::install();
        (Guard { _registration: registration }, Source { inbox: Some(inbox) })
    }
    #[cfg(not(target_os = "macos"))]
    {
        (Guard {}, Source::default())
    }
}

impl Source {
    /// The queue the UI drains ([`Services::os_events`](lightcraft_ui_egui::Services::os_events));
    /// events arriving later wake `ctx`. `None` where the OS sends none.
    pub fn connect(&self, ctx: &egui::Context) -> Option<OsEventsFn> {
        #[cfg(target_os = "macos")]
        {
            use fmv_macos_events::Event;
            use lightcraft_ui_egui::OsEvent;
            let inbox = self.inbox.clone()?;
            let ctx = ctx.clone();
            inbox.set_wake(move || ctx.request_repaint());
            Some(Box::new(move || {
                inbox
                    .drain()
                    .into_iter()
                    .map(|e| match e {
                        Event::Open(paths) => OsEvent::Open(paths.into_iter().map(|p| p.to_string_lossy().into_owned()).collect()),
                        Event::Quit => OsEvent::Quit,
                    })
                    .collect()
            }))
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = ctx;
            None
        }
    }
}
