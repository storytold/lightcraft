//! Every culling key confirms with a toast, whichever key it is: the toast comes from the rating, flag or
//! label command that ran, not from the key's own handler. The Shift and `[` / `]` variants used to change
//! the photo, advance, and say nothing.
use egui::{Key, Modifiers};
use lightcraft_catalog::{Flag, Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::LightcraftApp;
use crate::state::ViewMode;

/// Four photos rated `ratings` (a fifth star on photo 3, none on 1, two on 2 …), photo 1 selected and active.
fn app_with(ratings: [u8; 4]) -> LightcraftApp {
    let mut s = lightcraft_engine::Session::new();
    for (i, rating) in ratings.into_iter().enumerate() {
        let id = i as u64 + 1;
        let mut photo = Photo::new(
            PhotoId(id),
            Source::File { path: format!("/lightcraft-cull-feedback/{id}.jpg") },
            &format!("{id}.jpg"),
            "JPEG",
            40,
            30,
            "2026-11-03",
        );
        photo.rating = rating;
        s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    }
    s.execute("library.sort", &json!({"key": "fileName", "ascending": true})).unwrap();
    s.execute("library.select", &json!({"ids": [1]})).unwrap();
    let mut app = LightcraftApp::new(s, crate::Services { png: None, ..Default::default() });
    app.ui.view = ViewMode::PhotoGrid;
    app
}

fn fresh() -> LightcraftApp {
    app_with([0; 4])
}

fn press(app: &mut LightcraftApp, ctx: &egui::Context, key: Key, modifiers: Modifiers) {
    let events = vec![egui::Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers }];
    let mut out = ctx.run_ui(
        egui::RawInput { events, screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))), ..Default::default() },
        |ui| super::handle(app, ui.ctx()),
    );
    out.textures_delta.clear();
}

/// The toast now showing (its text), taken: the next key starts from none.
fn toast(app: &mut LightcraftApp) -> Option<String> {
    app.ui.toast.take().map(|t| t.0)
}

fn select(app: &mut LightcraftApp, ids: &[u64], active: u64) {
    app.session.execute("library.select", &json!({"ids": ids, "active": active})).unwrap();
}

fn flag_of(app: &LightcraftApp, id: u64) -> Flag {
    app.session.catalog.photo(PhotoId(id)).unwrap().flag
}

fn rating_of(app: &LightcraftApp, id: u64) -> u8 {
    app.session.catalog.photo(PhotoId(id)).unwrap().rating
}

#[test]
fn shift_flag_keys_toast_what_their_plain_keys_toast_and_still_advance() {
    for view in [ViewMode::PhotoGrid, ViewMode::SquareGrid, ViewMode::Detail] {
        for (key, flag, text) in
            [(Key::X, Flag::Reject, "Flagged as Reject"), (Key::Z, Flag::Pick, "Flagged as Pick"), (Key::U, Flag::None, "Unflagged")]
        {
            let mut app = fresh();
            app.ui.view = view;
            let ctx = egui::Context::default();
            press(&mut app, &ctx, key, Modifiers::SHIFT);
            assert_eq!(toast(&mut app).as_deref(), Some(text), "Shift+{key:?} in {view:?}");
            assert_eq!(flag_of(&app, 1), flag, "the photo it names is the one it changed (Shift+{key:?} in {view:?})");
            assert_eq!(app.session.active(), Some(PhotoId(2)), "Shift+{key:?} in {view:?} still advances");
        }
    }
}

#[test]
fn grid_shift_p_toasts_the_pick_and_advances() {
    for view in [ViewMode::PhotoGrid, ViewMode::SquareGrid] {
        let mut app = fresh();
        app.ui.view = view;
        let ctx = egui::Context::default();
        press(&mut app, &ctx, Key::P, Modifiers::SHIFT);
        assert_eq!(toast(&mut app).as_deref(), Some("Flagged as Pick"), "{view:?}");
        assert_eq!(flag_of(&app, 1), Flag::Pick);
        assert_eq!(app.session.active(), Some(PhotoId(2)));
    }
}

#[test]
fn bracket_keys_toast_the_rating_the_photo_has_now() {
    for view in [ViewMode::PhotoGrid, ViewMode::SquareGrid] {
        let mut app = app_with([2, 0, 0, 0]);
        app.ui.view = view;
        let ctx = egui::Context::default();
        press(&mut app, &ctx, Key::CloseBracket, Modifiers::NONE);
        assert_eq!(toast(&mut app).as_deref(), Some("Rated ★★★"), "{view:?}");
        press(&mut app, &ctx, Key::OpenBracket, Modifiers::NONE);
        assert_eq!(toast(&mut app).as_deref(), Some("Rated ★★"), "{view:?}");
        press(&mut app, &ctx, Key::OpenBracket, Modifiers::NONE);
        press(&mut app, &ctx, Key::OpenBracket, Modifiers::NONE);
        assert_eq!(toast(&mut app).as_deref(), Some("Rating cleared"), "{view:?}");
        assert_eq!(rating_of(&app, 1), 0);
    }
}

#[test]
fn a_bracket_key_at_the_limit_still_names_the_rating() {
    let mut app = app_with([5, 0, 0, 0]);
    let ctx = egui::Context::default();
    press(&mut app, &ctx, Key::CloseBracket, Modifiers::NONE);
    assert_eq!(toast(&mut app).as_deref(), Some("Rated ★★★★★"));
}

#[test]
fn the_toast_names_the_photo_that_changed_not_the_one_auto_advance_lands_on() {
    for auto_advance in [false, true] {
        let mut app = app_with([0, 3, 0, 0]);
        app.ui.auto_advance = auto_advance;
        let ctx = egui::Context::default();
        press(&mut app, &ctx, Key::Num5, Modifiers::NONE);
        assert_eq!(toast(&mut app).as_deref(), Some("Rated ★★★★★"), "Auto Advance={auto_advance}");
        // a relative step reads the changed photo (photo 2 holds 3 stars, the next photo)
        let mut app = app_with([1, 3, 0, 0]);
        app.ui.auto_advance = auto_advance;
        press(&mut app, &ctx, Key::CloseBracket, Modifiers::NONE);
        assert_eq!(toast(&mut app).as_deref(), Some("Rated ★★"), "Auto Advance={auto_advance}");
        // a flag step likewise
        let mut app = app_with([0; 4]);
        app.ui.auto_advance = auto_advance;
        press(&mut app, &ctx, Key::Backtick, Modifiers::NONE);
        assert_eq!(toast(&mut app).as_deref(), Some("Flagged as Pick"), "Auto Advance={auto_advance}");
    }
}

#[test]
fn shift_digits_and_aliases_name_the_photo_they_changed_in_compare() {
    // Compare / Survey act on the active photo only, though the selection holds three
    for view in [ViewMode::Compare, ViewMode::Survey] {
        let mut app = fresh();
        app.ui.view = view;
        select(&mut app, &[1, 2, 3], 1);
        let ctx = egui::Context::default();
        press(&mut app, &ctx, Key::X, Modifiers::SHIFT);
        assert_eq!(toast(&mut app).as_deref(), Some("Flagged as Reject"), "{view:?}: one photo, so no count");
        assert_eq!([flag_of(&app, 1), flag_of(&app, 2), flag_of(&app, 3)], [Flag::Reject, Flag::None, Flag::None]);
        press(&mut app, &ctx, Key::Num4, Modifiers::SHIFT);
        assert_eq!(toast(&mut app).as_deref(), Some("Rated ★★★★"), "{view:?}");
        assert_eq!([rating_of(&app, 1), rating_of(&app, 2), rating_of(&app, 3)].iter().filter(|r| **r == 4).count(), 1);
    }
}

#[test]
fn several_photos_changed_says_how_many() {
    let ctx = egui::Context::default();
    let mut app = fresh();
    select(&mut app, &[1, 2, 3], 2);
    press(&mut app, &ctx, Key::Num5, Modifiers::NONE);
    assert_eq!(toast(&mut app).as_deref(), Some("Rated ★★★★★ · 3 photos"));
    press(&mut app, &ctx, Key::Num0, Modifiers::NONE);
    assert_eq!(toast(&mut app).as_deref(), Some("Rating cleared · 3 photos"));
    press(&mut app, &ctx, Key::X, Modifiers::NONE);
    assert_eq!(toast(&mut app).as_deref(), Some("Flagged as Reject · 3 photos"));
    press(&mut app, &ctx, Key::P, Modifiers::NONE);
    assert_eq!(toast(&mut app).as_deref(), Some("Flagged as Pick · 3 photos"));
    press(&mut app, &ctx, Key::U, Modifiers::NONE);
    assert_eq!(toast(&mut app).as_deref(), Some("Unflagged · 3 photos"));
    press(&mut app, &ctx, Key::Num6, Modifiers::NONE);
    assert_eq!(toast(&mut app).as_deref(), Some("Red Label · 3 photos"));
    // Shift+X acts on the whole selection too, and says so
    press(&mut app, &ctx, Key::X, Modifiers::SHIFT);
    assert_eq!(toast(&mut app).as_deref(), Some("Flagged as Reject · 3 photos"));
}

#[test]
fn the_count_is_the_photos_the_key_acted_on_even_when_some_already_had_the_value() {
    let ctx = egui::Context::default();
    let mut app = app_with([5, 0, 5, 0]);
    select(&mut app, &[1, 2, 3], 1);
    press(&mut app, &ctx, Key::Num5, Modifiers::NONE);
    assert_eq!(toast(&mut app).as_deref(), Some("Rated ★★★★★ · 3 photos"));
}

#[test]
fn a_step_that_leaves_the_photos_with_different_values_says_so_instead_of_naming_one() {
    let ctx = egui::Context::default();
    // 0, 2 and 5 stars: ] makes 1, 3 and 5, so no single rating is true of all three
    let mut app = app_with([0, 2, 5, 0]);
    select(&mut app, &[1, 2, 3], 2);
    press(&mut app, &ctx, Key::CloseBracket, Modifiers::NONE);
    assert_eq!(toast(&mut app).as_deref(), Some("Rating changed · 3 photos"));
    // all end the same: the rating is named
    let mut app = app_with([4, 4, 5, 0]);
    select(&mut app, &[1, 2, 3], 1);
    press(&mut app, &ctx, Key::CloseBracket, Modifiers::NONE);
    assert_eq!(toast(&mut app).as_deref(), Some("Rated ★★★★★ · 3 photos"));
    // a flag cycle: unflagged, pick and reject go to pick, reject and unflagged
    let mut app = fresh();
    select(&mut app, &[1, 2, 3], 1);
    app.session.execute("photo.flag", &json!({"flag": "pick", "ids": [2]})).unwrap();
    app.session.execute("photo.flag", &json!({"flag": "reject", "ids": [3]})).unwrap();
    press(&mut app, &ctx, Key::Backtick, Modifiers::NONE);
    assert_eq!(toast(&mut app).as_deref(), Some("Flag changed · 3 photos"));
}

#[test]
fn a_refused_key_keeps_its_reason_and_claims_no_success() {
    let ctx = egui::Context::default();
    // nothing to rate or flag: every key is refused
    let mut empty = LightcraftApp::new(lightcraft_engine::Session::new(), crate::Services { png: None, ..Default::default() });
    empty.ui.view = ViewMode::PhotoGrid;
    for (key, modifiers) in [
        (Key::Num3, Modifiers::NONE),
        (Key::Num3, Modifiers::SHIFT),
        (Key::X, Modifiers::NONE),
        (Key::X, Modifiers::SHIFT),
        (Key::P, Modifiers::SHIFT),
        (Key::CloseBracket, Modifiers::NONE),
        (Key::Backtick, Modifiers::NONE),
        (Key::Num6, Modifiers::NONE),
    ] {
        empty.ui.status.clear();
        press(&mut empty, &ctx, key, modifiers);
        assert!(toast(&mut empty).is_none(), "{key:?} {modifiers:?} claimed success on an empty library");
        assert!(!empty.ui.status.is_empty(), "{key:?} {modifiers:?} lost the reason it was refused");
    }
}

#[test]
fn the_menu_and_the_buttons_say_the_same_thing_as_the_keys() {
    let mut app = fresh();
    select(&mut app, &[1, 2], 1);
    crate::menubar::run_item(&mut app, "photo.rate", json!({"rating": 3})).unwrap();
    assert_eq!(toast(&mut app).as_deref(), Some("Rated ★★★ · 2 photos"));
    crate::menubar::run_item(&mut app, "photo.flag", json!({"flag": "reject"})).unwrap();
    assert_eq!(toast(&mut app).as_deref(), Some("Flagged as Reject · 2 photos"));
}

#[test]
fn a_new_toast_replaces_the_one_before_it() {
    let ctx = egui::Context::default();
    let mut app = fresh();
    press(&mut app, &ctx, Key::Num3, Modifiers::NONE);
    press(&mut app, &ctx, Key::X, Modifiers::SHIFT);
    assert_eq!(toast(&mut app).as_deref(), Some("Flagged as Reject"));
}

#[test]
fn the_count_and_the_state_follow_the_interface_language() {
    use crate::i18n::{Locale, set_language};
    let ctx = egui::Context::default();
    set_language(Locale::De);
    let mut app = fresh();
    select(&mut app, &[1, 2, 3], 1);
    press(&mut app, &ctx, Key::X, Modifiers::NONE);
    let german = toast(&mut app).unwrap();
    set_language(Locale::En);
    assert!(german.starts_with(Locale::De.tr("Flagged as Reject")), "{german}");
    assert_ne!(german, "Flagged as Reject · 3 photos", "the count wording comes from the catalog too");
    assert!(german.contains('3'), "{german}");
}

#[test]
fn the_count_is_of_distinct_photos_when_a_call_names_one_twice() {
    // the control channel and agents give `ids` as they like: a photo named twice is one photo
    let mut app = fresh();
    app.run("photo.rate", json!({"rating": 3, "ids": [1, 1]})).unwrap();
    assert_eq!(toast(&mut app).as_deref(), Some("Rated ★★★"));
    app.run("photo.flag", json!({"flag": "pick", "ids": [1, 2, 2, 1]})).unwrap();
    assert_eq!(toast(&mut app).as_deref(), Some("Flagged as Pick · 2 photos"));
    app.run("photo.label", json!({"label": "red", "ids": [3, 3, 3]})).unwrap();
    assert_eq!(toast(&mut app).as_deref(), Some("Red Label"));
}

#[test]
fn a_refused_command_on_photos_that_exist_confirms_nothing_and_changes_nothing() {
    // The empty-library test above never reaches the confirmation (there is no photo to name); these calls name
    // real photos, so only a refusal stops the toast. `label` missing or not a color must not read as a clear.
    let mut app = app_with([2, 0, 0, 0]);
    for (id, params) in [
        ("photo.rate", json!({"rating": 9})),
        ("photo.rate", json!({"rating": 3, "ids": [1, 999]})),
        ("photo.flag", json!({"flag": "bogus"})),
        ("photo.flag", json!({"flag": "pick", "ids": [1, 999]})),
        ("photo.label", json!({})),
        ("photo.label", json!({"label": "chartreuse"})),
        ("photo.label", json!({"label": 7})),
        ("photo.label", json!({"label": "red", "ids": [1, 999]})),
        ("photo.increaseRating", json!({"ids": [1, 999]})),
        ("photo.cycleFlag", json!({"ids": [1, 999]})),
        ("photo.pick", json!({"ids": [1, 999]})),
        ("photo.reject", json!({"id": 999})),
    ] {
        app.ui.status.clear();
        let refusal = app.run_typed(id, params.clone());
        assert!(refusal.is_err(), "{id} {params} was meant to be refused");
        assert!(toast(&mut app).is_none(), "{id} {params} confirmed a change it refused");
        assert!(!app.ui.status.is_empty(), "{id} {params} lost the reason it was refused");
    }
    // a refusal changes none of the photos it named (the call is all or nothing)
    assert_eq!((rating_of(&app, 1), flag_of(&app, 1)), (2, Flag::None));
    assert_eq!(app.session.catalog.photo(PhotoId(1)).unwrap().label, None);
}

#[test]
fn a_call_that_names_no_photo_confirms_nothing() {
    // `ids: []` changes nothing; a label is the one confirmation that doesn't read the photos back, so it must
    // not say "Red Label" for it
    let mut app = fresh();
    for (id, params) in [
        ("photo.label", json!({"label": "red", "ids": []})),
        ("photo.clearLabel", json!({"ids": []})),
        ("photo.rate", json!({"rating": 3, "ids": []})),
        ("photo.flag", json!({"flag": "pick", "ids": []})),
    ] {
        app.run(id, params.clone()).unwrap();
        assert!(toast(&mut app).is_none(), "{id} {params}");
    }
}

#[test]
fn a_label_toast_says_set_or_cleared_and_carries_the_color() {
    let mut app = fresh();
    let tint = |app: &mut LightcraftApp| app.ui.toast.take().map(|(text, _, color)| (text, color));
    app.run("photo.label", json!({"label": "red"})).unwrap();
    assert_eq!(tint(&mut app), Some(("Red Label".into(), Some(lightcraft_catalog::ColorLabel::Red))));
    // `none` and an explicit null clear, as `photo.clearLabel` does
    for (id, params) in [
        ("photo.label", json!({"label": "none"})),
        ("photo.label", json!({"label": null})),
        ("photo.clearLabel", json!({})),
        // Clear Color Label takes no color: a stray `label` doesn't make it a set
        ("photo.clearLabel", json!({"label": "green"})),
    ] {
        app.run("photo.label", json!({"label": "blue"})).unwrap();
        app.ui.toast = None;
        app.run(id, params.clone()).unwrap();
        assert_eq!(tint(&mut app), Some(("Color label cleared".into(), None)), "{id} {params}");
        assert_eq!(app.session.catalog.photo(PhotoId(1)).unwrap().label, None, "{id} {params}");
    }
}
