use super::*;

fn ids(n: u64) -> Vec<PhotoId> {
    (1..=n).map(PhotoId).collect()
}

fn settings(slide: f64, fade: f64) -> PlaybackSettings {
    PlaybackSettings { slide_duration: slide, fade_duration: fade, ..Default::default() }
}

fn ph(n: u64) -> Item {
    Item::Photo(PhotoId(n))
}

fn all_ready(_: Item) -> bool {
    true
}

#[test]
fn timeline_durations_and_fade_progress() {
    let mut p = Playback::new(ids(3), Some(PhotoId(2)), settings(4.0, 1.0), 1, 10.0).unwrap();
    assert_eq!(p.current(), ph(2), "starts at the selected photo");
    assert_eq!(p.position(), (1, 3));
    assert_eq!(p.tick(13.9, all_ready), Tick::Idle);
    assert_eq!(p.frame(13.9).from, None);
    assert_eq!(p.tick(14.0, all_ready), Tick::Advanced(ph(3)));
    let f = p.frame(14.25);
    assert_eq!((f.current, f.from), (ph(3), Some(ph(2))));
    assert!((f.progress - 0.25).abs() < 1e-6, "{f:?}");
    assert!((p.frame(14.5).progress - 0.5).abs() < 1e-6);
    assert_eq!(p.tick(15.0, all_ready), Tick::Idle);
    assert_eq!(p.frame(15.0).from, None, "the fade is over after fadeDuration");
    // the slide period counts from its transition: next at 18
    assert_eq!(p.tick(17.99, all_ready), Tick::Idle);
    assert_eq!(p.tick(18.0, all_ready), Tick::Advanced(ph(1)), "wraps to the photos before the start");
    assert_eq!(p.position(), (3, 3));
}

#[test]
fn fade_zero_is_a_cut_and_fade_color_is_reported() {
    let mut p = Playback::new(ids(2), None, settings(2.0, 0.0), 1, 0.0).unwrap();
    assert_eq!(p.tick(2.0, all_ready), Tick::Advanced(ph(2)));
    assert_eq!(p.frame(2.0).from, None);
    let s = PlaybackSettings { fade_color: Some([255, 0, 0]), ..settings(2.0, 1.0) };
    let mut p = Playback::new(ids(2), None, s, 1, 0.0).unwrap();
    p.tick(2.0, all_ready);
    assert_eq!(p.frame(2.5).fade_color, Some([255, 0, 0]));
}

#[test]
fn arrows_cut_immediately_and_restart_the_slide_time() {
    let mut p = Playback::new(ids(4), None, settings(4.0, 2.0), 1, 0.0).unwrap();
    p.tick(4.0, all_ready);
    assert!(p.frame(4.5).from.is_some(), "fading");
    assert_eq!(p.step(1, 4.5), Tick::Advanced(ph(3)));
    assert_eq!(p.frame(4.5), Frame { current: ph(3), from: None, progress: 1.0, fade_color: None }, "a cut");
    assert_eq!(p.tick(8.4, all_ready), Tick::Idle, "full time from the step");
    assert_eq!(p.step(-1, 6.0), Tick::Advanced(ph(2)));
    assert_eq!(p.step(-1, 6.0), Tick::Advanced(ph(1)));
    // repeat on: before the first is the last
    assert_eq!(p.step(-1, 6.0), Tick::Advanced(ph(4)));
    assert_eq!(p.remaining(6.0), 4.0);
}

#[test]
fn random_order_is_deterministic_per_seed_and_covers_every_photo_once_per_pass() {
    let photos = ids(20);
    let run = |seed: u64| {
        let s = PlaybackSettings { random_order: true, ..settings(1.0, 0.0) };
        let mut p = Playback::new(photos.clone(), Some(PhotoId(5)), s, seed, 0.0).unwrap();
        let mut seen = vec![p.current_photo().unwrap()];
        for k in 1..60 {
            p.tick(k as f64, all_ready);
            seen.push(p.current_photo().unwrap());
        }
        seen
    };
    let a = run(42);
    assert_eq!(a, run(42), "same seed, same order");
    assert_ne!(a, run(43), "another seed, another order");
    assert_eq!(a[0], PhotoId(5), "starts at the selected photo");
    assert_ne!(a[..20].to_vec(), photos, "shuffled");
    for pass in a.chunks(20).take(2) {
        let mut sorted = pass.to_vec();
        sorted.sort();
        assert_eq!(sorted, photos, "every photo once per pass");
    }
    assert_ne!(a[..20], a[20..40], "a new order for the next pass");
    assert!(a.windows(2).all(|w| w[0] != w[1]), "never the same photo twice in a row");
}

#[test]
fn repeat_off_stops_after_the_last_slide() {
    let s = PlaybackSettings { repeat: false, ..settings(1.0, 0.0) };
    let mut p = Playback::new(ids(3), Some(PhotoId(2)), s.clone(), 1, 0.0).unwrap();
    assert_eq!(p.tick(1.0, all_ready), Tick::Advanced(ph(3)));
    assert_eq!(p.tick(2.0, all_ready), Tick::Advanced(ph(1)));
    assert_eq!(p.upcoming(2), vec![], "nothing after the last");
    assert_eq!(p.tick(3.0, all_ready), Tick::Ended);
    assert!(p.is_ended());
    // → past the last one ends it too
    let mut p = Playback::new(ids(2), None, s, 1, 0.0).unwrap();
    assert_eq!(p.step(1, 0.5), Tick::Advanced(ph(2)));
    assert_eq!(p.step(1, 0.6), Tick::Ended);
    // with repeat: it goes on
    let mut p = Playback::new(ids(2), None, settings(1.0, 0.0), 1, 0.0).unwrap();
    for k in 1..10 {
        assert!(matches!(p.tick(k as f64, all_ready), Tick::Advanced(_)));
    }
}

#[test]
fn pause_and_resume_keep_the_remaining_time() {
    let mut p = Playback::new(ids(3), None, settings(4.0, 1.0), 1, 0.0).unwrap();
    assert!(p.toggle_pause(3.0));
    assert_eq!(p.tick(100.0, all_ready), Tick::Idle, "paused: nothing moves");
    assert_eq!(p.remaining(100.0), 1.0);
    assert!(!p.toggle_pause(100.0));
    assert_eq!(p.tick(100.9, all_ready), Tick::Idle);
    assert_eq!(p.tick(101.0, all_ready), Tick::Advanced(ph(2)), "1 s left after resuming");
    // pausing mid-fade freezes the fade
    p.toggle_pause(101.25);
    assert!((p.frame(150.0).progress - 0.25).abs() < 1e-6);
    p.toggle_pause(150.0);
    assert!((p.frame(150.5).progress - 0.75).abs() < 1e-6);
}

#[test]
fn a_transition_waits_for_the_next_slide_to_be_ready() {
    let mut p = Playback::new(ids(3), None, settings(2.0, 1.0), 1, 0.0).unwrap();
    assert_eq!(p.upcoming(2), vec![ph(2), ph(3)], "what to prepare");
    let not_two = |id: Item| id != ph(2);
    assert_eq!(p.tick(2.0, not_two), Tick::Waiting(ph(2)));
    assert_eq!(p.frame(3.0), Frame { current: ph(1), from: None, progress: 1.0, fade_color: None }, "held, not flashed");
    assert_eq!(p.waiting_for(), Some(ph(2)));
    assert_eq!(p.tick(3.5, all_ready), Tick::Advanced(ph(2)));
    assert_eq!(p.frame(3.5).from, Some(ph(1)), "the fade starts once it is ready");
    assert!((p.frame(4.0).progress - 0.5).abs() < 1e-6, "and runs its full length");
}

#[test]
fn a_slide_s_time_starts_once_it_is_ready() {
    let mut p = Playback::new(ids(3), None, settings(2.0, 0.5), 1, 0.0).unwrap();
    let none = |_: Item| false;
    assert_eq!(p.tick(1.0, none), Tick::Idle);
    assert_eq!(p.tick(5.0, none), Tick::Idle, "the first slide isn't rendered yet: its clock waits");
    assert_eq!(p.remaining(5.0), 2.0);
    assert_eq!(p.tick(6.0, all_ready), Tick::Idle);
    assert_eq!(p.tick(7.0, all_ready), Tick::Advanced(ph(2)), "2 s from when it could be shown");
}

#[test]
fn hostile_settings_are_rejected_or_clamped() {
    let d = PlaybackSettings::default();
    for bad in [
        json!({"slideDuration": "fast"}),
        json!({"fadeDuration": null}),
        json!({"repeat": 1}),
        json!({"fadeColor": "#12345"}),
        json!({"fadeColor": "#ééé"}),
        json!({"fadeColor": "#+f+f+f"}),
        json!({"fadeColor": "-12345"}),
        json!({"fadeColor": [300, 0, 0]}),
        json!({"fadeColor": [1, 2]}),
        json!({"speed": 2}),
        json!([1, 2]),
    ] {
        assert!(d.merged(&bad).is_err(), "{bad}");
    }
    let s = d.merged(&json!({"slideDuration": 1e300, "fadeDuration": -5})).unwrap();
    assert_eq!((s.slide_duration, s.fade_duration), (SLIDE_MAX, 0.0));
    let s = d.merged(&json!({"slideDuration": 0.0, "fadeDuration": 9})).unwrap();
    assert_eq!((s.slide_duration, s.fade_duration), (SLIDE_MIN, SLIDE_MIN), "the fade fits in the slide");
    let s = d.merged(&json!({"fadeColor": "#FF8000"})).unwrap();
    assert_eq!(s.fade_color, Some([255, 128, 0]));
    assert_eq!(s.to_json()["fadeColor"], "#ff8000");
    assert_eq!(d.merged(&json!({"fadeColor": [1, 2, 3]})).unwrap().fade_color, Some([1, 2, 3]));
    // NaN / inf (from a hand-edited file or a caller in-process) take the defaults
    let s = PlaybackSettings { slide_duration: f64::NAN, fade_duration: f64::INFINITY, ..d.clone() }.sanitized();
    assert_eq!((s.slide_duration, s.fade_duration), (d.slide_duration, d.fade_duration));
    // and the timeline never panics on hostile times
    let mut p = Playback::new(ids(2), None, s, 0, f64::NAN).unwrap();
    p.tick(f64::NAN, all_ready);
    p.tick(f64::INFINITY, all_ready);
    p.toggle_pause(f64::NEG_INFINITY);
    p.step(i32::MIN, f64::NAN);
    let _ = p.frame(f64::NAN);
    assert!(Playback::new(vec![], None, d, 0, 0.0).is_none());
}

#[test]
fn a_manual_slideshow_moves_only_with_the_arrows() {
    let s = PlaybackSettings { manual: true, repeat: false, ..settings(1.0, 0.5) };
    let mut p = titled(3, s, Some(1.0), Some(1.0));
    assert_eq!(p.tick(1000.0, all_ready), Tick::Idle, "the intro stays up past its duration");
    assert_eq!(p.current(), Item::INTRO);
    assert_eq!(p.remaining(1000.0), f64::INFINITY, "nothing is ever due");
    assert_eq!(p.upcoming(1), vec![ph(1)], "the next slide is still prepared for →");
    assert_eq!(p.step(1, 1000.0), Tick::Advanced(ph(1)));
    assert_eq!(p.tick(5000.0, all_ready), Tick::Idle);
    assert_eq!(p.current(), ph(1), "a photo stays up too");
    assert_eq!(p.step(-1, 5000.0), Tick::Advanced(Item::INTRO), "← goes back");
    assert_eq!(p.step(4, 5000.0), Tick::Advanced(Item::ENDING));
    assert_eq!(p.tick(9000.0, all_ready), Tick::Idle, "the ending stays up until → ends the show");
    assert_eq!(p.step(1, 9000.0), Tick::Ended);
    // the setting round-trips and is checked like the others
    let d = PlaybackSettings::default();
    assert!(!d.manual, "off by default");
    let m = d.merged(&json!({"manual": true})).unwrap();
    assert!(m.manual);
    assert_eq!(m.to_json()["manual"], true);
    assert!(d.merged(&json!({"manual": "yes"})).is_err());
}

#[test]
fn pan_and_zoom_moves_each_photo_over_its_slide_on_its_own_path() {
    // off: every slide shows its whole picture
    let p = Playback::new(ids(3), None, settings(4.0, 1.0), 1, 0.0).unwrap();
    assert_eq!(p.pan_zoom(2.0), PanZoom { current: None, from: None });

    let s = PlaybackSettings { pan_and_zoom: true, repeat: true, ..settings(4.0, 1.0) };
    let mut p = Playback::new(ids(3), None, s.clone(), 9, 0.0).unwrap().with_titles(Some(2.0), None);
    assert_eq!(p.pan_zoom(1.0).current, None, "a title screen doesn't move");
    assert_eq!(p.tick(2.0, all_ready), Tick::Advanced(ph(1)));
    let m1 = Motion::new(p.motion_key(ph(1)).unwrap(), 0.5);
    assert_eq!(p.pan_zoom(2.0).current, Some(m1.start), "a photo comes up at its start view");
    assert_eq!(p.pan_zoom(2.5).from, None, "the intro fading out doesn't move either");
    assert_eq!(p.pan_zoom(4.0).current, Some(m1.at(0.5)), "half way through its 4 s, fade in included");
    // paused: frozen
    p.toggle_pause(4.0);
    assert_eq!(p.pan_zoom(100.0).current, Some(m1.at(0.5)));
    p.toggle_pause(100.0);
    assert_eq!(p.pan_zoom(101.0).current, Some(m1.at(0.75)), "and carries on from there");
    // the next photo: its own path from its start; the one fading out stays at its end view
    assert_eq!(p.tick(102.0, all_ready), Tick::Advanced(ph(2)));
    let z = p.pan_zoom(102.5);
    let m2 = Motion::new(p.motion_key(ph(2)).unwrap(), 0.5);
    assert_ne!(m1, m2, "every photo has its own path");
    assert_eq!(z.current, Some(m2.at(0.125)));
    assert_eq!(z.from, Some(m1.end));
    assert_eq!(p.pan_zoom(103.5).from, None, "after the fade");
    // a slide that is due but not ready holds its end view
    assert_eq!(p.tick(106.0, |i| i != ph(3)), Tick::Waiting(ph(3)));
    assert_eq!(p.pan_zoom(110.0).current, Some(m2.end));
    // the same seed plays the same paths; another seed other ones
    let q = Playback::new(ids(3), None, s.clone(), 9, 0.0).unwrap();
    assert_eq!(q.motion_key(ph(1)).map(|k| Motion::new(k, 0.5)), Some(m1));
    let r = Playback::new(ids(3), None, s, 10, 0.0).unwrap();
    assert_ne!(r.motion_key(ph(1)), q.motion_key(ph(1)));
}

#[test]
fn pan_and_zoom_paths_change_each_pass_and_hold_in_a_manual_slideshow() {
    let s = PlaybackSettings { pan_and_zoom: true, repeat: true, ..settings(4.0, 1.0) };
    // a new pass: a new path for the same photo
    let mut p = Playback::new(ids(2), None, PlaybackSettings { fade_duration: 0.0, ..s.clone() }, 9, 0.0).unwrap();
    let first = p.pan_zoom(0.0).current;
    p.tick(4.0, all_ready);
    assert_eq!(p.tick(8.0, all_ready), Tick::Advanced(ph(1)));
    assert_ne!(p.pan_zoom(8.0).current, first, "a fresh path in the next pass");
    // a single photo repeating starts again on a new path
    let mut p = Playback::new(ids(1), None, s.clone(), 9, 0.0).unwrap();
    let first = p.pan_zoom(0.0).current;
    assert_eq!(p.tick(4.0, all_ready), Tick::Idle);
    assert_ne!(p.pan_zoom(4.0).current, first);
    // the amount sets the zoom
    let close = |amount: f64| {
        let p = Playback::new(ids(1), None, PlaybackSettings { pan_and_zoom_amount: amount, ..s.clone() }, 9, 0.0).unwrap();
        let m = Motion::new(p.motion_key(ph(1)).unwrap(), amount);
        m.start.zoom.max(m.end.zoom)
    };
    assert!(close(1.0) > close(0.0));
    // a manual slideshow holds each photo at its start view; ← / → start the next one there
    let mut p = Playback::new(ids(3), None, PlaybackSettings { manual: true, ..s }, 9, 0.0).unwrap();
    let start = Motion::new(p.motion_key(ph(1)).unwrap(), 0.5).start;
    assert_eq!(p.pan_zoom(500.0).current, Some(start));
    p.step(1, 500.0);
    assert_eq!(p.pan_zoom(900.0).current, Some(Motion::new(p.motion_key(ph(2)).unwrap(), 0.5).start));
}

#[test]
fn pan_and_zoom_settings_round_trip_and_are_checked() {
    let d = PlaybackSettings::default();
    assert!(!d.pan_and_zoom, "off by default");
    assert_eq!(d.pan_and_zoom_amount, 0.5);
    let m = d.merged(&json!({"panAndZoom": true, "panAndZoomAmount": 0.8})).unwrap();
    assert!(m.pan_and_zoom);
    assert_eq!((m.to_json()["panAndZoom"].clone(), m.to_json()["panAndZoomAmount"].clone()), (json!(true), json!(0.8)));
    assert_eq!(d.merged(&json!({"panAndZoomAmount": 3})).unwrap().pan_and_zoom_amount, 1.0, "clamped");
    assert_eq!(d.merged(&json!({"panAndZoomAmount": -1})).unwrap().pan_and_zoom_amount, 0.0);
    assert!(d.merged(&json!({"panAndZoom": "on"})).is_err());
    assert!(d.merged(&json!({"panAndZoomAmount": "high"})).is_err());
    let s = PlaybackSettings { pan_and_zoom_amount: f64::NAN, ..d.clone() }.sanitized();
    assert_eq!(s.pan_and_zoom_amount, 0.5, "not finite: the default");
    let off = d.merged(&json!({"panAndZoomAmount": 0.2, "panAndZoom": false})).unwrap();
    assert_eq!(off.pan_and_zoom_amount, 0.2, "the amount is kept with the effect off");
}

#[test]
fn a_single_photo_repeats_without_changing() {
    let mut p = Playback::new(ids(1), None, settings(1.0, 0.5), 1, 0.0).unwrap();
    assert_eq!(p.upcoming(2), vec![]);
    assert_eq!(p.tick(5.0, all_ready), Tick::Idle);
    assert_eq!(p.frame(5.0).from, None);
}

fn titled(n: u64, s: PlaybackSettings, intro: Option<f64>, ending: Option<f64>) -> Playback {
    Playback::new(ids(n), None, s, 1, 0.0).unwrap().with_titles(intro, ending)
}

#[test]
fn titles_play_once_around_the_photos_for_their_own_durations() {
    let s = PlaybackSettings { repeat: false, ..settings(4.0, 1.0) };
    let mut p = titled(3, s, Some(2.0), Some(3.0));
    assert_eq!(p.current(), Item::INTRO, "the intro comes first");
    assert_eq!((p.position(), p.title(), p.current_photo()), ((0, 3), Some(Title::Intro), None), "titles aren't photos");
    assert_eq!(p.upcoming(2), vec![ph(1), ph(2)]);
    assert_eq!(p.tick(1.99, all_ready), Tick::Idle);
    assert_eq!(p.tick(2.0, all_ready), Tick::Advanced(ph(1)), "the intro's own 2 s");
    let f = p.frame(2.5);
    assert_eq!((f.current, f.from), (ph(1), Some(Item::INTRO)), "the intro fades out like a slide");
    assert!((f.progress - 0.5).abs() < 1e-6);
    assert_eq!(p.position(), (1, 3));
    assert_eq!(p.tick(6.0, all_ready), Tick::Advanced(ph(2)));
    assert_eq!(p.tick(10.0, all_ready), Tick::Advanced(ph(3)));
    assert_eq!(p.upcoming(2), vec![Item::ENDING], "the ending is prepared like a slide");
    assert_eq!(p.tick(14.0, all_ready), Tick::Advanced(Item::ENDING));
    assert_eq!((p.position(), p.title()), ((3, 3), Some(Title::Ending)));
    assert_eq!(p.frame(14.5).from, Some(ph(3)), "the ending fades in");
    assert_eq!(p.tick(16.99, all_ready), Tick::Idle);
    assert_eq!(p.tick(17.0, all_ready), Tick::Ended, "the ending's own 3 s, then the show ends");
    // without titles nothing changes
    let mut p = titled(2, PlaybackSettings { repeat: false, ..settings(1.0, 0.0) }, None, None);
    assert_eq!(p.current(), ph(1));
    assert_eq!(p.tick(1.0, all_ready), Tick::Advanced(ph(2)));
    assert_eq!(p.tick(2.0, all_ready), Tick::Ended);
}

#[test]
fn a_fade_never_outlasts_a_short_title() {
    let s = PlaybackSettings { repeat: false, ..settings(4.0, 2.0) };
    let mut p = titled(1, s, Some(0.5), Some(0.5));
    assert_eq!(p.tick(0.5, all_ready), Tick::Advanced(ph(1)));
    assert!((p.frame(1.5).progress - 0.5).abs() < 1e-6, "the photo fades in over the full 2 s");
    assert_eq!(p.tick(4.5, all_ready), Tick::Advanced(Item::ENDING));
    assert!((p.frame(4.75).progress - 0.5).abs() < 1e-6, "into the 0.5 s ending: a 0.5 s fade");
    assert_eq!(p.frame(5.0).from, None);
    assert_eq!(p.tick(5.0, all_ready), Tick::Ended);
}

#[test]
fn with_repeat_the_intro_plays_once_and_the_ending_never() {
    let mut p = titled(2, settings(1.0, 0.0), Some(1.0), Some(1.0));
    let mut seen = vec![p.current()];
    for k in 1..12 {
        p.tick(k as f64, all_ready);
        seen.push(p.current());
        assert!(!p.upcoming(3).contains(&Item::ENDING), "no ending while repeating");
    }
    assert_eq!(seen[0], Item::INTRO);
    assert!(seen[1..].iter().all(|i| i.photo().is_some()), "passes follow each other without titles: {seen:?}");
    assert_eq!(seen[1..5], [ph(1), ph(2), ph(1), ph(2)]);
}

#[test]
fn random_order_shuffles_the_photos_and_keeps_the_titles_in_place() {
    for seed in [1, 2, 3] {
        let s = PlaybackSettings { random_order: true, repeat: false, ..settings(1.0, 0.0) };
        let mut p = Playback::new(ids(10), None, s, seed, 0.0).unwrap().with_titles(Some(1.0), Some(1.0));
        let mut seen = vec![p.current()];
        let mut k = 1.0;
        while p.tick(k, all_ready) != Tick::Ended {
            seen.push(p.current());
            k += 1.0;
        }
        assert_eq!(seen.len(), 12, "{seen:?}");
        assert_eq!((seen[0], seen[11]), (Item::INTRO, Item::ENDING), "seed {seed}");
        let mut photos: Vec<PhotoId> = seen[1..11].iter().filter_map(|i| i.photo()).collect();
        assert_ne!(photos, ids(10), "shuffled");
        photos.sort();
        assert_eq!(photos, ids(10), "every photo once, between the titles");
    }
}

#[test]
fn arrows_step_across_the_titles() {
    let s = PlaybackSettings { repeat: false, ..settings(4.0, 1.0) };
    let mut p = titled(3, s, Some(2.0), Some(2.0));
    assert_eq!(p.step(-1, 0.5), Tick::Advanced(Item::INTRO), "← on the intro stays there");
    assert_eq!(p.step(1, 0.5), Tick::Advanced(ph(1)));
    assert_eq!(p.step(-1, 0.6), Tick::Advanced(Item::INTRO), "← from the first photo: back to the intro");
    assert_eq!(p.remaining(0.6), 2.0, "with its full time");
    assert_eq!(p.step(3, 1.0), Tick::Advanced(ph(3)));
    assert_eq!(p.step(1, 1.0), Tick::Advanced(Item::ENDING));
    assert_eq!(p.position(), (3, 3));
    assert_eq!(p.step(-1, 1.0), Tick::Advanced(ph(3)), "← from the ending: the last photo");
    assert_eq!(p.step(1, 1.0), Tick::Advanced(Item::ENDING));
    assert_eq!(p.step(1, 1.0), Tick::Ended, "→ past the ending ends the show");
    assert_eq!(p.step(1, 2.0), Tick::Ended);
    // repeating: ← from the first photo of a later pass wraps to the last, not the intro
    let mut p = titled(3, settings(4.0, 0.0), Some(2.0), None);
    assert_eq!(p.step(4, 0.0), Tick::Advanced(ph(1)), "intro, 1, 2, 3, then 1 again");
    assert_eq!(p.step(-1, 0.0), Tick::Advanced(ph(3)));
    assert_eq!(p.step(i32::MAX, 0.0), Tick::Advanced(p.current()));
    assert_eq!(p.step(i32::MIN, f64::NAN), Tick::Advanced(p.current()));
}

#[test]
fn a_title_pauses_and_waits_like_a_slide() {
    let mut p = titled(2, settings(4.0, 1.0), Some(3.0), None);
    let not_intro = |i: Item| i != Item::INTRO;
    assert_eq!(p.tick(5.0, not_intro), Tick::Idle, "the intro isn't rendered yet: its clock waits");
    assert_eq!(p.remaining(5.0), 3.0);
    assert!(p.toggle_pause(6.0));
    assert_eq!(p.tick(100.0, all_ready), Tick::Idle);
    assert_eq!(p.remaining(100.0), 2.0, "paused on the intro");
    assert!(!p.toggle_pause(100.0));
    assert_eq!(p.tick(101.9, all_ready), Tick::Idle);
    let not_one = |i: Item| i != ph(1);
    assert_eq!(p.tick(102.0, not_one), Tick::Waiting(ph(1)), "the first photo isn't ready: the intro is held");
    assert_eq!(p.frame(102.5).current, Item::INTRO);
    assert_eq!(p.tick(103.0, all_ready), Tick::Advanced(ph(1)));
}

#[test]
fn hostile_title_durations_are_clamped() {
    let p = titled(2, settings(4.0, 1.0), Some(f64::NAN), Some(1e300));
    assert_eq!((p.duration(Item::INTRO), p.duration(Item::ENDING)), (4.0, SLIDE_MAX));
    let p = titled(2, settings(4.0, 1.0), Some(-1.0), Some(f64::NEG_INFINITY));
    assert_eq!((p.duration(Item::INTRO), p.duration(Item::ENDING)), (SLIDE_MIN, 4.0));
    // a single photo between titles, repeating: the photo holds after the intro
    let mut p = titled(1, settings(1.0, 0.0), Some(1.0), Some(1.0));
    assert_eq!(p.upcoming(3), vec![ph(1)]);
    assert_eq!(p.tick(1.0, all_ready), Tick::Advanced(ph(1)));
    assert_eq!(p.upcoming(3), vec![]);
    assert_eq!(p.tick(9.0, all_ready), Tick::Idle);
    assert_eq!(Title::parse(" Ending "), Some(Title::Ending));
    assert_eq!(Title::parse("credits"), None);
}
