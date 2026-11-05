use super::*;

const SCREEN: [f32; 2] = [2560.0, 1440.0];

fn window(size: [f32; 2]) -> Seen {
    Seen { size: Some(size), screen: Some(SCREEN), ..Default::default() }
}

fn place(size: [f32; 2], zoomed: bool) -> WindowPlace {
    WindowPlace { size, zoomed }
}

/// Given a window that is resized and comes to rest, when it is watched, then its new size
/// is remembered once it has rested for a moment, and none of the sizes it passed through.
#[test]
fn a_window_is_remembered_at_the_size_it_comes_to_rest_at() {
    for live in [true, false] {
        let mut watch = WindowWatch::told_live(live);
        let mut remembered = None;
        for (i, width) in [1000.0, 1100.0, 1200.0, 1300.0].into_iter().enumerate() {
            remembered = watch.note(remembered, &window([width, 800.0]), i as f64 * 0.1);
            assert_eq!(remembered, None, "passing through {width}");
            assert!(watch.waits(i as f64 * 0.1).is_some(), "and the app looks again");
        }
        remembered = watch.note(remembered, &window([1300.0, 800.0]), 0.5);
        assert_eq!(remembered, None, "not yet at rest");
        // (it has been this size since 0.3: the app looks again just after 0.8)
        let wait = watch.waits(0.5).map(|w| w.as_secs_f64()).unwrap();
        assert!((wait - 0.35).abs() < 1e-6, "{wait}");
        remembered = watch.note(remembered, &window([1300.0, 800.0]), 0.9);
        assert_eq!(remembered, Some(place([1300.0, 800.0], false)));
        assert!(watch.waits(0.9).is_none());
        // as it is: nothing to wait for
        assert_eq!(watch.note(remembered, &window([1300.0, 800.0]), 5.0), remembered);
        assert!(watch.waits(5.0).is_none());
    }
}

/// Given a window at rest that is zoomed (the top bar double-clicked), when it is watched
/// where the window system says so, then it is remembered as zoomed at once, with the size
/// it had before, though it grew through other sizes on the way; un-zoomed, it is no longer
/// zoomed at once and keeps that size.
#[test]
fn a_zoomed_window_keeps_the_size_it_goes_back_to() {
    let mut watch = WindowWatch::told_live(true);
    let rest = place([1300.0, 800.0], false);
    let mut remembered = Some(rest);
    for (i, width) in [1600.0, 2000.0, 2400.0].into_iter().enumerate() {
        remembered = watch.note(remembered, &window([width, 1300.0]), 10.0 + i as f64 * 0.05);
        assert_eq!(remembered, Some(rest));
    }
    let zoomed = Seen { zoomed: true, ..window([2560.0, 1412.0]) };
    remembered = watch.note(remembered, &zoomed, 10.2);
    assert_eq!(remembered, Some(place([1300.0, 800.0], true)));
    assert_eq!(watch.note(remembered, &zoomed, 30.0), Some(place([1300.0, 800.0], true)), "however long it stays zoomed");
    assert!(watch.waits(30.0).is_none());
    remembered = watch.note(remembered, &window([2000.0, 1100.0]), 31.0);
    assert_eq!(remembered, Some(rest), "no longer zoomed, and the size it had");
    assert_eq!(watch.note(remembered, &window([1300.0, 800.0]), 31.2), Some(rest));
    // zoomed before anything was remembered: it opens zoomed, and goes back to the usual size
    assert_eq!(WindowWatch::told_live(true).note(None, &zoomed, 0.0), Some(place(DEFAULT_SIZE, true)));
    // the system's word is the answer there: a window as large as its screen is a window
    assert_eq!(WindowWatch::told_live(true).note(Some(rest), &window([2560.0, 1335.0]), 0.0).map(|p| p.zoomed), Some(false));
}

/// Given macOS, where the window system says whether the window is zoomed only as it opens
/// and when the app zooms it itself: when a window that opened zoomed is resized by its edge,
/// then it is a window again, remembered at the size it was given, though the system's word
/// still says zoomed; zoomed once more, it is zoomed.
#[test]
fn a_zoomed_window_resized_by_its_edge_is_a_window_again_where_the_system_does_not_say() {
    let mut watch = WindowWatch::told_live(false);
    let rest = place([1300.0, 800.0], true);
    let zoomed = Seen { zoomed: true, ..window([2560.0, 1335.0]) };
    let mut remembered = watch.note(Some(rest), &zoomed, 0.0);
    assert_eq!(remembered, Some(rest));
    assert!(watch.waits(0.0).is_some(), "the size it is zoomed at is still to be seen at rest");
    remembered = watch.note(remembered, &zoomed, 0.6);
    assert_eq!(remembered, Some(rest));
    assert!(watch.waits(0.6).is_none());
    assert_eq!(watch.note(remembered, &zoomed, 60.0), Some(rest), "however long it stays zoomed");
    // (a size goes through the screen's pixels and back: a point off is the same size)
    assert_eq!(watch.note(remembered, &Seen { zoomed: true, ..window([2559.0, 1336.0]) }, 60.5), Some(rest));
    // its right edge dragged in a little, far less than a Dock is wide; the word is unchanged
    let dragged = Seen { zoomed: true, ..window([2500.0, 1335.0]) };
    remembered = watch.note(remembered, &dragged, 61.0);
    assert_eq!(remembered, Some(place([1300.0, 800.0], false)), "not zoomed any more");
    remembered = watch.note(remembered, &dragged, 61.6);
    assert_eq!(remembered, Some(place([2500.0, 1335.0], false)), "and at the size it was given");
    assert!(watch.waits(61.6).is_none());
    // zoomed again from the Window menu: back at the size it is zoomed at
    assert_eq!(watch.note(remembered, &zoomed, 70.0), Some(place([2500.0, 1335.0], true)));
}

/// Given macOS, when the app zooms the window itself (the top bar double-clicked), then the
/// system says so at once and the window is zoomed through all the sizes it grows through,
/// with the size it had, wherever the Dock is; un-zoomed the same way, it is a window again
/// at that size.
#[test]
fn a_window_the_app_zooms_is_zoomed_at_once_where_the_system_does_not_say() {
    let mut watch = WindowWatch::told_live(false);
    let rest = place([1300.0, 800.0], false);
    let mut remembered = watch.note(Some(rest), &window([1300.0, 800.0]), 0.0);
    // (a Dock at the left: zoomed, the window is narrower than its screen)
    for (i, width) in [1300.0, 1700.0, 2100.0, 2410.0].into_iter().enumerate() {
        remembered = watch.note(remembered, &Seen { zoomed: true, ..window([width, 1415.0]) }, 1.0 + i as f64 * 0.05);
        assert_eq!(remembered, Some(place([1300.0, 800.0], true)), "growing through {width}");
    }
    remembered = watch.note(remembered, &Seen { zoomed: true, ..window([2410.0, 1415.0]) }, 2.0);
    assert_eq!(remembered, Some(place([1300.0, 800.0], true)));
    assert!(watch.waits(2.0).is_none());
    for (i, width) in [2100.0, 1700.0, 1300.0].into_iter().enumerate() {
        remembered = watch.note(remembered, &window([width, 800.0]), 3.0 + i as f64 * 0.05);
        assert_eq!(remembered, Some(rest), "shrinking through {width}");
    }
    assert!(watch.waits(3.1).is_none(), "back at the size it had: nothing to wait for");
}

/// Given macOS and a window zoomed from the Window menu (the system's word stays "not
/// zoomed"), when it is seen to come to fill its screen from side to side, then it is zoomed,
/// with the size it had, and zoomed back it is a window at that size again. A window that
/// only grows large is not zoomed, on any screen.
#[test]
fn a_window_that_comes_to_fill_its_screen_is_zoomed_where_the_system_does_not_say() {
    let rest = place([1300.0, 800.0], false);
    // a window of 1300 × 800 on `screen` that is next seen at `size`
    let zoomed_on = |screen: [f32; 2], size: [f32; 2]| {
        let mut watch = WindowWatch::told_live(false);
        let seen = |size: [f32; 2]| Seen { size: Some(size), screen: Some(screen), ..Default::default() };
        watch.note(Some(rest), &seen([1300.0, 800.0]), 0.0);
        watch.note(Some(rest), &seen(size), 0.2).map(|p| p.zoomed)
    };
    // under the menu bar, with the Dock hidden, small, or as large as it gets
    for (screen, size) in [
        ([2560.0, 1440.0], [2560.0, 1415.0]),
        ([2560.0, 1440.0], [2560.0, 1335.0]),
        ([2560.0, 1440.0], [2559.0, 1335.0]),
        ([1728.0, 1117.0], [1728.0, 1084.0]),
        ([1512.0, 982.0], [1512.0, 794.0]),
    ] {
        assert_eq!(zoomed_on(screen, size), Some(true), "{size:?} on {screen:?}");
    }
    // the usual size on a laptop's screen, windows sized by hand to nearly the screen, one
    // that is a little narrower than it, and one as wide as it and far from as tall
    for (screen, size) in [
        ([1728.0, 1117.0], DEFAULT_SIZE),
        ([1710.0, 1107.0], DEFAULT_SIZE),
        ([1512.0, 982.0], [1400.0, 820.0]),
        ([1470.0, 956.0], [1340.0, 780.0]),
        ([2560.0, 1440.0], [2430.0, 1270.0]),
        ([2560.0, 1440.0], [2300.0, 1200.0]),
        ([2560.0, 1440.0], [2550.0, 1415.0]),
        ([2560.0, 1440.0], [2560.0, 1190.0]),
    ] {
        assert_eq!(zoomed_on(screen, size), Some(false), "{size:?} on {screen:?}");
    }
    let mut watch = WindowWatch::told_live(false);
    let filling = window([2560.0, 1335.0]);
    let mut remembered = watch.note(Some(rest), &window([1300.0, 800.0]), 0.0);
    remembered = watch.note(remembered, &window([2000.0, 1100.0]), 0.9);
    remembered = watch.note(remembered, &filling, 1.0);
    assert_eq!(remembered, Some(place([1300.0, 800.0], true)));
    assert_eq!(watch.note(remembered, &filling, 9.0), remembered, "and its own size is not written over");
    remembered = watch.note(remembered, &window([1900.0, 1000.0]), 10.0);
    assert_eq!(remembered, Some(rest), "zoomed back: a window, at the size it had");
    assert_eq!(watch.note(remembered, &window([1300.0, 800.0]), 10.2), Some(rest));
    assert!(watch.waits(10.2).is_none());
    // not told apart: zoomed from the Window menu beside a Dock at the side, it is remembered
    // as a window of that size
    let mut watch = WindowWatch::told_live(false);
    let beside_a_dock = window([2410.0, 1415.0]);
    watch.note(Some(rest), &window([1300.0, 800.0]), 0.0);
    watch.note(Some(rest), &beside_a_dock, 0.3);
    assert_eq!(watch.note(Some(rest), &beside_a_dock, 0.9), Some(place([2410.0, 1415.0], false)));
    // without a screen's size to go by, the system's word is all there is
    let no_screen = Seen { size: Some([2560.0, 1335.0]), screen: None, zoomed: true, ..Default::default() };
    assert_eq!(WindowWatch::told_live(false).note(Some(rest), &no_screen, 0.0), Some(place([1300.0, 800.0], true)));
}

/// Given macOS and a window that was left as wide as its screen and a little shorter (its
/// lower edge dragged up after a zoom), when the app opens it at that size, then it is a
/// window of that size, launch after launch: filling the screen is no sign of a zoom unless
/// the window was seen to come to it. Dragged shorter still, it is remembered at that.
#[test]
fn a_window_that_opens_as_wide_as_its_screen_is_a_window_where_the_system_does_not_say() {
    for (screen, size) in [([1728.0, 1117.0], [1728.0, 1054.0]), ([1512.0, 982.0], [1512.0, 760.0]), ([3440.0, 1440.0], [3440.0, 1200.0])] {
        let left = place(size, false);
        let seen = |size: [f32; 2]| Seen { size: Some(size), screen: Some(screen), ..Default::default() };
        let mut watch = WindowWatch::told_live(false);
        for frame in 0..5 {
            assert_eq!(watch.note(Some(left), &seen(size), f64::from(frame) * 0.3), Some(left), "{size:?} on {screen:?}");
        }
        let shorter = [size[0], size[1] - 20.0];
        watch.note(Some(left), &seen(shorter), 2.0);
        assert_eq!(watch.note(Some(left), &seen(shorter), 2.6), Some(place(shorter, false)));
    }
    // the same after a double-click on the top bar that changed nothing (the system's word was
    // stale, and is now "not zoomed"): still the window it is
    let mut watch = WindowWatch::told_live(false);
    let screen = [1728.0, 1117.0];
    let left = place([1728.0, 1054.0], false);
    watch.note(Some(left), &Seen { size: Some(left.size), screen: Some(screen), zoomed: true, ..Default::default() }, 0.0);
    watch.note(Some(left), &Seen { size: Some(left.size), screen: Some(screen), zoomed: true, ..Default::default() }, 1.0);
    // (taken for zoomed on the system's word, until that word is taken back)
    let unsaid = Seen { size: Some(left.size), screen: Some(screen), zoomed: false, ..Default::default() };
    assert_eq!(watch.note(Some(left), &unsaid, 2.0), Some(left));
    assert_eq!(watch.note(Some(left), &unsaid, 9.0), Some(left));
}

/// Given macOS and a zoomed window, when it is zoomed back from the Window menu (or resized by
/// its edge) before it was ever seen at rest, then it is a window at once, and the size it
/// comes to rest at is the window's, not taken for the size it is zoomed at.
#[test]
fn a_window_zoomed_back_at_once_is_a_window_where_the_system_does_not_say() {
    let mut watch = WindowWatch::told_live(false);
    let rest = place([1240.0, 820.0], true);
    let mut remembered = watch.note(Some(rest), &Seen { zoomed: true, ..window([2560.0, 1415.0]) }, 0.0);
    assert_eq!(remembered, Some(rest));
    for (i, width) in [2200.0, 1700.0, 1240.0, 1240.0, 1240.0].into_iter().enumerate() {
        remembered = watch.note(remembered, &Seen { zoomed: true, ..window([width, 820.0]) }, 0.3 + i as f64 * 0.05);
        assert_eq!(remembered, Some(place([1240.0, 820.0], false)), "{width}");
    }
    assert_eq!(watch.note(remembered, &Seen { zoomed: true, ..window([1240.0, 820.0]) }, 60.0), Some(place([1240.0, 820.0], false)));
    assert!(watch.waits(60.0).is_none());
}

/// Given macOS and a zoomed window, when its screen changes (the display unplugged, another
/// resolution) and it is fitted to the new one, then it is still zoomed and keeps the size
/// it goes back to. Moved to a larger screen that it does not fill, it is a window.
#[test]
fn a_zoomed_window_is_still_zoomed_on_another_screen_where_the_system_does_not_say() {
    for said in [true, false] {
        let rest = place([1240.0, 820.0], false);
        let on = |screen: [f32; 2], size: [f32; 2]| Seen { size: Some(size), screen: Some(screen), zoomed: said, ..Default::default() };
        let mut watch = WindowWatch::told_live(false);
        let mut remembered = watch.note(Some(rest), &on([2560.0, 1440.0], [1240.0, 820.0]), 0.0);
        if said {
            // (the system's word as the window opened: a window; the app zooms it later)
            remembered = watch.note(remembered, &Seen { zoomed: false, ..on([2560.0, 1440.0], [1240.0, 820.0]) }, 0.0);
        }
        for now in [1.0, 2.0] {
            remembered = watch.note(remembered, &on([2560.0, 1440.0], [2560.0, 1415.0]), now);
        }
        assert_eq!(remembered, Some(place([1240.0, 820.0], true)), "said: {said}");
        for now in [10.0, 10.6, 60.0] {
            remembered = watch.note(remembered, &on([1728.0, 1117.0], [1728.0, 1084.0]), now);
            assert_eq!(remembered, Some(place([1240.0, 820.0], true)), "on the smaller screen at {now}, said: {said}");
        }
        assert!(watch.waits(60.0).is_none());
        // dragged over to a screen it does not fill
        remembered = watch.note(remembered, &on([3440.0, 1440.0], [1728.0, 1084.0]), 70.0);
        assert_eq!(remembered.map(|p| p.zoomed), Some(false), "said: {said}");
    }
    // seen zoomed in one frame only, and fitted to another screen long after: it had rested
    let mut watch = WindowWatch::told_live(false);
    let rest = place([1240.0, 820.0], true);
    watch.note(Some(rest), &Seen { zoomed: true, ..window([2560.0, 1415.0]) }, 0.0);
    let fitted = Seen { size: Some([1728.0, 1084.0]), screen: Some([1728.0, 1117.0]), zoomed: true, ..Default::default() };
    assert_eq!(watch.note(Some(rest), &fitted, 5.0), Some(rest));
}

/// Given a size the window has not rested at yet, when the window goes full screen or is
/// minimized, or reports no size, then nothing waits (the app is not woken again and again
/// for as long as that lasts), and back as a window its size is seen afresh.
#[test]
fn nothing_waits_while_the_window_is_full_screen_or_minimized() {
    let rest = place([1300.0, 800.0], false);
    for live in [true, false] {
        for away in
            [Seen { full_screen: true, ..window(SCREEN) }, Seen { minimized: true, ..window([160.0, 28.0]) }, Seen { size: None, ..window(SCREEN) }]
        {
            let mut watch = WindowWatch::told_live(live);
            watch.note(Some(rest), &window([1500.0, 900.0]), 1.0);
            assert!(watch.waits(1.0).is_some());
            assert_eq!(watch.note(Some(rest), &away, 1.1), Some(rest));
            assert!(watch.waits(1.1).is_none(), "{away:?}");
            assert_eq!(watch.note(Some(rest), &away, 900.0), Some(rest));
            assert!(watch.waits(900.0).is_none(), "{away:?}");
            assert_eq!(watch.note(Some(rest), &window([1500.0, 900.0]), 901.0), Some(rest));
            let mut remembered = Some(rest);
            for now in [902.2, 902.8] {
                remembered = watch.note(remembered, &window([1500.0, 900.0]), now);
            }
            assert_eq!(remembered, Some(place([1500.0, 900.0], false)), "{away:?}");
            assert!(watch.waits(902.8).is_none());
        }
    }
    // (macOS: the same of a zoomed window's size that was still to be seen at rest)
    for away in [Seen { full_screen: true, zoomed: true, ..window(SCREEN) }, Seen { size: None, zoomed: true, ..window(SCREEN) }] {
        let mut watch = WindowWatch::told_live(false);
        watch.note(Some(rest), &Seen { zoomed: true, ..window([2560.0, 1335.0]) }, 0.0);
        assert!(watch.waits(0.0).is_some());
        watch.note(Some(rest), &away, 0.1);
        assert!(watch.waits(0.1).is_none(), "{away:?}");
    }
}

/// Given a zoomed window that was full screen, when it leaves full screen, then for a moment
/// it is not looked at (the system animates the way back, at sizes that are neither the
/// window's nor the zoomed one), the app looks again once that is over, and the window is
/// zoomed with the size it goes back to, as before.
#[test]
fn a_window_that_has_just_left_full_screen_is_not_looked_at() {
    for live in [true, false] {
        let mut watch = WindowWatch::told_live(live);
        let rest = place([1240.0, 820.0], true);
        let zoomed = Seen { zoomed: true, ..window([2560.0, 1415.0]) };
        for now in [0.0, 1.0] {
            assert_eq!(watch.note(Some(rest), &zoomed, now), Some(rest));
        }
        assert_eq!(watch.note(Some(rest), &Seen { full_screen: true, zoomed: true, ..window(SCREEN) }, 5.0), Some(rest));
        // on the way back the system no longer says "full screen", nor "zoomed" where its word is live
        for now in [9.0, 9.3, 9.6, 9.9] {
            assert_eq!(watch.note(Some(rest), &Seen { zoomed: !live, ..window(SCREEN) }, now), Some(rest), "at {now}");
            let wait = watch.waits(now).map(|w| w.as_secs_f64()).unwrap();
            assert!((wait - (10.05 - now)).abs() < 1e-6, "at {now}: looks again after the moment, {wait}");
        }
        assert_eq!(watch.note(Some(rest), &zoomed, 10.1), Some(rest));
        assert_eq!(watch.note(Some(rest), &zoomed, 11.0), Some(rest));
        assert!(watch.waits(11.0).is_none());
    }
}

/// Given macOS and a window dragged by its corner, when it reaches the screen's width and is
/// then dragged on, taller and little by little narrower again, and left narrower than the
/// screen, then it is a window at the size it was left at: a zoom only grows on its way, so
/// what gets narrower again is somebody's hand.
#[test]
fn a_window_dragged_wide_and_narrower_again_is_a_window_where_the_system_does_not_say() {
    for per_frame in [1.0, 1.4] {
        let screen = [1728.0, 1117.0];
        let seen = |size: [f32; 2]| Seen { size: Some(size), screen: Some(screen), ..Default::default() };
        let rest = place([1600.0, 950.0], false);
        let mut watch = WindowWatch::told_live(false);
        let mut remembered = watch.note(Some(rest), &seen(rest.size), 0.0);
        let mut size = [1728.0, 950.0];
        let mut now = 1.0;
        for _ in 0..40 {
            remembered = watch.note(remembered, &seen(size), now);
            size = [size[0] - per_frame, size[1] + 3.0];
            now += 1.0 / 60.0;
        }
        for _ in 0..3 {
            remembered = watch.note(remembered, &seen(size), now);
            now += 0.3;
        }
        assert_eq!(remembered, Some(place(size, false)), "{per_frame} pt narrower a frame");
    }
    // a zoomed window whose edge is dragged in slowly: a window once it is no longer the size it was zoomed at
    let mut watch = WindowWatch::told_live(false);
    let rest = place([1300.0, 800.0], true);
    for now in [0.0, 1.0] {
        watch.note(Some(rest), &Seen { zoomed: true, ..window([2560.0, 1335.0]) }, now);
    }
    let mut remembered = Some(rest);
    for frame in 1..=10 {
        remembered = watch.note(remembered, &Seen { zoomed: true, ..window([2560.0 - frame as f32, 1335.0]) }, 2.0 + f64::from(frame) / 60.0);
    }
    assert_eq!(remembered.map(|p| p.zoomed), Some(false), "ten points narrower, a point at a time");
    // and one whose lower edge is dragged up a little: it still fills its screen, and is a window
    let mut watch = WindowWatch::told_live(false);
    for now in [0.0, 1.0] {
        watch.note(Some(rest), &Seen { zoomed: true, ..window([2560.0, 1415.0]) }, now);
    }
    let shorter = Seen { zoomed: true, ..window([2560.0, 1385.0]) };
    watch.note(Some(rest), &shorter, 2.0);
    assert_eq!(watch.note(Some(rest), &shorter, 2.6), Some(place([2560.0, 1385.0], false)));
}

/// Given macOS and a window that opened zoomed, when its edge is dragged in before it was
/// seen at rest (one side only gets smaller), or it is carried to a larger screen in that
/// moment, then it is a window; and a frame without the screen's size costs a zoomed window
/// nothing.
#[test]
fn a_zoomed_window_changed_before_it_rested_is_a_window_where_the_system_does_not_say() {
    let rest = place([1240.0, 820.0], true);
    let zoomed = Seen { zoomed: true, ..window([2560.0, 1415.0]) };
    for at in [0.1, 0.3] {
        let mut watch = WindowWatch::told_live(false);
        watch.note(Some(rest), &zoomed, 0.0);
        let dragged = Seen { zoomed: true, ..window([2000.0, 1415.0]) };
        assert_eq!(watch.note(Some(rest), &dragged, at), Some(place([1240.0, 820.0], false)), "dragged at {at}");
        assert_eq!(watch.note(Some(rest), &dragged, at + 0.6), Some(place([2000.0, 1415.0], false)));
    }
    // carried to a larger screen, as large as it is, a moment after it opened: no frame came in between
    for at in [0.2, 0.52, 0.9] {
        let mut watch = WindowWatch::told_live(false);
        watch.note(Some(rest), &zoomed, 0.0);
        let carried = Seen { size: Some([2560.0, 1415.0]), screen: Some([5120.0, 2880.0]), zoomed: true, ..Default::default() };
        assert_eq!(watch.note(Some(rest), &carried, at).map(|p| p.zoomed), Some(false), "carried at {at}");
    }
    // the screen's size missing for a frame
    let mut watch = WindowWatch::told_live(false);
    for now in [0.0, 1.0] {
        watch.note(Some(rest), &zoomed, now);
    }
    assert_eq!(watch.note(Some(rest), &Seen { screen: None, ..zoomed }, 2.0), Some(rest));
    assert_eq!(watch.note(Some(rest), &zoomed, 2.1), Some(rest));
}

/// Given macOS and a window that opened zoomed and was then resized by hand (the system's
/// word still says zoomed), when it goes full screen and back, or reports no size for a
/// frame, then it is still the window it was: what was made out of it is not let go of for
/// the system's old word.
#[test]
fn a_window_made_out_is_not_taken_for_zoomed_again_on_the_systems_old_word() {
    for away in [Seen { full_screen: true, zoomed: true, ..window(SCREEN) }, Seen { size: None, zoomed: true, ..window(SCREEN) }] {
        let mut watch = WindowWatch::told_live(false);
        let rest = place([1240.0, 820.0], true);
        for now in [0.0, 1.0] {
            watch.note(Some(rest), &Seen { zoomed: true, ..window([2560.0, 1415.0]) }, now);
        }
        let dragged = Seen { zoomed: true, ..window([1500.0, 900.0]) };
        let mut remembered = watch.note(Some(rest), &dragged, 2.0);
        remembered = watch.note(remembered, &dragged, 2.6);
        assert_eq!(remembered, Some(place([1500.0, 900.0], false)));
        remembered = watch.note(remembered, &away, 5.0);
        for now in [8.0, 8.6, 9.2, 30.0] {
            remembered = watch.note(remembered, &dragged, now);
            assert_eq!(remembered, Some(place([1500.0, 900.0], false)), "{away:?} at {now}");
        }
    }
}

/// Given a platform where the system says whether the window is maximized, when a window
/// grows until it fills its screen, then it is a window all the same: only the system's
/// word counts there. And the watch the app makes goes by that word everywhere but on macOS.
#[test]
fn where_the_system_says_only_its_word_counts() {
    let rest = place([1300.0, 800.0], false);
    let mut watch = WindowWatch::told_live(true);
    let mut remembered = Some(rest);
    for (i, size) in [[1300.0, 800.0], [2000.0, 1100.0], [2560.0, 1335.0], [2560.0, 1335.0], [2560.0, 1335.0]].into_iter().enumerate() {
        remembered = watch.note(remembered, &window(size), i as f64 * 0.4);
        assert_eq!(remembered.map(|p| p.zoomed), Some(false));
    }
    assert_eq!(remembered, Some(place([2560.0, 1335.0], false)));
    // the app's own watch: opened zoomed and then seen at a hand size, the system's word unchanged
    let mut watch = WindowWatch::default();
    for now in [0.0, 1.0] {
        watch.note(Some(rest), &Seen { zoomed: true, ..window([2560.0, 1335.0]) }, now);
    }
    let zoomed = watch.note(Some(rest), &Seen { zoomed: true, ..window([1500.0, 900.0]) }, 2.0).map(|p| p.zoomed);
    assert_eq!(zoomed, Some(!cfg!(target_os = "macos")), "macOS goes by the size, the others by the word");
}

/// Given a window that left full screen a moment ago, when it is minimized or goes full
/// screen again, then nothing waits; and a window that covers its screen altogether (full
/// screen, though nothing says so) is not remembered as a size, however long it stays.
#[test]
fn nothing_waits_when_a_window_just_back_from_full_screen_goes_away_again() {
    let rest = place([1300.0, 800.0], false);
    for live in [true, false] {
        for away in [Seen { minimized: true, ..window([160.0, 28.0]) }, Seen { full_screen: true, ..window(SCREEN) }] {
            let mut watch = WindowWatch::told_live(live);
            watch.note(Some(rest), &Seen { full_screen: true, ..window(SCREEN) }, 1.0);
            watch.note(Some(rest), &window([1300.0, 800.0]), 2.0);
            assert!(watch.waits(2.0).is_some(), "the moment after full screen");
            for now in [2.1, 2.7, 60.0] {
                assert_eq!(watch.note(Some(rest), &away, now), Some(rest));
                assert!(watch.waits(now).is_none(), "{away:?} at {now}");
            }
        }
        let mut watch = WindowWatch::told_live(live);
        watch.note(Some(rest), &window([1300.0, 800.0]), 0.0);
        for now in [1.0, 1.6, 2.2, 60.0] {
            // (where the system does not say, a window that comes to cover its screen is zoomed)
            assert_eq!(watch.note(Some(rest), &window(SCREEN), now).map(|p| p.size), Some(rest.size), "covering its screen at {now}");
        }
        assert!(watch.waits(60.0).is_none());
    }
}

/// Given a window smaller than the app's smallest (a window manager that does not keep to
/// it, a script), when it rests, then the smallest size is remembered, once: what is
/// remembered and what is seen agree from then on, and nothing waits.
#[test]
fn a_window_smaller_than_the_smallest_is_remembered_once() {
    for live in [true, false] {
        let mut watch = WindowWatch::told_live(live);
        let small = window([640.0, 700.0]);
        let mut remembered = watch.note(Some(place([1300.0, 800.0], false)), &small, 0.0);
        remembered = watch.note(remembered.and_then(WindowPlace::checked), &small, 0.6);
        assert_eq!(remembered, Some(place([MIN_SIZE[0], 700.0], false)));
        for frame in 0..20 {
            let now = 1.0 + f64::from(frame) * 0.1;
            assert_eq!(watch.note(remembered.and_then(WindowPlace::checked), &small, now), remembered);
            assert!(watch.waits(now).is_none(), "frame {frame}");
        }
    }
}

/// Full screen and a minimized window are not sizes to open at: nothing is remembered of
/// them. Nor of a window that reports no size.
#[test]
fn full_screen_and_minimized_windows_are_not_remembered() {
    let rest = place([1300.0, 800.0], false);
    for seen in [
        Seen { full_screen: true, ..window(SCREEN) },
        Seen { full_screen: true, zoomed: true, ..window(SCREEN) },
        Seen { minimized: true, ..window([160.0, 28.0]) },
        Seen { size: None, ..window([0.0, 0.0]) },
    ] {
        for live in [true, false] {
            let mut watch = WindowWatch::told_live(live);
            assert_eq!(watch.note(Some(rest), &seen, 0.0), Some(rest), "{seen:?}");
            assert_eq!(watch.note(Some(rest), &seen, 5.0), Some(rest), "{seen:?}");
            assert_eq!(watch.at_exit(Some(rest), 6.0), Some(rest));
        }
    }
}

/// Given a window that was resized, when the app quits before the size has rested its
/// moment, then the size is remembered if the window had been let go of, and not if it was
/// still changing with every frame (a zoom the system was animating).
#[test]
fn a_size_the_window_rested_at_is_remembered_when_the_app_quits() {
    let rest = place([1300.0, 800.0], false);
    let mut watch = WindowWatch::told_live(true);
    watch.note(Some(rest), &window([1500.0, 900.0]), 1.0);
    assert_eq!(watch.at_exit(Some(rest), 1.3), Some(place([1500.0, 900.0], false)));
    assert_eq!(watch.at_exit(Some(rest), 9.0), Some(rest), "said once");
    let mut watch = WindowWatch::told_live(true);
    watch.note(Some(rest), &window([1500.0, 900.0]), 1.00);
    watch.note(Some(rest), &window([1700.0, 1000.0]), 1.02);
    assert_eq!(watch.at_exit(Some(rest), 1.04), Some(rest), "a size it was passing through");
    assert_eq!(WindowWatch::told_live(true).at_exit(None, 3.0), None);
}

/// What egui holds of the window is read in the system's points: a size every platform
/// reports (the rectangle the app draws in), brought back from the interface's own scale.
#[test]
fn the_window_is_seen_in_the_systems_points() {
    use egui::{Rect, ViewportInfo, pos2, vec2};
    let info = ViewportInfo { monitor_size: Some(vec2(1280.0, 720.0)), maximized: Some(true), fullscreen: Some(false), ..Default::default() };
    let content = Rect::from_min_size(pos2(0.0, 0.0), vec2(650.0, 400.0));
    // (the interface at twice its size: egui's points are half the system's)
    let seen = Seen::of(&info, content, 2.0);
    assert_eq!(seen, Seen { size: Some([1300.0, 800.0]), screen: Some([2560.0, 1440.0]), zoomed: true, full_screen: false, minimized: false });
    let away = ViewportInfo { maximized: Some(false), fullscreen: Some(true), minimized: Some(true), ..Default::default() };
    let seen = Seen::of(&away, content, 1.0);
    assert!(!seen.zoomed && seen.full_screen && seen.minimized);
    // nothing reported but the content (Wayland): the size is there all the same
    assert_eq!(Seen::of(&ViewportInfo::default(), content, 1.0).size, Some([650.0, 400.0]));
    // a scale or a size that is none
    assert_eq!(Seen::of(&info, content, f32::NAN).size, Some([650.0, 400.0]));
    assert_eq!(Seen::of(&info, Rect::NOTHING, 1.0).size, None);
    assert_eq!(Seen::of(&info, Rect::from_min_size(pos2(0.0, 0.0), vec2(0.0, 0.0)), 1.0).size, None);
}

/// What the settings hold is not trusted as it is: a size is brought within what a window can
/// be, and anything that is no window reads as nothing remembered, without an error (which
/// would have the whole settings file taken for damaged).
#[test]
fn what_was_remembered_is_checked_and_never_costs_the_other_settings() {
    #[derive(Deserialize, Debug, PartialEq)]
    struct Settings {
        #[serde(default, deserialize_with = "WindowPlace::lenient")]
        window: Option<WindowPlace>,
        other: u8,
    }
    let read = |window: &str| serde_json::from_str::<Settings>(&format!(r#"{{"window":{window},"other":7}}"#)).map(|s| s.window);
    assert_eq!(read(r#"{"size":[1300.0,800.0],"zoomed":true}"#).unwrap(), Some(place([1300.0, 800.0], true)));
    assert_eq!(read(r#"{"size":[1300,800]}"#).unwrap(), Some(place([1300.0, 800.0], false)));
    // (a place on the desktop, as a version that remembered one would write it, is passed over)
    assert_eq!(read(r#"{"size":[1300,800],"position":[40,60],"zoomed":false}"#).unwrap(), Some(place([1300.0, 800.0], false)));
    assert_eq!(read(r#"{"size":[200,100]}"#).unwrap().map(|p| p.size), Some(MIN_SIZE), "too small to be the app's window");
    for odd in [
        "null",
        "{}",
        r#""x""#,
        "5",
        "[]",
        "true",
        r#"{"size":[1,2,3]}"#,
        r#"{"size":[1]}"#,
        r#"{"size":"big"}"#,
        r#"{"size":[null,800]}"#,
        r#"{"size":[1300,800],"zoomed":1}"#,
        r#"{"size":[0,0]}"#,
        r#"{"size":[-5,800]}"#,
        r#"{"size":[1e39,800]}"#,
        r#"{"size":[99999,800]}"#,
    ] {
        assert_eq!(read(odd).unwrap(), None, "{odd}");
    }
    assert_eq!(serde_json::from_str::<Settings>(r#"{"other":7}"#).unwrap(), Settings { window: None, other: 7 }, "settings from before");
    // and what is written is read back
    let written = serde_json::to_string(&place([1300.5, 800.25], true)).unwrap();
    assert_eq!(read(&written).unwrap(), Some(place([1300.5, 800.25], true)));
    for size in [[f32::NAN, 800.0], [f32::INFINITY, 800.0], [0.5, 800.0]] {
        assert_eq!(place(size, false).checked(), None, "{size:?}");
    }
}
