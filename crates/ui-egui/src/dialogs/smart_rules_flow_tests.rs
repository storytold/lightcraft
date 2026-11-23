//! Actual SmartRules parent workflows, including the SDK Name's bounded legacy close opt-in.
use crate::LightcraftApp;
use crate::headless::HeadlessView;
use crate::state::Dialog;
use egui::{Event, Key, Modifiers, PointerButton, Rect};
use lightcraft_catalog::rules::RuleSet;
use lightcraft_catalog::{AlbumId, Flag};
use serde_json::{Value, json};

struct Output(egui::FullOutput);
impl Drop for Output {
    fn drop(&mut self) {
        self.0.textures_delta.clear();
        self.0.platform_output.commands.clear();
        self.0.viewport_output.clear();
        self.0.shapes.clear();
    }
}
struct Review {
    app: LightcraftApp,
    ctx: egui::Context,
    tick: u32,
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, crate::widgets::WidgetHit)>,
    old_language: crate::i18n::Locale,
    sdk_name_owner: bool,
}
impl Review {
    fn new() -> Self {
        let app = LightcraftApp::new(
            lightcraft_engine::Session::with_demo(),
            crate::Services { local_places: Some(vec![]), png: None, ..Default::default() },
        );
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        Self { app, ctx, tick: 0, rects: vec![], hits: vec![], old_language: crate::i18n::language(), sdk_name_owner: false }
    }
    fn open(&mut self, id: Option<AlbumId>) {
        let params = id.map_or_else(|| json!({"name":"Untouched"}), |id| json!({"id":id.0}));
        self.app.run("dialog.smartAlbum", params).unwrap();
        for _ in 0..3 {
            self.frame(vec![]);
        }
        assert!(matches!(self.app.ui.dialog, Some(Dialog::SmartRules { .. })));
    }
    fn seed_legacy_album(&mut self) -> AlbumId {
        // Setup uses the actual engine legacy-filter route, not a draft setter. The editor
        // subsequently reads/upgrades it exactly as production; all edits below are input.
        let id = self.app.run("album.createSmart", json!({"name":"Keep legacy","rules":{"rating":3}})).unwrap()["id"].as_u64().unwrap();
        let id = AlbumId(id);
        assert!(self.app.session.catalog.album(id).unwrap().smart.as_ref().unwrap().rule_set.is_none());
        id
    }
    fn safety(&self) {
        assert!(self.app.services.write.is_none() && self.app.services.write_shared.is_none());
        assert!(self.app.services.picker.is_none() && self.app.services.pick_folder.is_none() && self.app.services.pick_files.is_none());
        assert!(self.app.session.library.is_none() && self.app.session.library_folder.is_none());
        assert!(self.app.session.smart_previews_dir.is_none() && self.app.session.media.rendered.disk().is_none());
        assert!(self.app.export.is_none() && self.app.import.is_none() && self.app.scan.is_none());
        assert_eq!(self.app.renderer.in_flight(), 0);
    }
    fn frame(&mut self, events: Vec<Event>) {
        self.frame_blocked(events, false);
    }
    fn frame_blocked(&mut self, events: Vec<Event>, blocked: bool) {
        self.safety();
        crate::widgets::take_registries(&self.ctx);
        self.tick += 1;
        let raw = HeadlessView::raw_input(egui::vec2(1600.0, 1200.0), 1.0, f64::from(self.tick) / 60.0, events);
        let app = &mut self.app;
        let ctx = self.ctx.clone();
        let mut final_pass = None;
        let out = Output(ctx.run_ui(raw, |root| {
            // Actual production Name, rules editor, problem/count view and button row.
            // Deliberately no full app.ui/logic/renderer worker tick.
            let (_, recorded) = craft::ui::runtime::inspect::record(root, egui::Id::unique("smart-rules-parent-inspect"), |_| {
                crate::dialogs::show(app, &ctx, blocked);
            });
            final_pass = Some((crate::widgets::take_registries(&ctx), recorded));
        }));
        let ((rects, hits), recorded) = final_pass.unwrap();
        self.sdk_name_owner = hits.iter().find(|(name, _)| name == "field:smartName").is_some_and(|(_, hit)| {
            recorded.iter().flat_map(|record| record.elements.iter()).any(|element| element.id == hit.id && element.component == "TextField")
        });
        (self.rects, self.hits) = (rects, hits);
        drop(out);
        self.safety();
    }
    fn draft(&self) -> &Dialog {
        self.app.ui.dialog.as_ref().expect("actual rules review remains open")
    }
    fn rules(&self) -> &RuleSet {
        let Dialog::SmartRules { rules, .. } = self.draft() else { panic!("not SmartRules") };
        rules
    }
    fn name(&self) -> &str {
        let Dialog::SmartRules { name, .. } = self.draft() else { panic!("not SmartRules") };
        name
    }
    fn serialized(&self) -> Value {
        serde_json::to_value(self.rules()).unwrap()
    }
    fn rect(&self, id: &str) -> Rect {
        let found: Vec<_> = self.rects.iter().filter(|(name, _)| name == id).collect();
        assert_eq!(found.len(), 1, "one current presentation for {id}");
        found[0].1
    }
    fn visible(&self, id: &str) -> Rect {
        // These deliberately small workflows fit without inventing a scroll/focus workaround.
        let rect = self.rect(id);
        let popup = id.starts_with("ruleFieldItem:") || id.starts_with("ruleFieldGroup:") || id.contains(":item:");
        if popup {
            assert!(
                craft::ui::runtime::overlay::any_open(&self.ctx, craft::ui::runtime::overlay::OpenFilter::Any),
                "{id} needs its actual child menu"
            );
        } else {
            assert!(self.rect("dialog:window").contains_rect(rect), "{id} must be inside its actual parent");
        }
        assert!(self.ctx.content_rect().contains_rect(rect), "{id} must fit the real viewport");
        if let Some((_, hit)) = self.hits.iter().find(|(name, _)| name == id) {
            assert!(hit.interact_rect.contains(rect.center()), "{id} is clipped");
        }
        rect
    }
    fn click(&mut self, id: &str) {
        let at = self.visible(id).center();
        for pressed in [true, false] {
            self.frame(vec![
                Event::PointerMoved(at),
                Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE },
            ]);
        }
        self.frame(vec![]);
    }
    fn key_mods(&mut self, key: Key, modifiers: Modifiers) {
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers }]);
        }
        self.frame(vec![]);
    }
    fn key(&mut self, key: Key) {
        self.key_mods(key, Modifiers::NONE);
    }
    fn replace_text(&mut self, id: &str, text: &str) {
        self.click(id);
        self.key_mods(Key::A, Modifiers { ctrl: true, command: true, ..Modifiers::NONE });
        if text.is_empty() {
            self.key(Key::Backspace);
        } else {
            self.frame(vec![Event::Text(text.into())]);
        }
    }
    fn edit(&mut self, id: &str, text: &str) {
        self.replace_text(id, text);
        self.key(Key::Tab); // Real blur commits, as the original rules_port155 helper does.
        assert!(matches!(self.app.ui.dialog, Some(Dialog::SmartRules { .. })), "field edit is not Create/Save");
    }
    fn choose(&mut self, face: &str, item: &str) {
        self.click(face);
        self.click(item);
        // (the dialog is a framework layer itself: no popup stays above it)
        assert!(!crate::dialogs::popup_open(&self.ctx));
    }
    fn field(&mut self, salt: &str, group: Option<&str>, field: &str) {
        self.click(&format!("ruleField:{salt}"));
        if let Some(group) = group {
            self.click(&format!("ruleFieldGroup:{group}:{salt}"));
        }
        self.click(&format!("ruleFieldItem:{field}:{salt}"));
    }
    fn nested_input(&mut self) -> Value {
        if self.rules().rules.is_empty() {
            self.click("button:ruleAddEnd-rules");
        }
        self.edit("field:rules-0", "4");
        self.click("button:ruleAddGroup-rules");
        self.edit("field:rules-1-0", "5");
        self.choose("ruleMatch:rules-1", "ruleMatch:rules-1:item:none");
        self.click("button:ruleAdd-rules-1-0");
        self.field("rules-1-1", None, "flag");
        self.choose("ruleChoice:rules-1-1", "ruleChoice:rules-1-1:item:reject");
        let expected = json!({"match":"all","rules":[
            {"field":"rating","op":"gte","value":4},
            {"group":{"match":"none","rules":[
                {"field":"rating","op":"gte","value":5},
                {"field":"flag","op":"is","value":"reject"}
            ]}}
        ]});
        assert_eq!(self.serialized(), expected);
        assert!(self.rules().check_for(&self.app.session.catalog, None).is_empty());
        expected
    }
    fn document(&self) -> Value {
        let s = &self.app.session;
        json!({"photos":self.photos(),"albums":self.albums(),"revision":s.catalog.revision,
            "undo":s.undo.len(),"redo":s.redo.len(),"commits":s.commits(),"selected":s.selected(),
            "settings":self.app.ui.settings,"imports":s.import_defaults,"xmp":s.xmp,"lastExport":s.last_export,
            "presets":s.presets.iter().cloned().collect::<Vec<_>>(),"journal":s.journal})
    }
    fn photos(&self) -> Value {
        let mut photos = self.app.session.catalog.photos().collect::<Vec<_>>();
        photos.sort_by_key(|photo| photo.id.0);
        serde_json::to_value(photos).unwrap()
    }
    fn albums(&self) -> Value {
        let mut albums = self.app.session.catalog.albums().collect::<Vec<_>>();
        albums.sort_by_key(|album| album.id.0);
        serde_json::to_value(albums).unwrap()
    }
    fn album(&self, id: AlbumId) -> Value {
        serde_json::to_value(self.app.session.catalog.album(id).expect("saved album")).unwrap()
    }
    fn idle(&mut self) {
        for _ in 0..3 {
            self.frame(vec![]);
        }
    }
}
impl Drop for Review {
    fn drop(&mut self) {
        self.app.ui.dialog = None;
        crate::i18n::set_language(self.old_language);
    }
}

#[test]
fn smart_rules_actual_create_serializes_nested_input_once_and_undo_redo_restore_the_same_album() {
    let mut r = Review::new();
    r.open(None);
    let before = r.document();
    let photos = r.photos();
    let albums = r.albums();
    let undo = r.app.session.undo.len();
    let journal = r.app.session.journal.len();
    r.edit("field:smartName", "   Typed Keepers   ");
    let expected = r.nested_input();
    assert_eq!(r.document(), before, "all rule/name edits are draft-only");
    r.click("button:dialogOk");
    assert!(r.app.ui.dialog.is_none());
    assert_eq!(
        &r.app.session.journal[journal..],
        &[("album.createSmart".into(), json!({"name":"Typed Keepers","rules":{"ruleSet":expected},"parent":null}))]
    );
    assert_eq!(r.app.session.undo.len(), undo + 1);
    let album = r.app.session.catalog.albums().find(|a| a.name == "Typed Keepers").expect("actual Create made the album").clone();
    let id = album.id;
    assert_eq!(serde_json::to_value(album.smart.as_ref().unwrap().rule_set.as_ref().unwrap()).unwrap(), expected);
    let count = r.app.session.catalog.photos().filter(|p| !p.deleted && p.rating == 4 && p.flag != Flag::Reject).count();
    assert_eq!(r.app.session.catalog.album_count(id), count, "nested None keeps rating-four non-rejected photos");
    assert_eq!(r.photos(), photos);
    let saved = r.album(id);
    r.idle();
    assert_eq!(r.app.session.journal.len(), journal + 1, "idle parent presentations must not repeat Create");
    r.app.run("edit.undo", json!({})).unwrap();
    assert!(r.app.session.catalog.album(id).is_none());
    assert_eq!(r.albums(), albums);
    assert_eq!(r.photos(), photos);
    r.app.run("edit.redo", json!({})).unwrap();
    assert_eq!(r.album(id), saved, "Redo restores the same identity, name and exact nested tree");
    r.safety();
}

#[test]
fn smart_rules_actual_invalid_edit_blocks_partial_rename_then_corrected_save_is_one_atomic_undo() {
    let mut r = Review::new();
    let id = r.seed_legacy_album();
    let original = r.album(id);
    r.open(Some(id));
    let before = r.document();
    let photos = r.photos();
    let undo = r.app.session.undo.len();
    let journal = r.app.session.journal.len();
    r.edit("field:smartName", "  Renamed by input  ");
    if r.rules().rules.is_empty() {
        r.click("button:ruleAddEnd-rules");
    }
    r.field("rules-0", Some("Date"), "captureDate");
    r.edit("field:rules-0", "banana");
    let invalid = json!({"match":"all","rules":[{"field":"captureDate","op":"is","value":"banana"}]});
    assert_eq!(r.serialized(), invalid);
    assert!(!r.rules().check_for(&r.app.session.catalog, Some(id)).is_empty());
    r.rect("ruleProblem:rules-0");
    assert!(!super::can_confirm(&r.app, r.draft(), false));
    let draft = r.draft().clone();
    r.click("button:dialogOk");
    assert_eq!(r.draft(), &draft, "disabled Save keeps the actual typed invalid input");
    assert_eq!(r.album(id), original, "no partial name or rule write");
    assert_eq!(r.document(), before);
    r.edit("field:rules-0", "2026");
    let expected = json!({"match":"all","rules":[{"field":"captureDate","op":"is","value":"2026"}]});
    assert_eq!(r.serialized(), expected);
    assert!(r.rules().check_for(&r.app.session.catalog, Some(id)).is_empty());
    assert!(!r.rects.iter().any(|(name, _)| name.starts_with("ruleProblem:")));
    assert!(super::can_confirm(&r.app, r.draft(), false));
    assert_eq!(r.name(), "  Renamed by input  ");
    r.click("button:dialogOk");
    assert!(r.app.ui.dialog.is_none());
    assert_eq!(
        &r.app.session.journal[journal..],
        &[("album.setRules".into(), json!({"id":id.0,"name":"Renamed by input","replace":true,"rules":{"ruleSet":expected}}))]
    );
    assert_eq!(r.app.session.undo.len(), undo + 1);
    let saved = r.album(id);
    let album = r.app.session.catalog.album(id).unwrap();
    assert_eq!(album.name, "Renamed by input");
    assert_eq!(serde_json::to_value(album.smart.as_ref().unwrap().rule_set.as_ref().unwrap()).unwrap(), expected);
    assert_eq!(r.photos(), photos);
    r.idle();
    assert_eq!(r.app.session.journal.len(), journal + 1, "Save must not replay");
    r.app.run("edit.undo", json!({})).unwrap();
    assert_eq!(r.album(id), original, "one Undo restores old name and exact legacy-filter representation");
    assert!(r.app.session.catalog.album(id).unwrap().smart.as_ref().unwrap().rule_set.is_none());
    r.app.run("edit.redo", json!({})).unwrap();
    assert_eq!(r.album(id), saved);
    assert_eq!(r.photos(), photos);
    r.safety();
}

#[test]
fn smart_rules_actual_cancel_discards_typed_name_and_nested_rules_without_any_catalog_or_history_write() {
    let mut r = Review::new();
    let id = r.seed_legacy_album();
    r.open(Some(id));
    let before = r.document();
    r.edit("field:smartName", "Never saved");
    r.nested_input();
    assert_eq!(r.document(), before);
    r.click("button:dialogCancel");
    assert!(r.app.ui.dialog.is_none());
    r.idle();
    assert_eq!(r.document(), before);
}

#[test]
fn smart_rules_sdk_name_single_escape_closes_actual_parent_without_writes() {
    let mut r = Review::new();
    let id = r.seed_legacy_album();
    r.open(Some(id));
    let before = r.document();
    assert!(r.sdk_name_owner, "the registered Name hit must belong to the actually mounted SDK TextField");
    // Keep the actual Name editor focused and dirty: no Tab/Return/forced focus.
    r.replace_text("field:smartName", "Changed SDK Name draft");
    assert_eq!(r.name(), "Changed SDK Name draft");
    let focused_before = r.ctx.memory(|memory| memory.focused());
    r.key(Key::Escape); // Exactly one real press/release, never replayed or routed by a test hook.
    let closed = r.app.ui.dialog.is_none();
    let observed = if closed { Value::Null } else { serde_json::to_value(r.draft()).unwrap() };
    println!("SMART_RULES_SDK_NAME_SINGLE_ESCAPE {}", json!({"closed":closed,"focusedBefore":format!("{focused_before:?}"),"dialogAfter":observed}));
    assert!(closed, "one physical Escape from the edited SDK Name must preserve the observed native parent dismissal");
    assert_eq!(before["albums"], r.albums(), "the original album and rule representation are unchanged");
    assert_eq!(r.document(), before, "one Escape cannot rename/save/change history");
    r.idle();
    assert_eq!(r.document(), before);
}

#[test]
fn smart_rules_sdk_name_quiet_blur_and_blocked_escape_keep_parent_and_catalog_unchanged() {
    let mut r = Review::new();
    let id = r.seed_legacy_album();
    r.open(Some(id));
    assert!(r.sdk_name_owner);
    let before = r.document();
    r.replace_text("field:smartName", "Only a draft");
    assert_eq!(r.name(), "Only a draft");
    let draft = r.draft().clone();
    r.idle();
    assert_eq!(r.draft(), &draft, "quiet passes are not an Escape close request");
    // The actual parent's frozen input blocker is passed for the entire physical key delivery.
    for pressed in [true, false] {
        r.frame_blocked(
            vec![Event::Key { key: Key::Escape, physical_key: Some(Key::Escape), pressed, repeat: false, modifiers: Modifiers::NONE }],
            true,
        );
    }
    r.frame_blocked(vec![], true);
    assert_eq!(r.draft(), &draft, "a covering prompt owns its Escape; Name must not close or edit behind it");
    assert_eq!(r.document(), before);
    r.frame(vec![]);
    r.key(Key::Tab); // Normal commit/blur is not an Escape close either.
    r.idle();
    assert_eq!(r.draft(), &draft);
    assert_eq!(r.document(), before);
    r.click("button:dialogCancel");
    assert!(r.app.ui.dialog.is_none());
    assert_eq!(r.document(), before);
}

#[test]
fn smart_rules_sdk_name_child_menu_escape_dismisses_only_the_popup_and_retains_the_typed_name() {
    let mut r = Review::new();
    r.open(None);
    let before = r.document();
    r.replace_text("field:smartName", "Keep while menu closes");
    r.click("ruleMatch:rules");
    assert!(crate::dialogs::popup_open(&r.ctx));
    let draft = r.draft().clone();
    r.key(Key::Escape);
    assert!(!crate::dialogs::popup_open(&r.ctx));
    r.idle();
    assert_eq!(r.draft(), &draft, "child menu Escape cannot become Name/parent dismissal");
    assert_eq!(r.name(), "Keep while menu closes");
    assert_eq!(r.document(), before);
    r.click("button:dialogCancel");
    assert!(r.app.ui.dialog.is_none());
    assert_eq!(r.document(), before);
}

#[test]
fn smart_rules_sdk_name_composition_only_escape_keeps_parent_and_original_value_without_writes() {
    let mut r = Review::new();
    r.open(None);
    let before = r.document();
    let original = r.draft().clone();
    r.click("field:smartName"); // Actual pointer focus; no text draft or forced focus.
    assert!(r.sdk_name_owner);
    r.frame(vec![Event::Ime(egui::ImeEvent::Preedit { text: "世".into(), active_range_chars: None })]);
    assert_eq!(r.draft(), &original, "preedit is not committed Name text");
    r.key(Key::Escape); // Ends only the composition: shown.escaped must not request parent close.
    r.idle();
    assert_eq!(r.draft(), &original);
    assert_eq!(r.document(), before);
}

#[test]
fn smart_rules_sdk_name_measurement_and_sizing_do_not_publish_name_mutate_draft_or_request_close() {
    let mut r = Review::new();
    r.open(None);
    let before = r.document();
    let original = r.draft().clone();
    for probe in [1, 2] {
        let ctx = r.ctx.clone();
        let app = &mut r.app;
        let mut draft = original.clone();
        let mut close = false;
        crate::widgets::take_registries(&ctx);
        r.tick += 1;
        let raw = HeadlessView::raw_input(egui::vec2(1600.0, 1200.0), 1.0, f64::from(r.tick) / 60.0, vec![]);
        let out = Output(ctx.run_ui(raw, |root| {
            egui::CentralPanel::default().show(root, |ui| {
                let t = crate::theme::Tokens::get(&ctx);
                if probe == 1 {
                    craft::ui::runtime::measure::run(&ctx, || super::smart_rules_body(app, ui, &t, &mut close, &mut draft));
                } else {
                    let mut sizing = ui.new_child(egui::UiBuilder::new().max_rect(ui.max_rect()).sizing_pass().invisible());
                    super::smart_rules_body(app, &mut sizing, &t, &mut close, &mut draft);
                }
                let (rects, hits) = crate::widgets::take_registries(&ctx);
                assert!(!rects.iter().any(|(name, _)| name == "field:smartName"));
                assert!(!hits.iter().any(|(name, _)| name == "field:smartName"));
                assert!(!close, "passive Name has no eligible close request");
                assert_eq!(draft, original);
            });
        }));
        drop(out);
        assert_eq!(r.document(), before);
    }
    r.frame(vec![]);
    assert_eq!(r.draft(), &original);
    assert_eq!(r.document(), before);
}

#[test]
fn smart_rules_sdk_name_trace_blur_prompt_presentation_and_prompt_escape_separately() {
    for scenario in ["tab-blur", "prompt-no-event", "prompt-escape-first-pass", "prompt-then-escape"] {
        let mut r = Review::new();
        let id = r.seed_legacy_album();
        r.open(Some(id));
        let before = r.document();
        r.replace_text("field:smartName", "Only a draft");
        let draft = r.draft().clone();
        r.idle();
        assert_eq!(r.draft(), &draft, "the existing exact quiet assertion remains");
        println!(
            "SMART_RULES_NAME_STAGE {}",
            json!({"scenario":scenario,"stage":"after-quiet","dialog":r.draft(),"focus":format!("{:?}",r.ctx.memory(|m|m.focused()))})
        );
        match scenario {
            "tab-blur" => r.key(Key::Tab),
            "prompt-no-event" => r.frame_blocked(vec![], true),
            "prompt-escape-first-pass" => r.frame_blocked(
                vec![Event::Key { key: Key::Escape, physical_key: Some(Key::Escape), pressed: true, repeat: false, modifiers: Modifiers::NONE }],
                true,
            ),
            "prompt-then-escape" => {
                r.frame_blocked(vec![], true);
                assert_eq!(r.draft(), &draft, "delayed control retains the exact draft before Escape");
                println!(
                    "SMART_RULES_NAME_STAGE {}",
                    json!({"scenario":scenario,"stage":"prompt-presented-before-key","dialog":r.draft(),"focus":format!("{:?}",r.ctx.memory(|m|m.focused()))})
                );
                r.frame_blocked(
                    vec![Event::Key { key: Key::Escape, physical_key: Some(Key::Escape), pressed: true, repeat: false, modifiers: Modifiers::NONE }],
                    true,
                );
            }
            _ => unreachable!(),
        }
        println!(
            "SMART_RULES_NAME_STAGE {}",
            json!({"scenario":scenario,"stage":"after-first-stage","dialog":r.draft(),"focus":format!("{:?}",r.ctx.memory(|m|m.focused()))})
        );
        assert_eq!(r.draft(), &draft, "{scenario}: first blocked pass/blur must retain the exact typed draft");
        if scenario.contains("escape") {
            r.frame_blocked(
                vec![Event::Key { key: Key::Escape, physical_key: Some(Key::Escape), pressed: false, repeat: false, modifiers: Modifiers::NONE }],
                true,
            );
            println!("SMART_RULES_NAME_STAGE {}", json!({"scenario":scenario,"stage":"after-release","dialog":r.draft()}));
            assert_eq!(r.draft(), &draft, "{scenario}: release must retain the exact typed draft");
        }
        assert_eq!(r.document(), before, "trace cannot create catalog or history writes");
    }
}
