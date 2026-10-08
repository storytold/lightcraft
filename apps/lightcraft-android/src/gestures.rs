//! Single-touch viewer gestures. Multi-touch and editing tools remain with egui.
#![forbid(unsafe_code)]

#[derive(Default)]
pub struct ViewerGesture {
    start: Option<(f64, [f32; 2])>,
    current: [f32; 2],
}
impl ViewerGesture {
    pub fn start(&mut self, time: f64, pos: [f32; 2], viewer: bool) {
        self.start = viewer.then_some((time, pos));
        self.current = pos;
    }
    pub fn move_to(&mut self, pos: [f32; 2]) {
        self.current = pos;
    }
    pub fn cancel(&mut self) {
        self.start = None;
    }
    pub fn original(&self, time: f64) -> bool {
        self.start.is_some_and(|(t, p)| time - t >= 0.5 && (self.current[0] - p[0]).hypot(self.current[1] - p[1]) < 10.0)
    }
    pub fn end(&mut self, pos: [f32; 2], fit: bool) -> i32 {
        let Some((_, start)) = self.start.take() else { return 0 };
        let dx = pos[0] - start[0];
        let dy = pos[1] - start[1];
        if fit && dx.abs() > 70.0 && dx.abs() > dy.abs() * 2.0 { if dx < 0.0 { 1 } else { -1 } } else { 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn long_press_requires_stationary_single_touch_inside_viewer() {
        let mut g = ViewerGesture::default();
        g.start(0.0, [100.0, 100.0], true);
        assert!(!g.original(0.4));
        assert!(g.original(0.6));
        g.move_to([130.0, 100.0]);
        assert!(!g.original(0.7));
        g.start(1.0, [100.0, 100.0], false);
        assert!(!g.original(2.0));
    }
    #[test]
    fn pinch_or_tool_drag_never_becomes_photo_swipe() {
        let mut g = ViewerGesture::default();
        g.start(0.0, [100.0, 100.0], true);
        g.cancel();
        assert_eq!(g.end([10.0, 100.0], true), 0);
        g.start(0.0, [100.0, 100.0], true);
        assert_eq!(g.end([10.0, 100.0], false), 0);
        g.start(0.0, [100.0, 100.0], true);
        assert_eq!(g.end([10.0, 100.0], true), 1);
        g.start(0.0, [100.0, 100.0], true);
        assert_eq!(g.end([200.0, 100.0], true), -1);
    }
}
