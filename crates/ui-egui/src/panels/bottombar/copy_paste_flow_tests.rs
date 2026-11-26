//! Native bottom-bar Copy/Paste + gear baseline. No image frame, writer, renderer or external services.
use crate::{
    LightcraftApp, Services,
    headless::HeadlessView,
    state::{Dialog, ViewMode},
};
use egui::{Event, Id, Key, Modifiers, PointerButton, Pos2, Rect, Response};
use lightcraft_catalog::{Flag, HistoryStep, Op, Photo, PhotoId, Source, Version};
use lightcraft_develop::{DevelopSettings, SettingsGroup};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};

const PRIMARY: &str = "button:copySettings";
const GEAR: &str = "icon:copyGear";
const OK: &str = "button:dialogOk";
const CANCEL: &str = "button:dialogCancel";
const VIEWPORT: egui::Vec2 = egui::Vec2::new(1200.0, 900.0);
const CLOCK: &str = "2026-11-21T12:00:00";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pass {
    Real,
    Disabled,
    Measure,
    Sizing,
}
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
    pass: Pass,
    rects: BTreeMap<String, Rect>,
    hits: BTreeMap<String, crate::widgets::WidgetHit>,
    responses: Vec<Response>,
    focused: Option<Response>,
    clicked: Option<Id>,
    owners: BTreeMap<String, Id>,
    painted: Vec<(String, Rect)>,
    initial: BTreeMap<u64, Photo>,
    stable: Value,
    pending: Option<Value>,
    inverse: Option<Op>,
    old_language: crate::i18n::Locale,
}
fn default_groups() -> Vec<SettingsGroup> {
    use SettingsGroup::*;
    // Explicitly frozen native default groups, including the RedEye exclusion missing in old help.
    vec![
        Profile,
        Treatment,
        WhiteBalance,
        Light,
        ToneCurve,
        Color,
        ColorMixer,
        ColorGrading,
        Effects,
        Vignette,
        Grain,
        Detail,
        Optics,
        Geometry,
        LensBlur,
        Calibration,
    ]
}
fn names(groups: &[SettingsGroup]) -> Vec<String> {
    groups.iter().map(|g| serde_json::to_value(g).unwrap().as_str().unwrap().to_owned()).collect()
}
impl Review {
    fn new() -> Self {
        let old_language = crate::i18n::language();
        crate::i18n::set_language(crate::i18n::Locale::En);
        let mut session = lightcraft_engine::Session::new();
        session.clock = Box::new(|| CLOCK.into());
        let mut initial = BTreeMap::new();
        for id in 101..=105 {
            let mut photo = Photo::new(PhotoId(id), Source::Demo { scene: 0 }, &format!("transfer-{id}.jpg"), "JPEG", 64, 48, CLOCK);
            photo.flag = if id % 2 == 0 { Flag::Reject } else { Flag::None };
            photo.rating = (id % 5) as u8;
            photo.meta.title = format!("Untouched title {id}");
            photo.meta.caption = format!("Untouched caption {id}");
            photo.meta.keywords = vec![format!("untouched-{id}")];
            let mut settings = DevelopSettings::default();
            settings.light.exposure = (id - 100) as f64 * 0.25;
            settings.light.contrast = (id - 100) as f64 * 2.0;
            settings.color.vibrance = (id - 100) as f64 * 9.0;
            settings.effects.clarity = (id - 100) as f64 * 3.0;
            settings.crop.geometry.angle = (id - 100) as f64;
            photo.develop = Arc::new(settings);
            photo.edited = Some(format!("2026-11-20T10:00:0{}", id - 100));
            photo.history = vec![HistoryStep { label: format!("Existing {id}"), settings: photo.develop.clone() }];
            // Existing saved versions prevent unrelated automatic-version creation on leaving
            // these already-edited sources. They are real catalog data, retained by every guard.
            photo.versions = vec![Version { name: format!("Saved {id}"), created: CLOCK.into(), settings: photo.develop.clone(), auto: false }];
            initial.insert(id, photo.clone());
            session.commit("Copy/Paste fixture", Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        }
        assert_eq!(session.copy_groups, default_groups());
        session.execute("library.select", &json!({"ids":[101,102],"active":101})).unwrap();
        session.recent_keywords = vec!["untouched recent".into()];
        let mut app = LightcraftApp::new(session, Services { local_places: Some(vec![]), png: None, ..Default::default() });
        app.set_simulated_clock(Some(0.0));
        app.ui.auto_advance = false;
        app.run("view.photoGrid", json!({})).unwrap();
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        let mut r = Self {
            app,
            ctx,
            tick: 0,
            pass: Pass::Real,
            rects: BTreeMap::new(),
            hits: BTreeMap::new(),
            responses: vec![],
            focused: None,
            clicked: None,
            owners: BTreeMap::new(),
            painted: vec![],
            initial,
            stable: Value::Null,
            pending: None,
            inverse: None,
            old_language,
        };
        r.stable = r.snapshot();
        r.idle();
        assert!(r.rects.contains_key(PRIMARY) && r.rects.contains_key(GEAR), "actual native transfer controls must fit the fixed ordinary viewport");
        assert_eq!(r.rect(PRIMARY).height(), 30.0);
        assert_eq!(r.rect(GEAR).size(), egui::vec2(30.0, 30.0));
        r.caption("Copy Edit Settings");
        r.observe("opened-native-transfer-parent");
        r
    }
    fn history(entries: &[lightcraft_engine::UndoEntry]) -> Value {
        json!(entries.iter().map(|e| json!({"label":e.label,"op":e.op,"folder":format!("{:?}",e.folder)})).collect::<Vec<_>>())
    }
    fn snapshot(&self) -> Value {
        let s = &self.app.session;
        // UiState intentionally skips transient fields in saved preferences. Observe the
        // actual dialog/toast/Compare state explicitly, and guard every remaining UiState
        // field with a Debug projection of a CLONE. No application state is overwritten.
        let mut ui = json!(self.app.ui);
        ui["dialog"] = json!(self.app.ui.dialog);
        ui["toast"] = json!(self.app.ui.toast);
        ui["compare"] = json!(self.app.ui.compare);
        let mut ui_rest = self.app.ui.clone();
        ui_rest.dialog = None;
        ui_rest.toast = None;
        ui_rest.compare = None;
        ui_rest.view = ViewMode::PhotoGrid;
        json!({"uiRest":format!("{ui_rest:?}"),"catalog":s.catalog,"revision":s.catalog.revision,"undo":Self::history(&s.undo),"redo":Self::history(&s.redo),
            "commits":s.commits(),"journal":s.journal,"recent":s.recent_keywords,"ui":ui,
            "selection":s.selection,"previousActive":s.previous_active,"clipboard":s.clipboard,"copyGroups":s.copy_groups,
            "source":s.source,"filter":s.filter,"sort":s.sort,"autoSync":s.auto_sync,
            "activeMask":s.active_mask,"activeSpot":s.active_spot,"toolEffects":s.mask_tool_effects,
            "lastMerge":s.last_merge,"imports":s.import_defaults,"xmp":s.xmp,"lastExport":s.last_export,
            "presets":s.presets.iter().cloned().collect::<Vec<_>>(),"print":s.print,"printTemplates":s.print_templates,
            "effects":s.mask_effects,"keywordSets":s.keyword_sets,"keywordSet":s.keyword_set,"interaction":s.interaction.is_some()})
    }
    fn safety(&self) {
        let s = &self.app.session;
        assert_eq!(s.catalog.len(), 5);
        assert!(s.catalog.photos().all(|p| matches!(p.source, Source::Demo { .. })));
        assert!(s.library.is_none() && s.library_folder.is_none());
        assert!(s.smart_previews_dir.is_none() && s.media.rendered.disk().is_none());
        assert!(self.app.services.write.is_none() && self.app.services.write_shared.is_none());
        assert!(self.app.services.picker.is_none() && self.app.services.pick_files.is_none() && self.app.services.pick_folder.is_none());
        assert!(self.app.services.save_preset_file.is_none() && self.app.services.png.is_none());
        assert!(self.app.pending_picks.is_empty() && self.app.import.is_none() && self.app.export.is_none() && self.app.scan.is_none());
        assert_eq!(self.app.renderer.in_flight(), 0);
        assert!(self.app.merge.preview_task.is_none() && self.app.merge.final_task.is_none());
        assert!(self.app.merge.preview.is_none() && self.app.merge.preview_pixels.is_none());
        assert!(!s.segmenter.analyzing() && !s.segmenter.busy());
        assert!(s.activity.list().is_empty());
        let actual = self.snapshot();
        assert!(
            actual == self.stable || self.pending.as_ref() == Some(&actual),
            "unpredicted transfer/dialog action or replay in {:?}, tick {}: actual={actual}; stable={}; pending={:?}",
            self.pass,
            self.tick,
            self.stable,
            self.pending
        );
    }
    fn text_shapes(shape: &egui::epaint::Shape, clip: Rect, painted: &mut Vec<(String, Rect)>) {
        match shape {
            egui::epaint::Shape::Vec(parts) => {
                for part in parts {
                    Self::text_shapes(part, clip, painted);
                }
            }
            egui::epaint::Shape::Text(text) => {
                let rect = text.galley.rect.translate(text.pos.to_vec2());
                if clip.contains_rect(rect) {
                    painted.push((text.galley.job.text.clone(), rect));
                }
            }
            _ => {}
        }
    }
    fn frame(&mut self, mut events: Vec<Event>) {
        self.safety();
        crate::widgets::take_registries(&self.ctx);
        self.tick += 1;
        let modifiers = events
            .iter()
            .find_map(|e| match e {
                Event::Key { modifiers, .. } | Event::PointerButton { modifiers, .. } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or_default();
        let input = events
            .iter()
            .filter_map(|e| match e {
                Event::PointerButton { pos, button, pressed, .. } => {
                    Some(json!({"kind":"pointer","point":[pos.x,pos.y],"button":format!("{button:?}"),"pressed":pressed}))
                }
                Event::Key { key, pressed, modifiers, .. } => {
                    Some(json!({"kind":"key","key":format!("{key:?}"),"pressed":pressed,"shift":modifiers.shift}))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        events.insert(0, Event::ModifiersChanged(modifiers));
        let raw = HeadlessView::raw_input(VIEWPORT, 1.0, f64::from(self.tick) / 60.0, events);
        let app = &mut self.app;
        let ctx = self.ctx.clone();
        let pass = self.pass;
        let known = self.owners.values().copied().collect::<Vec<_>>();
        let mut last = None;
        let output = Output(ctx.run_ui(raw, |root| {
            let mut draw = |ui: &mut egui::Ui| {
                // Same production barrier/parent order as app/frame.rs. This does not run
                // the image canvas, full app frame, logic, service polls or any fake dialog.
                crate::dialogs::prepare(app, ui);
                super::show(app, ui);
                crate::dialogs::show_root(app, ui, false);
            };
            match pass {
                Pass::Real => draw(root),
                Pass::Disabled => {
                    root.disable();
                    draw(root);
                }
                Pass::Measure => craft::ui::runtime::measure::run(&ctx, || draw(root)),
                Pass::Sizing => {
                    let mut child = root.new_child(egui::UiBuilder::new().max_rect(root.max_rect()).sizing_pass());
                    draw(&mut child);
                }
            }
            // Query genuine responses in the callback. Native register() aliases are hover
            // probes, not command owners. Current register_response() IDs also cover the
            // unchanged SDK Pick/dialog footer; no owner is constructed or forced here.
            let registry = crate::widgets::take_registries(&ctx);
            let (clicked, mut ids) = ctx.interaction_snapshot(|s| (s.clicked, s.contains_pointer.iter().copied().collect::<Vec<_>>()));
            ids.extend(known.iter().copied());
            ids.extend(registry.1.iter().map(|(_, hit)| hit.id));
            if let Some(id) = clicked {
                ids.push(id);
            }
            let focused = ctx.memory(|m| m.focused()).and_then(|id| ctx.read_response(id));
            if let Some(r) = &focused {
                ids.push(r.id);
            }
            ids.sort_by_key(Id::value);
            ids.dedup();
            let responses = ids.into_iter().filter_map(|id| ctx.read_response(id)).collect::<Vec<_>>();
            last = Some((registry, responses, focused, clicked));
        }));
        let ((rects, hits), responses, focused, clicked) = last.unwrap();
        self.rects = rects.into_iter().collect();
        self.hits = hits.into_iter().collect();
        self.responses = responses;
        self.focused = focused;
        self.clicked = clicked;
        self.painted.clear();
        for shape in &output.0.shapes {
            Self::text_shapes(&shape.shape, shape.clip_rect, &mut self.painted);
        }
        drop(output);
        if !input.is_empty() {
            println!(
                "COPY_NATIVE_INPUT {}",
                json!({"tick":self.tick,"eguiPass":self.ctx.cumulative_pass_nr(),"pass":format!("{:?}",self.pass),
                "view":self.app.ui.view,"events":input,"focus":self.focus_name(),"clicked":format!("{:?}",self.clicked),
                "owners":self.responses.iter().filter(|r|r.is_pointer_button_down_on()||r.clicked()).map(|r|json!({"id":format!("{:?}",r.id),"enabled":r.enabled(),"down":r.is_pointer_button_down_on(),"clicked":r.clicked()})).collect::<Vec<_>>(),
                "clipboardPresent":self.app.session.clipboard.is_some(),"dialog":self.app.ui.dialog,"commits":self.app.session.commits(),"journalLast":self.app.session.journal.last()})
            );
        }
        self.safety();
    }
    fn idle(&mut self) {
        for _ in 0..3 {
            self.frame(vec![]);
        }
    }
    fn rect(&self, alias: &str) -> Rect {
        *self.rects.get(alias).unwrap_or_else(|| panic!("missing actual parent locator {alias}: {:?}", self.rects.keys()))
    }
    fn clipped(&self, alias: &str) -> Rect {
        self.rect(alias).intersect(self.hits.get(alias).unwrap().interact_rect).intersect(Rect::from_min_size(Pos2::ZERO, VIEWPORT))
    }
    fn actual_at(&self, alias: &str, point: Pos2) -> Option<&Response> {
        let hit = self.hits.get(alias)?;
        let mut candidates = self
            .responses
            .iter()
            .filter(|r| r.sense.senses_click() && r.rect == self.rect(alias) && r.layer_id == hit.layer_id && r.interact_rect.contains(point));
        let first = candidates.next()?;
        assert!(candidates.next().is_none(), "ambiguous actual click owner at {alias}");
        Some(first)
    }
    fn ready(&mut self, alias: &str) -> (Pos2, Id) {
        assert_eq!(self.pass, Pass::Real);
        let mut point = self.clipped(alias).center();
        let mut previous = None;
        let mut consecutive = 0;
        for attempt in 0..32 {
            self.frame(vec![Event::PointerMoved(point)]);
            let clipped = self.clipped(alias);
            let inside = point.x > clipped.left() && point.x < clipped.right() && point.y > clipped.top() && point.y < clipped.bottom();
            let owner = self.actual_at(alias, point).map(|r| (r.id, r.layer_id, r.enabled()));
            let eligible = inside && owner.is_some_and(|(_, layer, enabled)| enabled && self.ctx.layer_id_at(point) == Some(layer));
            consecutive = if eligible && owner == previous { consecutive + 1 } else { usize::from(eligible) };
            previous = owner;
            if consecutive >= 3 {
                let id = owner.unwrap().0;
                self.owners.insert(alias.into(), id);
                println!(
                    "COPY_NATIVE_READY {}",
                    json!({"alias":alias,"attempt":attempt,"owner":format!("{id:?}"),"registered":format!("{:?}",self.hits[alias].id),
                    "point":[point.x,point.y],"clip":[clipped.left(),clipped.top(),clipped.right(),clipped.bottom()]})
                );
                return (point, id);
            }
            if !inside {
                point = clipped.center();
                consecutive = 0;
            }
        }
        panic!("actual native parent never obtains a stable interior hit: {alias}");
    }
    fn pointer(&mut self, at: Pos2, pressed: bool) {
        self.frame(vec![
            Event::PointerMoved(at),
            Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE },
        ]);
    }
    fn key(&mut self, key: Key, shift: bool) {
        let modifiers = Modifiers { shift, ..Default::default() };
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers }]);
        }
        self.frame(vec![]);
    }
    fn expect(&mut self, next: Value) {
        assert!(self.pending.is_none());
        self.safety();
        self.pending = Some(next);
    }
    fn accept(&mut self, stage: &str) {
        let expected = self.pending.take().unwrap();
        assert_eq!(self.snapshot(), expected, "{stage}: exact full-state prediction must hold");
        self.stable = expected;
        self.idle();
        self.observe(stage);
    }
    fn click_expected(&mut self, alias: &str, next: Value, stage: &str) {
        let (point, id) = self.ready(alias);
        self.pointer(point, true);
        let response = self.actual_at(alias, point).expect("actual owner remains on press");
        assert_eq!(response.id, id);
        assert!(response.is_pointer_button_down_on());
        assert_eq!(self.snapshot(), self.stable, "press alone cannot dispatch");
        self.expect(next);
        self.pointer(point, false);
        assert_eq!(self.clicked, Some(id), "release must belong to the actual control, not its locator probe");
        self.accept(stage);
    }
    fn caption(&self, message: &str) {
        let text = crate::i18n::tr(message);
        let rect = self.rect(PRIMARY);
        assert!(
            self.painted.iter().any(|(s, r)| s == text && rect.contains_rect(*r)),
            "complete localized primary caption must be painted inside its real hit: {text}"
        );
    }
    fn predict_copy(&self, source: u64, groups: &[SettingsGroup], dialog: bool) -> Value {
        assert_eq!(self.app.session.active(), Some(PhotoId(source)));
        let mut next = self.stable.clone();
        let settings = &self.initial[&source].develop;
        next["clipboard"] = lightcraft_develop::extract_groups(settings, groups);
        let payload = if dialog { json!({"groups":names(groups)}) } else { json!({}) };
        next["journal"].as_array_mut().unwrap().push(json!(["develop.copy", payload]));
        if dialog {
            next["copyGroups"] = json!(groups);
            next["ui"]["dialog"] = Value::Null;
        } else {
            next["ui"]["toast"] = json!([crate::i18n::tr("Edit settings copied"), crate::TOAST_SECONDS, null]);
        }
        next
    }
    fn copy(&mut self, source: u64) {
        assert!(self.app.session.clipboard.is_none());
        let groups = default_groups();
        let next = self.predict_copy(source, &groups, false);
        self.click_expected(PRIMARY, next, "primary-copied-active-source");
        self.caption("Paste Edit Settings");
        assert_eq!(self.app.session.copy_groups, groups);
        assert_eq!(self.app.session.catalog.photo(PhotoId(source)).unwrap().as_ref(), &self.initial[&source]);
    }
    fn select(&mut self, ids: &[u64], active: u64) {
        let before = self.app.session.active();
        let params = json!({"ids":ids,"active":active});
        let mut next = self.stable.clone();
        next["selection"] = json!({"ids":ids,"active":active});
        if before != Some(PhotoId(active)) && before.is_some() {
            next["previousActive"] = json!(before);
        }
        next["journal"].as_array_mut().unwrap().push(json!(["library.select", params]));
        self.expect(next);
        self.app.run("library.select", params).unwrap();
        self.accept("selected-different-transfer-targets");
    }
    fn compare(&mut self, a: u64, b: u64) {
        assert_eq!(self.app.session.active(), Some(PhotoId(a)));
        assert_eq!(self.app.session.selected(), vec![PhotoId(a), PhotoId(b)]);
        let mut next = self.stable.clone();
        next["ui"]["view"] = json!(ViewMode::Compare);
        next["ui"]["compare"] = json!([a, b]);
        next["selection"] = json!({"ids":[a,b],"active":b});
        next["previousActive"] = json!(a);
        next["journal"].as_array_mut().unwrap().push(json!(["library.select",{"ids":[a,b],"active":b}]));
        self.expect(next);
        self.app.run("view.compare", json!({})).unwrap();
        self.accept("entered-genuine-compare");
        assert_eq!(self.app.ui.compare, Some((a, b)));
        assert_eq!(self.app.session.active(), Some(PhotoId(b)));
    }
    fn predict_paste(&mut self, ids: &[u64]) -> Value {
        assert_eq!(self.app.session.selected(), ids.iter().copied().map(PhotoId).collect::<Vec<_>>());
        let clip = self.app.session.clipboard.as_ref().unwrap();
        let mut catalog = self.app.session.catalog.clone();
        let ops = ids
            .iter()
            .copied()
            .map(|id| {
                let before = &catalog.photo(PhotoId(id)).unwrap().develop;
                let settings = Arc::new(lightcraft_develop::apply_partial(before, clip, 1.0));
                assert!(settings.masks.is_empty(), "fixture does not need image-space mask attachment");
                Op::Batch {
                    ops: vec![
                        Op::SetDevelop { id: PhotoId(id), settings: settings.clone(), label: "Paste Settings".into(), edited: Some(CLOCK.into()) },
                        Op::PushHistory { id: PhotoId(id), step: HistoryStep { label: "Paste Settings".into(), settings } },
                    ],
                }
            })
            .collect();
        let inverse = catalog.apply(Op::Batch { ops }).unwrap();
        let mut next = self.stable.clone();
        next["catalog"] = json!(catalog);
        next["revision"] = json!(catalog.revision);
        next["undo"].as_array_mut().unwrap().push(json!({"label":"Paste Settings","op":inverse,"folder":"None"}));
        next["redo"] = json!([]);
        next["commits"] = json!(self.app.session.commits() + 1);
        next["journal"].as_array_mut().unwrap().push(json!(["develop.paste", {}]));
        next["ui"]["toast"] = json!([crate::i18n::tr("Settings pasted"), crate::TOAST_SECONDS, null]);
        self.inverse = Some(inverse);
        next
    }
    fn paste(&mut self, ids: &[u64]) {
        self.caption("Paste Edit Settings");
        let next = self.predict_paste(ids);
        self.click_expected(PRIMARY, next, "primary-pasted-entire-selection");
    }
    fn undo_redo(&mut self) {
        let mut catalog = self.app.session.catalog.clone();
        let forward = catalog.apply(self.inverse.clone().unwrap()).unwrap();
        let mut next = self.stable.clone();
        next["catalog"] = json!(catalog);
        next["revision"] = json!(catalog.revision);
        next["undo"].as_array_mut().unwrap().pop().unwrap();
        next["redo"].as_array_mut().unwrap().push(json!({"label":"Paste Settings","op":forward,"folder":"None"}));
        next["journal"].as_array_mut().unwrap().push(json!(["edit.undo", {}]));
        self.expect(next);
        self.app.run("edit.undo", json!({})).unwrap();
        self.accept("genuine-batch-undo");
        let inverse = catalog.apply(forward).unwrap();
        let mut next = self.stable.clone();
        next["catalog"] = json!(catalog);
        next["revision"] = json!(catalog.revision);
        next["redo"].as_array_mut().unwrap().pop().unwrap();
        next["undo"].as_array_mut().unwrap().push(json!({"label":"Paste Settings","op":inverse,"folder":"None"}));
        next["journal"].as_array_mut().unwrap().push(json!(["edit.redo", {}]));
        self.expect(next);
        self.app.run("edit.redo", json!({})).unwrap();
        self.accept("genuine-batch-redo");
        self.inverse = Some(inverse);
    }
    fn open_gear(&mut self) {
        let groups = names(&self.app.session.copy_groups);
        let mut next = self.stable.clone();
        next["ui"]["dialog"] = json!(Dialog::CopySettings { groups: groups.clone() });
        self.click_expected(GEAR, next, "gear-opened-genuine-copy-dialog");
        assert!(matches!(&self.app.ui.dialog,Some(Dialog::CopySettings{groups:g}) if *g==groups));
        assert!(
            self.rects.contains_key("copyGroup:none")
                && self.rects.contains_key("copyGroup:light")
                && self.rects.contains_key(OK)
                && self.rects.contains_key(CANCEL)
        );
    }
    fn groups(&mut self) {
        let mut next = self.stable.clone();
        next["ui"]["dialog"] = json!(Dialog::CopySettings { groups: vec![] });
        self.click_expected("copyGroup:none", next, "copy-dialog-draft-none-only");
        for group in [SettingsGroup::Light, SettingsGroup::Color] {
            let mut next = self.stable.clone();
            let mut draft = next["ui"]["dialog"]["groups"].as_array().unwrap().clone();
            draft.push(json!(group));
            next["ui"]["dialog"]["groups"] = json!(draft);
            self.click_expected(&format!("copyGroup:{}", names(&[group])[0]), next, "copy-dialog-draft-added-one-group-only");
        }
    }
    fn cancel(&mut self, escape: bool) {
        let mut next = self.stable.clone();
        next["ui"]["dialog"] = Value::Null;
        if escape {
            self.expect(next);
            self.key(Key::Escape, false);
            self.accept("copy-dialog-real-escape-cancelled");
        } else {
            self.click_expected(CANCEL, next, "copy-dialog-real-cancel-button");
        }
    }
    fn untouched(&self, ids: &[u64]) {
        for id in ids {
            assert_eq!(
                self.app.session.catalog.photo(PhotoId(*id)).unwrap().as_ref(),
                &self.initial[id],
                "source/unselected photo {id} must remain wholly untouched"
            );
        }
    }
    fn focus_name(&self) -> String {
        if let Some(record) = craft::ui::runtime::focus::focused_rec(&self.ctx)
            && let Some((name, _)) = self.hits.iter().find(|(_, hit)| hit.id == record.stop.id)
        {
            return name.clone();
        }
        self.focused.as_ref().map_or_else(
            || "none".into(),
            |r| self.rects.iter().find(|(_, rect)| **rect == r.rect).map_or_else(|| format!("unregistered:{:?}", r.id), |(name, _)| name.clone()),
        )
    }
    fn observe(&self, stage: &str) {
        let s = &self.app.session;
        println!(
            "COPY_NATIVE_FLOW {}",
            json!({"stage":stage,"tick":self.tick,"eguiPass":self.ctx.cumulative_pass_nr(),"pass":format!("{:?}",self.pass),
            "view":self.app.ui.view,"selection":s.selection,"compare":self.app.ui.compare,"focus":self.focus_name(),"copyGroups":s.copy_groups,"clipboard":s.clipboard,
            "dialog":self.app.ui.dialog,"commits":s.commits(),"undo":s.undo.len(),"redo":s.redo.len(),"journalLast":s.journal.last(),
            "photos":s.catalog.photos().map(|p|json!({"id":p.id,"exposure":p.develop.light.exposure,"vibrance":p.develop.color.vibrance,
                "angle":p.develop.crop.geometry.angle,"history":p.history.len(),"versions":p.versions.len(),"edited":p.edited})).collect::<Vec<_>>()})
        );
    }
    fn gates(&mut self, pass: Pass, alias: &str, paste: bool) {
        if paste {
            self.copy(101);
            self.select(&[103, 104], 103);
        }
        let (point, ordinary_owner) = self.ready(alias);
        let ordinary_rect = self.rect(alias);
        self.pass = pass;
        self.idle();
        if pass == Pass::Sizing {
            // Observed SDK sizing output is inert, not an ordinary clickable allocation.
            // Keep the physical point from the genuine ordinary target; never invent a hit.
            assert_eq!(self.rect(alias).size(), egui::Vec2::ZERO, "sizing output retains its observed zero-size form");
            let output = self.hits.get(alias).expect("genuine returned sizing output remains registered");
            assert_eq!(output.id, ordinary_owner, "sizing returns the same semantic component identity");
            assert_eq!(output.interact_rect, Rect::NOTHING, "sizing output has no interaction bounds");
            assert!(!self.responses.iter().any(|response| response.id == output.id), "sizing must not register a real read-back widget");
            self.observe("observed-inert-sizing-output-before-physical-press");
        } else {
            // Disabled and measurement retain their original geometry observation unchanged.
            assert!(self.rect(alias).contains(point), "blocked native draw retains the physical target");
        }
        self.observe("before-blocked-press");
        self.pointer(point, true);
        self.pass = Pass::Real;
        self.pointer(point, false);
        self.idle();
        if pass == Pass::Sizing {
            assert_eq!(self.rect(alias), ordinary_rect, "ordinary release returns the exact pre-sizing target");
            let response = self.actual_at(alias, point).expect("ordinary release is observed at the original physical point");
            assert_eq!(response.id, ordinary_owner, "ordinary release returns the genuine original owner");
            assert!(response.enabled() && response.interact_rect.contains(point));
            assert!(!response.is_pointer_button_down_on(), "blocked sizing press cannot remain armed after real release");
        }
        self.observe("blocked-press-real-release-inert");
        let (point, _) = self.ready(alias);
        self.pass = pass;
        self.pointer(point, true);
        self.pointer(point, false);
        self.pass = Pass::Real;
        self.idle();
        self.observe("complete-blocked-click-inert");
        if alias == GEAR {
            self.open_gear();
            self.cancel(false);
        } else if paste {
            self.paste(&[103, 104]);
            self.undo_redo();
        } else {
            self.copy(101);
            self.select(&[103, 104], 103);
            self.paste(&[103, 104]);
            self.undo_redo();
        }
        self.observe("fresh-positive-after-blocked-input");
    }
}
impl Drop for Review {
    fn drop(&mut self) {
        crate::i18n::set_language(self.old_language);
        if !std::thread::panicking() {
            let raw = HeadlessView::raw_input(VIEWPORT, 1.0, f64::from(self.tick + 1) / 60.0, vec![]);
            drop(Output(self.ctx.run_ui(raw, |_| {})));
            crate::widgets::take_registries(&self.ctx);
        }
    }
}

#[test]
fn copy_paste_native_library_active_source_selected_batch_and_genuine_undo_redo() {
    let mut r = Review::new();
    r.copy(101);
    r.select(&[103, 104], 103);
    let before = r.app.session.catalog.clone();
    r.paste(&[103, 104]);
    r.untouched(&[101, 102, 105]);
    for id in [103, 104] {
        let p = r.app.session.catalog.photo(PhotoId(id)).unwrap();
        assert_eq!(p.develop.light.exposure, r.initial[&101].develop.light.exposure);
        assert_eq!(p.develop.crop, before.photo(PhotoId(id)).unwrap().develop.crop);
        assert_eq!(p.history.len(), r.initial[&id].history.len() + 1);
    }
    r.undo_redo();
    r.untouched(&[101, 102, 105]);
}
#[test]
fn copy_paste_native_compare_keeps_selection_wide_paste_not_pick_targeting() {
    let mut r = Review::new();
    r.copy(101);
    r.select(&[103, 104], 103);
    r.compare(103, 104);
    r.paste(&[103, 104]);
    for id in [103, 104] {
        assert_eq!(r.app.session.catalog.photo(PhotoId(id)).unwrap().develop.light.exposure, r.initial[&101].develop.light.exposure);
    }
    assert_eq!(r.app.session.journal.last().unwrap(), &("develop.paste".to_owned(), json!({})));
    r.untouched(&[101, 102, 105]);
    r.undo_redo();
    r.untouched(&[101, 102, 105]);
}
#[test]
fn copy_paste_native_gear_recopies_real_group_draft_confirm_and_cancel() {
    let mut r = Review::new();
    r.copy(101);
    r.select(&[102, 105], 102);
    let original_clip = r.app.session.clipboard.clone();
    for escape in [false, true] {
        r.open_gear();
        r.groups();
        r.cancel(escape);
        assert_eq!(r.app.session.clipboard, original_clip);
        assert_eq!(r.app.session.copy_groups, default_groups());
    }
    r.open_gear();
    r.groups();
    let chosen = vec![SettingsGroup::Light, SettingsGroup::Color];
    let next = r.predict_copy(102, &chosen, true);
    r.click_expected(OK, next, "real-dialog-confirm-recopied-new-active-source");
    assert_ne!(r.app.session.clipboard, original_clip);
    r.caption("Paste Edit Settings");
    r.select(&[103, 104], 103);
    r.paste(&[103, 104]);
    for id in [103, 104] {
        let p = r.app.session.catalog.photo(PhotoId(id)).unwrap();
        assert_eq!(p.develop.light.exposure, r.initial[&102].develop.light.exposure);
        assert_eq!(p.develop.color, r.initial[&102].develop.color);
        assert_eq!(p.develop.effects, r.initial[&id].develop.effects);
        assert_eq!(p.develop.crop, r.initial[&id].develop.crop);
    }
    r.untouched(&[101, 102, 105]);
    r.undo_redo();
    r.untouched(&[101, 102, 105]);
}
#[test]
fn copy_paste_native_repeat_paste_preserves_exact_history_and_batch_undo_contract() {
    let mut r = Review::new();
    r.copy(101);
    r.select(&[103, 104], 103);
    r.paste(&[103, 104]);
    let commits = r.app.session.commits();
    let prior = r.app.session.catalog.clone();
    r.paste(&[103, 104]);
    assert_eq!(r.app.session.commits(), commits + 1);
    for id in [103, 104] {
        let p = r.app.session.catalog.photo(PhotoId(id)).unwrap();
        let old = prior.photo(PhotoId(id)).unwrap();
        assert_eq!(p.develop, old.develop);
        assert_eq!(p.history.len(), old.history.len() + 1);
    }
    r.undo_redo();
    r.untouched(&[101, 102, 105]);
}
#[test]
fn copy_paste_native_freezes_natural_tab_shift_tab_orders_for_both_routes_and_views() {
    for compare in [false, true] {
        for paste in [false, true] {
            let mut r = Review::new();
            if paste {
                r.copy(101);
            }
            if compare {
                r.select(&[103, 104], 103);
                r.compare(103, 104);
            }
            r.ready(PRIMARY);
            r.frame(vec![Event::PointerGone]);
            let mut orders = vec![];
            for shift in [false, true] {
                let mut order = vec![];
                for step in 0..64 {
                    r.key(Key::Tab, shift);
                    order.push(json!({"step":step,"name":r.focus_name(),"raw":format!("{:?}",r.ctx.memory(|m|m.focused()))}));
                }
                orders.push(json!({"shift":shift,"order":order}));
            }
            println!(
                "COPY_NATIVE_FOCUS {}",
                json!({"compare":compare,"paste":paste,"orders":orders,"scope":"Natural traversal only; no forced focus or inferred Enter/Space qualification."})
            );
            r.safety();
        }
    }
}
#[test]
fn copy_paste_native_copy_disabled_press_real_release_then_fresh_positive() {
    Review::new().gates(Pass::Disabled, PRIMARY, false);
}
#[test]
fn copy_paste_native_copy_measure_press_real_release_then_fresh_positive() {
    Review::new().gates(Pass::Measure, PRIMARY, false);
}
#[test]
fn copy_paste_native_copy_sizing_press_real_release_then_fresh_positive() {
    Review::new().gates(Pass::Sizing, PRIMARY, false);
}
#[test]
fn copy_paste_native_paste_disabled_press_real_release_then_fresh_positive() {
    Review::new().gates(Pass::Disabled, PRIMARY, true);
}
#[test]
fn copy_paste_native_paste_measure_press_real_release_then_fresh_positive() {
    Review::new().gates(Pass::Measure, PRIMARY, true);
}
#[test]
fn copy_paste_native_paste_sizing_press_real_release_then_fresh_positive() {
    Review::new().gates(Pass::Sizing, PRIMARY, true);
}
#[test]
fn copy_paste_native_gear_disabled_press_real_release_then_fresh_positive() {
    Review::new().gates(Pass::Disabled, GEAR, false);
}
#[test]
fn copy_paste_native_gear_measure_press_real_release_then_fresh_positive() {
    Review::new().gates(Pass::Measure, GEAR, false);
}
#[test]
fn copy_paste_native_gear_sizing_press_real_release_then_fresh_positive() {
    Review::new().gates(Pass::Sizing, GEAR, false);
}

// SDK comparison cases append to the exact repaired native harness above.
// All owner changes below are ordinary app commands between a real observed press/release.
impl Review {
    fn sdk_hold(&mut self, alias: &str) -> (Pos2, Id) {
        let (point, owner) = self.ready(alias);
        self.pointer(point, true);
        let response = self.actual_at(alias, point).expect("observed SDK owner exists on held press");
        assert_eq!(response.id, owner);
        assert!(response.is_pointer_button_down_on());
        assert_eq!(self.snapshot(), self.stable);
        self.observe("sdk-real-held-press-before-owner-change");
        (point, owner)
    }
    fn sdk_cancel_old_release(&mut self, alias: &str, point: Pos2, old: Id) {
        assert!(self.clipped(alias).contains(point), "old press must remain over the real current target");
        let response = self.actual_at(alias, point).expect("current owner is genuinely observed before old release");
        assert_ne!(response.id, old, "changed command context must have a different genuine owner");
        assert!(!response.is_pointer_button_down_on(), "changed owner must not inherit the held press");
        self.pointer(point, false);
        self.idle();
        self.observe("sdk-old-release-cancelled-under-changed-owner");
    }
    fn sdk_external_copy(&mut self, source: u64) {
        let groups = self.app.session.copy_groups.clone();
        let mut next = self.predict_copy(source, &groups, false);
        // This is the real engine command, not the toolbar caller: it does not create a toast.
        next["ui"]["toast"] = self.stable["ui"]["toast"].clone();
        self.expect(next);
        self.app.run("develop.copy", json!({})).unwrap();
        self.accept("sdk-real-copy-command-changed-clipboard-during-hold");
    }
    fn sdk_fresh_paste(&mut self) {
        self.select(&[103, 104], 103);
        self.paste(&[103, 104]);
        self.undo_redo();
        self.untouched(&[101, 102, 105]);
    }
    fn sdk_tab_to(&mut self, alias: &str) {
        self.frame(vec![Event::PointerGone]);
        for _ in 0..32 {
            if self.focus_name() == alias {
                let actual = self.hits[alias].id;
                assert_eq!(self.ctx.memory(|m| m.focused()), Some(actual));
                self.observe("sdk-target-reached-by-ordinary-tab");
                return;
            }
            self.key(Key::Tab, false);
        }
        panic!("ordinary Tab never reached SDK target {alias}");
    }
}
#[test]
fn copy_paste_sdk_copy_held_selection_handoff_cancels_and_restored_fresh_click_works() {
    let mut r = Review::new();
    let (point, old) = r.sdk_hold(PRIMARY);
    r.select(&[101, 105], 101);
    r.sdk_cancel_old_release(PRIMARY, point, old);
    r.select(&[101, 102], 101);
    r.observe("sdk-restored-copy-selection-without-replay");
    r.copy(101);
    r.sdk_fresh_paste();
}
#[test]
fn copy_paste_sdk_copy_held_active_owner_handoff_cancels_and_restored_fresh_click_works() {
    let mut r = Review::new();
    let (point, old) = r.sdk_hold(PRIMARY);
    r.select(&[101, 102], 102);
    r.sdk_cancel_old_release(PRIMARY, point, old);
    r.select(&[101, 102], 101);
    r.observe("sdk-restored-copy-active-owner-without-replay");
    r.copy(101);
    r.sdk_fresh_paste();
}
#[test]
fn copy_paste_sdk_copy_held_clipboard_mode_handoff_cannot_turn_old_release_into_paste() {
    let mut r = Review::new();
    let (point, old) = r.sdk_hold(PRIMARY);
    r.sdk_external_copy(101);
    r.caption("Paste Edit Settings");
    r.sdk_cancel_old_release(PRIMARY, point, old);
    r.untouched(&[101, 102, 103, 104, 105]);
    r.sdk_fresh_paste();
}
#[test]
fn copy_paste_sdk_paste_held_selection_handoff_cancels_and_restored_fresh_click_works() {
    let mut r = Review::new();
    r.copy(101);
    r.select(&[103, 104], 103);
    let (point, old) = r.sdk_hold(PRIMARY);
    r.select(&[103, 105], 103);
    r.sdk_cancel_old_release(PRIMARY, point, old);
    r.select(&[103, 104], 103);
    r.observe("sdk-restored-paste-selection-without-replay");
    r.sdk_fresh_paste();
}
#[test]
fn copy_paste_sdk_compare_paste_held_active_owner_handoff_cancels_and_restored_fresh_click_works() {
    let mut r = Review::new();
    r.copy(101);
    r.select(&[103, 104], 103);
    r.compare(103, 104);
    let (point, old) = r.sdk_hold(PRIMARY);
    r.select(&[103, 104], 103);
    r.sdk_cancel_old_release(PRIMARY, point, old);
    r.select(&[103, 104], 104);
    r.observe("sdk-restored-compare-paste-active-owner-without-replay");
    r.paste(&[103, 104]);
    r.undo_redo();
    r.untouched(&[101, 102, 105]);
}
#[test]
fn copy_paste_sdk_paste_held_clipboard_contents_handoff_cannot_use_new_settings_on_old_release() {
    let mut r = Review::new();
    r.copy(101);
    r.select(&[103, 104], 103);
    let original = r.app.session.clipboard.clone();
    let (point, old) = r.sdk_hold(PRIMARY);
    r.sdk_external_copy(103);
    assert_ne!(r.app.session.clipboard, original);
    r.caption("Paste Edit Settings");
    r.sdk_cancel_old_release(PRIMARY, point, old);
    r.sdk_fresh_paste();
}
#[test]
fn copy_paste_sdk_gear_held_selection_handoff_cancels_and_restored_fresh_click_works() {
    let mut r = Review::new();
    r.copy(101);
    let (point, old) = r.sdk_hold(GEAR);
    r.select(&[101, 105], 101);
    r.sdk_cancel_old_release(GEAR, point, old);
    r.select(&[101, 102], 101);
    r.observe("sdk-restored-gear-selection-without-replay");
    r.open_gear();
    r.cancel(false);
}
#[test]
fn copy_paste_sdk_gear_held_active_owner_handoff_cancels_and_restored_fresh_click_works() {
    let mut r = Review::new();
    r.copy(101);
    let (point, old) = r.sdk_hold(GEAR);
    r.select(&[101, 102], 102);
    r.sdk_cancel_old_release(GEAR, point, old);
    r.select(&[101, 102], 101);
    r.observe("sdk-restored-gear-active-owner-without-replay");
    r.open_gear();
    r.cancel(true);
}
#[test]
fn copy_paste_sdk_gear_held_clipboard_mode_handoff_cancels_and_fresh_click_still_opens_copy_dialog() {
    let mut r = Review::new();
    let (point, old) = r.sdk_hold(GEAR);
    r.sdk_external_copy(101);
    r.caption("Paste Edit Settings");
    r.sdk_cancel_old_release(GEAR, point, old);
    r.open_gear();
    r.cancel(false);
}
#[test]
fn copy_paste_sdk_primary_and_gear_ordinary_tab_enter_space_activate_exactly_once_in_both_views_and_modes() {
    for compare in [false, true] {
        for paste in [false, true] {
            for key in [Key::Enter, Key::Space] {
                for alias in [PRIMARY, GEAR] {
                    let mut r = Review::new();
                    if paste {
                        r.copy(101);
                    }
                    if compare {
                        r.select(&[103, 104], 103);
                        r.compare(103, 104);
                    }
                    r.sdk_tab_to(alias);
                    let next = if alias == GEAR {
                        let mut next = r.stable.clone();
                        next["ui"]["dialog"] = json!(Dialog::CopySettings { groups: names(&r.app.session.copy_groups) });
                        next
                    } else if paste {
                        let targets = r.app.session.selected().iter().map(|id| id.0).collect::<Vec<_>>();
                        r.predict_paste(&targets)
                    } else {
                        r.predict_copy(r.app.session.active().unwrap().0, &default_groups(), false)
                    };
                    r.expect(next);
                    r.key(key, false);
                    r.accept("sdk-real-keyboard-activation-exactly-once");
                    if alias == GEAR {
                        r.cancel(true);
                    } else if paste {
                        r.undo_redo();
                    } else {
                        r.caption("Paste Edit Settings");
                        r.untouched(&[101, 102, 103, 104, 105]);
                    }
                    println!(
                        "COPY_SDK_KEYBOARD {}",
                        json!({"compare":compare,"paste":paste,"key":format!("{key:?}"),"alias":alias,"qualification":"ordinary Tab and actual key press/release; exact whole-state prediction"})
                    );
                }
            }
        }
    }
}
