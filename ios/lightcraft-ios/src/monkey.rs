use eframe::egui::{Context, Event, PointerButton, Pos2, RawInput, Rect, pos2};
use lightcraft_ui_egui::LightcraftApp;

pub struct SimpleRng {
    state: u64,
}

impl SimpleRng {
    pub fn new(seed: u64) -> Self {
        Self { state: seed.max(1) }
    }
    pub fn next_u32(&mut self) -> u32 {
        self.state ^= self.state << 13;
        self.state ^= self.state >> 7;
        self.state ^= self.state << 17;
        self.state as u32
    }
    pub fn gen_range(&mut self, min: f32, max: f32) -> f32 {
        if max <= min {
            return min;
        }
        let norm = (self.next_u32() % 10000) as f32 / 10000.0;
        min + norm * (max - min)
    }
    pub fn gen_usize(&mut self, min: usize, max: usize) -> usize {
        if max <= min {
            return min;
        }
        min + (self.next_u32() as usize % (max - min))
    }
}

pub struct MonkeyTester {
    pub active: bool,
    pub step: usize,
    pub max_steps: usize,
    rng: SimpleRng,
    drag_state: Option<(Pos2, Pos2, usize)>,
    release_needed: Option<Pos2>,
    pub stats_taps: usize,
    pub stats_drags: usize,
    pub stats_texts: usize,
    pub stats_dialog_changes: usize,
    pub last_dialog: Option<String>,
    pub screen: Rect,
    pub anomalies: Vec<String>,
}

impl MonkeyTester {
    pub fn new() -> Self {
        let env_val = std::env::var("LIGHTCRAFT_MONKEY").ok();
        let (active, max_steps) = match env_val {
            Some(val) => {
                let steps = val.trim().parse::<usize>().unwrap_or(120);
                (true, steps)
            }
            None => (false, 0),
        };

        if active {
            log::info!("[MONKEY] Monkey testing ENABLED with target steps: {max_steps}");
        }

        Self {
            active,
            step: 0,
            max_steps,
            rng: SimpleRng::new(123456789),
            drag_state: None,
            release_needed: None,
            stats_taps: 0,
            stats_drags: 0,
            stats_texts: 0,
            stats_dialog_changes: 0,
            last_dialog: None,
            screen: Rect::NOTHING,
            anomalies: Vec::new(),
        }
    }

    pub fn on_raw_input(&mut self, ctx: &Context, raw: &mut RawInput) {
        if !self.active || self.step >= self.max_steps {
            return;
        }

        ctx.request_repaint();

        if let Some(rect) = raw.screen_rect {
            self.screen = rect;
        }

        let w = self.screen.width();
        let h = self.screen.height();

        if w < 100.0 || h < 100.0 {
            return;
        }

        // If a release was pending from the previous tap frame, emit it now
        if let Some(pos) = self.release_needed.take() {
            raw.events.push(Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, modifiers: Default::default() });
            return;
        }

        // If dragging in progress, step through drag trajectory
        if let Some((curr, target, frames_left)) = &mut self.drag_state {
            if *frames_left > 0 {
                *frames_left -= 1;
                let t = 1.0 - (*frames_left as f32 / 4.0);
                let next_pos = pos2(curr.x + (target.x - curr.x) * t, curr.y + (target.y - curr.y) * t);
                raw.events.push(Event::PointerMoved(next_pos));
                return;
            } else {
                let end = *target;
                self.drag_state = None;
                raw.events.push(Event::PointerButton { pos: end, button: PointerButton::Primary, pressed: false, modifiers: Default::default() });
                self.stats_drags += 1;
                return;
            }
        }

        self.step += 1;
        let kind = self.rng.gen_usize(0, 100);

        if kind < 20 {
            // Zone 1: Top Bar
            let x = self.rng.gen_range(16.0, w - 16.0);
            let y = self.rng.gen_range(30.0, 72.0);
            let pos = pos2(x, y);
            raw.events.push(Event::PointerMoved(pos));
            raw.events.push(Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers: Default::default() });
            self.release_needed = Some(pos);
            self.stats_taps += 1;
            log::info!("[MONKEY] Step {}/{}: Tap Top Bar at ({:.1}, {:.1})", self.step, self.max_steps, x, y);
        } else if kind < 40 {
            // Zone 2: Bottom Bar
            let x = self.rng.gen_range(16.0, w - 16.0);
            let y = self.rng.gen_range(h - 60.0, h - 16.0);
            let pos = pos2(x, y);
            raw.events.push(Event::PointerMoved(pos));
            raw.events.push(Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers: Default::default() });
            self.release_needed = Some(pos);
            self.stats_taps += 1;
            log::info!("[MONKEY] Step {}/{}: Tap Bottom Bar at ({:.1}, {:.1})", self.step, self.max_steps, x, y);
        } else if kind < 60 {
            // Zone 3: Sidebars / Sliders Drag
            let is_right = self.rng.next_u32().is_multiple_of(2);
            let start_x = if is_right { self.rng.gen_range(w - 240.0, w - 40.0) } else { self.rng.gen_range(20.0, 220.0) };
            let start_y = self.rng.gen_range(120.0, h - 120.0);
            let end_x = (start_x + self.rng.gen_range(-80.0, 80.0)).clamp(20.0, w - 20.0);
            let start = pos2(start_x, start_y);
            let end = pos2(end_x, start_y);

            raw.events.push(Event::PointerMoved(start));
            raw.events.push(Event::PointerButton { pos: start, button: PointerButton::Primary, pressed: true, modifiers: Default::default() });
            self.drag_state = Some((start, end, 3));
            log::info!(
                "[MONKEY] Step {}/{}: Drag slider from ({:.1}, {:.1}) to ({:.1}, {:.1})",
                self.step,
                self.max_steps,
                start_x,
                start_y,
                end_x,
                start_y
            );
        } else if kind < 80 {
            // Zone 4: Center Workspace / Dialog Cards
            let x = self.rng.gen_range(w * 0.25, w * 0.75);
            let y = self.rng.gen_range(h * 0.25, h * 0.75);
            let pos = pos2(x, y);
            raw.events.push(Event::PointerMoved(pos));
            raw.events.push(Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers: Default::default() });
            self.release_needed = Some(pos);
            self.stats_taps += 1;
            log::info!("[MONKEY] Step {}/{}: Tap Center/Dialog at ({:.1}, {:.1})", self.step, self.max_steps, x, y);
        } else if kind < 90 {
            // Text Input Fuzzing
            if ctx.egui_wants_keyboard_input() {
                let sample_keys = ["a", "b", "c", " ", "photo", "1", "2"];
                let idx = self.rng.gen_usize(0, sample_keys.len());
                raw.events.push(Event::Text(sample_keys[idx].to_string()));
                self.stats_texts += 1;
                log::info!("[MONKEY] Step {}/{}: Typing text '{}'", self.step, self.max_steps, sample_keys[idx]);
            } else {
                let x = self.rng.gen_range(20.0, w - 20.0);
                let y = self.rng.gen_range(20.0, h - 20.0);
                let pos = pos2(x, y);
                raw.events.push(Event::PointerMoved(pos));
                raw.events.push(Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers: Default::default() });
                self.release_needed = Some(pos);
                self.stats_taps += 1;
            }
        } else {
            // Canvas Zoom / Pan gesture
            let factor = if self.rng.next_u32().is_multiple_of(2) { 1.25 } else { 0.8 };
            raw.events.push(Event::Zoom(factor));
            log::info!("[MONKEY] Step {}/{}: Zoom gesture factor {:.2}", self.step, self.max_steps, factor);
        }

        if self.step == self.max_steps {
            self.print_summary();
        }
    }

    pub fn audit_state(&mut self, app: &LightcraftApp, _ctx: &Context) {
        if !self.active {
            return;
        }

        let curr_dialog = app.ui.dialog.as_ref().map(|d| format!("{d:?}"));
        if curr_dialog != self.last_dialog {
            self.stats_dialog_changes += 1;
            log::info!("[MONKEY] Dialog transition: {:?} -> {:?}", self.last_dialog, curr_dialog);
            self.last_dialog = curr_dialog;
        }

        // Layout bounds invariant check
        if self.screen.width() > 0.0 && self.screen.height() > 0.0 {
            if app.ui.left_width < 100.0 || app.ui.left_width > self.screen.width() {
                self.anomalies.push(format!("left_width anomaly: {:.1}", app.ui.left_width));
            }
            if app.ui.right_width < 100.0 || app.ui.right_width > self.screen.width() {
                self.anomalies.push(format!("right_width anomaly: {:.1}", app.ui.right_width));
            }
        }
    }

    fn print_summary(&self) {
        log::info!("==================================================");
        log::info!("       LIGHTCRAFT IPAD MONKEY TEST SUMMARY        ");
        log::info!("==================================================");
        log::info!("Target Device Screen: {:.0} x {:.0} pt", self.screen.width(), self.screen.height());
        log::info!("Total Steps Completed: {}", self.step);
        log::info!("Total Taps Dispatched: {}", self.stats_taps);
        log::info!("Total Drags/Swipes Dispatched: {}", self.stats_drags);
        log::info!("Total Text Keystrokes: {}", self.stats_texts);
        log::info!("Total Dialog Transitions: {}", self.stats_dialog_changes);
        log::info!("Layout/State Anomalies: {}", self.anomalies.len());
        for a in &self.anomalies {
            log::warn!("[MONKEY ANOMALY] {}", a);
        }
        if self.anomalies.is_empty() {
            log::info!("[MONKEY RESULT] ALL UI INVARIANTS PASSED! No crashes, panics, or bounds errors.");
        }
        log::info!("==================================================");
    }
}
