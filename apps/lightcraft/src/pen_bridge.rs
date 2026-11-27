//! Pen tablet fields that winit drops on Linux X11 (pressure/tilt/rotation).
//!
//! winit 0.30 delivers the pointer, but the XInput2 tablet valuators never
//! reach egui-winit. `rust-motion-tablet` listens on a second pure-Rust X
//! connection (raw XI2 events, also during a drag grab) and hands us the pen
//! sample. The app already reads pressure as `egui::Event::Touch { force }`
//! (`pen_force` in the develop panel), so we inject that event before egui
//! processes the frame. Tilt/rotation are available in the same sample for
//! brush engines that consume them.

use egui::{Event, Pos2, RawInput};

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use rust_motion_tablet::Sample;
    use std::sync::{
        Mutex, OnceLock,
        mpsc::{self, Receiver},
    };

    static RX: OnceLock<Mutex<Receiver<Option<Sample>>>> = OnceLock::new();
    static LAST_POS: Mutex<Option<Pos2>> = Mutex::new(None);

    pub fn start() {
        let (tx, rx) = mpsc::channel();
        if RX.set(Mutex::new(rx)).is_err() {
            return;
        }
        match rust_motion_tablet::x11::spawn(None, move |sample| {
            let _ = tx.send(sample);
        }) {
            Ok(_thread) => log::info!("pen: XInput2 tablet bridge started"),
            Err(e) => log::info!("pen: tablet bridge unavailable: {e}"),
        }
    }

    pub fn inject(raw: &mut RawInput) {
        // The pen sample carries no position: pair it with the pointer the
        // host delivered this frame (the pen is a pointer for winit).
        if let Some(pos) = raw.events.iter().rev().find_map(|e| match e {
            Event::PointerMoved(pos) => Some(*pos),
            Event::PointerButton { pos, .. } => Some(*pos),
            _ => None,
        }) {
            if let Ok(mut last) = LAST_POS.lock() {
                *last = Some(pos);
            }
        }

        let Some(rx) = RX.get() else {
            return;
        };
        let mut latest: Option<Option<Sample>> = None;
        if let Ok(rx) = rx.lock() {
            while let Ok(sample) = rx.try_recv() {
                latest = Some(sample);
            }
        }
        // Some(sample): pen in use. None: the pointer became a mouse again.
        let Some(Some(sample)) = latest else {
            return;
        };
        let sample = sample.sanitized();
        let pos = LAST_POS
            .lock()
            .ok()
            .and_then(|p| *p)
            .or_else(|| raw.screen_rect.map(|r| r.center()))
            .unwrap_or(Pos2::ZERO);
        raw.events.push(Event::Touch {
            device_id: egui::TouchDeviceId(0x57544d), // "WTM"
            id: egui::TouchId(1),
            phase: egui::TouchPhase::Move,
            pos,
            force: Some(sample.pressure),
        });
        // Available for brush engines that use tilt/rotation today:
        let _ = (sample.tilt_x, sample.tilt_y, sample.rotation, sample.eraser);
    }
}

#[cfg(target_os = "linux")]
pub use imp::{inject, start};

#[cfg(not(target_os = "linux"))]
pub fn start() {}

#[cfg(not(target_os = "linux"))]
pub fn inject(_raw: &mut RawInput) {}
