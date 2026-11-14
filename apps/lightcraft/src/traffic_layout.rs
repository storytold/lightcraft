//! The arithmetic behind `crate::traffic_lights`: where the title-bar container and the window's
//! close, minimize and zoom buttons go so the buttons sit in the middle of the top bar. It is plain
//! numbers, so it is built and tested on every platform; only the AppKit calls are macOS-only.

/// Frames, in points, that put the buttons' centre at the middle of a `bar_h` tall bar.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    /// The title-bar container's height.
    pub container_h: f32,
    /// The container's bottom edge above the window's bottom edge (AppKit's origin is bottom-left).
    pub container_y: f32,
    /// The buttons' bottom edge above the container's bottom edge.
    pub button_y: f32,
}

/// Where the container and the buttons go for a window `window_h` tall, a bar `bar_h` tall and
/// buttons `button_h` tall. A bar shorter than the buttons is as tall as they are.
///
/// Whole device pixels win over exact centring: on a 1x screen the buttons' centre can sit half a
/// point from the middle of the bar (20 pt for a 41 pt bar whose own controls are centred at
/// 20.5 pt), where a button half a pixel off would draw blurred. On a 2x screen the pixels are
/// half as large, so the centre is closer to the middle.
pub fn layout(window_h: f32, bar_h: f32, button_h: f32, scale: f32) -> Layout {
    let container_h = (bar_h.max(button_h) * scale).round() / scale;
    let button_y = ((container_h - button_h) / 2.0 * scale).round() / scale;
    Layout { container_h, container_y: window_h - container_h, button_y }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How far below the window's top edge the buttons' centre is.
    fn centre_from_top(window_h: f32, button_h: f32, l: Layout) -> f32 {
        window_h - (l.container_y + l.button_y + button_h / 2.0)
    }

    // Scenario: the traffic lights are centred on the top bar, whatever the window's height, and
    // on whole device pixels (a button half a pixel off draws blurred on a 1x screen)
    #[test]
    fn the_buttons_are_centred_on_the_bar_on_whole_pixels() {
        for scale in [1.0, 2.0] {
            for window_h in [400.0, 1000.0, 2160.0] {
                for bar_h in [42.0, 41.0, 60.0, 61.5, 51.25] {
                    let l = layout(window_h, bar_h, 16.0, scale);
                    let off = (centre_from_top(window_h, 16.0, l) - bar_h / 2.0).abs();
                    assert!(off <= 0.75 / scale, "scale {scale}, window {window_h}, bar {bar_h}: {off} off");
                    let whole = |points: f32| points * scale == (points * scale).round();
                    assert!(whole(l.button_y) && whole(l.container_h), "scale {scale}, bar {bar_h}: {l:?}");
                    assert!((l.container_h - bar_h).abs() <= 0.5 / scale, "the container covers the bar, so a click on a button lands on it");
                    assert_eq!(l.container_y + l.container_h, window_h, "and starts at the window's top edge");
                }
            }
        }
    }

    // Scenario: a bar shorter than the buttons still holds them
    #[test]
    fn a_bar_shorter_than_the_buttons_is_as_tall_as_they_are() {
        let l = layout(1000.0, 10.0, 16.0, 2.0);
        assert_eq!(l.container_h, 16.0);
        assert_eq!(l.button_y, 0.0);
    }
}
