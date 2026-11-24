//! Speed-edit keys, as in Capture One: hold a key and drag or scroll over the photo to move a
//! Develop slider (hold E and scroll for Exposure), without reaching for the panel.
//!
//! The key → control map is the user's (`app.setSpeedKey`, Settings ▸ Keyboard Shortcuts lists
//! it); [`DEFAULT_KEYS`] fills a fresh settings file. While Develop shows a photo, a mapped key's
//! press (with no modifier) is held back from the shortcuts: a scroll or a drag on the photo
//! while it is down moves the slider, and the whole gesture is one history step (it runs inside
//! `develop.beginInteraction` … `develop.endInteraction`, as a slider drag does). Let go without
//! adjusting anything and the press goes on to the key's own shortcut, so a tap still does what
//! it always did.

use std::collections::BTreeMap;

use egui::{Key, Modifiers, Rect, Sense};
use lcc_develop::{ControlSpec, controls};
use serde_json::{Value, json};

use crate::LccApp;
use crate::state::ViewMode;

/// The user's speed-edit keys: key name (`"E"`, as shortcuts name keys) → develop control id.
pub type SpeedKeys = BTreeMap<String, String>;

/// The keys a fresh settings file starts with (Light's tone sliders under memorable letters).
pub const DEFAULT_KEYS: &[(&str, &str)] = &[
    ("E", "light.exposure"),
    ("C", "light.contrast"),
    ("H", "light.highlights"),
    ("S", "light.shadows"),
    ("W", "light.whites"),
    ("B", "light.blacks"),
];

/// [`DEFAULT_KEYS`] as a map.
pub fn default_keys() -> SpeedKeys {
    DEFAULT_KEYS.iter().map(|(k, c)| ((*k).to_string(), (*c).to_string())).collect()
}

/// A speed-edit key is held.
#[derive(Clone, Debug, PartialEq)]
pub struct SpeedEdit {
    pub key: Key,
    pub control: String,
    /// The gesture opened a develop interaction (it moved the slider): one undo step, ended on release.
    pub began: bool,
    /// Pointer travel (points) and trackpad scroll not yet worth a whole step.
    remainder: f32,
}

/// The key a speed-edit entry names: a single key with no modifier, not a modifier itself.
fn key_of(name: &str) -> Option<Key> {
    // a lone letter may come in lower case (`"e"`)
    let name = if name.chars().count() == 1 { name.to_uppercase() } else { name.to_string() };
    let (m, k) = crate::shortcuts::parse(&name)?;
    (m == Modifiers::NONE && !crate::shortcuts::is_modifier(k)).then_some(k)
}

/// The control `key` adjusts, if the user's map gives it one that exists.
fn control_for(map: &SpeedKeys, key: Key) -> Option<&'static ControlSpec> {
    map.iter().find(|(k, _)| key_of(k) == Some(key)).and_then(|(_, c)| controls::find(c))
}

/// Speed edit works here: Develop's loupe with a photo, nothing modal open.
fn available(app: &LccApp) -> bool {
    app.ui.module == crate::module::ModuleId::Develop
        && app.ui.view == ViewMode::Detail
        && app.session.active().is_some()
        && app.ui.dialog.is_none()
        && app.ui.slideshow.is_none()
}

/// Runs in `shortcuts::handle` before the keys are matched (and only when no text field or modal
/// has the keyboard): holds back a mapped key's presses, starts and ends the hold, and lets a
/// tap's press through to the shortcuts on release.
pub(crate) fn intercept(app: &mut LccApp, ctx: &egui::Context) {
    let on = available(app);
    // keys already down while speed edit wasn't available (the E that brought Develop up) are
    // their shortcuts' presses, not holds, until they are let go
    let stale_id = egui::Id::new("speed-edit-stale-keys");
    let down: Vec<Key> = ctx.input(|i| i.keys_down.iter().copied().collect());
    let mut stale: Vec<Key> = ctx.data(|d| d.get_temp::<Vec<Key>>(stale_id)).unwrap_or_default();
    // the frame before had no speed edit (a key handled earlier in this frame may have opened
    // Develop): this frame's keys belong to what was up before
    let prev_id = egui::Id::new("speed-edit-was-on");
    let was_on = ctx.data(|d| d.get_temp::<bool>(prev_id)).unwrap_or(false);
    ctx.data_mut(|d| d.insert_temp(prev_id, on));
    if on && was_on {
        stale.retain(|k| down.contains(k));
    } else {
        stale = down.clone();
        // a press arriving this frame is down too
        ctx.input(|i| {
            for e in &i.events {
                if let egui::Event::Key { key, pressed: true, .. } = e
                    && !stale.contains(key)
                {
                    stale.push(*key);
                }
            }
        });
    }
    ctx.data_mut(|d| d.insert_temp(stale_id, stale.clone()));
    if !on {
        finish(app, ctx, false);
        return;
    }
    let map = app.ui.settings.speed_keys.clone();
    let held = app.speed_edit.as_ref().map(|s| s.key);
    let mut start: Option<Key> = None;
    let mut released = false;
    ctx.input_mut(|i| {
        i.events.retain(|e| match e {
            egui::Event::Key { key, pressed: true, modifiers, repeat, .. } if *modifiers == Modifiers::NONE && control_for(&map, *key).is_some() => {
                if !repeat && held.is_none() && start.is_none() && !stale.contains(key) {
                    start = Some(*key);
                }
                false
            }
            egui::Event::Key { key, pressed: false, .. } if Some(*key) == held => {
                released = true;
                true
            }
            _ => true,
        });
    });
    // a key-up the window never saw (focus went elsewhere) ends it too
    let gone = held.is_some_and(|k| !ctx.input(|i| i.key_down(k)));
    if released || gone {
        finish(app, ctx, true);
    }
    if let Some(key) = start
        && app.speed_edit.is_none()
        && let Some(spec) = control_for(&map, key)
    {
        app.speed_edit = Some(SpeedEdit { key, control: spec.id.to_string(), began: false, remainder: 0.0 });
    }
}

/// Ends the hold: closes the gesture's history step, or (a tap that adjusted nothing, `replay`)
/// hands the press on to the key's shortcut.
fn finish(app: &mut LccApp, ctx: &egui::Context, replay: bool) {
    let Some(s) = app.speed_edit.take() else { return };
    if s.began {
        let _ = app.run("develop.endInteraction", json!({}));
    } else if replay {
        ctx.input_mut(|i| {
            i.events.push(egui::Event::Key { key: s.key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE });
        });
    }
}

/// Moves the held key's slider by `steps` nudges (as the panel's arrow keys and wheel do),
/// opening the gesture's interaction on the first move.
fn adjust(app: &mut LccApp, steps: f64) {
    if steps == 0.0 || !steps.is_finite() || app.ui.dragging_control.is_some() {
        return;
    }
    let Some(s) = app.speed_edit.as_ref() else { return };
    let Some(spec) = controls::find(&s.control) else { return };
    let Some(id) = app.session.active() else { return };
    let current = app.session.develop_of(id).and_then(|d| controls::get(&d, spec.id)).unwrap_or(spec.default);
    let value = crate::widgets::nudged(spec, current, steps);
    if (value - current).abs() < 1e-12 {
        return;
    }
    if !s.began {
        app.end_midi_interaction();
        let _ = app.run("develop.beginInteraction", json!({"label": spec.label}));
        if let Some(s) = app.speed_edit.as_mut() {
            s.began = true;
        }
    }
    let _ = app.run("develop.set", json!({"control": spec.id, "value": value}));
}

/// Pointer travel (points) per nudge of a drag.
const DRAG_POINTS_PER_STEP: f32 = 2.0;

/// The wheel over the photo while a key is held: up / right moves the slider up, one nudge per
/// notch (or per [`crate::theme::WHEEL_LINE_POINTS`] of trackpad scroll). True when it took the
/// scroll, so the loupe neither zooms nor pans.
pub(crate) fn wheel(app: &mut LccApp, ui: &egui::Ui) -> bool {
    if app.speed_edit.is_none() {
        return false;
    }
    let (notches, points) = ui.input(|i| {
        let mut notches = 0.0_f32;
        let mut points = 0.0_f32;
        for e in &i.events {
            if let egui::Event::MouseWheel { unit, delta, .. } = e
                && delta.is_finite()
            {
                // a sideways scroll counts as well (a trackpad's horizontal swipe)
                let d = if delta.y != 0.0 { delta.y } else { delta.x };
                match unit {
                    egui::MouseWheelUnit::Point => points += d,
                    egui::MouseWheelUnit::Line | egui::MouseWheelUnit::Page => notches += d,
                }
            }
        }
        (notches, points)
    });
    ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
    let line = crate::theme::WHEEL_LINE_POINTS;
    let mut steps = f64::from(notches.clamp(-50.0, 50.0).trunc());
    if let Some(s) = app.speed_edit.as_mut() {
        let total = (s.remainder + points).clamp(-10.0 * line, 10.0 * line);
        let whole = (total / line).trunc();
        s.remainder = total - whole * line;
        steps += f64::from(whole);
    }
    adjust(app, steps);
    true
}

/// While a key is held, the photo takes a drag for the slider (sideways: right raises it), above
/// the loupe and the tool overlays, so neither pans nor paints.
pub(crate) fn interact(app: &mut LccApp, ui: &mut egui::Ui, canvas: Rect) {
    if app.speed_edit.is_none() {
        return;
    }
    let resp = ui.interact(canvas, egui::Id::new("loupe-speed-edit"), Sense::drag());
    crate::widgets::register(ui.ctx(), "loupe:speedEdit", canvas);
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    let dx = resp.drag_delta().x;
    if !resp.dragged() || !dx.is_finite() || dx == 0.0 {
        return;
    }
    let mut steps = 0.0;
    if let Some(s) = app.speed_edit.as_mut() {
        let total = (s.remainder + dx).clamp(-1000.0, 1000.0);
        let whole = (total / DRAG_POINTS_PER_STEP).trunc();
        s.remainder = total - whole * DRAG_POINTS_PER_STEP;
        steps = f64::from(whole);
    }
    adjust(app, steps);
}

/// `app.setSpeedKey {key, control?, reset?}`: gives `key` (one key, no modifier) to a develop
/// slider; `control` null or `""` takes it away; `reset: true` restores the default keys.
pub fn set_speed_key(app: &mut LccApp, p: &Value) -> Result<Value, String> {
    if p.get("reset").and_then(Value::as_bool).unwrap_or(false) {
        app.ui.settings.speed_keys = default_keys();
        return Ok(list(app));
    }
    let name = p.get("key").and_then(Value::as_str).ok_or("missing key")?;
    let key = key_of(name).ok_or_else(|| format!("{name:?} is not a single key without modifiers"))?;
    let canonical = crate::shortcuts::format(Modifiers::NONE, key).ok_or_else(|| format!("{name:?} can't be a speed-edit key"))?;
    let control = match p.get("control") {
        None | Some(Value::Null) => None,
        Some(Value::String(c)) if c.is_empty() => None,
        Some(Value::String(c)) => Some(controls::find(c).ok_or_else(|| format!("unknown develop control: {c}"))?.id),
        Some(_) => return Err("control must be a string or null".into()),
    };
    let map = &mut app.ui.settings.speed_keys;
    // one entry per key, however it was spelled
    map.retain(|k, _| key_of(k) != Some(key));
    if let Some(id) = control {
        map.insert(canonical, id.to_string());
    }
    Ok(list(app))
}

/// `app.speedKeys`: the speed-edit keys in effect, `[{key, control, label}]`.
pub fn list(app: &LccApp) -> Value {
    json!(rows(&app.ui.settings.speed_keys).iter().map(|(k, c, l)| json!({"key": k, "control": c, "label": l})).collect::<Vec<_>>())
}

/// The usable entries of `map`: (key, control id, the control's label untranslated).
pub(crate) fn rows(map: &SpeedKeys) -> Vec<(String, &'static str, &'static str)> {
    map.iter().filter(|(k, _)| key_of(k).is_some()).filter_map(|(k, c)| controls::find(c).map(|s| (k.clone(), s.id, s.label))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_keys_and_controls_are_rejected_not_stored() {
        let mut app = LccApp::new(lcc_engine::Session::with_demo(), crate::Services::default());
        let before = app.ui.settings.speed_keys.clone();
        for p in [
            json!({}),
            json!({"key": 5}),
            json!({"key": ""}),
            json!({"key": "Cmd+E"}),
            json!({"key": "Shift"}),
            json!({"key": "E", "control": "no.such"}),
            json!({"key": "E", "control": 7}),
            json!({"key": "\u{0}\u{ffff}", "control": "light.exposure"}),
        ] {
            assert!(set_speed_key(&mut app, &p).is_err(), "{p}");
        }
        assert_eq!(app.ui.settings.speed_keys, before);
        // a settings file naming nonsense is skipped, never a crash
        let mut map = SpeedKeys::new();
        map.insert("Ctrl+Q".into(), "light.exposure".into());
        map.insert("Q".into(), "bogus".into());
        assert!(rows(&map).is_empty());
        assert!(control_for(&map, Key::Q).is_none());
    }

    #[test]
    fn a_key_is_reassigned_and_removed() {
        let mut app = LccApp::new(lcc_engine::Session::with_demo(), crate::Services::default());
        set_speed_key(&mut app, &json!({"key": "e", "control": "wb.temp"})).unwrap();
        assert_eq!(control_for(&app.ui.settings.speed_keys, Key::E).map(|s| s.id), Some("wb.temp"));
        assert_eq!(app.ui.settings.speed_keys.keys().filter(|k| key_of(k) == Some(Key::E)).count(), 1);
        set_speed_key(&mut app, &json!({"key": "E", "control": null})).unwrap();
        assert!(control_for(&app.ui.settings.speed_keys, Key::E).is_none());
        set_speed_key(&mut app, &json!({"reset": true})).unwrap();
        assert_eq!(app.ui.settings.speed_keys, default_keys());
    }
}
