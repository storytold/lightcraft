//! The rating (0–5) and color-label (6–9) keys are commands of their own, so Help ▸ Keyboard Shortcuts can give them other keys,
//! and the digit row works by its place on layouts that type punctuation there (French AZERTY).
use egui::{Key, Modifiers};
use lightcraft_catalog::{ColorLabel, Op, Photo, PhotoId, Source};
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::menubar::MenuNode;
use crate::state::{Dialog, ViewMode};

const NUM: [Key; 10] = [Key::Num0, Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
const LABELS: [ColorLabel; 4] = [ColorLabel::Red, ColorLabel::Yellow, ColorLabel::Green, ColorLabel::Blue];

/// Each key's command, the label the shortcuts editor lists it under, and the digit it has until the user changes it.
const COMMANDS: [(&str, &str, &str); 10] = [
    ("photo.rate0", "Clear rating", "0"),
    ("photo.rate1", "Rate ★", "1"),
    ("photo.rate2", "Rate ★★", "2"),
    ("photo.rate3", "Rate ★★★", "3"),
    ("photo.rate4", "Rate ★★★★", "4"),
    ("photo.rate5", "Rate ★★★★★", "5"),
    ("photo.labelRed", "Red label", "6"),
    ("photo.labelYellow", "Yellow label", "7"),
    ("photo.labelGreen", "Green label", "8"),
    ("photo.labelBlue", "Blue label", "9"),
];

/// The characters the digit row of French AZERTY (Windows, Linux and macOS's "French – PC") types without Shift, from the 1 key to the 0 key.
const FRENCH_ROW: [&str; 10] = ["&", "é", "\"", "'", "(", "-", "è", "_", "ç", "à"];

fn app() -> lightcraft_engine::Result<LightcraftApp> {
    let mut s = lightcraft_engine::Session::new();
    for id in 1..=4 {
        let photo = Photo::new(
            PhotoId(id),
            Source::File { path: format!("/lightcraft-culling-keys/{id}.jpg") },
            &format!("{id}.jpg"),
            "JPEG",
            40,
            30,
            "2026-11-03",
        );
        s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) })?;
    }
    s.execute("library.sort", &json!({"key": "fileName", "ascending": true}))?;
    s.execute("library.select", &json!({"ids": [1]}))?;
    let mut app = LightcraftApp::new(s, crate::Services { png: None, ..Default::default() });
    app.ui.view = ViewMode::PhotoGrid;
    Ok(app)
}

/// An app and the egui context its frames run in.
struct Rig {
    app: LightcraftApp,
    ctx: egui::Context,
}

impl Rig {
    fn new() -> Self {
        Rig { app: app().unwrap(), ctx: egui::Context::default() }
    }

    fn raw(events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput { events, screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))), ..Default::default() }
    }

    /// One frame with these events and the global shortcut pass; a text field has the keyboard when `text` is given.
    fn frame_with(&mut self, events: Vec<egui::Event>, text: Option<&mut String>) {
        let app = &mut self.app;
        let mut text = text;
        let mut out = self.ctx.run_ui(Self::raw(events), |ui| {
            if let Some(text) = text.as_deref_mut() {
                ui.add(egui::TextEdit::singleline(text).id(egui::Id::unique("culling-keys-search"))).request_focus();
            }
            super::handle(app, ui.ctx());
        });
        out.textures_delta.clear();
    }

    fn frame(&mut self, events: Vec<egui::Event>) {
        self.frame_with(events, None);
    }

    fn event(&mut self, event: egui::Event) {
        self.frame(vec![event]);
    }

    /// What the shortcut pass finds in these events: the commands it would fire and the aliases it would run. Nothing runs.
    fn pressed(&mut self, events: Vec<egui::Event>) -> (Vec<String>, Vec<(&'static str, Value)>) {
        let app = &mut self.app;
        let mut found = None;
        let mut out = self.ctx.run_ui(Self::raw(events), |ui| found = Some(super::pressed_commands(app, ui.ctx())));
        out.textures_delta.clear();
        let (fire, aliased, _h_press) = found.unwrap();
        (fire, aliased)
    }

    /// Give command `id` the key `sc` as Help ▸ Keyboard Shortcuts does; the commands that lost it.
    fn assign(&mut self, id: &str, sc: &str) -> Vec<&'static str> {
        super::assign(&mut self.app.ui.settings.keymap, id, Some(sc)).unwrap()
    }

    fn photo(&self, id: u64) -> &Photo {
        self.app.session.catalog.photo(PhotoId(id)).unwrap()
    }

    fn active(&self) -> Option<PhotoId> {
        self.app.session.active()
    }
}

fn key_event(key: Key, place: Key, modifiers: Modifiers) -> egui::Event {
    egui::Event::Key { key, physical_key: Some(place), pressed: true, repeat: false, modifiers }
}

/// A press of the shortcut `sc` on a layout whose keys type what the shortcut names.
fn chord(sc: &str) -> egui::Event {
    let (modifiers, key) = super::parse(sc).unwrap();
    key_event(key, key, modifiers)
}

/// What the windowing layer hands the app for a press of `typed` (the character the layout types) with the
/// digit-row key `place` under the finger. egui-winit turns the typed character into an egui key with
/// `Key::from_name` and, when egui has no key for it, reports the key's place instead
/// (`logical_key.or(physical_key)` in its `on_keyboard_input`); the place stays `physical_key`.
fn press(typed: &str, place: Key, modifiers: Modifiers) -> egui::Event {
    key_event(Key::from_name(typed).unwrap_or(place), place, modifiers)
}

/// The key `digit` of the digit row on French AZERTY, pressed without Shift.
fn french(digit: usize) -> egui::Event {
    press(FRENCH_ROW[(digit + 9) % 10], NUM[digit], Modifiers::NONE)
}

/// Whether the press of digit key `digit` did its job on photo 1 and left the selection where it was.
fn did(rig: &Rig, digit: usize) -> bool {
    let photo = rig.photo(1);
    let done = if digit <= 5 { photo.rating == digit as u8 } else { photo.label == Some(LABELS[digit - 6]) };
    done && rig.active() == Some(PhotoId(1))
}

/// The shortcut of each menu row that runs `command`, in menu order.
fn menu_keys(app: &LightcraftApp, command: &str) -> Vec<Option<String>> {
    fn walk(nodes: &[MenuNode], command: &str, out: &mut Vec<Option<String>>) {
        for node in nodes {
            match node {
                MenuNode::Item { id, shortcut, .. } if id == command => out.push(shortcut.clone()),
                MenuNode::Submenu { children, .. } => walk(children, command, out),
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    for (_, nodes) in crate::menubar::menu_bar(app) {
        walk(&nodes, command, &mut out);
    }
    out
}

fn some(keys: &[&str]) -> Vec<Option<String>> {
    keys.iter().map(|k| Some(k.to_string())).collect()
}

#[test]
fn french_digit_row_rates_and_labels_without_shift_or_advance() {
    let mut failed = Vec::new();
    for digit in 0..=9 {
        // `-` over the 6 is Decrease Grid Thumbnail Size's key: it keeps it (see the next test)
        if digit == 6 {
            continue;
        }
        let mut rig = Rig::new();
        // the 0 is already set on a fresh photo: start every key from another rating and label
        rig.app.session.execute("photo.rate", &json!({"rating": 5 - digit.min(5)})).unwrap();
        rig.event(french(digit));
        if !did(&rig, digit) {
            failed.push(format!("{} (the {digit} key)", FRENCH_ROW[(digit + 9) % 10]));
        }
    }
    assert!(failed.is_empty(), "no rating or label on French AZERTY without Shift: {failed:?}");
}

#[test]
fn a_key_another_command_has_is_never_taken_for_a_digit() {
    // `-` over the 6 is Decrease Grid Thumbnail Size's key
    let mut rig = Rig::new();
    let (fire, aliased) = rig.pressed(vec![french(6)]);
    assert_eq!(fire, ["view.thumbSizeDown"]);
    assert!(aliased.is_empty(), "{aliased:?}");
    // so is one the user gave away: Detail has the `'` that sits on the 4 key
    rig.assign("view.detail", "'");
    let (fire, _) = rig.pressed(vec![french(4)]);
    assert_eq!(fire, ["view.detail"]);
    // and a punctuation key nothing has, held with ⌘, ⌥ or ⌃, is no digit either
    let mut rig = Rig::new();
    for modifiers in [Modifiers::COMMAND, Modifiers::ALT, Modifiers::CTRL, Modifiers::ALT | Modifiers::CTRL] {
        rig.event(press("'", Key::Num4, modifiers));
        assert_eq!(rig.photo(1).rating, 0, "{modifiers:?}");
    }
}

#[test]
fn a_numpad_arrow_with_num_lock_off_is_not_a_rating() {
    // the numpad's 4 without Num Lock is ←: the key is the arrow, its place is still the 4
    let mut rig = Rig::new();
    let (fire, _) = rig.pressed(vec![key_event(Key::ArrowLeft, Key::Num4, Modifiers::NONE)]);
    assert_eq!(fire, ["library.previous"]);
}

#[test]
fn a_numpad_home_with_num_lock_off_is_not_a_label() {
    // the numpad's 7 without Num Lock is Home: no punctuation, so it is no yellow label whatever the 7 key's place says
    let mut rig = Rig::new();
    let (fire, _) = rig.pressed(vec![key_event(Key::Home, Key::Num7, Modifiers::NONE)]);
    assert!(!fire.iter().any(|f| f.starts_with("label:")), "{fire:?}");
}

#[test]
fn other_layouts_that_type_punctuation_on_the_digit_row() {
    // macOS's French and the Belgian layout type `!` on the 8 key: nothing has it, so the key sets the green label
    let mut rig = Rig::new();
    rig.event(press("!", Key::Num8, Modifiers::NONE));
    assert_eq!(rig.photo(1).label, Some(ColorLabel::Green));
    // the Czech and Slovak layouts type `+` on the 1 key: it is Increase Grid Thumbnail Size's second key, which keeps it
    let (fire, aliased) = rig.pressed(vec![press("+", Key::Num1, Modifiers::NONE)]);
    assert!(fire.is_empty(), "{fire:?}");
    assert_eq!(aliased, [("view.thumbSizeUp", json!({}))]);
}

#[test]
fn only_the_digit_defaults_are_found_by_their_place() {
    // the 4 moved to F4: the `'` on a French keyboard is no rating any more
    let mut rig = Rig::new();
    rig.assign("photo.rate4", "F4");
    let (fire, aliased) = rig.pressed(vec![french(4)]);
    assert!(fire.is_empty() && aliased.is_empty(), "{fire:?} {aliased:?}");
    // and when another command has the 4, the rating has none for the `'` to stand in for
    let mut rig = Rig::new();
    rig.assign("view.detail", "4");
    let (fire, aliased) = rig.pressed(vec![french(4)]);
    assert!(fire.is_empty() && aliased.is_empty(), "{fire:?} {aliased:?}");
}

#[test]
fn a_rating_moved_onto_another_digit_is_not_found_by_that_digits_place() {
    // Rate ★★★ has the 4 now and Rate ★★★★ none: the `'` on the 4 key is nobody's, so it rates nothing
    let mut rig = Rig::new();
    assert_eq!(rig.assign("photo.rate3", "4"), ["photo.rate4"]);
    let (fire, aliased) = rig.pressed(vec![french(4)]);
    assert!(fire.is_empty() && aliased.is_empty(), "{fire:?} {aliased:?}");
}

#[test]
fn shift_types_the_digits_on_french_keyboards_and_advances() {
    let mut rig = Rig::new();
    rig.event(press("4", Key::Num4, Modifiers::SHIFT));
    assert_eq!((rig.photo(1).rating, rig.active()), (4, Some(PhotoId(2))));
}

#[test]
fn rating_and_label_keys_are_commands_with_the_digit_defaults() {
    for (id, label, digit) in COMMANDS {
        let b = super::find_bindable(id).unwrap_or_else(|| panic!("{id} is a command the shortcuts editor lists"));
        assert_eq!((b.label, b.default), (label, Some(digit)), "{id}");
        assert_eq!(super::shortcut_of(&super::Keymap::new(), id), Some(digit), "{id}");
    }
}

/// The ten ids are declared twice: as commands in `UI_COMMANDS` (label and digit, pinned to `COMMANDS` by the test above) and
/// as what they set in `CULLING_KEYS`. Each one sets the rating or label its editor label and digit name.
#[test]
fn each_command_sets_the_rating_or_label_its_label_and_digit_name() {
    assert_eq!(super::CULLING_KEYS.len(), COMMANDS.len());
    for (id, label, digit) in COMMANDS {
        let n: usize = digit.parse().unwrap();
        let (command, params, fired) = if n <= 5 {
            assert_eq!(label.matches('★').count(), n, "{id}: the digit is the stars");
            ("photo.rate", json!({"rating": n}), format!("rate:{n}:0"))
        } else {
            let color = label.trim_end_matches(" label").to_lowercase();
            ("photo.label", json!({"label": color}), format!("label:{color}"))
        };
        assert!(super::is_culling_command(id), "{id}");
        assert_eq!(super::culling_command(id), Some((command, params)), "{id}");
        assert_eq!(super::culling_fire(id), Some(fired), "{id}");
    }
    // the digits 6–9 are red, yellow, green, blue
    let labels: Vec<&str> = COMMANDS[6..].iter().map(|(_, label, _)| *label).collect();
    assert_eq!(labels, ["Red label", "Yellow label", "Green label", "Blue label"]);
    // the engine commands these run, and any other id, are no culling key
    for id in ["photo.rate", "photo.label", "photo.rate6", "photo.labelPurple", "photo.rate0 ", "view.detail", ""] {
        assert!(!super::is_culling_command(id), "{id:?}");
        assert_eq!((super::culling_command(id), super::culling_fire(id)), (None, None), "{id:?}");
    }
}

#[test]
fn a_punctuation_key_at_its_own_place_or_with_shift_is_no_plain_digit() {
    // the `'` key of a US keyboard is no 4
    let mut rig = Rig::new();
    let (fire, aliased) = rig.pressed(vec![press("'", Key::Quote, Modifiers::NONE)]);
    assert!(fire.is_empty() && aliased.is_empty(), "{fire:?} {aliased:?}");
    // Shift+1 types `!` on a US keyboard: the one key is Rate ★ and advance, not Rate ★ as well
    let (fire, aliased) = rig.pressed(vec![press("!", Key::Num1, Modifiers::SHIFT)]);
    assert_eq!(fire, ["rate:1:1"]);
    assert!(aliased.is_empty(), "{aliased:?}");
}

#[test]
fn a_key_given_to_a_rating_rates_and_the_digit_stops() {
    for sc in ["F3", "Alt+3"] {
        let mut rig = Rig::new();
        // a key that fired twice would go two photos on
        rig.app.ui.auto_advance = true;
        assert!(rig.assign("photo.rate3", sc).is_empty(), "{sc} is free");
        // Alt+3 is the third keyword's key too: the rating takes it from that alias
        let (_, aliased) = rig.pressed(vec![chord(sc)]);
        assert!(aliased.is_empty(), "{sc}: {aliased:?}");
        rig.event(chord(sc));
        assert_eq!((rig.photo(1).rating, rig.active()), (3, Some(PhotoId(2))), "{sc} rates once");
        // the 3 does nothing now
        rig.event(key_event(Key::Num3, Key::Num3, Modifiers::NONE));
        assert_eq!((rig.photo(2).rating, rig.active()), (0, Some(PhotoId(2))), "{sc}: the 3 no longer rates");
    }
}

#[test]
fn a_key_given_to_a_label_labels_and_the_digit_stops() {
    for sc in ["F4", "Alt+6"] {
        let mut rig = Rig::new();
        rig.app.ui.auto_advance = true;
        assert!(rig.assign("photo.labelRed", sc).is_empty(), "{sc} is free");
        let (_, aliased) = rig.pressed(vec![chord(sc)]);
        assert!(aliased.is_empty(), "{sc}: {aliased:?}");
        rig.event(chord(sc));
        assert_eq!((rig.photo(1).label, rig.active()), (Some(ColorLabel::Red), Some(PhotoId(2))), "{sc} labels once");
        rig.event(key_event(Key::Num6, Key::Num6, Modifiers::NONE));
        assert_eq!((rig.photo(2).label, rig.active()), (None, Some(PhotoId(2))), "{sc}: the 6 no longer labels");
    }
}

#[test]
fn reset_gives_the_rating_its_digit_back() {
    let mut rig = Rig::new();
    rig.assign("photo.rate3", "F3");
    super::reset(&mut rig.app.ui.settings.keymap, "photo.rate3").unwrap();
    assert!(rig.app.ui.settings.keymap.is_empty(), "declared keys aren't stored: {:?}", rig.app.ui.settings.keymap);
    rig.event(chord("F3"));
    assert_eq!(rig.photo(1).rating, 0);
    rig.event(chord("3"));
    assert_eq!(rig.photo(1).rating, 3);
}

#[test]
fn a_digit_given_to_another_command_is_taken_from_the_rating() {
    // Detail takes the 3: the rating loses it, and the editor says so ("Removed from Rate ★★★")
    let mut rig = Rig::new();
    assert_eq!(rig.assign("view.detail", "3"), ["photo.rate3"]);
    assert_eq!(super::shortcut_of(&rig.app.ui.settings.keymap, "photo.rate3"), None);
    rig.event(chord("3"));
    assert_eq!((rig.photo(1).rating, rig.app.ui.view), (0, ViewMode::Detail));
    // a rating given another rating's digit takes it from that one
    let mut rig = Rig::new();
    assert_eq!(rig.assign("photo.rate3", "4"), ["photo.rate4"]);
    rig.event(chord("4"));
    assert_eq!(rig.photo(1).rating, 3);
}

#[test]
fn the_shift_digit_keys_stay_fixed_keys_that_advance() {
    let mut rig = Rig::new();
    // the plain keys moved: the Shift ones are the fixed keys the shortcuts editor lists, and stay
    rig.assign("photo.rate3", "F3");
    rig.assign("photo.labelYellow", "F4");
    rig.event(chord("Shift+3"));
    assert_eq!((rig.photo(1).rating, rig.active()), (3, Some(PhotoId(2))));
    rig.event(chord("Shift+7"));
    assert_eq!((rig.photo(2).label, rig.active()), (Some(ColorLabel::Yellow), Some(PhotoId(3))));
    // until the user gives such a key to a command
    rig.assign("view.detail", "Shift+3");
    rig.event(chord("Shift+3"));
    assert_eq!((rig.photo(3).rating, rig.app.ui.view), (0, ViewMode::Detail));
}

#[test]
fn a_remapped_key_goes_through_the_culling_path() {
    // Compare: the key labels the active photo only, not the whole selection
    let mut rig = Rig::new();
    rig.assign("photo.labelRed", "F4");
    rig.app.ui.view = ViewMode::Compare;
    rig.app.session.execute("library.select", &json!({"ids": [1, 2], "active": 2})).unwrap();
    rig.event(chord("F4"));
    assert_eq!((rig.photo(1).label, rig.photo(2).label), (None, Some(ColorLabel::Red)));
    // the rating key rates the active photo of Compare the same way
    rig.assign("photo.rate5", "F5");
    rig.event(chord("F5"));
    assert_eq!((rig.photo(1).rating, rig.photo(2).rating), (0, 5));
}

#[test]
fn the_menus_rating_and_label_rows_show_the_keys_they_have() {
    let mut rig = Rig::new();
    assert_eq!(menu_keys(&rig.app, "photo.rate"), some(&["0", "1", "2", "3", "4", "5"]));
    assert_eq!(menu_keys(&rig.app, "photo.label"), [Some("6"), Some("7"), Some("8"), Some("9"), None].map(|k| k.map(str::to_string)));
    // a key given to one of them, or taken away, shows in its row (and in the native menu bar's)
    rig.assign("photo.rate3", "F3");
    rig.assign("photo.labelGreen", "Alt+8");
    super::assign(&mut rig.app.ui.settings.keymap, "photo.labelBlue", None).unwrap();
    assert_eq!(menu_keys(&rig.app, "photo.rate"), some(&["0", "1", "2", "F3", "4", "5"]));
    assert_eq!(menu_keys(&rig.app, "photo.label"), [Some("6"), Some("7"), Some("Alt+8"), None, None].map(|k| k.map(str::to_string)));
}

#[test]
fn the_commands_run_by_name_like_the_menu_rows() {
    let mut rig = Rig::new();
    rig.app.run("photo.rate3", json!({})).unwrap();
    rig.app.run("photo.labelBlue", json!({})).unwrap();
    assert_eq!((rig.photo(1).rating, rig.photo(1).label), (3, Some(ColorLabel::Blue)));
    // in Compare only the active photo, as the keys and the rows do
    rig.app.ui.view = ViewMode::Compare;
    rig.app.session.execute("library.select", &json!({"ids": [1, 2], "active": 2})).unwrap();
    rig.app.run("photo.rate5", json!({})).unwrap();
    assert_eq!((rig.photo(1).rating, rig.photo(2).rating), (3, 5));
}

#[test]
fn a_remapped_key_does_nothing_while_a_text_field_or_a_dialog_has_the_keyboard() {
    let mut rig = Rig::new();
    rig.assign("photo.rate3", "F3");
    let mut text = String::new();
    rig.frame_with(vec![], Some(&mut text));
    rig.frame_with(vec![chord("F3")], Some(&mut text));
    assert!(rig.ctx.egui_wants_keyboard_input());
    assert_eq!(rig.photo(1).rating, 0);
    let mut rig = Rig::new();
    rig.assign("photo.rate3", "F3");
    rig.app.ui.dialog = Some(Dialog::About);
    rig.event(chord("F3"));
    assert_eq!(rig.photo(1).rating, 0);
}

#[test]
fn keymaps_saved_by_older_builds_keep_working() {
    // an older build let Detail take the 3: its entry is all the keymap has, and the 3 stays Detail's
    let mut rig = Rig::new();
    rig.app.ui.settings.keymap.insert("view.detail".into(), "3".into());
    assert_eq!(super::shortcut_of(&rig.app.ui.settings.keymap, "photo.rate3"), None);
    rig.event(chord("3"));
    assert_eq!((rig.photo(1).rating, rig.app.ui.view), (0, ViewMode::Detail));
    // an entry that is no shortcut leaves the digit alone
    let mut rig = Rig::new();
    rig.app.ui.settings.keymap.insert("photo.rate4".into(), "Hyper+4".into());
    rig.event(chord("4"));
    assert_eq!(rig.photo(1).rating, 4);
}

#[test]
fn the_fixed_keys_list_the_shift_digits_once_each_and_no_digit_that_is_a_row() {
    let rows = crate::panels::keymap::fixed_rows();
    for n in 0..=9 {
        assert!(!rows.iter().any(|(sc, _)| *sc == n.to_string()), "the {n} key is a row of its own");
    }
    let shifted: Vec<&str> =
        (0..=9).map(|n| rows.iter().find(|(sc, _)| *sc == format!("Shift+{n}")).unwrap_or_else(|| panic!("Shift+{n} is listed")).1).collect();
    let mut once = shifted.clone();
    once.sort_unstable();
    once.dedup();
    assert_eq!(once.len(), 10, "every Shift+digit key has a label of its own: {shifted:?}");
}
