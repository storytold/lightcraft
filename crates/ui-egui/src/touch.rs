//! Touch and stylus gesture processing for iPad / touch screen interactions.
//!
//! Handles:
//! - 2-finger tap -> Undo (`edit.undo`)
//! - 3-finger tap -> Redo (`edit.redo`)
//! - Apple Pencil 2 double-tap -> Loupe 100% / Fit toggle (`view.zoomToggle`)
//! - Multi-touch tracking from egui touch events.

use egui::{Event, Pos2, TouchPhase};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchAction {
    Undo,
    Redo,
}

#[derive(Clone, Copy, Debug)]
struct TouchPoint {
    id: u64,
    start_pos: Pos2,
    current_pos: Pos2,
    #[allow(dead_code)]
    start_time: f64,
}

#[derive(Clone, Debug, Default)]
pub struct TouchGestureTracker {
    touches: Vec<TouchPoint>,
    max_simultaneous: usize,
    first_touch_time: f64,
    all_stationary: bool,
}

impl TouchGestureTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Process raw egui events for multi-touch gestures.
    /// Returns `Some(TouchAction)` when a multi-finger tap gesture is completed.
    pub fn process_events(&mut self, now: f64, events: &[Event]) -> Option<TouchAction> {
        let mut result = None;

        for event in events {
            if let Event::Touch { id, phase, pos, .. } = event {
                let uid = id.0;
                match phase {
                    TouchPhase::Start => {
                        if self.touches.is_empty() {
                            self.first_touch_time = now;
                            self.max_simultaneous = 0;
                            self.all_stationary = true;
                        }
                        self.touches.push(TouchPoint { id: uid, start_pos: *pos, current_pos: *pos, start_time: now });
                        self.max_simultaneous = self.max_simultaneous.max(self.touches.len());
                    }
                    TouchPhase::Move => {
                        if let Some(tp) = self.touches.iter_mut().find(|t| t.id == uid) {
                            tp.current_pos = *pos;
                            if tp.start_pos.distance(*pos) > 30.0 {
                                self.all_stationary = false;
                            }
                        }
                    }
                    TouchPhase::End => {
                        if let Some(pos_idx) = self.touches.iter().position(|t| t.id == uid) {
                            let tp = self.touches.remove(pos_idx);
                            if tp.start_pos.distance(*pos) > 30.0 {
                                self.all_stationary = false;
                            }
                        }
                        // When all fingers have lifted, evaluate multi-finger tap
                        if self.touches.is_empty() {
                            let duration = now - self.first_touch_time;
                            // Tap must be quick (<= 0.45s) and stationary
                            if self.all_stationary && duration <= 0.45 {
                                if self.max_simultaneous == 2 {
                                    result = Some(TouchAction::Undo);
                                } else if self.max_simultaneous == 3 {
                                    result = Some(TouchAction::Redo);
                                }
                            }
                            self.max_simultaneous = 0;
                            self.all_stationary = true;
                        }
                    }
                    TouchPhase::Cancel => {
                        self.touches.clear();
                        self.max_simultaneous = 0;
                        self.all_stationary = true;
                    }
                }
            }
        }

        result
    }
}
