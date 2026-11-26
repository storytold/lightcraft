//! Actual Create/Edit Keyword Tag SDK workflow. Controllers, commands and engine/model are unchanged.
use crate::LightcraftApp;
use crate::headless::HeadlessView;
use crate::state::Dialog;
use egui::{Event, Key, Modifiers, PointerButton, Rect};
use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug)]
struct ObservedNativeOption {
    id: egui::Id,
    layer_id: egui::LayerId,
    rect: Rect,
    interact_rect: Rect,
    sense: egui::Sense,
    enabled: bool,
    semantic_disabled: Option<bool>,
}
impl ObservedNativeOption {
    fn enabled(&self) -> bool {
        self.semantic_disabled.map_or(self.enabled, |disabled| !disabled)
    }
}

struct KeywordOptions {
    app: LightcraftApp,
    ctx: egui::Context,
    tick: u32,
    rects: Vec<(String, Rect)>,
    hits: Vec<(String, crate::widgets::WidgetHit)>,
    witnessed: Vec<(String, ObservedNativeOption)>,
    focused_response: Option<egui::Response>,
    sdk_focus: Option<craft::ui::runtime::focus::StopRec>,
    native_has_focus: bool,
    recorded: Option<std::sync::Arc<craft::ui::runtime::inspect::Recorded>>,
    proxies: Vec<(String, Option<ObservedNativeOption>)>,
    ids: [PhotoId; 2],
    blocked: bool,
    measuring: bool,
    original_locale: crate::i18n::Locale,
}
impl KeywordOptions {
    fn new() -> Self {
        let original_locale = crate::i18n::language();
        crate::i18n::set_language(crate::i18n::Locale::En);
        let mut session = lightcraft_engine::Session::new();
        assert!(session.library.is_none() && session.library_folder.is_none(), "fixture starts in memory before any engine command");
        let ids = [session.catalog.alloc_photo_id(), session.catalog.alloc_photo_id()];
        for (id, name) in [(ids[0], "Group source"), (ids[1], "Group target")] {
            let photo = Photo::new(id, Source::Demo { scene: 0 }, name, "JPEG", 6000, 4000, "2026-01-14T05:58:48");
            session.commit("fixture", Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        }
        session.execute("library.select", &json!({"ids":ids})).unwrap();
        session.execute("keyword.create", &json!({"name":"Places"})).unwrap();
        session.execute("keyword.create", &json!({"name":"Nature"})).unwrap();
        session.execute("keyword.create", &json!({"name":"Existing","parent":"Places","synonyms":["old"]})).unwrap();
        session.execute("keyword.create", &json!({"name":"Other","parent":"Nature","person":true})).unwrap();
        let app = LightcraftApp::new(session, crate::Services { local_places: Some(vec![]), png: None, ..Default::default() });
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        Self {
            app,
            ctx,
            tick: 0,
            rects: vec![],
            hits: vec![],
            witnessed: vec![],
            focused_response: None,
            sdk_focus: None,
            native_has_focus: false,
            recorded: None,
            proxies: vec![],
            ids,
            blocked: false,
            measuring: false,
            original_locale,
        }
    }
    fn state(&self) -> Value {
        let s = &self.app.session;
        json!({"catalog":s.catalog,"undo":format!("{:?}",s.undo),"redo":format!("{:?}",s.redo),
            "commits":s.commits(),"journal":s.journal,"selected":s.selected(),"selection":format!("{:?}",s.selection),
            "copyGroups":s.copy_groups,"clipboard":s.clipboard,"presets":s.presets.iter().cloned().collect::<Vec<_>>(),
            "settings":self.app.ui.settings,"importDefaults":s.import_defaults,"xmp":s.xmp,"lastExport":s.last_export,
            "print":s.print,"printTemplates":s.print_templates, "keywordSets":s.keyword_sets,"keywordSet":s.keyword_set,"recentKeywords":s.recent_keywords, "filter":s.filter})
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
            ctx.data_mut(|d| d.remove::<Vec<(String, ObservedNativeOption)>>(witness_key()));
            // The installed dialog dispatch, window, body, confirmation and cancellation paths.
            // No AppShell grid/renderer pass and no leaf-only substitute.
            let (_, recorded) = craft::ui::runtime::inspect::record(root, egui::Id::unique("keyword-tag-focus-state-observation"), |_| {
                if measuring {
                    craft::ui::runtime::measure::run(&ctx, || crate::dialogs::show(app, &ctx, blocked));
                } else {
                    crate::dialogs::show(app, &ctx, blocked);
                }
            });
            let mut witnessed = ctx.data(|d| d.get_temp::<Vec<(String, ObservedNativeOption)>>(witness_key()).unwrap_or_default());
            for (_, witness) in &mut witnessed {
                witness.semantic_disabled = recorded
                    .as_ref()
                    .and_then(|record| record.elements.iter().find(|e| e.id == witness.id && e.part.is_none() && e.component == "Checkbox"))
                    .and_then(|e| e.facts.as_ref())
                    .map(|facts| facts.disabled);
            }
            let proxies = witnessed
                .iter()
                .map(|(alias, r)| {
                    (
                        alias.clone(),
                        ctx.read_response(r.id).map(|r| ObservedNativeOption {
                            id: r.id,
                            layer_id: r.layer_id,
                            rect: r.rect,
                            interact_rect: r.interact_rect,
                            sense: r.sense,
                            enabled: r.enabled(),
                            semantic_disabled: None,
                        }),
                    )
                })
                .collect();
            let focused_response = ctx.memory(|m| m.focused()).and_then(|id| ctx.read_response(id));
            let native_has_focus = focused_response.as_ref().is_some_and(|r| r.has_focus());
            last = Some((
                crate::widgets::take_registries(&ctx),
                witnessed,
                focused_response,
                craft::ui::runtime::focus::focused_rec(&ctx),
                recorded,
                proxies,
                native_has_focus,
            ));
        });
        let ((rects, hits), witnessed, focused_response, sdk_focus, recorded, proxies, native_has_focus) = last.unwrap();
        self.native_has_focus = native_has_focus;
        self.sdk_focus = sdk_focus;
        self.recorded = recorded;
        self.proxies = proxies;
        self.focused_response = focused_response;
        self.witnessed = witnessed;
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

    fn rect(&self, key: &str) -> Rect {
        let observed;
        let rects = if key.starts_with("check:keyword") {
            observed = self.witnessed.iter().map(|(name, response)| (name.clone(), response.interact_rect)).collect::<Vec<_>>();
            &observed
        } else {
            &self.rects
        };
        let matches: Vec<_> = rects.iter().filter(|(name, _)| name == key).collect();
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
        // SDK ownership is the registered node and its published native root, not the indicator's box.
        if let Some(record) = &self.sdk_focus {
            let role = if key.starts_with("field:keyword") {
                "TextInput"
            } else if key.starts_with("button:dialog") {
                "Button"
            } else {
                "CheckBox"
            };
            let nodes = self
                .recorded
                .as_ref()
                .into_iter()
                .flat_map(|r| r.elements.iter())
                .filter(|e| e.id == record.stop.id && e.part.is_none() && e.facts.as_ref().is_some_and(|f| f.role == role && !f.disabled))
                .collect::<Vec<_>>();
            return record.stop.part.is_none()
                && self.native_has_focus
                && self.focused_response.as_ref().is_some_and(|r| {
                    r.id == record.stop.root && self.hits.iter().any(|(name, hit)| name == "dialog:window" && hit.layer_id == r.layer_id)
                })
                && self.hits.iter().filter(|(name, hit)| name == key && hit.id == record.stop.id).count() == 1
                && nodes.len() == 1
                && nodes[0].stop.is_some();
        }
        if key.starts_with("field:keyword") {
            // This is the adapter's public genuine TextEdit id, not the hover-only alias probe.
            return self.focused_response.as_ref().is_some_and(|r| r.id == crate::text_field::id(key));
        }
        if key.starts_with("button:dialog") {
            // Observe the actual focused native Response. The alias supplies geometry only;
            // its hover probe id is never used as the native button's keyboard identity.
            return self.focused_response.as_ref().is_some_and(|r| {
                r.rect == self.rect(key)
                    && r.sense == egui::Sense::click()
                    && self.hits.iter().any(|(name, hit)| name == "dialog:window" && hit.layer_id == r.layer_id)
            });
        }
        let actual = &self.witnessed.iter().find(|(name, _)| name == key).expect("genuine option Response in final callback").1;
        self.focused_response.as_ref().is_some_and(|r| {
            r.id == actual.id
                && r.layer_id == actual.layer_id
                && r.rect == actual.rect
                && r.interact_rect == actual.interact_rect
                && r.sense == actual.sense
        })
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
}
impl Drop for KeywordOptions {
    fn drop(&mut self) {
        crate::i18n::set_language(self.original_locale);
    }
}

fn witness_key() -> egui::Id {
    egui::Id::unique("test-keyword-tag-options-native-response-observations")
}
/// Pure observation of the genuine response already produced by the installed Checkbox. This never
/// registers an alias/hit, interacts, changes focus, replays input or unions callback registries.
pub(super) fn observe(ui: &egui::Ui, alias: &str, response: &egui::Response) {
    let observed = ObservedNativeOption {
        id: response.id,
        layer_id: response.layer_id,
        rect: response.rect,
        interact_rect: response.interact_rect,
        sense: response.sense,
        enabled: response.enabled(),
        semantic_disabled: None,
    };
    // Only immutable observed fields live in Context data; no retained Response/Context cycle.
    ui.ctx().data_mut(|d| d.get_temp_mut_or_default::<Vec<(String, ObservedNativeOption)>>(witness_key()).push((alias.to_owned(), observed)));
}
impl KeywordOptions {
    fn create(&mut self, parent: Option<&str>) {
        self.app.ui.keyword_list_selected = parent.map(str::to_owned);
        self.app.ui.dialog = Some(crate::panels::keyword_list::create_dialog(&self.app));
        self.idle();
    }
    fn edit(&mut self, path: &str) {
        self.app.ui.keyword_list_selected = Some(path.to_owned());
        self.app.ui.dialog = Some(crate::panels::keyword_list::edit_dialog(&self.app, path));
        self.idle();
    }
}

const FLAGS: [&str; 4] = ["check:keywordIncludeOnExport", "check:keywordExportContaining", "check:keywordExportSynonyms", "check:keywordPerson"];
impl KeywordOptions {
    fn flag(&self, alias: &str) -> bool {
        let Some(Dialog::KeywordTag { inside, add_to_selected, include_on_export, export_containing, export_synonyms, person, .. }) =
            &self.app.ui.dialog
        else {
            panic!("real keyword controller missing");
        };
        match alias {
            "check:keywordInside" => *inside,
            "check:keywordAddToSelected" => *add_to_selected,
            "check:keywordIncludeOnExport" => *include_on_export,
            "check:keywordExportContaining" => *export_containing,
            "check:keywordExportSynonyms" => *export_synonyms,
            "check:keywordPerson" => *person,
            _ => panic!("unknown real option {alias}"),
        }
    }
    fn draft(&self) -> String {
        format!("{:?}", self.app.ui.dialog)
    }
    fn fill(&mut self, key: &str, text: &str) {
        let document = self.state();
        self.click(key);
        assert!(self.focused(key), "pointer reached the actual retained text editor {key}");
        self.key_with(Key::A, Modifiers { command: true, ctrl: true, ..Modifiers::NONE });
        self.frame(vec![Event::Text(text.to_owned())]);
        self.idle();
        let Some(Dialog::KeywordTag { name, synonyms, .. }) = &self.app.ui.dialog else {
            panic!("retained text editor unexpectedly confirmed");
        };
        let actual = match key {
            "field:keywordTagName" => name,
            "field:keywordSynonyms" => synonyms,
            _ => panic!("wrong retained text editor"),
        };
        assert_eq!(actual, text, "actual retained native text input, not a synthetic field assignment");
        assert_eq!(self.state(), document, "retained name/synonyms edits are draft-only");
    }
    fn open_mode(&mut self, editing: bool) {
        if editing {
            self.edit("Places|Existing");
        } else {
            self.create(Some("Places"));
        }
    }
    fn changed_flag(&mut self, alias: &str) {
        let before = self.flag(alias);
        let document = self.state();
        let pos = self.rect(alias).center();
        self.frame(vec![Event::PointerMoved(pos)]);
        self.button(pos, true);
        assert_eq!(self.flag(alias), before, "no option changes on pointer-down");
        assert_eq!(self.state(), document);
        self.button(pos, false);
        self.idle();
        assert_ne!(self.flag(alias), before, "one actual option activation on matching release: {alias}");
        assert_eq!(self.state(), document, "option changes are draft-only");
    }
}
fn options_cancel(editing: bool) {
    let mut f = KeywordOptions::new();
    let before = f.state();
    f.open_mode(editing);
    if editing {
        assert!(!f.witnessed.iter().any(|(name, _)| name == "check:keywordInside" || name == "check:keywordAddToSelected"));
    } else {
        for alias in ["check:keywordInside", "check:keywordAddToSelected"] {
            f.changed_flag(alias);
        }
    }
    for alias in FLAGS {
        f.changed_flag(alias);
    }
    assert_eq!(f.state(), before);
    if editing {
        f.key(Key::Escape);
    } else {
        f.click("button:dialogCancel");
    }
    assert!(f.app.ui.dialog.is_none());
    f.idle();
    assert_eq!(f.state(), before, "complete two-photo catalog, selection, commit/history and all protected settings unchanged");
}
#[test]
fn keyword_tag_options_create_all_six_choices_are_draft_only_and_real_cancel_preserves_full_state() {
    options_cancel(false);
}
#[test]
fn keyword_tag_options_edit_four_choices_omit_create_only_targets_and_escape_preserves_full_state() {
    options_cancel(true);
}
fn expected_flags() -> Value {
    json!({"includeOnExport":false,"exportContaining":false,"exportSynonyms":false,"person":true})
}
fn compare_history(f: &mut KeywordOptions, expected: &mut KeywordOptions, before: &Value) {
    assert!(f.app.ui.dialog.is_none());
    assert_eq!(f.state(), expected.state(), "exact native command result, complete catalog, metadata recipients and history");
    assert_ne!(&f.state(), before, "not a vacuous UI or document no-op");
    let after = f.state();
    f.idle();
    assert_eq!(f.state(), after, "no replay on later idle frames");
    for command in ["edit.undo", "edit.redo"] {
        f.app.run(command, json!({})).unwrap();
        expected.app.run(command, json!({})).unwrap();
        assert_eq!(f.state(), expected.state(), "complete document and history after real {command}");
        if command == "edit.undo" {
            assert_eq!(f.state()["catalog"], before["catalog"], "Undo restores all photos and keyword hierarchy/metadata, including the neighbor");
        }
    }
}
#[test]
fn keyword_tag_options_create_parent_metadata_synonyms_and_selected_photo_batch_equal_exact_command_and_undo_redo() {
    let mut f = KeywordOptions::new();
    let mut expected = KeywordOptions::new();
    let before = f.state();
    let undo = f.app.session.undo.len();
    f.create(Some("Places"));
    f.fill("field:keywordTagName", " Alpine ");
    f.fill("field:keywordSynonyms", "ridge, summit, , ridge");
    f.changed_flag("check:keywordAddToSelected");
    for alias in FLAGS {
        f.changed_flag(alias);
    }
    assert!(f.flag("check:keywordInside"));
    assert_eq!(f.state(), before);
    f.click("button:dialogOk");
    let mut payload = expected_flags();
    payload["name"] = json!(" Alpine ");
    payload["parent"] = json!("Places");
    payload["synonyms"] = json!(["ridge", "summit", "ridge"]);
    payload["addToSelected"] = json!(true);
    expected.app.run("keyword.create", payload).unwrap();
    assert_eq!(f.app.session.undo.len(), undo + 1, "one real create+metadata+photo batch");
    assert!(f.app.session.catalog.photos().all(|p| p.meta.keywords.iter().any(|k| k == "Places|Alpine")));
    compare_history(&mut f, &mut expected, &before);
}
#[test]
fn keyword_tag_options_create_unchecked_inside_sends_explicit_null_even_with_real_default_parent() {
    let mut f = KeywordOptions::new();
    let mut expected = KeywordOptions::new();
    for fixture in [&mut f, &mut expected] {
        fixture.app.run("keyword.setDefaultParent", json!({"keyword":"Places"})).unwrap();
    }
    let before = f.state();
    f.create(None);
    assert!(matches!(&f.app.ui.dialog, Some(Dialog::KeywordTag { parent: Some(parent), inside:true, .. }) if parent == "Places"));
    f.fill("field:keywordTagName", "Top level");
    f.changed_flag("check:keywordInside");
    f.click("button:dialogOk");
    expected.app.run("keyword.create", json!({"name":"Top level","parent":null,"synonyms":[],"includeOnExport":true,"exportContaining":true,"exportSynonyms":true,"person":false,"addToSelected":false})).unwrap();
    assert!(f.app.session.catalog.has_keyword("Top level"));
    assert!(!f.app.session.catalog.has_keyword("Places|Top level"));
    assert!(f.app.session.catalog.photos().all(|p| !p.meta.keywords.iter().any(|k| k == "Top level")));
    compare_history(&mut f, &mut expected, &before);
}
#[test]
fn keyword_tag_options_edit_rename_synonyms_flags_follow_and_one_undo_batch_equal_exact_command() {
    let mut f = KeywordOptions::new();
    let mut expected = KeywordOptions::new();
    for fixture in [&mut f, &mut expected] {
        fixture.app.run("library.filter", json!({"keyword":"Places|Existing"})).unwrap();
    }
    let before = f.state();
    let undo = f.app.session.undo.len();
    f.edit("Places|Existing");
    f.fill("field:keywordTagName", "Renamed");
    f.fill("field:keywordSynonyms", "ridge, summit");
    for alias in FLAGS {
        f.changed_flag(alias);
    }
    assert_eq!(f.state(), before);
    f.click("button:dialogOk");
    let mut payload = expected_flags();
    payload["keyword"] = json!("Places|Existing");
    payload["name"] = json!("Renamed");
    payload["synonyms"] = json!(["ridge", "summit"]);
    expected.app.run("keyword.edit", payload).unwrap();
    assert_eq!(f.app.ui.keyword_list_selected.as_deref(), Some("Places|Renamed"), "real caller follows the renamed keyword");
    assert_eq!(f.app.session.filter.keyword.as_deref(), Some("Places|Renamed"), "the actual active library filter follows the edit command");
    assert_eq!(f.app.session.undo.len(), undo + 1);
    assert!(!f.app.session.catalog.has_keyword("Places|Existing"));
    compare_history(&mut f, &mut expected, &before);
}
#[test]
fn keyword_tag_options_add_to_selected_uses_current_real_selection_at_confirm_not_a_captured_recipient() {
    let mut f = KeywordOptions::new();
    let mut expected = KeywordOptions::new();
    f.create(Some("Places"));
    f.fill("field:keywordTagName", "Current target");
    f.changed_flag("check:keywordAddToSelected");
    for fixture in [&mut f, &mut expected] {
        fixture.app.run("library.select", json!({"ids":[fixture.ids[1]]})).unwrap();
    }
    let before = f.state();
    f.click("button:dialogOk");
    expected.app.run("keyword.create", json!({"name":"Current target","parent":"Places","synonyms":[],"includeOnExport":true,"exportContaining":true,"exportSynonyms":true,"person":false,"addToSelected":true})).unwrap();
    assert!(!f.app.session.catalog.photo(f.ids[0]).unwrap().meta.keywords.iter().any(|k| k == "Places|Current target"));
    assert!(f.app.session.catalog.photo(f.ids[1]).unwrap().meta.keywords.iter().any(|k| k == "Places|Current target"));
    compare_history(&mut f, &mut expected, &before);
}
#[test]
fn keyword_tag_options_no_parent_omits_inside_and_no_selection_disables_add_without_a_fake_tab_stop() {
    let mut f = KeywordOptions::new();
    f.app.run("library.select", json!({"ids":[]})).unwrap();
    let before = f.state();
    f.create(None);
    assert!(!f.witnessed.iter().any(|(name, _)| name == "check:keywordInside"));
    let add = &f.witnessed.iter().find(|(name, _)| name == "check:keywordAddToSelected").unwrap().1;
    assert!(!add.enabled());
    let before_draft = f.draft();
    f.click("check:keywordAddToSelected");
    assert_eq!(f.draft(), before_draft);
    assert_eq!(f.state(), before);
    f.tab_to("field:keywordSynonyms");
    f.key(Key::Tab);
    assert!(f.focused("check:keywordIncludeOnExport"), "natural Tab skips the real disabled Add control");
    assert!(!f.flag("check:keywordAddToSelected"));
    f.key(Key::Space);
    assert!(!f.flag("check:keywordIncludeOnExport"));
    assert_eq!(f.state(), before);
}
fn natural_order(editing: bool) {
    let mut f = KeywordOptions::new();
    let before = f.state();
    f.open_mode(editing);
    assert!(f.focused("field:keywordTagName"), "unchanged real product open-focus behavior");
    let mut order = vec!["field:keywordSynonyms"];
    if !editing {
        order.extend(["check:keywordInside", "check:keywordAddToSelected"]);
    }
    order.extend(FLAGS);
    order.extend(["button:dialogCancel", "button:dialogOk"]);
    for alias in order {
        f.key(Key::Tab);
        assert!(f.focused(alias), "genuine next native keyboard owner must be {alias}; actual {:?}", f.ctx.memory(|m| m.focused()));
    }
    f.key_with(Key::Tab, Modifiers::SHIFT);
    assert!(f.focused("button:dialogCancel"));
    f.key(Key::Enter);
    assert!(f.app.ui.dialog.is_none());
    assert_eq!(f.state(), before, "genuine footer Cancel, not a fake intermediate target");
}
#[test]
fn keyword_tag_options_create_natural_tab_order_includes_both_retained_fields_six_choices_and_real_footer_cancel() {
    natural_order(false);
}
#[test]
fn keyword_tag_options_edit_natural_tab_order_includes_both_retained_fields_four_choices_and_real_footer_cancel() {
    natural_order(true);
}
fn keyboard_flags(editing: bool) {
    let mut f = KeywordOptions::new();
    let before = f.state();
    f.open_mode(editing);
    f.tab_to("check:keywordIncludeOnExport");
    let old = f.flag("check:keywordIncludeOnExport");
    f.key(Key::Space);
    assert_ne!(f.flag("check:keywordIncludeOnExport"), old);
    assert!(f.app.ui.dialog.is_some());
    f.key(Key::Enter);
    assert_eq!(f.flag("check:keywordIncludeOnExport"), old, "one genuine focused-checkbox Enter activation");
    assert!(f.app.ui.dialog.is_some(), "checkbox Enter must not reach parent confirmation");
    assert_eq!(f.state(), before);
}
#[test]
fn keyword_tag_options_create_natural_keyboard_space_and_enter_toggle_the_actual_choice_without_parent_confirm() {
    keyboard_flags(false);
}
#[test]
fn keyword_tag_options_edit_natural_keyboard_space_and_enter_toggle_the_actual_choice_without_parent_confirm() {
    keyboard_flags(true);
}
fn blocked(editing: bool) {
    let mut f = KeywordOptions::new();
    f.open_mode(editing);
    let before = f.state();
    let draft = f.draft();
    f.blocked = true;
    f.idle();
    f.click("check:keywordPerson");
    f.key(Key::Space);
    f.key(Key::Enter);
    f.key(Key::Escape);
    assert_eq!(f.draft(), draft);
    assert_eq!(f.state(), before);
    f.blocked = false;
    f.idle();
    assert_eq!(f.draft(), draft);
    assert_eq!(f.state(), before);
    f.changed_flag("check:keywordPerson");
}
#[test]
fn keyword_tag_options_create_blocked_parent_refuses_pointer_keyboard_and_escape_then_fresh_input_works() {
    blocked(false);
}
#[test]
fn keyword_tag_options_edit_blocked_parent_refuses_pointer_keyboard_and_escape_then_fresh_input_works() {
    blocked(true);
}
fn measurement(editing: bool) {
    let mut f = KeywordOptions::new();
    f.open_mode(editing);
    let before = f.state();
    let draft = f.draft();
    let pos = f.rect("check:keywordPerson").center();
    f.measuring = true;
    f.frame(vec![Event::PointerMoved(pos)]);
    f.button(pos, true);
    f.button(pos, false);
    assert_eq!(f.draft(), draft, "SDK measurement is speculative, even through the installed parent");
    assert_eq!(f.state(), before);
    f.measuring = false;
    f.idle();
    assert_eq!(f.draft(), draft);
    assert_eq!(f.state(), before);
    f.changed_flag("check:keywordPerson");
}
#[test]
fn keyword_tag_options_create_measured_parent_click_is_draft_and_document_passive_with_no_delayed_activation() {
    measurement(false);
}
#[test]
fn keyword_tag_options_edit_measured_parent_click_is_draft_and_document_passive_with_no_delayed_activation() {
    measurement(true);
}
fn measured_down(editing: bool) {
    let mut f = KeywordOptions::new();
    f.open_mode(editing);
    let before = f.state();
    let draft = f.draft();
    let pos = f.rect("check:keywordPerson").center();
    f.frame(vec![Event::PointerMoved(pos)]);
    f.measuring = true;
    f.button(pos, true);
    assert_eq!(f.draft(), draft);
    assert_eq!(f.state(), before);
    f.measuring = false;
    f.button(pos, false);
    assert_eq!(f.draft(), draft, "first real release after a speculative measured press is inert");
    assert_eq!(f.state(), before);
    f.idle();
    assert_eq!(f.draft(), draft);
    f.changed_flag("check:keywordPerson");
}
#[test]
fn keyword_tag_options_create_measured_press_cannot_arm_first_restored_real_release() {
    measured_down(false);
}
#[test]
fn keyword_tag_options_edit_measured_press_cannot_arm_first_restored_real_release() {
    measured_down(true);
}
fn held_blocked(editing: bool) {
    let mut f = KeywordOptions::new();
    f.open_mode(editing);
    let before = f.state();
    let draft = f.draft();
    let pos = f.rect("check:keywordPerson").center();
    f.frame(vec![Event::PointerMoved(pos)]);
    f.button(pos, true);
    f.blocked = true;
    f.frame(vec![]);
    f.button(pos, false);
    f.blocked = false;
    f.frame(vec![]);
    assert_eq!(f.draft(), draft);
    assert_eq!(f.state(), before);
    f.changed_flag("check:keywordPerson");
}
#[test]
fn keyword_tag_options_create_live_press_cannot_complete_through_a_blocked_parent() {
    held_blocked(false);
}
#[test]
fn keyword_tag_options_edit_live_press_cannot_complete_through_a_blocked_parent() {
    held_blocked(true);
}
fn wrong_button(editing: bool) {
    let mut f = KeywordOptions::new();
    f.open_mode(editing);
    let before = f.state();
    let draft = f.draft();
    let pos = f.rect("check:keywordPerson").center();
    f.frame(vec![Event::PointerMoved(pos)]);
    for pressed in [true, false] {
        f.frame(vec![Event::PointerButton { pos, button: PointerButton::Secondary, pressed, modifiers: Modifiers::NONE }]);
    }
    f.idle();
    assert_eq!(f.draft(), draft);
    assert_eq!(f.state(), before);
    f.changed_flag("check:keywordPerson");
}
#[test]
fn keyword_tag_options_create_secondary_button_does_not_toggle_and_fresh_primary_still_works() {
    wrong_button(false);
}
#[test]
fn keyword_tag_options_edit_secondary_button_does_not_toggle_and_fresh_primary_still_works() {
    wrong_button(true);
}
fn retarget(kind: u8) {
    let mut f = KeywordOptions::new();
    if kind == 0 {
        f.edit("Places|Existing");
    } else {
        f.create(Some("Places"));
    }
    let pos = f.rect("check:keywordPerson").center();
    f.frame(vec![Event::PointerMoved(pos)]);
    f.button(pos, true);
    match kind {
        0 => f.edit("Nature|Other"),
        1 => f.create(Some("Nature")),
        _ => f.edit("Places|Existing"),
    }
    let before = f.state();
    let replacement = f.draft();
    f.button(pos, false);
    assert_eq!(f.draft(), replacement, "old release cannot edit a replacement genuine controller target");
    assert_eq!(f.state(), before);
    f.idle();
    assert_eq!(f.draft(), replacement);
    f.changed_flag("check:keywordPerson");
}
#[test]
fn keyword_tag_options_held_edit_a_release_cannot_change_actual_edit_b_draft_or_document() {
    retarget(0);
}
#[test]
fn keyword_tag_options_held_create_parent_a_release_cannot_change_actual_parent_b_draft_or_document() {
    retarget(1);
}
#[test]
fn keyword_tag_options_held_create_release_cannot_change_actual_edit_mode_draft_or_document() {
    retarget(2);
}
#[test]
fn keyword_tag_options_empty_name_real_confirm_is_refused_without_any_document_or_history_change() {
    let mut f = KeywordOptions::new();
    let before = f.state();
    f.create(Some("Places"));
    let draft = f.draft();
    f.click("button:dialogOk");
    assert_eq!(f.draft(), draft);
    assert_eq!(f.state(), before);
    f.idle();
    assert_eq!(f.state(), before);
}
#[test]
fn keyword_tag_options_duplicate_create_preserves_draft_and_complete_model_when_real_command_is_refused() {
    let mut f = KeywordOptions::new();
    let before = f.state();
    f.create(Some("Places"));
    f.fill("field:keywordTagName", "Existing");
    f.changed_flag("check:keywordPerson");
    let draft = f.draft();
    f.click("button:dialogOk");
    assert_eq!(f.draft(), draft);
    assert_eq!(f.state(), before);
    assert!(f.app.ui.dialog.is_some());
    f.idle();
    assert_eq!(f.state(), before);
}
#[test]
fn keyword_tag_options_no_op_edit_confirm_matches_native_command_and_never_replays() {
    let mut f = KeywordOptions::new();
    let mut expected = KeywordOptions::new();
    f.edit("Places|Existing");
    f.click("button:dialogOk");
    expected.app.run("keyword.edit", json!({"keyword":"Places|Existing","name":"Existing","synonyms":["old"],"includeOnExport":true,"exportContaining":true,"exportSynonyms":true,"person":false})).unwrap();
    assert!(f.app.ui.dialog.is_none());
    assert_eq!(f.state(), expected.state());
    let after = f.state();
    f.idle();
    assert_eq!(f.state(), after);
}

// Independent diagnostic only. Existing enabled/focus observations and all original test bodies remain unchanged.
impl KeywordOptions {
    fn dump_real_focus_state(&self, step: &str) {
        println!(
            "DIAG {step} native={:?} response={:?} sdk_stop={:?}",
            self.focused_response.as_ref().map(|r| r.id),
            self.focused_response,
            self.sdk_focus
        );
        if let Some(record) = &self.sdk_focus {
            println!(
                "DIAG {step} owner_aliases={:?} root_equals_native={} actual_root_has_focus={}",
                self.hits.iter().filter(|(_, h)| h.id == record.stop.id).map(|(n, _)| n).collect::<Vec<_>>(),
                self.focused_response.as_ref().is_some_and(|r| r.id == record.stop.root),
                self.native_has_focus
            );
        }
        for (alias, observed) in &self.witnessed {
            let elements = self.recorded.as_ref().unwrap().elements.iter().filter(|e| e.id == observed.id && e.part.is_none()).collect::<Vec<_>>();
            println!(
                "DIAG {step} {alias} old_observer={} facade={observed:?} native_proxy={:?} semantic={elements:?}",
                self.focused(alias),
                self.proxies.iter().find(|(n, _)| n == alias)
            );
        }
        println!("DIAG {step} native_footer_aliases={:?}", self.rects.iter().filter(|(n, _)| n.starts_with("button:dialog")).collect::<Vec<_>>());
    }
}
fn diagnose_keyword_route(editing: bool) {
    let mut f = KeywordOptions::new();
    f.open_mode(editing);
    let document = f.state();
    let draft = f.draft();
    f.dump_real_focus_state("opened");
    // Trace the full original route length plus its natural return, never changing the original journey or settling recipe.
    for step in 0..if editing { 8 } else { 10 } {
        f.key(Key::Tab);
        f.dump_real_focus_state(&format!("tab-{step}"));
        assert_eq!(f.state(), document);
        assert_eq!(f.draft(), draft);
    }
}
#[test]
fn keyword_tag_diagnostic_create_actual_sdk_owner_native_root_and_complete_tab_chain() {
    diagnose_keyword_route(false);
}
#[test]
fn keyword_tag_diagnostic_edit_actual_sdk_owner_native_root_and_complete_tab_chain() {
    diagnose_keyword_route(true);
}
#[test]
fn keyword_tag_diagnostic_disabled_add_semantics_pointer_tab_skip_and_real_selection_restore() {
    let mut f = KeywordOptions::new();
    f.app.run("library.select", json!({"ids":[]})).unwrap();
    f.create(None);
    let document = f.state();
    let draft = f.draft();
    f.dump_real_focus_state("empty-selection");
    let owner = f.witnessed.iter().find(|(n, _)| n == "check:keywordAddToSelected").unwrap().1.id;
    let node = f.recorded.as_ref().unwrap().elements.iter().find(|e| e.id == owner && e.part.is_none() && e.component == "Checkbox").unwrap();
    assert!(node.facts.as_ref().unwrap().disabled, "actual public semantic Checkbox is disabled, not its facade proxy");
    assert!(node.stop.is_none(), "actual disabled Add is absent from Tab stops");
    f.click("check:keywordAddToSelected");
    assert_eq!(f.draft(), draft);
    assert_eq!(f.state(), document);
    f.click("field:keywordTagName");
    for step in 0..8 {
        f.key(Key::Tab);
        f.dump_real_focus_state(&format!("disabled-tab-{step}"));
        assert!(f.sdk_focus.as_ref().is_none_or(|r| r.stop.id != owner));
        assert_eq!(f.draft(), draft);
        assert_eq!(f.state(), document);
    }
    f.app.run("library.select", json!({"ids":[f.ids[0]]})).unwrap();
    f.idle();
    f.dump_real_focus_state("selection-restored");
    let node = f.recorded.as_ref().unwrap().elements.iter().find(|e| e.id == owner && e.part.is_none() && e.component == "Checkbox").unwrap();
    assert!(!node.facts.as_ref().unwrap().disabled && node.stop.is_some());
    f.changed_flag("check:keywordAddToSelected");
    assert!(f.flag("check:keywordAddToSelected"));
}

fn keyword_sdk_signature(f: &KeywordOptions) -> std::collections::BTreeMap<String, egui::Id> {
    let mut aliases = vec![
        "field:keywordTagName",
        "field:keywordSynonyms",
        "check:keywordIncludeOnExport",
        "check:keywordExportContaining",
        "check:keywordExportSynonyms",
        "check:keywordPerson",
        "button:dialogCancel",
        "button:dialogOk",
    ];
    if let Some(Dialog::KeywordTag { editing, parent, .. }) = &f.app.ui.dialog {
        if editing.is_none() {
            aliases.push("check:keywordAddToSelected");
            if parent.is_some() {
                aliases.push("check:keywordInside");
            }
        }
    } else {
        panic!("actual KeywordTag controller required");
    }
    let record = f.recorded.as_ref().unwrap();
    let mut ids = std::collections::BTreeMap::new();
    for alias in aliases {
        let hits = f.hits.iter().filter(|(n, _)| n == alias).collect::<Vec<_>>();
        assert_eq!(hits.len(), 1, "one actual public alias {alias}");
        let owner = hits[0].1.id;
        let role = if alias.starts_with("field:") {
            "TextInput"
        } else if alias.starts_with("check:") {
            "CheckBox"
        } else {
            "Button"
        };
        let nodes = record
            .elements
            .iter()
            .filter(|e| e.id == owner && e.part.is_none() && e.facts.as_ref().is_some_and(|a| a.role == role))
            .collect::<Vec<_>>();
        assert_eq!(nodes.len(), 1, "one actual SDK {role} owner for {alias}");
        let node = nodes[0];
        let facts = node.facts.as_ref().unwrap();
        let disabled = alias == "check:keywordAddToSelected" && f.app.session.selection.ids.is_empty();
        assert_eq!(facts.disabled, disabled);
        assert_eq!(facts.interactive, !disabled);
        assert_eq!(node.stop.is_some(), !disabled);
        if alias == "field:keywordTagName" {
            assert_eq!(facts.name.as_deref(), Some(crate::i18n::tr("Keyword Name")));
        }
        if alias == "field:keywordSynonyms" {
            assert_eq!(facts.name.as_deref(), Some(crate::i18n::tr("Synonyms")));
        }
        ids.insert(alias.to_owned(), owner);
    }
    assert_eq!(ids.values().collect::<std::collections::HashSet<_>>().len(), ids.len(), "real controls have distinct owners");
    ids
}
fn keyword_sdk_names_metadata_workflow(edit: bool) {
    let mut f = KeywordOptions::new();
    if edit {
        f.edit("Places|Existing");
    } else {
        f.create(Some("Places"));
    }
    assert!(f.focused("field:keywordTagName"), "real initial SDK Name owner");
    let document = f.state();
    let draft = f.draft();
    let initial = keyword_sdk_signature(&f);
    for locale in [crate::i18n::Locale::En, crate::i18n::Locale::De] {
        crate::i18n::set_language(locale);
        f.idle();
        assert_eq!(keyword_sdk_signature(&f), initial, "translation is not identity");
        assert_eq!(f.state(), document);
        assert_eq!(f.draft(), draft);
    }
    f.measuring = true;
    f.frame(vec![]);
    assert!(initial.keys().all(|key| !f.hits.iter().any(|(n, _)| n == key)), "measurement publishes no real owned target");
    assert!(
        f.recorded.as_ref().unwrap().elements.iter().all(|e| !matches!(e.component, "TextField" | "Checkbox" | "Button")),
        "measurement publishes no live SDK control or focus stop"
    );
    assert_eq!(f.state(), document);
    assert_eq!(f.draft(), draft);
    f.measuring = false;
    f.idle();
    assert_eq!(keyword_sdk_signature(&f), initial);
    f.fill("field:keywordTagName", "Actual SDK draft name");
    f.changed_flag("check:keywordPerson");
    assert_eq!(keyword_sdk_signature(&f), initial, "draft text and flags are not identity");
    assert_eq!(f.state(), document);
    f.app.run("library.select", json!({"ids":[]})).unwrap();
    f.idle();
    assert_eq!(keyword_sdk_signature(&f), initial, "selection changes disabled semantics, not identity");
    let after_selection = f.state();
    if edit {
        f.edit("Nature|Other");
    } else {
        f.create(Some("Nature"));
    }
    let retargeted = keyword_sdk_signature(&f);
    assert_eq!(initial.keys().collect::<Vec<_>>(), retargeted.keys().collect::<Vec<_>>());
    for (alias, id) in initial {
        assert_ne!(retargeted[&alias], id, "real caller identity changes with edit target or creation parent: {alias}");
    }
    assert_eq!(f.state(), after_selection);
}
#[test]
fn keyword_tag_sdk_create_real_field_labels_locale_identity_and_measurement() {
    keyword_sdk_names_metadata_workflow(false);
}
#[test]
fn keyword_tag_sdk_edit_real_field_labels_locale_identity_and_measurement() {
    keyword_sdk_names_metadata_workflow(true);
}

#[test]
fn keyword_tag_sdk_create_name_return_confirms_once_with_exact_command_and_undo_redo() {
    let mut f = KeywordOptions::new();
    let mut expected = KeywordOptions::new();
    let before = f.state();
    let undo = f.app.session.undo.len();
    f.create(Some("Places"));
    f.fill("field:keywordTagName", " Alpine ");
    f.fill("field:keywordSynonyms", "ridge, summit, , ridge");
    f.changed_flag("check:keywordAddToSelected");
    for alias in FLAGS {
        f.changed_flag(alias);
    }
    assert!(f.flag("check:keywordInside"));
    assert_eq!(f.state(), before);
    f.click("field:keywordTagName");
    assert!(f.focused("field:keywordTagName"), "real SDK text owner before Return");
    f.key(Key::Enter);
    assert!(f.app.ui.dialog.is_none(), "actual field Return confirms through the original parent");
    let mut payload = expected_flags();
    payload["name"] = json!(" Alpine ");
    payload["parent"] = json!("Places");
    payload["synonyms"] = json!(["ridge", "summit", "ridge"]);
    payload["addToSelected"] = json!(true);
    expected.app.run("keyword.create", payload).unwrap();
    assert_eq!(f.app.session.undo.len(), undo + 1, "one real create+metadata+photo batch");
    assert!(f.app.session.catalog.photos().all(|p| p.meta.keywords.iter().any(|k| k == "Places|Alpine")));
    compare_history(&mut f, &mut expected, &before);
}

#[test]
fn keyword_tag_sdk_create_synonyms_return_confirms_once_with_exact_command_and_undo_redo() {
    let mut f = KeywordOptions::new();
    let mut expected = KeywordOptions::new();
    let before = f.state();
    let undo = f.app.session.undo.len();
    f.create(Some("Places"));
    f.fill("field:keywordTagName", " Alpine ");
    f.fill("field:keywordSynonyms", "ridge, summit, , ridge");
    f.changed_flag("check:keywordAddToSelected");
    for alias in FLAGS {
        f.changed_flag(alias);
    }
    assert!(f.flag("check:keywordInside"));
    assert_eq!(f.state(), before);
    f.click("field:keywordSynonyms");
    assert!(f.focused("field:keywordSynonyms"), "real SDK text owner before Return");
    f.key(Key::Enter);
    assert!(f.app.ui.dialog.is_none(), "actual field Return confirms through the original parent");
    let mut payload = expected_flags();
    payload["name"] = json!(" Alpine ");
    payload["parent"] = json!("Places");
    payload["synonyms"] = json!(["ridge", "summit", "ridge"]);
    payload["addToSelected"] = json!(true);
    expected.app.run("keyword.create", payload).unwrap();
    assert_eq!(f.app.session.undo.len(), undo + 1, "one real create+metadata+photo batch");
    assert!(f.app.session.catalog.photos().all(|p| p.meta.keywords.iter().any(|k| k == "Places|Alpine")));
    compare_history(&mut f, &mut expected, &before);
}

#[test]
fn keyword_tag_sdk_edit_name_return_confirms_once_with_exact_command_and_undo_redo() {
    let mut f = KeywordOptions::new();
    let mut expected = KeywordOptions::new();
    for fixture in [&mut f, &mut expected] {
        fixture.app.run("library.filter", json!({"keyword":"Places|Existing"})).unwrap();
    }
    let before = f.state();
    let undo = f.app.session.undo.len();
    f.edit("Places|Existing");
    f.fill("field:keywordTagName", "Renamed");
    f.fill("field:keywordSynonyms", "ridge, summit");
    for alias in FLAGS {
        f.changed_flag(alias);
    }
    assert_eq!(f.state(), before);
    f.click("field:keywordTagName");
    assert!(f.focused("field:keywordTagName"), "real SDK text owner before Return");
    f.key(Key::Enter);
    assert!(f.app.ui.dialog.is_none(), "actual field Return confirms through the original parent");
    let mut payload = expected_flags();
    payload["keyword"] = json!("Places|Existing");
    payload["name"] = json!("Renamed");
    payload["synonyms"] = json!(["ridge", "summit"]);
    expected.app.run("keyword.edit", payload).unwrap();
    assert_eq!(f.app.ui.keyword_list_selected.as_deref(), Some("Places|Renamed"), "real caller follows the renamed keyword");
    assert_eq!(f.app.session.filter.keyword.as_deref(), Some("Places|Renamed"), "the actual active library filter follows the edit command");
    assert_eq!(f.app.session.undo.len(), undo + 1);
    assert!(!f.app.session.catalog.has_keyword("Places|Existing"));
    compare_history(&mut f, &mut expected, &before);
}

#[test]
fn keyword_tag_sdk_edit_synonyms_return_confirms_once_with_exact_command_and_undo_redo() {
    let mut f = KeywordOptions::new();
    let mut expected = KeywordOptions::new();
    for fixture in [&mut f, &mut expected] {
        fixture.app.run("library.filter", json!({"keyword":"Places|Existing"})).unwrap();
    }
    let before = f.state();
    let undo = f.app.session.undo.len();
    f.edit("Places|Existing");
    f.fill("field:keywordTagName", "Renamed");
    f.fill("field:keywordSynonyms", "ridge, summit");
    for alias in FLAGS {
        f.changed_flag(alias);
    }
    assert_eq!(f.state(), before);
    f.click("field:keywordSynonyms");
    assert!(f.focused("field:keywordSynonyms"), "real SDK text owner before Return");
    f.key(Key::Enter);
    assert!(f.app.ui.dialog.is_none(), "actual field Return confirms through the original parent");
    let mut payload = expected_flags();
    payload["keyword"] = json!("Places|Existing");
    payload["name"] = json!("Renamed");
    payload["synonyms"] = json!(["ridge", "summit"]);
    expected.app.run("keyword.edit", payload).unwrap();
    assert_eq!(f.app.ui.keyword_list_selected.as_deref(), Some("Places|Renamed"), "real caller follows the renamed keyword");
    assert_eq!(f.app.session.filter.keyword.as_deref(), Some("Places|Renamed"), "the actual active library filter follows the edit command");
    assert_eq!(f.app.session.undo.len(), undo + 1);
    assert!(!f.app.session.catalog.has_keyword("Places|Existing"));
    compare_history(&mut f, &mut expected, &before);
}
