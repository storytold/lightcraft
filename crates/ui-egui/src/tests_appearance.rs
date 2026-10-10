//! Appearance Mode: Auto (follow the system), Dark or Light, each with its own saved theme.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::{AppSettings, AppearanceMode, DarkTheme, LightTheme, UiState};
use crate::theme::ThemeKind;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn app() -> LightcraftApp {
    LightcraftApp::new(lightcraft_engine::Session::new(), Services { png: None, ..Default::default() })
}

/// One frame of logic on `ctx`: where the theme is applied.
fn frame(app: &mut LightcraftApp, ctx: &egui::Context) {
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| app.logic(ui.ctx()));
    output.textures_delta.clear();
}

#[test]
fn new_users_keep_the_dark_look_and_old_settings_load_unchanged() {
    let s = AppSettings::default();
    assert_eq!((s.appearance_mode, s.dark_theme, s.light_theme), (AppearanceMode::Dark, DarkTheme::Charcoal, LightTheme::Silver));
    assert_eq!(s.theme(Some(egui::Theme::Light)), ThemeKind::Charcoal, "Auto is opt-in");
    // a ui.json saved before appearance modes existed
    let old: UiState = serde_json::from_str(r#"{"settings": {"gridBadges": "always", "confirmDelete": true}}"#).unwrap();
    assert_eq!(old.settings.appearance_mode, AppearanceMode::Dark);
    assert_eq!(old.settings.dark_theme, DarkTheme::Charcoal);
    assert_eq!(old.settings.light_theme, LightTheme::Silver);
    assert!(old.settings.confirm_delete);
    // and the Charcoal tokens are the ones LightCraft always had
    assert_eq!(crate::theme::Tokens::for_kind(ThemeKind::Charcoal).chrome, crate::theme::Tokens::default().chrome);
}

#[test]
fn choices_validate_and_survive_a_restart() {
    let mut a = app();
    let ctx = egui::Context::default();
    let set = |a: &mut LightcraftApp, p: serde_json::Value| {
        let (req, _) = crate::control::ControlRequest::new("ui.set", p);
        match crate::control::handle(a, &ctx, &req) {
            crate::control::Outcome::Done(reply) => reply,
            _ => panic!("expected a reply"),
        }
    };
    let ok = set(&mut a, json!({"settings": {"appearanceMode": "auto", "darkTheme": "midnight", "lightTheme": "paper"}}));
    assert_eq!(ok["ok"], true, "{ok}");
    assert_eq!(a.ui.settings.appearance_mode, AppearanceMode::Auto);
    for bad in [json!({"appearanceMode": "sepia"}), json!({"darkTheme": "paper"}), json!({"lightTheme": "midnight"}), json!({"appearanceMode": 3})] {
        let reply = set(&mut a, json!({ "settings": bad }));
        assert_eq!(reply["ok"], false, "{bad} accepted");
    }
    assert_eq!((a.ui.settings.dark_theme, a.ui.settings.light_theme), (DarkTheme::Midnight, LightTheme::Paper), "a rejected value changes nothing");
    let saved = serde_json::to_string(&a.ui).unwrap();
    assert!(saved.contains(r#""appearanceMode":"auto""#) && saved.contains(r#""darkTheme":"midnight""#) && saved.contains(r#""lightTheme":"paper""#));
    let restarted: UiState = serde_json::from_str(&saved).unwrap();
    assert_eq!(restarted.settings, a.ui.settings);
}

#[test]
fn the_button_cycles_auto_light_dark_without_losing_theme_choices() {
    let mut a = app();
    let ctx = egui::Context::default();
    a.services.system_theme = Some(Box::new(|_| Some(egui::Theme::Dark)));
    a.run("view.theme", json!({"theme": "paper"})).unwrap();
    a.run("view.theme", json!({"theme": "midnight"})).unwrap();
    assert_eq!(a.ui.settings.appearance_mode, AppearanceMode::Dark, "a dark theme fixes the mode to Dark");
    for (mode, shown) in
        [(AppearanceMode::Auto, ThemeKind::Midnight), (AppearanceMode::Light, ThemeKind::Paper), (AppearanceMode::Dark, ThemeKind::Midnight)]
    {
        let r = a.run("view.appearance", json!({})).unwrap();
        assert_eq!(a.ui.settings.appearance_mode, mode);
        assert_eq!(r["darkTheme"], "midnight");
        assert_eq!(r["lightTheme"], "paper");
        frame(&mut a, &ctx);
        assert_eq!(crate::theme::current(&ctx), Some(shown));
        assert_eq!(crate::theme::Tokens::get(&ctx).chrome, crate::theme::Tokens::for_kind(shown).chrome);
        let icon = match mode {
            AppearanceMode::Auto => crate::icons::Icon::Monitor,
            AppearanceMode::Light => crate::icons::Icon::Sun,
            AppearanceMode::Dark => crate::icons::Icon::Moon,
        };
        assert_eq!(crate::panels::topbar::appearance_icon(mode).0, icon);
    }
    // a light theme from the menu fixes Light
    a.run("view.theme", json!({"theme": "silver"})).unwrap();
    assert_eq!(
        (a.ui.settings.appearance_mode, a.ui.settings.light_theme, a.ui.settings.dark_theme),
        (AppearanceMode::Light, LightTheme::Silver, DarkTheme::Midnight)
    );
    // bad parameters are errors, never a panic, and change nothing
    for p in [json!({}), json!({"theme": 4}), json!({"theme": "neon"})] {
        assert!(a.run("view.theme", p.clone()).is_err(), "{p}");
    }
    for p in [json!({"mode": "sepia"}), json!({"mode": 1}), json!({"mode": []})] {
        assert!(a.run("view.appearance", p.clone()).is_err(), "{p}");
    }
    assert_eq!(a.ui.settings.appearance_mode, AppearanceMode::Light);
    a.run("view.appearance", json!({"mode": "auto"})).unwrap();
    assert_eq!(a.ui.settings.appearance_mode, AppearanceMode::Auto);
}

#[test]
fn auto_follows_the_system_and_falls_back_to_dark() {
    use std::sync::{Arc, Mutex};
    let system = Arc::new(Mutex::new(Some(egui::Theme::Light)));
    let mut a = app();
    let reported = Arc::clone(&system);
    a.services.system_theme = Some(Box::new(move |_| *reported.lock().unwrap()));
    a.run("view.appearance", json!({"mode": "auto"})).unwrap();
    let ctx = egui::Context::default();
    frame(&mut a, &ctx);
    assert_eq!(crate::theme::current(&ctx), Some(ThemeKind::Silver));
    assert_eq!(ctx.theme(), egui::Theme::Light);
    *system.lock().unwrap() = Some(egui::Theme::Dark);
    frame(&mut a, &ctx);
    assert_eq!(crate::theme::current(&ctx), Some(ThemeKind::Charcoal));
    assert_eq!(ctx.theme(), egui::Theme::Dark);
    // no answer from the host or from egui: dark
    *system.lock().unwrap() = None;
    frame(&mut a, &ctx);
    assert_eq!(crate::theme::current(&ctx), Some(ThemeKind::Charcoal));
    // without a host service, egui's report is used
    let mut plain = app();
    plain.ui.settings.appearance_mode = AppearanceMode::Auto;
    let ctx = egui::Context::default();
    let input = egui::RawInput { system_theme: Some(egui::Theme::Light), ..Default::default() };
    let mut output = ctx.run_ui(input, |ui| {
        assert_eq!(plain.system_theme(ui.ctx()), Some(egui::Theme::Light));
        plain.logic(ui.ctx());
    });
    output.textures_delta.clear();
    assert_eq!(crate::theme::current(&ctx), Some(ThemeKind::Silver));
}

#[test]
fn view_menu_lists_modes_and_themes_with_checks() {
    let mut a = app();
    a.run("view.theme", json!({"theme": "paper"})).unwrap();
    let menus = crate::menubar::menu_bar(&a);
    let text = serde_json::to_string(&menus).unwrap();
    for label in ["Appearance", "Sync with System", "Light Mode", "Dark Mode", "Next Appearance Mode", "Charcoal", "Midnight", "Silver", "Paper"] {
        assert!(text.contains(&format!("\"{label}\"")), "{label} missing from the menus");
    }
    assert!(text.contains(r#""params":{"theme":"paper"}"#));
}

#[test]
fn the_top_bar_button_and_settings_cards_drive_the_mode() {
    let mut h = Headless::new(app(), [1400.0, 900.0], 1.0);
    h.settle(Duration::from_secs(60));
    assert_eq!(h.request("ui.clickWidget", json!({"id": "icon:appearance"}), T)["ok"], true);
    h.step();
    assert_eq!(h.app.ui.settings.appearance_mode, AppearanceMode::Auto);
    assert_eq!(h.request("ui.clickWidget", json!({"id": "icon:appearance"}), T)["ok"], true);
    h.step();
    h.step();
    assert_eq!(h.app.ui.settings.appearance_mode, AppearanceMode::Light);
    assert_eq!(crate::theme::current(&h.view.ctx), Some(ThemeKind::Silver));
    // Settings ▸ Interface: the mode buttons and the theme radios
    h.app.ui.dialog = Some(crate::state::Dialog::Settings { tab: "interface".into() });
    h.step();
    h.step();
    assert_eq!(h.request("ui.clickWidget", json!({"id": "radio:theme-paper"}), T)["ok"], true);
    h.step();
    h.step();
    assert_eq!(h.app.ui.settings.light_theme, LightTheme::Paper);
    assert_eq!(crate::theme::current(&h.view.ctx), Some(ThemeKind::Paper));
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:settingsAppearance-1"}), T)["ok"], true);
    h.step();
    h.step();
    assert_eq!(h.app.ui.settings.appearance_mode, AppearanceMode::Dark);
    assert_eq!(crate::theme::current(&h.view.ctx), Some(ThemeKind::Charcoal));
}

#[test]
fn interface_settings_fit_small_windows_and_stack_theme_choices() {
    // Desktop windows have a 900×560 minimum; also stress a smaller headless viewport.
    for size in [[900.0, 560.0], [640.0, 480.0]] {
        let mut h = Headless::new(app(), size, 1.0);
        h.app.ui.dialog = Some(crate::state::Dialog::Settings { tab: "interface".into() });
        for _ in 0..4 {
            h.step();
        }
        let window = h.app.widgets.iter().find(|(id, _)| id == "dialog:window").unwrap().1;
        assert!(window.width() <= size[0], "{size:?}: {window:?}");
        assert!(window.height() <= size[1], "{size:?}: {window:?}");
        let silver = h.app.widgets.iter().find(|(id, _)| id == "radio:theme-silver").unwrap().1;
        let mode = h.app.widgets.iter().find(|(id, _)| id == "button:settingsAppearance-0").unwrap().1;
        assert!(silver.top() > mode.bottom() + 80.0, "theme choices must sit below their preview");
        assert!(silver.right() <= window.right(), "theme choices stay inside the modal");
    }
}
