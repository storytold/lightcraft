//! Actual installed settings-group callers. No engine/model/outer-dialog adapter is replaced.
use crate::LightcraftApp;
use crate::headless::HeadlessView;
use crate::state::Dialog;
use egui::{Event, Key, Modifiers, PointerButton, Rect};
use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use lightcraft_develop::SettingsGroup;
use serde_json::{Value, json};

struct Checklist {
    app: LightcraftApp,
    ctx: egui::Context,
    tick: u32,
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, crate::widgets::WidgetHit)>,
    ids: [PhotoId; 2],
    blocked: bool,
    measuring: bool,
    recorded: Option<std::sync::Arc<craft::ui::runtime::inspect::Recorded>>,
    original_locale: crate::i18n::Locale,
}
impl Checklist {
    fn new() -> Self {
        let original_locale = crate::i18n::language();
        crate::i18n::set_language(crate::i18n::Locale::En);
        let mut session = lightcraft_engine::Session::new();
        let ids = [session.catalog.alloc_photo_id(), session.catalog.alloc_photo_id()];
        for (id, name) in [(ids[0], "Group source"), (ids[1], "Group target")] {
            let photo = Photo::new(id, Source::Demo { scene: 0 }, name, "JPEG", 6000, 4000, "2026-01-14T05:58:48");
            session.commit("fixture", Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        }
        session.execute("library.select", &json!({"ids":[ids[0]]})).unwrap();
        session.execute("develop.set", &json!({"values":{"light.exposure":1.0,"color.vibrance":30.0}})).unwrap();
        session.execute("develop.copy", &json!({"groups":["light","color"]})).unwrap();
        session.execute("library.select", &json!({"ids":[ids[1]]})).unwrap();
        let app = LightcraftApp::new(session, crate::Services { local_places: Some(vec![]), png: None, ..Default::default() });
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        Self { app, ctx, tick: 0, rects: vec![], hits: vec![], ids, blocked: false, measuring: false, recorded: None, original_locale }
    }
    fn state(&self) -> Value {
        let s = &self.app.session;
        json!({"catalog":s.catalog,"undo":format!("{:?}",s.undo),"redo":format!("{:?}",s.redo),
            "commits":s.commits(),"journal":s.journal,"selected":s.selected(),"selection":format!("{:?}",s.selection),
            "copyGroups":s.copy_groups,"clipboard":s.clipboard,"presets":s.presets.iter().cloned().collect::<Vec<_>>(),
            "settings":self.app.ui.settings,"importDefaults":s.import_defaults,"xmp":s.xmp,"lastExport":s.last_export,
            "print":s.print,"printTemplates":s.print_templates})
    }
    fn safety(&self) {
        let s = &self.app.session;
        assert_eq!(s.catalog.len(), 2);
        assert!(s.catalog.photos().all(|p| matches!(p.source, Source::Demo { .. })));
        assert!(s.library.is_none() && s.library_folder.is_none());
        assert!(s.smart_previews_dir.is_none() && s.media.rendered.disk().is_none());
        assert!(self.app.services.write.is_none() && self.app.services.write_shared.is_none());
        assert!(self.app.services.pick_folder.is_none() && self.app.services.pick_files.is_none() && self.app.services.picker.is_none());
        assert!(self.app.services.png.is_none());
        assert!(self.app.pending_picks.is_empty());
        assert!(self.app.import.is_none() && self.app.export.is_none() && self.app.scan.is_none());
        assert_eq!(self.app.renderer.in_flight(), 0);
    }
    fn frame(&mut self, events: Vec<Event>) {
        self.safety();
        crate::widgets::take_registries(&self.ctx);
        self.tick += 1;
        let raw = HeadlessView::raw_input(egui::vec2(1400.0, 1100.0), 1.0, f64::from(self.tick) / 60.0, events);
        let blocked = self.blocked;
        let measuring = self.measuring;
        let app = &mut self.app;
        let ctx = self.ctx.clone();
        let mut last = None;
        let mut output = ctx.run_ui(raw, |root| {
            // The installed dialog dispatch, window, body, confirmation and cancellation paths.
            // No AppShell grid/renderer pass and no leaf-only substitute.
            let (_, recorded) = craft::ui::runtime::inspect::record(root, egui::Id::unique("settings-group-checklist-owners"), |_| {
                if measuring {
                    craft::ui::runtime::measure::run(&ctx, || crate::dialogs::show(app, &ctx, blocked));
                } else {
                    crate::dialogs::show(app, &ctx, blocked);
                }
            });
            last = Some((crate::widgets::take_registries(&ctx), recorded));
        });
        let ((rects, hits), recorded) = last.unwrap();
        self.recorded = recorded;
        self.rects = rects;
        self.hits = hits;
        output.textures_delta.clear();
        output.platform_output.commands.clear();
        output.viewport_output.clear();
        output.shapes.clear();
        self.safety();
    }
    fn idle(&mut self) {
        for _ in 0..3 {
            self.frame(vec![]);
        }
    }
    fn open(&mut self, command: &str) {
        assert!(self.app.ui.dialog.is_none());
        self.app.run(command, json!({})).unwrap();
        self.idle();
    }
    fn groups(&self) -> Vec<String> {
        match self.app.ui.dialog.as_ref().unwrap() {
            Dialog::CopySettings { groups } | Dialog::PasteSettings { groups } | Dialog::CreatePreset { groups, .. } => groups.clone(),
            other => panic!("wrong installed caller: {other:?}"),
        }
    }
    fn rect(&self, key: &str) -> Rect {
        let matches: Vec<_> = self.rects.iter().filter(|(name, _)| name == key).collect();
        assert_eq!(matches.len(), 1, "one final-pass actual target for {key}");
        let rect = matches[0].1;
        assert!(rect.is_positive() && self.ctx.content_rect().contains_rect(rect), "actual visible target: {key}: {rect:?}");
        rect
    }
    fn click(&mut self, key: &str) {
        let pos = self.rect(key).center();
        self.frame(vec![Event::PointerMoved(pos)]);
        self.frame(vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }]);
        self.frame(vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }]);
        self.idle();
    }
    fn key(&mut self, key: Key) {
        self.key_with(key, Modifiers::NONE);
    }
    fn key_with(&mut self, key: Key, modifiers: Modifiers) {
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers }]);
        }
        self.idle();
    }
    fn focused(&self, key: &str) -> bool {
        let hit = self.hits.iter().find(|(name, _)| name == key).unwrap().1;
        self.ctx.memory(|m| m.focused()) == Some(hit.id)
    }
    fn tab_to(&mut self, key: &str) {
        for _ in 0..64 {
            if self.focused(key) {
                return;
            }
            self.key(Key::Tab);
            assert!(self.app.ui.dialog.is_some(), "natural Tab must not confirm");
        }
        panic!("natural Tab never reached {key}; actual focused {:?}", self.ctx.memory(|m| m.focused()));
    }
    fn button(&mut self, pos: egui::Pos2, pressed: bool) {
        self.frame(vec![Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }]);
    }
    fn select_source(&mut self) {
        self.app.run("library.select", json!({"ids":[self.ids[0]]})).unwrap();
    }
}
impl Drop for Checklist {
    fn drop(&mut self) {
        crate::i18n::set_language(self.original_locale);
    }
}
fn all_groups() -> Vec<String> {
    SettingsGroup::ALL.iter().map(|g| serde_json::to_value(g).unwrap().as_str().unwrap().to_owned()).collect()
}

fn membership(command: &str, tag: &str, cancel: &str) {
    let mut f = Checklist::new();
    let before = f.state();
    f.open(command);
    f.click(&format!("{tag}:none"));
    assert!(f.groups().is_empty());
    let mut expected = vec![];
    for group in all_groups() {
        f.click(&format!("{tag}:{group}"));
        expected.push(group);
        assert_eq!(f.groups(), expected, "actual individual membership and insertion order: {command}");
        assert_eq!(f.state(), before, "all twenty draft controls are document-passive");
    }
    f.click(&format!("{tag}:all"));
    assert_eq!(f.groups(), all_groups(), "All is canonical and never duplicates");
    f.click(&format!("{tag}:none"));
    assert!(f.groups().is_empty());
    for group in ["light", "color", "light", "light"] {
        f.click(&format!("{tag}:{group}"));
    }
    assert_eq!(f.groups(), ["color", "light"], "unchecked removal and checked append retain native order");
    assert_eq!(f.state(), before);
    if cancel == "Escape" {
        f.key(Key::Escape);
    } else {
        f.click("button:dialogCancel");
    }
    assert!(f.app.ui.dialog.is_none(), "actual parent cancelled: {command}");
    assert_eq!(f.state(), before, "complete catalog/history/clipboard/presets remain unchanged after Cancel");
}
#[test]
fn settings_groups_copy_parent_all_twenty_members_all_none_and_real_cancel_preserve_state() {
    membership("dialog.copySettings", "copyGroup", "Cancel");
}
#[test]
fn settings_groups_paste_parent_all_twenty_members_all_none_and_escape_preserve_state() {
    membership("dialog.pasteSettings", "pasteGroup", "Escape");
}
#[test]
fn settings_groups_preset_parent_all_twenty_members_all_none_and_escape_preserve_state() {
    membership("dialog.createPreset", "presetInclude", "Escape");
}
#[test]
fn settings_groups_copy_confirm_equals_exact_native_command_and_stored_copy_groups() {
    let mut f = Checklist::new();
    let mut expected = Checklist::new();
    f.select_source();
    expected.select_source();
    let before = f.state();
    f.open("dialog.copySettings");
    f.click("copyGroup:none");
    f.click("copyGroup:color");
    assert_eq!(f.groups(), ["color"]);
    assert_eq!(f.state(), before);
    f.click("button:dialogOk");
    expected.app.run("develop.copy", json!({"groups":["color"]})).unwrap();
    assert!(f.app.ui.dialog.is_none());
    assert_eq!(f.state(), expected.state(), "exact copy command, clipboard, remembered groups and full document/history");
    assert_ne!(f.state(), before, "not a vacuous no-op");
}
#[test]
fn settings_groups_selective_paste_confirm_and_undo_redo_equal_exact_native_command() {
    let mut f = Checklist::new();
    let mut expected = Checklist::new();
    let before = f.state();
    f.open("dialog.pasteSettings");
    f.click("pasteGroup:none");
    f.click("pasteGroup:light");
    assert_eq!(f.groups(), ["light"]);
    assert_eq!(f.state(), before);
    f.click("button:dialogOk");
    expected.app.run("develop.paste", json!({"groups":["light"]})).unwrap();
    assert!(f.app.ui.dialog.is_none());
    assert_eq!(f.state(), expected.state(), "one real command batch, complete two-photo catalog and exact history");
    assert_ne!(f.state(), before);
    for command in ["edit.undo", "edit.redo"] {
        f.app.run(command, json!({})).unwrap();
        expected.app.run(command, json!({})).unwrap();
        assert_eq!(f.state(), expected.state(), "exact native {command}, not a shortened state oracle");
    }
}
#[test]
fn settings_groups_preset_confirm_keeps_native_default_name_identity_content_and_history_contract() {
    let mut f = Checklist::new();
    let mut expected = Checklist::new();
    f.select_source();
    expected.select_source();
    let before = f.state();
    f.open("dialog.createPreset");
    f.click("presetInclude:none");
    f.click("presetInclude:color");
    assert_eq!(f.groups(), ["color"]);
    assert_eq!(f.state(), before);
    f.click("button:dialogOk");
    expected.app.run("preset.create", json!({"name":"My Preset","group":"User Presets","groups":["color"]})).unwrap();
    assert!(f.app.ui.dialog.is_none());
    assert_eq!(f.state(), expected.state(), "native deterministic preset identity/defaults/content and non-undoable creation contract");
    assert_ne!(f.state(), before);
    assert_eq!(f.state()["undo"], before["undo"]);
    assert_eq!(f.state()["redo"], before["redo"]);
}
fn keyboard(command: &str, tag: &str) {
    let mut f = Checklist::new();
    let before = f.state();
    f.open(command);
    let original = f.groups();
    f.tab_to(&format!("{tag}:light"));
    assert_eq!(f.groups(), original, "natural navigation never changes membership");
    f.key(Key::Space);
    let mut toggled = original.clone();
    if toggled.iter().any(|g| g == "light") {
        toggled.retain(|g| g != "light");
    } else {
        toggled.push("light".into());
    }
    assert_eq!(f.groups(), toggled, "one physical Space activation");
    assert!(f.focused(&format!("{tag}:light")));
    f.key(Key::Enter);
    if toggled.iter().any(|g| g == "light") {
        toggled.retain(|g| g != "light");
    } else {
        toggled.push("light".into());
    }
    assert_eq!(f.groups(), toggled, "Enter belongs to the focused checkbox, not parent confirmation");
    assert_eq!(f.state(), before);
    f.key_with(Key::Tab, Modifiers { shift: true, ..Modifiers::NONE });
    assert!(!f.focused(&format!("{tag}:light")), "Shift+Tab leaves the checkbox");
    f.key(Key::Tab);
    assert!(f.focused(&format!("{tag}:light")), "forward Tab naturally returns through the same predecessor");
    f.key(Key::Escape);
    assert!(f.app.ui.dialog.is_none());
    assert_eq!(f.state(), before);
}
#[test]
fn settings_groups_copy_natural_tab_space_enter_shift_tab_and_escape_are_draft_only() {
    keyboard("dialog.copySettings", "copyGroup");
}
#[test]
fn settings_groups_paste_natural_tab_space_enter_shift_tab_and_escape_are_draft_only() {
    keyboard("dialog.pasteSettings", "pasteGroup");
}
#[test]
fn settings_groups_preset_natural_tab_space_enter_shift_tab_and_escape_are_draft_only() {
    keyboard("dialog.createPreset", "presetInclude");
}
fn blocked(command: &str, tag: &str) {
    let mut f = Checklist::new();
    f.open(command);
    let before = f.state();
    let draft = f.app.ui.dialog.clone();
    let at = f.rect(&format!("{tag}:light")).center();
    f.blocked = true;
    f.idle();
    f.frame(vec![Event::PointerMoved(at)]);
    f.button(at, true);
    assert_eq!(f.app.ui.dialog, draft);
    f.key(Key::Escape);
    assert_eq!(f.app.ui.dialog, draft, "covering prompt owns Escape");
    f.blocked = false;
    f.idle();
    f.button(at, false);
    f.idle();
    assert_eq!(f.app.ui.dialog, draft, "a disabled press cannot become a restored release activation");
    assert_eq!(f.state(), before);
    let original = f.groups();
    f.click(&format!("{tag}:light"));
    assert_ne!(f.groups(), original, "fresh physical positive after the covering prompt leaves");
    assert_eq!(f.state(), before);
}
#[test]
fn settings_groups_copy_blocked_parent_old_release_is_inert_then_fresh_gesture_works() {
    blocked("dialog.copySettings", "copyGroup");
}
#[test]
fn settings_groups_paste_blocked_parent_old_release_is_inert_then_fresh_gesture_works() {
    blocked("dialog.pasteSettings", "pasteGroup");
}
#[test]
fn settings_groups_preset_blocked_parent_old_release_is_inert_then_fresh_gesture_works() {
    blocked("dialog.createPreset", "presetInclude");
}
fn retarget(old_command: &str, old_tag: &str, new_command: &str, new_tag: &str) {
    let mut f = Checklist::new();
    let before = f.state();
    f.open(old_command);
    let at = f.rect(&format!("{old_tag}:light")).center();
    f.frame(vec![Event::PointerMoved(at)]);
    f.button(at, true);
    f.app.run(new_command, json!({})).unwrap();
    let new_draft = f.app.ui.dialog.clone();
    f.idle();
    assert!(f.rects.iter().all(|(name, _)| !name.starts_with(&format!("{old_tag}:"))), "old caller no longer publishes any checklist alias");
    f.button(at, false);
    f.idle();
    assert_eq!(f.app.ui.dialog, new_draft, "old caller's release never changes the replacement caller");
    assert_eq!(f.state(), before);
    let original = f.groups();
    f.click(&format!("{new_tag}:light"));
    assert_ne!(f.groups(), original, "fresh replacement caller uses its own genuine hit");
    assert_eq!(f.state(), before);
}
#[test]
fn settings_groups_copy_to_paste_owner_handoff_does_not_retarget_held_release() {
    retarget("dialog.copySettings", "copyGroup", "dialog.pasteSettings", "pasteGroup");
}
#[test]
fn settings_groups_paste_to_preset_owner_handoff_does_not_retarget_held_release() {
    retarget("dialog.pasteSettings", "pasteGroup", "dialog.createPreset", "presetInclude");
}
#[test]
fn settings_groups_preset_to_copy_owner_handoff_does_not_retarget_held_release() {
    retarget("dialog.createPreset", "presetInclude", "dialog.copySettings", "copyGroup");
}

fn measurement(command: &str, tag: &str) {
    let mut f = Checklist::new();
    f.open(command);
    let before = f.state();
    let draft = f.app.ui.dialog.clone();
    let at = f.rect(&format!("{tag}:light")).center();
    f.frame(vec![Event::PointerMoved(at)]);
    f.measuring = true;
    f.button(at, true);
    f.button(at, false);
    assert_eq!(f.app.ui.dialog, draft, "a speculative actual-parent measurement must not change its draft");
    assert_eq!(f.state(), before);
    f.measuring = false;
    f.idle();
    f.button(at, false);
    f.idle();
    assert_eq!(f.app.ui.dialog, draft, "a speculative gesture cannot leave a deferred release activation");
    f.click(&format!("{tag}:light"));
    assert_ne!(f.app.ui.dialog, draft, "fresh real positive after measurement");
    assert_eq!(f.state(), before);
}
#[test]
fn settings_groups_copy_actual_parent_measurement_and_old_release_are_pure_then_fresh_works() {
    measurement("dialog.copySettings", "copyGroup");
}
#[test]
fn settings_groups_paste_actual_parent_measurement_and_old_release_are_pure_then_fresh_works() {
    measurement("dialog.pasteSettings", "pasteGroup");
}
#[test]
fn settings_groups_preset_actual_parent_measurement_and_old_release_are_pure_then_fresh_works() {
    measurement("dialog.createPreset", "presetInclude");
}

fn sdk_owners(command: &str, tag: &str) {
    for locale in [crate::i18n::Locale::En, crate::i18n::Locale::De] {
        let mut f = Checklist::new();
        crate::i18n::set_language(locale);
        f.open(command);
        let before = f.state();
        for (key, label, component, role) in SettingsGroup::ALL
            .iter()
            .map(|g| {
                let key = serde_json::to_value(g).unwrap().as_str().unwrap().to_owned();
                (key, g.label(), "Checkbox", "CheckBox")
            })
            .chain([("all".into(), "All", "Button", "Button"), ("none".into(), "None", "Button", "Button")])
        {
            let alias = format!("{tag}:{key}");
            let hits: Vec<_> = f.hits.iter().filter(|(name, _)| name == &alias).collect();
            assert_eq!(hits.len(), 1, "one genuine response for {alias}");
            let hit = hits[0].1;
            let recorded = f.recorded.as_ref().unwrap();
            let elements: Vec<_> = recorded.elements.iter().filter(|e| e.id == hit.id && e.part.is_none()).collect();
            assert_eq!(elements.len(), 1, "the alias is the actual public component, not a hover probe: {alias}");
            let element = elements[0];
            assert_eq!(element.component, component);
            let facts = element.facts.as_ref().unwrap();
            assert_eq!(facts.role, role);
            assert_eq!(facts.name.as_deref(), Some(crate::i18n::tr(label)));
            assert!(facts.interactive && !facts.disabled && element.stop.is_some());
            let target = f.rect(&alias).center();
            assert!(hit.interact_rect.contains(target));
            assert!(recorded.hits.iter().any(|h| h.owner == hit.id && h.part.is_none() && h.rect.contains(target)));
            assert_eq!(f.ctx.layer_id_at(target), Some(hit.layer_id), "the real control owns the delivered pointer layer");
        }
        assert_eq!(f.state(), before, "passive metadata never applies a command");
    }
}
#[test]
fn settings_groups_copy_public_checkbox_and_buttons_have_genuine_localized_identity_and_focus_owners() {
    sdk_owners("dialog.copySettings", "copyGroup");
}
#[test]
fn settings_groups_paste_public_checkbox_and_buttons_have_genuine_localized_identity_and_focus_owners() {
    sdk_owners("dialog.pasteSettings", "pasteGroup");
}
#[test]
fn settings_groups_preset_public_checkbox_and_buttons_have_genuine_localized_identity_and_focus_owners() {
    sdk_owners("dialog.createPreset", "presetInclude");
}

fn held_measurement(command: &str, tag: &str) {
    let mut f = Checklist::new();
    f.open(command);
    let before = f.state();
    let draft = f.app.ui.dialog.clone();
    let at = f.rect(&format!("{tag}:light")).center();
    f.frame(vec![Event::PointerMoved(at)]);
    f.measuring = true;
    f.button(at, true);
    assert_eq!(f.app.ui.dialog, draft);
    f.measuring = false;
    f.idle();
    f.button(at, false);
    f.idle();
    assert_eq!(f.app.ui.dialog, draft, "the first real release after a held speculative press remains inert");
    assert_eq!(f.state(), before);
    f.click(&format!("{tag}:light"));
    assert_ne!(f.app.ui.dialog, draft, "fresh real gesture after speculative owner withdrawal");
    assert_eq!(f.state(), before);
}
#[test]
fn settings_groups_copy_held_measuring_press_first_restored_release_is_inert_then_fresh_works() {
    held_measurement("dialog.copySettings", "copyGroup");
}
#[test]
fn settings_groups_paste_held_measuring_press_first_restored_release_is_inert_then_fresh_works() {
    held_measurement("dialog.pasteSettings", "pasteGroup");
}
#[test]
fn settings_groups_preset_held_measuring_press_first_restored_release_is_inert_then_fresh_works() {
    held_measurement("dialog.createPreset", "presetInclude");
}
#[test]
fn settings_groups_sdk_standalone_sizing_publishes_no_targets_and_cannot_capture_a_restored_release() {
    fn frame(ctx: &egui::Context, tick: &mut u32, groups: &mut Vec<String>, events: Vec<Event>, sizing: bool) -> Vec<(String, Rect)> {
        crate::widgets::take_registries(ctx);
        *tick += 1;
        let raw = HeadlessView::raw_input(egui::vec2(1400.0, 1100.0), 1.0, f64::from(*tick) / 60.0, events);
        let mut last = None;
        let mut output = ctx.run_ui(raw, |root| {
            let mut builder = egui::UiBuilder::new().id_salt("sdk-group-checklist-sizing-host").max_rect(root.available_rect_before_wrap());
            if sizing {
                builder = builder.sizing_pass();
            }
            let mut child = root.new_child(builder);
            super::group_checklist(&mut child, "groupSizing", groups);
            last = Some(crate::widgets::take_registries(ctx).0);
        });
        output.textures_delta.clear();
        output.platform_output.commands.clear();
        output.viewport_output.clear();
        output.shapes.clear();
        last.unwrap()
    }
    let f = Checklist::new();
    let before = f.state();
    let mut tick = 0;
    let mut groups = vec!["light".into()];
    let original = groups.clone();
    let mut rects = vec![];
    for _ in 0..3 {
        rects = frame(&f.ctx, &mut tick, &mut groups, vec![], false);
    }
    let at = rects.iter().find(|(name, _)| name == "groupSizing:light").unwrap().1.center();
    frame(&f.ctx, &mut tick, &mut groups, vec![Event::PointerMoved(at)], false);
    let down = Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE };
    let up = Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE };
    let measured = frame(&f.ctx, &mut tick, &mut groups, vec![down.clone()], true);
    assert!(measured.iter().all(|(name, _)| !name.starts_with("groupSizing:")), "sizing geometry is never a live alias");
    assert_eq!(groups, original);
    for _ in 0..3 {
        frame(&f.ctx, &mut tick, &mut groups, vec![], false);
    }
    frame(&f.ctx, &mut tick, &mut groups, vec![up.clone()], false);
    assert_eq!(groups, original, "first restored release cannot activate a sizing-only press");
    // The same actual live target remains at its stable normal geometry; no point is invented.
    let live = frame(&f.ctx, &mut tick, &mut groups, vec![], false);
    assert_eq!(live.iter().find(|(name, _)| name == "groupSizing:light").unwrap().1.center(), at);
    frame(&f.ctx, &mut tick, &mut groups, vec![Event::PointerMoved(at)], false);
    frame(&f.ctx, &mut tick, &mut groups, vec![down], false);
    frame(&f.ctx, &mut tick, &mut groups, vec![up], false);
    assert!(groups.is_empty(), "fresh genuine live click works after sizing");
    assert_eq!(f.state(), before);
}
