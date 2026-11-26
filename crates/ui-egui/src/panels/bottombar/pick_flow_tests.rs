//! Actual bottom-bar Pick comparison and owner-lifecycle tests. No image frame, renderer, writer or services.
use crate::{LightcraftApp, Services, headless::HeadlessView};
use egui::{Event, Id, Key, Modifiers, PointerButton, Pos2, Rect, Response};
use lightcraft_catalog::{Flag, Op, Photo, PhotoId, Source};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const SDK_PICK: bool = true;
const PICK: &str = "flag:pick";
const VIEWPORT: egui::Vec2 = egui::Vec2::new(1200.0, 900.0);
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
    compare: bool,
    ctx: egui::Context,
    tick: u32,
    pass: Pass,
    rects: BTreeMap<String, Rect>,
    hits: BTreeMap<String, crate::widgets::WidgetHit>,
    responses: Vec<Response>,
    focused: Option<Response>,
    owners: BTreeMap<String, Id>,
    painted: Vec<(String, Rect)>,
    clicked: Option<Id>,
    invariant: Value,
    stable: Value,
    pending: Option<Value>,
    inverse: Option<Op>,
    old_language: crate::i18n::Locale,
}
impl Review {
    fn new(compare: bool) -> Self {
        let old_language = crate::i18n::language();
        crate::i18n::set_language(crate::i18n::Locale::En);
        let mut session = lightcraft_engine::Session::new();
        for (id, flag) in [(101, Flag::None), (102, Flag::Reject), (103, Flag::None)] {
            let mut photo = Photo::new(PhotoId(id), Source::Demo { scene: 0 }, &format!("demo-{id}.jpg"), "JPEG", 64, 48, "2026-11-21T00:00:00");
            photo.flag = flag;
            photo.rating = (id % 5) as u8;
            photo.meta.title = format!("Untouched title {id}");
            photo.meta.caption = format!("Untouched caption {id}");
            photo.meta.keywords = vec![format!("untouched-{id}")];
            session.commit("Pick fixture", Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        }
        session.execute("library.select", &json!({"ids":[101,102],"active":101})).unwrap();
        session.recent_keywords = vec!["untouched recent".into()];
        let mut app = LightcraftApp::new(session, Services { local_places: Some(vec![]), png: None, ..Default::default() });
        app.ui.auto_advance = false;
        app.run("view.photoGrid", json!({})).unwrap();
        if compare {
            // Enter through the actual command: the candidate 102, not the select 101, becomes active.
            app.run("view.compare", json!({})).unwrap();
            assert_eq!(app.ui.compare, Some((101, 102)));
            assert_eq!(app.session.active(), Some(PhotoId(102)));
        } else {
            assert_eq!(app.ui.view, crate::state::ViewMode::PhotoGrid);
            assert_eq!(app.session.active(), Some(PhotoId(101)));
        }
        assert_eq!(app.session.selected(), vec![PhotoId(101), PhotoId(102)]);
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        app.install_framework(&ctx);
        let mut r = Self {
            app,
            compare,
            ctx,
            tick: 0,
            pass: Pass::Real,
            rects: BTreeMap::new(),
            hits: BTreeMap::new(),
            responses: vec![],
            focused: None,
            owners: BTreeMap::new(),
            painted: vec![],
            clicked: None,
            invariant: Value::Null,
            stable: Value::Null,
            pending: None,
            inverse: None,
            old_language,
        };
        r.invariant = r.invariants();
        r.stable = r.mutable();
        r.idle();
        assert!(r.rects.contains_key(PICK), "the real bottom-bar pill must fit the ordinary viewport");
        assert!(r.rects.contains_key("flag:unflag") && r.rects.contains_key("flag:reject"));
        assert_eq!(r.rect(PICK).size(), egui::vec2(22.0, 22.0));
        r.observe("opened-actual-bottom-bar");
        r
    }
    fn history(entries: &[lightcraft_engine::UndoEntry]) -> Value {
        json!(entries.iter().map(|e| json!({"label":e.label,"op":e.op,"folder":format!("{:?}",e.folder)})).collect::<Vec<_>>())
    }
    fn mutable(&self) -> Value {
        let s = &self.app.session;
        json!({"catalog":s.catalog,"revision":s.catalog.revision,"undo":Self::history(&s.undo),"redo":Self::history(&s.redo),
            "commits":s.commits(),"journal":s.journal,"recent":s.recent_keywords})
    }
    fn invariants(&self) -> Value {
        let s = &self.app.session;
        json!({"ui":self.app.ui,"selection":s.selection,"source":s.source,"filter":s.filter,
            "activeMask":s.active_mask,"toolEffects":s.mask_tool_effects,"lastMerge":s.last_merge,"imports":s.import_defaults,
            "xmp":s.xmp,"lastExport":s.last_export,"presets":s.presets.iter().cloned().collect::<Vec<_>>(),
            "print":s.print,"printTemplates":s.print_templates,"effects":s.mask_effects,
            "keywordSets":s.keyword_sets,"keywordSet":s.keyword_set,"interaction":s.interaction.is_some(),"clipboard":s.clipboard})
    }
    fn safety(&self) {
        let s = &self.app.session;
        assert_eq!(s.catalog.len(), 3);
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
        assert_eq!(self.invariants(), self.invariant, "only the precisely predicted Pick/history transaction may change");
        let actual = self.mutable();
        assert!(
            actual == self.stable || self.pending.as_ref() == Some(&actual),
            "unexpected native Pick action or replay in {:?}: actual={actual}",
            self.pass
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
    fn response_trace(response: Option<&Response>) -> Value {
        response
            .map(|r| {
                json!({
                    "id":format!("{:?}",r.id),"layer":format!("{:?}",r.layer_id),
                    "rect":[r.rect.left(),r.rect.top(),r.rect.right(),r.rect.bottom()],
                    "clip":[r.interact_rect.left(),r.interact_rect.top(),r.interact_rect.right(),r.interact_rect.bottom()],
                    "enabled":r.enabled(),"clickSense":r.sense.senses_click(),"pointerDownOn":r.is_pointer_button_down_on(),
                    "clicked":r.clicked(),"hasFocus":r.has_focus()
                })
            })
            .unwrap_or(Value::Null)
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
        let input_trace = events
            .iter()
            .filter_map(|event| match event {
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
        let known: Vec<_> = self.owners.values().copied().collect();
        let prior_pick = self.hits.get(PICK).copied();
        let prior_read = prior_pick.and_then(|hit| ctx.read_response(hit.id));
        let tick = self.tick;
        let compare = self.compare;
        let mut callback_observation = None;
        let mut last = None;
        let output = Output(ctx.run_ui(raw, |root| {
            let mut draw = |ui: &mut egui::Ui| super::show(app, ui);
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
            let registry = crate::widgets::take_registries(&ctx);
            // Read real egui widgets after this completed draw, not the hover-only alias
            // responses or a cached post-run rectangle. Observation only: no input/focus hook.
            let (clicked, mut ids) = ctx.interaction_snapshot(|s| (s.clicked, s.contains_pointer.iter().copied().collect::<Vec<_>>()));
            ids.extend(known.iter().copied());
            if let Some(id) = clicked {
                ids.push(id);
            }
            let focused = ctx.memory(|m| m.focused()).and_then(|id| ctx.read_response(id));
            if let Some(r) = &focused {
                ids.push(r.id);
            }
            ids.sort_by_key(Id::value);
            ids.dedup();
            let legacy_ids = ids.clone();
            // interaction_snapshot is resolved at begin-pass from prior geometry. A newly
            // drawn SDK owner is not necessarily in it or in `known`. Query the actual
            // response id that THIS completed draw registered, without making an owner,
            // requesting focus, changing input, or moving the physical release.
            let current_pick = registry.1.iter().find(|(name, _)| name == PICK).map(|(_, hit)| *hit);
            if SDK_PICK && let Some(hit) = current_pick {
                ids.push(hit.id);
            }
            ids.sort_by_key(Id::value);
            ids.dedup();
            let responses = ids.into_iter().filter_map(|id| ctx.read_response(id)).collect::<Vec<_>>();
            if SDK_PICK && let Some(hit) = current_pick {
                let (point, down, released) = ctx.input(|i| (
                    i.pointer.latest_pos(), i.pointer.button_down(PointerButton::Primary),
                    i.pointer.button_released(PointerButton::Primary)));
                let changed = prior_pick.is_some_and(|old| old.id != hit.id);
                if changed || down || released {
                    let response = ctx.read_response(hit.id);
                    callback_observation = Some((hit.id, json!({
                        "tick":tick,"eguiPass":ctx.cumulative_pass_nr(),"compare":compare,
                        "point":point.map(|p|[p.x,p.y]),"primaryDown":down,"primaryReleased":released,
                        "ownerChanged":changed,"priorRegisteredId":prior_pick.map(|old|format!("{:?}",old.id)),
                        "priorBeforeRunRead":Self::response_trace(prior_read.as_ref()),
                        "legacyCandidateIds":legacy_ids.iter().map(|id|format!("{id:?}")).collect::<Vec<_>>(),
                        "legacyQueriedCurrent":legacy_ids.contains(&hit.id),
                        "currentRegisteredId":format!("{:?}",hit.id),
                        "currentRegisteredRect":registry.0.iter().find(|(name,_)|name==PICK).map(|(_,r)|[r.left(),r.top(),r.right(),r.bottom()]),
                        "currentRegisteredLayer":format!("{:?}",hit.layer_id),
                        "currentRegisteredClip":[hit.interact_rect.left(),hit.interact_rect.top(),hit.interact_rect.right(),hit.interact_rect.bottom()],
                        "pointInsideCurrentClip":point.is_some_and(|p|hit.interact_rect.contains(p)),
                        "layerAtPoint":point.and_then(|p|ctx.layer_id_at(p)).map(|layer|format!("{layer:?}")),
                        "callbackRead":Self::response_trace(response.as_ref()),
                        "includedFromCurrentRegistry":responses.iter().any(|r|r.id==hit.id)
                    })));
                }
            }
            last = Some((registry, responses, focused, clicked));
        }));
        let ((rects, hits), responses, focused, clicked) = last.unwrap();
        if let Some((id, mut trace)) = callback_observation {
            // Compare the in-callback read with the post-run lookup, but never substitute
            // the latter for this pass's registered/observed response.
            trace["postRunRead"] = Self::response_trace(ctx.read_response(id).as_ref());
            println!("PICK_CALLBACK_OWNER {trace}");
        }
        for alias in ["flag:unflag", PICK, "flag:reject"] {
            // the three flags are framework buttons: none publishes a target in a measuring or sizing pass
            let expected = usize::from(!(SDK_PICK && matches!(self.pass, Pass::Measure | Pass::Sizing)));
            assert_eq!(rects.iter().filter(|(name, _)| name == alias).count(), expected, "only real passes publish an interactive flag target");
        }
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
        if !input_trace.is_empty() {
            println!(
                "PICK_NATIVE_INPUT {}",
                json!({"tick":self.tick,"eguiPass":self.ctx.cumulative_pass_nr(),"view":if self.compare {"Compare"} else {"Library"},"pass":format!("{:?}",self.pass),"events":input_trace,
                "focus":self.focus_name(),"clicked":format!("{:?}",self.clicked),
                "realActiveOwners":self.responses.iter().filter(|r|r.is_pointer_button_down_on() || r.clicked()).map(|r|format!("{:?}",r.id)).collect::<Vec<_>>(),
                "journalLast":self.app.session.journal.last(),"commits":self.app.session.commits()})
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
        *self.rects.get(alias).unwrap_or_else(|| panic!("missing real panel locator {alias}: {:?}", self.rects.keys()))
    }
    fn clipped(&self, alias: &str) -> Rect {
        self.rect(alias).intersect(self.hits.get(alias).unwrap().interact_rect).intersect(Rect::from_min_size(Pos2::ZERO, VIEWPORT))
    }
    fn actual_at(&self, alias: &str, point: Pos2) -> Option<&Response> {
        let hit = self.hits.get(alias)?;
        let mut candidates = self.responses.iter().filter(|r| {
            r.sense.senses_click()
                && (!SDK_PICK || r.id == hit.id)
                && r.rect == self.rect(alias)
                && r.layer_id == hit.layer_id
                && r.interact_rect.contains(point)
        });
        let first = candidates.next()?;
        assert!(candidates.next().is_none(), "ambiguous genuine clickable owner at {alias}");
        Some(first)
    }
    fn ready(&mut self) -> (Pos2, Id) {
        assert_eq!(self.pass, Pass::Real);
        let alias = PICK.to_owned();
        let mut point = self.clipped(&alias).center();
        let mut previous = None;
        let mut consecutive = 0;
        for attempt in 0..32 {
            self.frame(vec![Event::PointerMoved(point)]);
            let clipped = self.clipped(&alias);
            let inside = point.x > clipped.left() && point.x < clipped.right() && point.y > clipped.top() && point.y < clipped.bottom();
            let owner = self.actual_at(&alias, point).map(|r| (r.id, r.layer_id, r.enabled()));
            let eligible = inside && owner.is_some_and(|(_, layer, enabled)| enabled && self.ctx.layer_id_at(point) == Some(layer));
            if eligible && owner == previous {
                consecutive += 1;
            } else {
                consecutive = usize::from(eligible);
            }
            previous = owner;
            if consecutive >= 3 {
                let id = owner.unwrap().0;
                if !SDK_PICK {
                    assert_ne!(id, self.hits[&alias].id, "native hover probe is not the actual button");
                } else {
                    assert_eq!(id, self.hits[&alias].id, "SDK locator is the actual current owner's response");
                }
                self.owners.insert(alias.clone(), id);
                println!(
                    "PICK_NATIVE_READY {}",
                    json!({"alias":alias,"attempt":attempt,"actual":format!("{id:?}"),
                    "probe":format!("{:?}",self.hits[&alias].id),"point":[point.x,point.y],"clipped":[clipped.left(),clipped.top(),clipped.right(),clipped.bottom()]})
                );
                return (point, id);
            }
            if !inside {
                point = clipped.center();
                consecutive = 0;
            }
        }
        panic!("native Pick never obtains a stable real interior owner: {alias}");
    }
    fn pointer(&mut self, at: Pos2, button: PointerButton, pressed: bool) {
        self.frame(vec![Event::PointerMoved(at), Event::PointerButton { pos: at, button, pressed, modifiers: Modifiers::NONE }]);
    }
    fn key(&mut self, key: Key, shift: bool) {
        let modifiers = Modifiers { shift, ..Default::default() };
        for pressed in [true, false] {
            self.frame(vec![Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers }]);
        }
        self.frame(vec![]);
    }
    fn focus_name(&self) -> String {
        if let Some(response) = &self.focused
            && let Some((name, _)) =
                self.rects.iter().find(|(name, rect)| name.starts_with("flag:") && **rect == response.rect && response.sense.senses_click())
        {
            return name.clone();
        }
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
    fn focused_pick(&self) -> bool {
        self.rects.get(PICK).is_some_and(|rect| {
            self.focused.as_ref().is_some_and(|r| {
                r.sense.senses_click()
                    && r.has_focus()
                    && r.rect == *rect
                    && r.interact_rect.is_positive()
                    && (SDK_PICK || r.id != self.hits[PICK].id)
            })
        })
    }
    fn observe(&self, stage: &str) {
        let s = &self.app.session;
        println!(
            "PICK_NATIVE_FLOW {}",
            json!({"stage":stage,"view":if self.compare {"Compare"} else {"Library"},"sdk":SDK_PICK,
            "tick":self.tick,"eguiPass":self.ctx.cumulative_pass_nr(),"pass":format!("{:?}",self.pass),
            "flags":s.catalog.photos().map(|p|json!([p.id,p.flag])).collect::<Vec<_>>(),
            "selection":s.selection,"pair":self.app.ui.compare,"focus":self.focus_name(),"pickFocused":self.focused_pick(),
            "rawFocus":format!("{:?}",self.ctx.memory(|m|m.focused())),"popup":egui::Popup::is_any_open(&self.ctx),
            "journal":s.journal,"undoCount":s.undo.len(),"redoCount":s.redo.len(),"commits":s.commits()})
        );
    }
    fn prepare(&mut self) {
        assert!(self.pending.is_none());
        self.safety();
        let mut catalog = self.app.session.catalog.clone();
        // Targets and payload are independently fixed by the fixture contract, not computed
        // with target_active or a second execution of the command under test.
        let ids = if self.compare { vec![PhotoId(102)] } else { vec![PhotoId(101), PhotoId(102)] };
        let ops = ids.into_iter().map(|id| Op::SetFlag { id, flag: Flag::Pick }).collect();
        let inverse = catalog.apply(Op::Batch { ops }).unwrap();
        let mut next = self.stable.clone();
        next["catalog"] = json!(catalog);
        next["revision"] = json!(catalog.revision);
        next["undo"].as_array_mut().unwrap().push(json!({"label":"Set Flag","op":inverse,"folder":"None"}));
        next["redo"] = json!([]);
        next["commits"] = json!(self.app.session.commits() + 1);
        let payload = if self.compare { json!({"flag":"pick","ids":[102]}) } else { json!({"flag":"pick"}) };
        next["journal"].as_array_mut().unwrap().push(json!(["photo.flag", payload]));
        self.pending = Some(next);
        self.inverse = Some(inverse);
    }
    fn accept(&mut self, stage: &str) {
        let expected = self.pending.take().unwrap();
        assert_eq!(self.mutable(), expected, "actual Pick must match the entire independently predicted catalog, payload, transaction and history");
        self.stable = expected;
        self.idle();
        self.observe(stage);
    }
    fn click(&mut self) {
        let (point, id) = self.ready();
        self.pointer(point, PointerButton::Primary, true);
        let response = self.actual_at(PICK, point).expect("actual Pick owner remains on press");
        assert_eq!(response.id, id);
        assert!(response.is_pointer_button_down_on());
        assert_eq!(self.mutable(), self.stable, "press alone is not Pick dispatch");
        self.prepare();
        self.pointer(point, PointerButton::Primary, false);
        assert_eq!(self.clicked, Some(id), "release belongs to the genuine Pick control, not its locator probe");
        self.accept("pointer-pick-applied");
    }
    fn undo_redo(&mut self) {
        let inverse = self.inverse.clone().unwrap();
        let mut catalog = self.app.session.catalog.clone();
        let forward = catalog.apply(inverse).unwrap();
        let mut next = self.stable.clone();
        next["catalog"] = json!(catalog);
        next["revision"] = json!(catalog.revision);
        next["undo"].as_array_mut().unwrap().pop().unwrap();
        next["redo"].as_array_mut().unwrap().push(json!({"label":"Set Flag","op":forward,"folder":"None"}));
        next["journal"].as_array_mut().unwrap().push(json!(["edit.undo", {}]));
        // Undo restores flags but does not rewind journal or the commit counter.
        self.pending = Some(next);
        self.app.run("edit.undo", json!({})).unwrap();
        self.accept("undo");
        let undo = catalog.apply(forward).unwrap();
        let mut next = self.stable.clone();
        next["catalog"] = json!(catalog);
        next["revision"] = json!(catalog.revision);
        next["redo"].as_array_mut().unwrap().pop().unwrap();
        next["undo"].as_array_mut().unwrap().push(json!({"label":"Set Flag","op":undo,"folder":"None"}));
        next["journal"].as_array_mut().unwrap().push(json!(["edit.redo", {}]));
        self.pending = Some(next);
        self.app.run("edit.redo", json!({})).unwrap();
        self.accept("redo");
    }
    fn tab_to_pick(&mut self) {
        self.frame(vec![Event::PointerGone]);
        for _ in 0..64 {
            if self.focused_pick() {
                return;
            }
            self.key(Key::Tab, false);
        }
        panic!("SDK Pick is not naturally reachable in the actual toolbar");
    }
    fn select_exact(&mut self, ids: &[u64], active: u64) {
        self.safety();
        assert!(self.pending.is_none());
        let params = json!({"ids":ids,"active":active});
        let mut next = self.stable.clone();
        next["journal"].as_array_mut().unwrap().push(json!(["library.select", params]));
        let mut invariant = self.invariant.clone();
        invariant["selection"] = json!({"ids":ids,"active":active});
        self.app.run("library.select", params).unwrap();
        assert_eq!(self.mutable(), next, "owner handoff may append only the exact selection journal row");
        assert_eq!(self.invariants(), invariant, "owner handoff may change only the exact selection");
        self.stable = next;
        self.invariant = invariant;
        self.safety();
    }
    fn owner_change(&mut self, change_selection: bool) {
        let (point, old_owner) = self.ready();
        let old_rect = self.rect(PICK);
        self.pointer(point, PointerButton::Primary, true);
        assert!(self.actual_at(PICK, point).unwrap().is_pointer_button_down_on());
        let original_active = if self.compare { 102 } else { 101 };
        let next_ids = if change_selection { vec![101, 103] } else { vec![101, 102] };
        let next_active = if change_selection {
            original_active
        } else if self.compare {
            101
        } else {
            102
        };
        self.select_exact(&next_ids, next_active);
        // The changed owner is genuinely drawn before release, at the unchanged physical target.
        self.frame(vec![]);
        assert_eq!(self.rect(PICK), old_rect);
        let new_owner = self.actual_at(PICK, point).expect("actual changed owner is visible").id;
        assert_ne!(new_owner, old_owner, "active/selection handoff must change the SDK press owner");
        assert_eq!(new_owner, self.hits[PICK].id);
        self.observe("changed-owner-before-old-release");
        self.pointer(point, PointerButton::Primary, false);
        self.idle();
        self.observe("old-release-cannot-retarget-changed-owner");
        // Returning to the original owner must not revive that release either. A completely
        // fresh normal gesture then proves this is lifecycle cancellation, not a stuck control.
        self.select_exact(&[101, 102], original_active);
        self.idle();
        self.click();
        self.undo_redo();
        self.observe("restored-owner-fresh-positive");
    }
    fn gates(&mut self, pass: Pass) {
        // Run the split sequence FIRST: a complete blocked click cannot hide this evidence
        // behind its own earlier native failure. No keyboard reachability prerequisite.
        let (point, _) = self.ready();
        self.pass = pass;
        self.idle();
        if SDK_PICK && matches!(pass, Pass::Measure | Pass::Sizing) {
            assert!(!self.rects.contains_key(PICK), "non-real SDK sensing cannot publish an interactive Pick target");
        } else {
            assert!(self.rect(PICK).contains(point), "blocked draw keeps the same physical Pick target");
        }
        self.observe("before-blocked-press");
        self.pointer(point, PointerButton::Primary, true);
        self.pass = Pass::Real;
        self.pointer(point, PointerButton::Primary, false);
        self.idle();
        self.observe("blocked-press-real-release-no-replay");
        let (point, _) = self.ready();
        self.pass = pass;
        self.pointer(point, PointerButton::Primary, true);
        self.pointer(point, PointerButton::Primary, false);
        self.pass = Pass::Real;
        self.idle();
        self.observe("complete-blocked-click-no-replay");
        // A negative is not enough: the same parent must accept a fresh real gesture.
        self.click();
        self.undo_redo();
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
fn pick_actual_library_selection_wide_payload_and_undo_redo() {
    let mut r = Review::new(false);
    let (point, _) = r.ready();
    r.pointer(point, PointerButton::Secondary, true);
    r.pointer(point, PointerButton::Secondary, false);
    r.idle();
    assert_eq!(r.mutable(), r.stable, "secondary input cannot set Pick");
    r.click();
    r.undo_redo();
}
#[test]
fn pick_actual_compare_active_only_payload_and_undo_redo() {
    let mut r = Review::new(true);
    r.click();
    r.undo_redo();
}
#[test]
fn pick_actual_repeat_pick_records_exact_state_and_undo_in_both_views() {
    for compare in [false, true] {
        let mut r = Review::new(compare);
        r.click();
        r.observe("before-repeat-pick");
        let flags = json!(r.app.session.catalog.photos().map(|p| json!([p.id, p.flag])).collect::<Vec<_>>());
        let commits = r.app.session.commits();
        let undo = r.app.session.undo.len();
        // The current native receiver is an unconditional SETTER. Derive its full op
        // oracle from for_targets/Session::commit, not an assumed toggle or no-op UI.
        // Actual counts/payload are logged on release even if that prediction fails.
        r.click();
        assert_eq!(json!(r.app.session.catalog.photos().map(|p| json!([p.id, p.flag])).collect::<Vec<_>>()), flags);
        println!(
            "PICK_NATIVE_REPEAT {}",
            json!({"view":if compare {"Compare"} else {"Library"},
            "commitsBefore":commits,"commitsAfter":r.app.session.commits(),"undoBefore":undo,
            "undoAfter":r.app.session.undo.len(),"journalLast":r.app.session.journal.last(),"flags":flags})
        );
        r.observe("after-repeat-pick");
        r.undo_redo();
    }
}
#[test]
fn pick_actual_natural_tab_shift_tab_records_both_view_orders() {
    for compare in [false, true] {
        let mut r = Review::new(compare);
        r.ready();
        r.frame(vec![Event::PointerGone]);
        r.observe("initial-natural-focus");
        let mut reached = false;
        let mut orders = Vec::new();
        for shift in [false, true] {
            let mut order = Vec::new();
            for step in 0..64 {
                r.key(Key::Tab, shift);
                reached |= r.focused_pick();
                order.push(json!({"step":step,"name":r.focus_name(),"raw":format!("{:?}",r.ctx.memory(|m|m.focused())),"pick":r.focused_pick()}));
                r.observe(&format!("natural-{}-{step}", if shift { "reverse" } else { "forward" }));
            }
            orders.push(json!({"shift":shift,"order":order}));
        }
        println!(
            "PICK_NATIVE_FOCUS {}",
            json!({"view":if compare {"Compare"} else {"Library"},"pickNaturallyReached":reached,"orders":orders,
            "scope":"Recorded genuine Tab/Shift-Tab only; no request_focus, key bridge, or Enter/Space qualification is inferred."})
        );
        r.safety();
    }
}
#[test]
fn pick_actual_disabled_press_real_release_no_replay_then_fresh_positive() {
    Review::new(false).gates(Pass::Disabled);
}
#[test]
fn pick_actual_sdk_measure_press_real_release_no_replay_then_fresh_positive() {
    Review::new(false).gates(Pass::Measure);
}
#[test]
fn pick_actual_egui_sizing_press_real_release_no_replay_then_fresh_positive() {
    Review::new(false).gates(Pass::Sizing);
}

#[test]
fn pick_actual_frozen_native_focus_order_with_only_pick_insertion() {
    for compare in [false, true] {
        let mut r = Review::new(compare);
        // Tab visits the right-hand group first (it is shown first), then the view buttons and Sort, then the three
        // flags, Copy and the gear. The thumbnail slider and the zoom menus are framework stops too.
        let native: Vec<&str> = if compare {
            vec![
                "icon:autoAdvance",
                "icon:beforeAfter",
                "icon:filmstrip",
                "dropdown:zoom",
                "dropdown:clickZoom",
                "icon:photoGrid",
                "icon:squareGrid",
                "icon:detail",
                "icon:compare",
                "icon:survey",
                "icon:people",
                "icon:sort",
            ]
        } else {
            vec!["slider:thumbSize", "icon:photoGrid", "icon:squareGrid", "icon:detail", "icon:compare", "icon:survey", "icon:people", "icon:sort"]
        };
        // The rating pill's stars are one framework stop (the focused star) between Sort and the flags.
        let mut expected = native.clone();
        expected.push("star:bottom:1");
        expected.extend(["flag:unflag", PICK, "flag:reject"]);
        expected.extend(["button:copySettings", "icon:copyGear"]);
        r.ready();
        r.frame(vec![Event::PointerGone]);
        let mut forward = Vec::new();
        for _ in 0..expected.len() * 2 {
            r.key(Key::Tab, false);
            forward.push(r.focus_name());
        }
        assert_eq!(forward, expected.iter().cycle().take(expected.len() * 2).map(|s| s.to_string()).collect::<Vec<_>>());
        // Complete forward cycles end at gear. Reverse starts at Copy, then the flags,
        // every native stop in reverse order, and finally gear.
        let mut expected_reverse = vec!["button:copySettings", "flag:reject", PICK, "flag:unflag", "star:bottom:1"];
        expected_reverse.extend(native.iter().rev().copied());
        expected_reverse.push("icon:copyGear");
        let mut reverse = Vec::new();
        for _ in 0..expected.len() * 2 {
            r.key(Key::Tab, true);
            reverse.push(r.focus_name());
        }
        assert_eq!(reverse, expected_reverse.iter().cycle().take(expected.len() * 2).map(|s| s.to_string()).collect::<Vec<_>>());
        println!(
            "PICK_SDK_NATIVE_ORDER {}",
            json!({"compare":compare,"native":native,"expectedForward":expected,"expectedReverse":expected_reverse,"forward":forward,"reverse":reverse})
        );
    }
}
#[test]
fn pick_actual_natural_focused_enter_space_targeting_and_undo_redo() {
    for compare in [false, true] {
        for key in [Key::Enter, Key::Space] {
            let mut r = Review::new(compare);
            r.tab_to_pick();
            assert!(r.focused_pick());
            r.observe(&format!("before-focused-{key:?}"));
            r.prepare();
            r.key(key, false);
            r.accept(&format!("focused-{key:?}-applied"));
            r.undo_redo();
        }
    }
}
#[test]
fn pick_actual_active_photo_change_cancels_old_press_then_fresh_click() {
    for compare in [false, true] {
        Review::new(compare).owner_change(false);
    }
}
#[test]
fn pick_actual_selection_change_cancels_old_press_then_fresh_click() {
    Review::new(false).owner_change(true);
}
