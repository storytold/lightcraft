//! Copyright-status adapter contract. No file-backed photos, loaders, sidecars or app frame.
use super::*;
use craft::ui::runtime::{inspect, overlay};
use egui::{Event, Key, Pos2, Rect};
use lightcraft_catalog::{CopyrightStatus as Status, Op, Photo, Source};

struct Fixture {
    app: LightcraftApp,
    ctx: egui::Context,
    ids: Vec<PhotoId>,
    targets: Vec<PhotoId>,
    elements: Vec<inspect::Element>,
    hits: Vec<(String, Rect)>,
    frame: u32,
    whole_panel: bool,
    enabled: bool,
    response: Option<egui::Id>,
    text: Vec<String>,
}

impl Fixture {
    fn new(statuses: [Status; 3], targets: &[usize]) -> Self {
        let mut session = lightcraft_engine::Session::new();
        let mut ids = Vec::new();
        for (index, status) in statuses.into_iter().enumerate() {
            let id = session.catalog.alloc_photo_id();
            let mut photo = Photo::new(id, Source::Demo { scene: index as u32 }, "copyright-fixture", "DEMO", 8, 8, "2026-01-01");
            photo.meta.copyright_status = status;
            photo.meta.title = format!("unchanged-{index}");
            session.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
            ids.push(id);
        }
        session.selection = lightcraft_engine::Selection::single(ids[0]);
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        ctx.enable_accesskit();
        let mut fixture = Self {
            app: LightcraftApp::new(session, crate::Services::default()),
            ctx,
            targets: targets.iter().map(|&at| ids[at]).collect(),
            ids,
            elements: Vec::new(),
            hits: Vec::new(),
            frame: 0,
            whole_panel: false,
            enabled: true,
            response: None,
            text: Vec::new(),
        };
        fixture.app.install_framework(&fixture.ctx);
        fixture.idle();
        fixture
    }

    fn values(&self) -> Vec<Status> {
        self.ids.iter().map(|&id| self.app.session.catalog.photo(id).unwrap().meta.copyright_status).collect()
    }

    fn show(&mut self, events: Vec<Event>) {
        self.show_checked(events, 0, |_, _| {});
    }
    fn show_checked(&mut self, events: Vec<Event>, pass: u8, mut after: impl FnMut(&egui::Ui, &LightcraftApp)) {
        let raw = crate::headless::HeadlessView::raw_input(
            vec2(400.0, if self.whole_panel { 1500.0 } else { 260.0 }),
            1.0,
            f64::from(self.frame) / 60.0,
            events,
        );
        self.frame += 1;
        let mut out = self.ctx.run_ui(raw, |ui| {
            let current = self.app.session.catalog.photo(self.ids[0]).unwrap().meta.copyright_status;
            let mixed = self.targets.iter().any(|&id| self.app.session.catalog.photo(id).unwrap().meta.copyright_status != current);
            let mut draw = |ui: &mut egui::Ui| {
                let (_, record) = inspect::record(ui, egui::Id::unique("copyright-contract"), |ui| {
                    if self.whole_panel {
                        info(&mut self.app, ui, self.ids[0]);
                    } else {
                        copyright_status(&mut self.app, ui, current, mixed, &self.targets);
                    }
                });
                self.elements = record.iter().flat_map(|record| record.elements.iter().cloned()).collect();
                let (rects, hits) = crate::widgets::take_registries(ui.ctx());
                self.hits = rects;
                self.response = hits.iter().find(|(name, _)| name == "field:copyrightStatus").map(|(_, r)| r.id);
                if pass != 0 {
                    assert!(self.response.is_none() && !self.hits.iter().any(|(name, _)| name == "field:copyrightStatus"));
                }
                after(ui, &self.app);
            };
            ui.add_enabled_ui(self.enabled, |ui| match pass {
                0 => draw(ui),
                1 => {
                    let ctx = ui.ctx().clone();
                    craft::ui::runtime::measure::run(&ctx, || draw(ui));
                }
                2 => {
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(ui.max_rect()).sizing_pass().invisible());
                    draw(&mut child);
                }
                _ => unreachable!(),
            });
        });
        out.textures_delta.clear();

        fn words(shape: &egui::Shape, text: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(shape) => text.push(shape.galley.job.text.clone()),
                egui::Shape::Vec(shapes) => shapes.iter().for_each(|shape| words(shape, text)),
                _ => {}
            }
        }
        self.text.clear();
        for shape in out.shapes {
            words(&shape.shape, &mut self.text);
        }
    }

    fn idle(&mut self) {
        for _ in 0..4 {
            self.show(Vec::new());
        }
    }

    fn key(&mut self, key: Key) {
        self.show(vec![Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers: Default::default() }]);
        self.show(vec![Event::Key { key, physical_key: Some(key), pressed: false, repeat: false, modifiers: Default::default() }]);
        self.idle();
    }

    fn click(&mut self, at: Pos2) {
        self.show(vec![Event::PointerMoved(at)]);
        for pressed in [true, false] {
            self.show(vec![Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }]);
        }
        self.idle();
    }

    fn opener(&self) -> Rect {
        self.hits.iter().find(|(id, _)| id == "field:copyrightStatus").unwrap().1
    }

    fn option(&self, label: &str) -> Rect {
        self.elements
            .iter()
            .find(|element| element.facts.as_ref().is_some_and(|facts| facts.name.as_deref() == Some(label) && facts.role != "ComboBox"))
            .unwrap_or_else(|| panic!("missing option {label}: {:?}", self.elements))
            .rect
    }

    fn open(&self) -> bool {
        overlay::any_open(&self.ctx, overlay::OpenFilter::Any)
    }

    fn pick(&mut self, label: &str) {
        self.click(self.opener().center());
        assert!(self.open());
        self.click(self.option(label).center());
        assert!(!self.open());
    }

    fn unchanged_titles(&self) {
        for (index, id) in self.ids.iter().enumerate() {
            assert_eq!(self.app.session.catalog.photo(*id).unwrap().meta.title, format!("unchanged-{index}"));
        }
    }
}

#[test]
fn pointer_mixed_pick_targets_only_once_then_undo_restores_divergent_values() {
    let original = [Status::Unknown, Status::Copyrighted, Status::PublicDomain];
    let mut f = Fixture::new(original, &[0, 1]);
    assert!(f.elements.iter().any(|element| {
        element.facts.as_ref().is_some_and(|facts| facts.role == "ComboBox" && facts.name.as_deref() == Some("Copyright Status"))
    }));
    assert!(f.app.session.undo.is_empty());
    assert!(f.text.iter().any(|text| text == "Mixed"));
    f.pick("Public Domain");
    assert_eq!(f.values(), vec![Status::PublicDomain; 3]);
    assert_eq!(f.app.session.undo.len(), 1, "one command for the whole selection");
    f.unchanged_titles();
    f.app.session.undo_step().unwrap();
    assert_eq!(f.values(), original);
    assert!(f.app.session.undo.is_empty());
}

#[test]
fn pointer_can_apply_each_exact_status_and_reselect_is_noop() {
    for status in Status::ALL {
        let mut f = Fixture::new([Status::PublicDomain; 3], &[0]);
        f.pick(status.label());
        assert_eq!(f.values(), vec![status, Status::PublicDomain, Status::PublicDomain]);
        let n = f.app.session.undo.len();
        assert_eq!(n, usize::from(status != Status::PublicDomain));
        f.pick(status.label());
        assert_eq!(f.app.session.undo.len(), n);
        f.unchanged_titles();
    }
}

#[test]
fn mixed_pick_equal_to_first_photo_still_updates_other_target() {
    let mut f = Fixture::new([Status::Unknown, Status::Copyrighted, Status::PublicDomain], &[0, 1]);
    f.pick("Unknown");
    assert_eq!(f.values(), vec![Status::Unknown, Status::Unknown, Status::PublicDomain]);
    assert_eq!(f.app.session.undo.len(), 1);
}

#[test]
fn unforced_tab_enter_end_enter_commits_once_and_undo_works() {
    let mut f = Fixture::new([Status::Unknown; 3], &[0, 1]);
    f.key(Key::Tab); // No request_focus, focus override, command invocation or direct binding mutation.
    f.key(Key::Enter);
    assert!(f.open(), "Tab must naturally reach the SDK field");
    f.key(Key::End);
    assert_eq!(f.values(), vec![Status::Unknown; 3], "navigation is only an SDK draft");
    assert!(f.app.session.undo.is_empty());
    f.key(Key::Enter);
    assert!(!f.open());
    assert_eq!(f.values(), vec![Status::PublicDomain, Status::PublicDomain, Status::Unknown]);
    assert_eq!(f.app.session.undo.len(), 1);
    f.app.session.undo_step().unwrap();
    assert_eq!(f.values(), vec![Status::Unknown; 3]);
}

#[test]
fn keyboard_escape_drops_draft_and_reopen_has_original_value() {
    let mut f = Fixture::new([Status::Copyrighted; 3], &[0, 1]);
    let original = metadata_state(&f.app, &f.ids);
    let selection = f.app.session.selection.clone();
    f.key(Key::Tab);
    let opener = f.response.expect("one registered copyright field");
    assert_eq!(f.ctx.memory(|m| m.focused()), Some(opener), "Tab did not land on the field");
    f.key(Key::Space);
    assert!(f.open());
    f.key(Key::End);
    assert_eq!(metadata_state(&f.app, &f.ids), original, "navigation is a controller draft, not a document command");
    f.key(Key::Escape);
    assert_eq!(f.response, Some(opener), "the field identity changed after cancel");
    assert_eq!(f.ctx.memory(|m| m.focused()), Some(opener), "Escape closed the list without returning keyboard focus to the same field");
    assert!(!f.open());
    assert_eq!(f.values(), vec![Status::Copyrighted; 3]);
    assert!(f.app.session.undo.is_empty());
    f.key(Key::Enter);
    assert!(f.open());
    f.key(Key::Enter); // Reopening must not retain the cancelled Public Domain draft.
    assert!(!f.open());
    assert_eq!(f.values(), vec![Status::Copyrighted; 3]);
    assert_eq!(metadata_state(&f.app, &f.ids), original, "accepting the original common value is completely quiet");
    assert!(f.app.session.undo.is_empty());

    // The same field must still open and apply a real subsequent keyboard pick exactly once.
    f.key(Key::Space);
    assert!(f.open());
    f.key(Key::End);
    assert_eq!(metadata_state(&f.app, &f.ids), original);
    f.key(Key::Enter);
    assert!(!f.open());
    assert_eq!(f.values(), [Status::PublicDomain, Status::PublicDomain, Status::Copyrighted]);
    assert_eq!(f.app.session.undo.len(), 1);
    assert_eq!(f.app.session.journal.len(), 1);
    assert_eq!(
        f.app.session.journal[0],
        ("photo.setMeta".into(), json!({"ids":f.targets.iter().map(|id|id.0).collect::<Vec<_>>(),"copyrightStatus":Status::PublicDomain.id()}))
    );
    f.app.session.undo_step().unwrap();
    assert_eq!(metadata_state(&f.app, &f.ids)["photos"], original["photos"]);
    assert_eq!(f.app.session.selection, selection);
    assert!(f.app.session.undo.is_empty());
    f.app.session.redo_step().unwrap();
    assert_eq!(f.values(), [Status::PublicDomain, Status::PublicDomain, Status::Copyrighted]);
    assert_eq!(f.app.session.undo.len(), 1);
    assert_eq!(f.app.session.selection, selection);
}

#[test]
fn outside_pointer_dismissal_drops_keyboard_draft_without_history() {
    let mut f = Fixture::new([Status::Copyrighted; 3], &[0]);
    f.click(f.opener().center());
    assert!(f.open());
    f.key(Key::End);
    f.click(egui::pos2(380.0, 245.0));
    assert!(!f.open());
    assert_eq!(f.values(), vec![Status::Copyrighted; 3]);
    assert!(f.app.session.undo.is_empty());
}

#[test]
fn empty_targets_cannot_open_or_edit_even_with_pointer_and_keys() {
    let mut f = Fixture::new([Status::Unknown; 3], &[]);
    f.click(f.opener().center());
    for key in [Key::Tab, Key::Enter, Key::End, Key::Enter] {
        f.key(key);
    }
    assert!(!f.open());
    assert_eq!(f.values(), vec![Status::Unknown; 3]);
    assert!(f.app.session.undo.is_empty());
}

#[test]
fn both_sizing_modes_are_document_and_registry_inert() {
    let mut f = Fixture::new([Status::Unknown; 3], &[0, 1]);
    f.app.install_framework(&f.ctx);
    let mut out = f.ctx.run_ui(egui::RawInput::default(), |ui| {
        crate::widgets::take_registries(&f.ctx);
        let mut sizing = ui.new_child(egui::UiBuilder::new().sizing_pass().invisible());
        assert!(sizing.is_sizing_pass());
        copyright_status(&mut f.app, &mut sizing, Status::Unknown, false, &f.targets);
        let (rects, hits) = crate::widgets::take_registries(&f.ctx);
        assert!(rects.is_empty() && hits.is_empty());
        craft::ui::runtime::measure::run(&f.ctx, || {
            copyright_status(&mut f.app, ui, Status::Unknown, true, &f.targets);
        });
        let (rects, hits) = crate::widgets::take_registries(&f.ctx);
        assert!(rects.is_empty() && hits.is_empty());
    });
    out.textures_delta.clear();
    assert_eq!(f.values(), vec![Status::Unknown; 3]);
    assert!(f.app.session.undo.is_empty());
}

#[test]
fn mixed_word_keeps_the_app_translation_without_faking_a_selected_status() {
    struct Language(crate::i18n::Locale);
    impl Drop for Language {
        fn drop(&mut self) {
            crate::i18n::set_language(self.0);
        }
    }
    let _restore = Language(crate::i18n::language());
    crate::i18n::set_language(crate::i18n::Locale::parse("fr").unwrap());
    let f = Fixture::new([Status::Unknown, Status::Copyrighted, Status::PublicDomain], &[0, 1]);
    let translated = crate::i18n::tr("Mixed");
    assert!(f.text.iter().any(|text| text == translated), "{translated}: {:?}", f.text);
    assert_eq!(f.values(), vec![Status::Unknown, Status::Copyrighted, Status::PublicDomain]);
    assert!(f.app.session.undo.is_empty());
}

#[test]
fn info_scope_controls_target_one_then_selected_pair_then_undo_divergent_statuses() {
    let original = [Status::Unknown, Status::Copyrighted, Status::PublicDomain];
    let mut f = Fixture::new(original, &[0, 1]);
    let a = f.ids[0];
    let b = f.ids[1];
    f.app.session.execute("library.select", &json!({"ids":[a.0,b.0],"active":a.0})).unwrap();
    f.app.ui.settings.info_selected_photos = true;
    f.whole_panel = true;
    f.idle();
    let selection = f.app.session.selection.clone();
    let base_undo = f.app.session.undo.len();
    let scope = |f: &mut Fixture, selected: bool| {
        let name = if selected { "button:infoFor-selected" } else { "button:infoFor-target" };
        let at = f.hits.iter().find(|(n, _)| n == name).unwrap().1.center();
        let revision = f.app.session.catalog.revision;
        let undo = f.app.session.undo.len();
        f.click(at);
        assert_eq!(f.app.ui.settings.info_selected_photos, selected);
        assert_eq!(f.app.session.catalog.revision, revision);
        assert_eq!(f.app.session.undo.len(), undo);
    };
    scope(&mut f, false);
    f.pick(Status::PublicDomain.label());
    assert_eq!(f.values(), [Status::PublicDomain, Status::Copyrighted, Status::PublicDomain]);
    assert_eq!(f.app.session.journal.last(), Some(&("photo.setMeta".into(), json!({"ids":[a.0],"copyrightStatus":Status::PublicDomain.id()}))));
    scope(&mut f, true);
    assert!(f.text.iter().any(|s| s == crate::i18n::tr("Mixed")));
    f.pick(Status::Unknown.label());
    assert_eq!(f.values(), [Status::Unknown, Status::Unknown, Status::PublicDomain]);
    assert_eq!(f.app.session.journal.last(), Some(&("photo.setMeta".into(), json!({"ids":[a.0,b.0],"copyrightStatus":Status::Unknown.id()}))));
    let journal = f.app.session.journal.clone();
    let revision = f.app.session.catalog.revision;
    f.pick(Status::Unknown.label());
    assert_eq!(f.app.session.journal, journal);
    assert_eq!(f.app.session.catalog.revision, revision);
    assert_eq!(f.app.session.undo.len(), base_undo + 2);
    f.app.run("edit.undo", json!({})).unwrap();
    assert_eq!(f.values(), [Status::PublicDomain, Status::Copyrighted, Status::PublicDomain]);
    f.app.run("edit.undo", json!({})).unwrap();
    assert_eq!(f.values(), original);
    assert_eq!(f.app.session.selection, selection);
    f.unchanged_titles();
}

fn metadata_state(app: &LightcraftApp, ids: &[PhotoId]) -> serde_json::Value {
    json!({"photos":ids.iter().map(|id|app.session.catalog.photo(*id).unwrap()).collect::<Vec<_>>(),"selection":app.session.selection,"journal":app.session.journal,"undo":app.session.undo.len(),"redo":app.session.redo.len(),"revision":app.session.catalog.revision,"scope":app.ui.settings.info_selected_photos})
}
fn measured_opener(pass: u8) {
    let mut f = Fixture::new([Status::Unknown, Status::Copyrighted, Status::PublicDomain], &[0, 1]);
    let owner = f.response.unwrap();
    let at = f.opener().center();
    let ids = f.ids.clone();
    let selection = f.app.session.selection.clone();
    let before = metadata_state(&f.app, &ids);
    let ptr = |pressed| Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
    f.show(vec![Event::PointerMoved(at)]);
    f.show_checked(vec![ptr(true)], 0, |ui, app| {
        assert!(ui.ctx().read_response(owner).unwrap().is_pointer_button_down_on());
        assert_eq!(metadata_state(app, &ids), before);
    });
    // Select FACE.toggles opens on the ordinary press. The speculative release must not
    // create a second interaction, edit metadata or replace this pre-existing popup.
    assert!(f.open(), "the real pointer press opens the SDK list before measurement");
    let mut n = 0;
    f.show_checked(vec![ptr(false)], pass, |ui, app| {
        n += 1;
        assert_eq!(ui.is_sizing_pass(), pass == 2);
        assert_eq!(craft::ui::runtime::measure::active(ui.ctx()), pass == 1);
        assert_eq!(metadata_state(app, &ids), before);
    });
    assert!(n > 0);
    assert!(f.elements.is_empty(), "the speculative pass constructs no interactive controller or popup rows");
    // Only the measured child was drawn in that frame. Its registry is empty; the next
    // ordinary draw must resume the existing open slot, not treat release as a new pick.
    f.idle();
    assert!(f.open(), "the original popup resumes after the speculative-only frame");
    assert_eq!(metadata_state(&f.app, &ids), before);
    assert_eq!(f.response, Some(owner));
    f.key(Key::Escape);
    assert!(!f.open(), "dismiss the pre-existing popup without applying a draft");
    assert_eq!(metadata_state(&f.app, &ids), before);
    f.pick("Unknown");
    assert_eq!(f.values(), [Status::Unknown, Status::Unknown, Status::PublicDomain]);
    assert_eq!(f.app.session.undo.len(), 1);
    assert_eq!(f.app.session.journal.len(), 1, "one accepted command, not a speculative release and a real pick");
    assert_eq!(
        f.app.session.journal[0],
        ("photo.setMeta".into(), json!({"ids":f.targets.iter().map(|id|id.0).collect::<Vec<_>>(),"copyrightStatus":Status::Unknown.id()}))
    );
    f.app.session.undo_step().unwrap();
    assert_eq!(metadata_state(&f.app, &ids)["photos"], before["photos"], "Undo restores every photo, including the untouched third target");
    assert_eq!(f.app.session.selection, selection);
    assert!(f.app.session.undo.is_empty());
}
#[test]
fn copyright_sdk_measured_release_then_same_opener_edits_the_pair() {
    measured_opener(1);
}
#[test]
fn copyright_native_sizing_release_then_same_opener_edits_the_pair() {
    measured_opener(2);
}
#[test]
fn disabled_parent_rejects_pointer_and_keyboard_then_recovers() {
    let mut f = Fixture::new([Status::Unknown; 3], &[0, 1]);
    let before = metadata_state(&f.app, &f.ids);
    f.enabled = false;
    f.idle();
    let owner = f.response.unwrap();
    f.click(f.opener().center());
    for k in [Key::Tab, Key::Enter, Key::End, Key::Enter] {
        f.key(k);
    }
    assert!(!f.open());
    assert_eq!(metadata_state(&f.app, &f.ids), before);
    f.enabled = true;
    f.idle();
    assert_eq!(f.response, Some(owner));
    f.pick("Copyrighted");
    assert_eq!(f.values(), [Status::Copyrighted, Status::Copyrighted, Status::Unknown]);
    assert_eq!(f.app.session.undo.len(), 1);
}
