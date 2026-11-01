//! Pending library input belongs to real widget buffers and raw host events, not provider IO.

use egui::{Context, Event, Id, PointerButton, RawInput};

use super::Draft;

#[derive(Clone, Default)]
struct RawState {
    pending: bool,
    buttons: Vec<PointerButton>,
}

fn raw_key() -> Id {
    Id::unique("provider-library-raw-input")
}

fn drafts_key() -> Id {
    Id::unique("provider-library-slider-drafts")
}

/// One ID per existing slider Draft. Hiding a panel does not retire its text buffer.
pub(super) fn track_draft(ctx: &Context, id: Id, active: bool) {
    ctx.data_mut(|data| {
        let mut ids = data.get_temp::<Vec<Id>>(drafts_key()).unwrap_or_default();
        ids.retain(|id| data.get_temp::<Draft>(*id).is_some());
        if active && !ids.contains(&id) {
            ids.push(id);
        } else if !active {
            ids.retain(|old| *old != id);
        }
        if ids.is_empty() {
            data.remove::<Vec<Id>>(drafts_key());
        } else {
            data.insert_temp(drafts_key(), ids);
        }
    });
}

pub(crate) fn has_draft(ctx: &Context) -> bool {
    ctx.data_mut(|data| {
        let mut ids = data.get_temp::<Vec<Id>>(drafts_key()).unwrap_or_default();
        ids.retain(|id| data.get_temp::<Draft>(*id).is_some());
        let pending = !ids.is_empty();
        if pending {
            data.insert_temp(drafts_key(), ids);
        } else {
            data.remove::<Vec<Id>>(drafts_key());
        }
        pending
    })
}

/// Called before input processing and after synthetic injection. No input is read from logic.
pub(crate) fn note_raw(ctx: &Context, raw: &RawInput) {
    ctx.data_mut(|data| {
        let mut state = data.get_temp::<RawState>(raw_key()).unwrap_or_default();
        if !raw.focused {
            state.buttons.clear();
        }
        state.pending |= !raw.dropped_files.is_empty();
        for event in &raw.events {
            match event {
                Event::PointerButton { button, pressed, .. } => {
                    state.buttons.retain(|old| old != button);
                    if *pressed {
                        state.buttons.push(*button);
                    }
                    state.pending = true;
                }
                Event::PointerMoved(_) => state.pending |= !state.buttons.is_empty(),
                Event::PointerGone | Event::WindowFocused(false) => {
                    state.pending |= !state.buttons.is_empty();
                    state.buttons.clear();
                }
                Event::ModifiersChanged(_) | Event::WindowFocused(true) => {}
                _ => state.pending = true,
            }
        }
        data.insert_temp(raw_key(), state);
    });
}

/// Mirror an actual pointer cancellation, without manufacturing a release or changing a draft.
pub(crate) fn cancel_pointer(ctx: &Context) {
    ctx.data_mut(|data| {
        let mut state = data.get_temp::<RawState>(raw_key()).unwrap_or_default();
        state.buttons.clear();
        state.pending = true;
        data.insert_temp(raw_key(), state);
    });
}

/// Only a completed nondiscarded UI pass may consume the raw-input barrier.
pub(crate) fn finish_pass(ctx: &Context) {
    ctx.data_mut(|data| {
        let mut state = data.get_temp::<RawState>(raw_key()).unwrap_or_default();
        state.pending = false;
        data.insert_temp(raw_key(), state);
    });
}

pub(crate) fn pending_raw(ctx: &Context) -> bool {
    ctx.data(|data| data.get_temp::<RawState>(raw_key()).is_some_and(|state| state.pending || !state.buttons.is_empty()))
}
