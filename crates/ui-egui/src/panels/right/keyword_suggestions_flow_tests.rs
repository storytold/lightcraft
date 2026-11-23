//! Suggestion-only SDK candidate. Original native failure packet remains separate and unchanged.
use crate::{LightcraftApp, Services, headless::HeadlessView};
use egui::{Event, Id, Key, Modifiers, PointerButton, Pos2, Rect, Response, vec2};
use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const ROME: &str = "Italy|Rome|Market";
const PARIS: &str = "France|Paris|Market";
const SDK_SUGGESTIONS: bool = true;
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
    fn new(grouped: bool) -> Self {
        let old_language = crate::i18n::language();
        crate::i18n::set_language(crate::i18n::Locale::En);
        let mut session = lightcraft_engine::Session::new();
        for (id, keywords) in [(101, vec!["beach", ROME, "Travel|Lisbon"]), (102, vec!["beach"]), (103, vec!["unrelated", PARIS])] {
            let mut photo = Photo::new(PhotoId(id), Source::Demo { scene: 0 }, &format!("demo-{id}.jpg"), "JPEG", 64, 48, "2026-11-21T00:00:00");
            photo.meta.keywords = keywords.into_iter().map(str::to_owned).collect();
            photo.meta.title = format!("Untouched title {id}");
            photo.meta.caption = format!("Untouched caption {id}");
            session.commit("suggestion fixture", Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        }
        session.execute("library.select", &json!({"ids":[101,102],"active":101})).unwrap();
        // Fixture preferences, before the immutable baseline. A case variant already in recents
        // must be replaced by this exact canonical path and promoted, not duplicated.
        session.recent_keywords =
            ["old0", "old1", "old2", "old3", "italy|rome|market", "old5", "old6", "old7", "old8"].into_iter().map(str::to_owned).collect();
        let mut app = LightcraftApp::new(session, Services { local_places: Some(vec![]), png: None, ..Default::default() });
        app.ui.settings.keyword_chips = if grouped { crate::state::KeywordChips::Grouped } else { crate::state::KeywordChips::Paths };
        app.run("panel.keywords", json!({})).unwrap();
        assert_eq!(app.ui.right, crate::state::RightPanel::Keywords);
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
        assert!(r.rects.contains_key(&Self::alias(ROME)));
        assert!(r.rects.contains_key(&Self::alias(PARIS)));
        assert!(!r.rects.contains_key("kwSuggest:beach"), "common keywords are not suggestions");
        r.observe("opened-keywords");
        r
    }
    fn alias(keyword: &str) -> String {
        format!("kwSuggest:{keyword}")
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
            "keywordSets":s.keyword_sets,"keywordSet":s.keyword_set,"interaction":s.interaction.is_some()})
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
        assert_eq!(self.invariants(), self.invariant, "only the precise metadata/recents/history transaction may change");
        let actual = self.mutable();
        assert!(
            actual == self.stable || self.pending.as_ref() == Some(&actual),
            "unexpected native action or replay in {:?}: actual={actual}",
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
        let mut last = None;
        let output = Output(ctx.run_ui(raw, |root| {
            egui::CentralPanel::default().show(root, |ui| {
                // Ordinary Library tool-panel width in the existing ordinary viewport.
                ui.set_width(360.0);
                let mut draw = |ui: &mut egui::Ui| crate::panels::right::show(app, ui);
                match pass {
                    Pass::Real => draw(ui),
                    Pass::Disabled => {
                        ui.disable();
                        draw(ui);
                    }
                    Pass::Measure => craft::ui::runtime::measure::run(&ctx, || draw(ui)),
                    Pass::Sizing => {
                        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(ui.max_rect()).sizing_pass());
                        draw(&mut child);
                    }
                }
            });
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
            let responses = ids.into_iter().filter_map(|id| ctx.read_response(id)).collect::<Vec<_>>();
            last = Some((crate::widgets::take_registries(&ctx), responses, focused, clicked));
        }));
        let ((rects, hits), responses, focused, clicked) = last.unwrap();
        for (name, _) in rects.iter().filter(|(name, _)| name.starts_with("kwSuggest:")) {
            assert_eq!(rects.iter().filter(|(other, _)| other == name).count(), 1, "one real suggestion per full-path locator");
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
                "KEYWORD_SUGGEST_INPUT {}",
                json!({"frame":self.tick,"eguiPass":self.ctx.cumulative_pass_nr(),"pass":format!("{:?}",self.pass),"events":input_trace,
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
        let mut candidates = self
            .responses
            .iter()
            .filter(|r| r.sense.senses_click() && r.rect == self.rect(alias) && r.layer_id == hit.layer_id && r.interact_rect.contains(point));
        let first = candidates.next()?;
        assert!(candidates.next().is_none(), "ambiguous genuine clickable owner at {alias}");
        Some(first)
    }
    fn ready(&mut self, keyword: &str) -> (Pos2, Id) {
        assert_eq!(self.pass, Pass::Real);
        let alias = Self::alias(keyword);
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
                if !SDK_SUGGESTIONS {
                    assert_ne!(id, self.hits[&alias].id, "native hover probe is not the actual button");
                }
                self.owners.insert(alias.clone(), id);
                println!(
                    "KEYWORD_SUGGEST_READY {}",
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
        panic!("native suggestion never obtains a stable real interior owner: {alias}");
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
        if let Some(response) = &self.focused {
            if let Some((name, _)) =
                self.rects.iter().find(|(name, rect)| name.starts_with("kwSuggest:") && **rect == response.rect && response.sense.senses_click())
            {
                return name.clone();
            }
        }
        if let Some(record) = craft::ui::runtime::focus::focused_rec(&self.ctx) {
            if let Some((name, _)) = self.hits.iter().find(|(_, hit)| hit.id == record.stop.id) {
                return name.clone();
            }
        }
        self.focused.as_ref().map_or_else(
            || "none".into(),
            |r| self.rects.iter().find(|(_, rect)| **rect == r.rect).map_or_else(|| format!("unregistered:{:?}", r.id), |(name, _)| name.clone()),
        )
    }
    fn focused_suggestion(&self, keyword: &str) -> bool {
        let alias = Self::alias(keyword);
        self.focused.as_ref().is_some_and(|r| {
            r.sense.senses_click()
                && r.has_focus()
                && r.rect == self.rect(&alias)
                && r.interact_rect.is_positive()
                && (SDK_SUGGESTIONS || r.id != self.hits[&alias].id)
        })
    }
    fn tab_to(&mut self, keyword: &str, label: &str) {
        self.frame(vec![Event::PointerGone]);
        for step in 0..64 {
            if self.focused_suggestion(keyword) {
                self.owners.insert(Self::alias(keyword), self.focused.as_ref().unwrap().id);
                return;
            }
            self.key(Key::Tab, false);
            self.observe(&format!("{label}-tab-{step}"));
        }
        panic!("natural native Tab cannot reach {}", Self::alias(keyword));
    }
    fn observe(&self, stage: &str) {
        let s = &self.app.session;
        println!(
            "KEYWORD_SUGGEST_FLOW {}",
            json!({"stage":stage,"pass":format!("{:?}",self.pass),"sdk":SDK_SUGGESTIONS,
            "focus":self.focus_name(),"rawFocus":format!("{:?}",self.ctx.memory(|m|m.focused())),
            "popup":egui::Popup::is_any_open(&self.ctx),"suggestions":self.rects.keys().filter(|k|k.starts_with("kwSuggest:")).collect::<Vec<_>>(),
            "keywords":s.catalog.photos().map(|p|json!([p.id,p.meta.keywords])).collect::<Vec<_>>(),
            "recent":s.recent_keywords,"journal":s.journal,"undoCount":s.undo.len(),"redoCount":s.redo.len(),"commits":s.commits()})
        );
    }
    fn prepare(&mut self, keyword: &str) {
        assert!(self.pending.is_none());
        self.safety();
        let mut expected_catalog = self.app.session.catalog.clone();
        let ops = [PhotoId(101), PhotoId(102)]
            .into_iter()
            .map(|id| {
                let mut meta = expected_catalog.photo(id).unwrap().meta.clone();
                if !meta.keywords.iter().any(|k| k.eq_ignore_ascii_case(keyword)) {
                    meta.keywords.push(keyword.to_owned());
                }
                Op::SetMeta { id, meta: Box::new(meta) }
            })
            .collect();
        // Pure independently predicted SetMeta batch on a COPY, not the command under test.
        let inverse = expected_catalog.apply(Op::Batch { ops }).unwrap();
        let mut next = self.stable.clone();
        next["catalog"] = json!(expected_catalog);
        next["revision"] = json!(expected_catalog.revision);
        next["undo"].as_array_mut().unwrap().push(json!({"label":"Edit Info","op":inverse,"folder":"None"}));
        next["redo"] = json!([]);
        next["commits"] = json!(self.app.session.commits() + 1);
        next["journal"].as_array_mut().unwrap().push(json!(["photo.setMeta",{"addKeywords":[keyword]}]));
        let mut recent = self.app.session.recent_keywords.clone();
        recent.retain(|k| !k.eq_ignore_ascii_case(keyword));
        recent.insert(0, keyword.to_owned());
        recent.truncate(9);
        next["recent"] = json!(recent);
        self.pending = Some(next);
        self.inverse = Some(inverse);
    }
    fn accept(&mut self, stage: &str) {
        let expected = self.pending.take().unwrap();
        assert_eq!(self.mutable(), expected, "actual suggestion must produce the complete independently predicted transaction");
        self.stable = expected;
        self.idle();
        self.observe(stage);
    }
    fn click(&mut self, keyword: &str) {
        let alias = Self::alias(keyword);
        let (point, id) = self.ready(keyword);
        self.pointer(point, PointerButton::Primary, true);
        let response = self.actual_at(&alias, point).expect("actual owner still exists on press");
        assert_eq!(response.id, id);
        assert!(response.is_pointer_button_down_on(), "genuine native button, not its hover probe, owns the press");
        assert_eq!(self.mutable(), self.stable, "press alone must not dispatch");
        self.prepare(keyword);
        self.pointer(point, PointerButton::Primary, false);
        assert_eq!(self.clicked, Some(id), "release belongs to the actual suggestion");
        println!(
            "KEYWORD_SUGGEST_INPUT {}",
            json!({"alias":alias,"actual":format!("{id:?}"),"phase":"release","clicked":format!("{:?}",self.clicked),"point":[point.x,point.y]})
        );
        self.accept("pointer-applied");
        assert!(!self.rects.contains_key(&alias), "a now-common keyword disappears, so it cannot replay as this suggestion");
    }
    fn undo_redo(&mut self) {
        let inverse = self.inverse.clone().unwrap();
        let mut catalog = self.app.session.catalog.clone();
        let forward = catalog.apply(inverse).unwrap();
        let mut next = self.stable.clone();
        next["catalog"] = json!(catalog);
        next["revision"] = json!(catalog.revision);
        next["undo"].as_array_mut().unwrap().pop().unwrap();
        next["redo"].as_array_mut().unwrap().push(json!({"label":"Edit Info","op":forward,"folder":"None"}));
        next["journal"].as_array_mut().unwrap().push(json!(["edit.undo", {}]));
        // Undo restores metadata but does not rewind Recent Keywords or the commit counter.
        self.pending = Some(next);
        self.app.run("edit.undo", json!({})).unwrap();
        self.accept("undo");
        let undo = catalog.apply(forward).unwrap();
        let mut next = self.stable.clone();
        next["catalog"] = json!(catalog);
        next["revision"] = json!(catalog.revision);
        next["redo"].as_array_mut().unwrap().pop().unwrap();
        next["undo"].as_array_mut().unwrap().push(json!({"label":"Edit Info","op":undo,"folder":"None"}));
        next["journal"].as_array_mut().unwrap().push(json!(["edit.redo", {}]));
        self.pending = Some(next);
        self.app.run("edit.redo", json!({})).unwrap();
        self.accept("redo");
    }
    fn gates(&mut self, pass: Pass) {
        // Pointer and split-frame replay probes do not depend on keyboard reachability.
        let (point, _) = self.ready(ROME);
        self.pass = pass;
        self.idle();
        if SDK_SUGGESTIONS && matches!(pass, Pass::Measure | Pass::Sizing) {
            assert!(!self.rects.contains_key(&Self::alias(ROME)), "non-real SDK runs publish no interactive suggestion locator");
            assert!(!self.hits.contains_key(&Self::alias(ROME)), "non-real SDK runs publish no actual input owner");
        } else {
            assert!(self.rect(&Self::alias(ROME)).contains(point), "blocked pass still lays out the same real suggestion");
        }
        self.observe("blocked-before-pointer");
        self.pointer(point, PointerButton::Primary, true);
        self.pointer(point, PointerButton::Primary, false);
        self.pass = Pass::Real;
        self.idle();
        self.observe("after-blocked-pointer-no-replay");
        // A captured measuring/disabled press must not turn a later ordinary release into an action.
        let (point, _) = self.ready(ROME);
        self.pass = pass;
        self.pointer(point, PointerButton::Primary, true);
        self.pass = Pass::Real;
        self.pointer(point, PointerButton::Primary, false);
        self.idle();
        self.observe("blocked-press-real-release-no-replay");
        // Keyboard negatives begin with a naturally reached genuine native suggestion, never
        // a memory/request_focus mutation. Freeze whether the blocked parent retains or clears it.
        self.tab_to(ROME, "gate-focus");
        assert!(self.focused_suggestion(ROME));
        self.pass = pass;
        self.idle();
        self.observe("blocked-before-keyboard");
        let held_focus = self.ctx.memory(|m| m.focused());
        self.key(Key::Enter, false);
        self.key(Key::Space, false);
        assert!(
            self.ctx.memory(|m| m.focused()).is_none() || self.ctx.memory(|m| m.focused()) == held_focus,
            "blocked input must not steal another focus owner"
        );
        self.pass = Pass::Real;
        self.idle();
        self.observe("after-blocked-keyboard-no-replay");
        // Positive control on this SAME real parent after the blocked inputs.
        self.click(ROME);
        self.undo_redo();
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
fn keyword_suggestions_actual_partly_shared_full_path_click_recents_and_undo_redo() {
    let mut r = Review::new(false);
    let alias = Review::alias(ROME);
    let (point, _) = r.ready(ROME);
    r.pointer(point, PointerButton::Secondary, true);
    r.pointer(point, PointerButton::Secondary, false);
    r.idle();
    assert_eq!(r.mutable(), r.stable, "secondary click is not an add action");
    // Reacquire the actual button after every sequence; do not reuse a stale probe.
    assert!(r.rects.contains_key(&alias));
    r.click(ROME);
    r.undo_redo();
}
#[test]
fn keyword_suggestions_actual_grouped_duplicate_leaf_paths_stay_distinct() {
    for keyword in [ROME, PARIS] {
        let mut r = Review::new(true);
        for path in [ROME, PARIS] {
            let rect = r.rect(&Review::alias(path));
            let label = format!("+ {}", path.replace('|', " › "));
            assert!(
                r.painted.iter().any(|(text, painted)| text == &label && rect.contains_rect(*painted)),
                "real grouped duplicate leaf must disambiguate {path}"
            );
        }
        assert_ne!(r.rect(&Review::alias(ROME)), r.rect(&Review::alias(PARIS)));
        let (_, rome) = r.ready(ROME);
        let (_, paris) = r.ready(PARIS);
        assert_ne!(rome, paris, "distinct native owners for full paths with the same leaf");
        r.click(keyword);
        r.undo_redo();
    }
}
#[test]
fn keyword_suggestions_actual_natural_tab_shift_tab_and_focused_enter_space() {
    for key in [Key::Enter, Key::Space] {
        let mut r = Review::new(false);
        r.tab_to(ROME, &format!("{key:?}"));
        r.observe("first-focused-suggestion");
        r.key(Key::Tab, true);
        r.observe("reverse-neighbor");
        assert!(!r.focused_suggestion(ROME), "Shift-Tab really leaves this button");
        r.key(Key::Tab, false);
        assert!(r.focused_suggestion(ROME), "forward Tab returns through the real native neighbor");
        r.observe("forward-return");
        r.key(Key::Tab, false);
        r.observe("forward-neighbor");
        assert!(!r.focused_suggestion(ROME), "Tab really leaves this button");
        r.key(Key::Tab, true);
        assert!(r.focused_suggestion(ROME), "Shift-Tab returns through the real native neighbor");
        r.observe("reverse-return");
        r.prepare(ROME);
        r.key(key, false);
        r.accept(&format!("focused-{key:?}-applied"));
        r.undo_redo();
    }
}
#[test]
fn keyword_suggestions_actual_disabled_parent_does_not_dispatch_or_replay() {
    Review::new(false).gates(Pass::Disabled);
}
#[test]
fn keyword_suggestions_actual_sdk_measure_does_not_dispatch_or_replay() {
    Review::new(false).gates(Pass::Measure);
}
#[test]
fn keyword_suggestions_actual_egui_sizing_does_not_dispatch_or_replay() {
    Review::new(false).gates(Pass::Sizing);
}

// Frozen from 64 actual native forward-Tab observations: two complete 26-stop cycles and
// twelve more stops. No native suggestion was a stop. Reverse traversal/activation were
// unreached natively; this candidate must add only its four real suggestion owners.
const NATIVE_TAB_CYCLE: [&str; 26] = [
    "field:keyword",
    "keywordView:keywords",
    "keywordView:containing",
    "keywordView:willExport",
    "field:keywordPainter",
    "button:keywordPaint",
    "field:keywordFilter",
    "keywordList:create",
    "keywordRow:beach",
    "keywordCheck:beach",
    "keywordShow:beach",
    "keywordRow:France",
    "keywordRowToggle:France",
    "keywordCheck:France",
    "keywordShow:France",
    "keywordRow:Italy",
    "keywordRowToggle:Italy",
    "keywordCheck:Italy",
    "keywordShow:Italy",
    "keywordRow:Travel",
    "keywordRowToggle:Travel",
    "keywordCheck:Travel",
    "keywordShow:Travel",
    "keywordRow:unrelated",
    "keywordCheck:unrelated",
    "keywordShow:unrelated",
];

#[test]
fn keyword_suggestions_actual_frozen_native_order_with_only_suggestion_insertions() {
    let mut r = Review::new(false);
    r.frame(vec![Event::PointerGone]);
    assert!(r.ctx.memory(|m| m.focused()).is_none());
    let mut expected = NATIVE_TAB_CYCLE.iter().map(|name| (*name).to_owned()).collect::<Vec<_>>();
    expected.splice(6..6, [ROME, "Travel|Lisbon", PARIS, "unrelated"].into_iter().map(Review::alias));
    assert_eq!(expected.len(), 30);
    for step in 0..60 {
        r.key(Key::Tab, false);
        r.observe(&format!("frozen-forward-{step}"));
        assert_eq!(r.focus_name(), expected[step % expected.len()], "only suggestion stops may be inserted in the measured native cycle");
        assert!(!egui::Popup::is_any_open(&r.ctx));
    }
    for step in 0..60 {
        r.key(Key::Tab, true);
        r.observe(&format!("frozen-reverse-{step}"));
        let index = expected.len() - 1 - ((step + 1) % expected.len());
        assert_eq!(r.focus_name(), expected[index], "ordinary Shift-Tab must reverse the same real owner cycle");
        assert!(!egui::Popup::is_any_open(&r.ctx));
    }
    assert_eq!(r.mutable(), r.stable, "focus-only journeys never write metadata or recents");
}

impl Review {
    fn select_owner(&mut self, active: u64) {
        self.safety();
        assert!(matches!(active, 101 | 102));
        let args = json!({"ids":[101,102],"active":active});
        let mut next = self.stable.clone();
        next["journal"].as_array_mut().unwrap().push(json!(["library.select", args]));
        let mut invariant = self.invariant.clone();
        invariant["selection"] = json!({"ids":[101,102],"active":active});
        // Exactly this genuine selection command is allowed. Do not accept a fresh whole-state
        // snapshot: all other UI, catalog, preset, history and third-photo bytes stay frozen.
        self.app.run("library.select", args).unwrap();
        assert_eq!(self.mutable(), next);
        assert_eq!(self.invariants(), invariant);
        self.invariant = invariant;
        self.stable = next;
        self.idle();
        self.observe("selected-current-owner");
    }
}

#[test]
fn keyword_suggestions_actual_active_owner_change_does_not_replay() {
    let mut r = Review::new(false);
    let (point, old) = r.ready(ROME);
    let rect = r.rect(&Review::alias(ROME));
    r.pointer(point, PointerButton::Primary, true);
    assert!(r.responses.iter().any(|response| response.id == old && response.is_pointer_button_down_on()));
    // Both originals remain selected; only the active right-panel owner changes. The catalog,
    // suggestion geometry, full-path target and untouched third photo must remain identical.
    r.select_owner(102);
    let (next_point, current) = r.ready(ROME);
    assert_eq!(r.rect(&Review::alias(ROME)), rect);
    assert_eq!(next_point, point);
    assert_ne!(old, current, "a physical press cannot cross the actual right-panel owner");
    r.pointer(point, PointerButton::Primary, false);
    r.idle();
    r.observe("old-owner-release-does-not-apply");
    assert_eq!(r.mutable(), r.stable);
    r.select_owner(101);
    r.click(ROME);
    r.undo_redo();
}
