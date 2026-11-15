use super::*;
use lightcraft_develop::{Mask, MaskComponent, MaskOp, SegMask};
use lightcraft_geom::Point;

fn settings() -> DevelopSettings {
    let source = SegMask::from_logits(2, &[8.0, -8.0, -8.0, -8.0]);
    let mut d = DevelopSettings::default();
    d.light.exposure = 0.7;
    d.masks.push(Mask {
        id: 7,
        name: "People".into(),
        components: vec![
            MaskComponent {
                name: Some("Description".into()),
                op: MaskOp::Add,
                invert: false,
                shape: MaskShape::Prompt { text: "person".into(), seg: Some(source.clone()), detail: vec![source.clone()], edge: 24.0 },
            },
            MaskComponent {
                name: Some("Object".into()),
                op: MaskOp::Subtract,
                invert: true,
                shape: MaskShape::Object {
                    hint: vec![Point::new(0.2, 0.3)],
                    exclude: vec![Point::new(0.8, 0.7)],
                    seg: Some(source.clone()),
                    detail: vec![source],
                    edge: -12.0,
                },
            },
            MaskComponent {
                name: Some("Manual".into()),
                op: MaskOp::Intersect,
                invert: false,
                shape: MaskShape::Linear { start: Point::new(0.1, 0.2), end: Point::new(0.7, 0.8) },
            },
        ],
        ..Default::default()
    });
    d.masks[0].adjust.exposure = 1.3;
    d
}

fn copied() -> (Session, PhotoId, PhotoId) {
    let mut s = Session::with_demo();
    let source = s.active().unwrap();
    let target = s.catalog.photos().find(|p| p.id != source).unwrap().id;
    s.set_develop(source, settings(), "Source").unwrap();
    s.execute("develop.copy", &json!({"groups": ["light", "masks"]})).unwrap();
    s.previous_active = Some(source);
    (s, source, target)
}

/// These commands formerly succeeded by installing the source photo's cached segmentation.
#[test]
fn paste_ai_masks_requires_a_model_without_changing_any_destination() {
    for background in [false, true] {
        for command in ["develop.paste", "develop.pastePrevious"] {
            let (mut s, _, target) = copied();
            s.segmenter.background = background;
            let before = s.develop_of(target).unwrap();
            let commits = s.commits();
            let e = s.execute(command, &json!({"ids": [target.0], "groups": ["light", "masks"]})).unwrap_err();
            assert!(e.to_string().contains("AI masks") || e.to_string().contains("SAM 3"), "{command}: {e}");
            assert_eq!(s.develop_of(target).unwrap(), before);
            assert_eq!(s.commits(), commits);
            assert!(s.activity.list().is_empty());
        }
    }
}

#[test]
fn paste_without_masking_preserves_the_destinations_masks_without_needing_a_model() {
    let (mut s, _, target) = copied();
    let mut own = settings();
    own.light.exposure = -0.4;
    s.set_develop(target, own.clone(), "Own").unwrap();
    s.execute("develop.paste", &json!({"ids": [target.0], "groups": ["light"]})).unwrap();
    let after = s.develop_of(target).unwrap();
    assert_eq!(after.light.exposure, 0.7);
    assert_eq!(after.masks, own.masks);
    s.undo_step().unwrap();
    assert_eq!(*s.develop_of(target).unwrap(), own);
}

#[test]
fn manual_masks_still_paste_immediately_in_one_undo_step() {
    let (mut s, source, target) = copied();
    let mut manual = settings();
    manual.masks[0].components.drain(..2);
    s.set_develop(source, manual.clone(), "Manual").unwrap();
    s.execute("develop.copy", &json!({"groups": ["masks"]})).unwrap();
    let before = s.develop_of(target).unwrap();
    let commits = s.commits();
    let out = s.execute("develop.paste", &json!({"ids": [target.0]})).unwrap();
    assert_eq!(out["changed"], 1);
    assert!(out.get("pending").is_none());
    assert_eq!(s.develop_of(target).unwrap().masks, manual.masks);
    assert_eq!(s.commits(), commits + 1);
    s.undo_step().unwrap();
    assert_eq!(s.develop_of(target).unwrap(), before);
}

#[test]
fn paste_bounds_inference_work_before_cloning_the_clipboard_for_all_targets() {
    let (mut s, _, target) = copied();
    let before = s.develop_of(target).unwrap();
    let partial = s.clipboard.clone().unwrap();
    let ids = vec![target; MAX_SELECTIONS];
    let err = s.paste_settings(&ids, &partial, "Paste Settings").unwrap_err();
    assert!(err.to_string().contains("fewer photos"), "{err}");
    assert_eq!(s.develop_of(target).unwrap(), before);
    assert!(s.activity.list().is_empty());
}

#[cfg(feature = "sam")]
fn outcome(result: std::result::Result<Option<SegMask>, String>) -> worker::Outcome {
    worker::Outcome { tag: Tag::Wait, result, superseded: false, notice: None }
}

#[cfg(feature = "sam")]
#[test]
fn recompute_keeps_adjustments_and_manual_components_but_replaces_every_cached_shape() {
    let (mut s, source, target) = copied();
    let other = s.catalog.photos().find(|p| p.id != source && p.id != target).unwrap().id;
    let before = [s.develop_of(target).unwrap(), s.develop_of(other).unwrap()];
    let original = s.develop_of(source).unwrap();
    let mut pending = Pending::new(&s, vec![(target, (*original).clone()), (other, (*original).clone())], "Paste Settings").unwrap();
    assert_eq!(pending.queue.len(), 4);
    assert_eq!(s.activity.list()[0].label, "Recomputing AI masks");
    for t in &pending.targets {
        assert_eq!(t.settings.masks[0].adjust, original.masks[0].adjust);
        assert_eq!(t.settings.masks[0].components[2], original.masks[0].components[2]);
        for c in &t.settings.masks[0].components[..2] {
            match &c.shape {
                MaskShape::Object { seg, detail, .. } | MaskShape::Prompt { seg, detail, .. } => {
                    assert!(seg.is_none() && detail.is_empty(), "nothing from the source photo");
                }
                _ => panic!("an AI selection"),
            }
        }
        let (_, input) = s.segment_key_for(t.id, &t.settings).unwrap();
        assert!(input.masks.is_empty());
        assert_eq!(input.light.exposure, 0.7, "model sees the pasted look, not the old look");
    }
    let mut expected = [(*original).clone(), (*original).clone()];
    let mut n = 0;
    while let Some(selection) = pending.queue.pop_front() {
        // Each component of each photo gets a different result. The final one finds nothing.
        let found = (n != 3).then(|| SegMask::from_logits(2, &[n as f32 + 1.0; 4]));
        let c = &mut expected[selection.target].masks[selection.mask].components[selection.component];
        match &mut c.shape {
            MaskShape::Object { seg, detail, .. } | MaskShape::Prompt { seg, detail, .. } => {
                *seg = Some(found.clone().unwrap_or_else(|| SegMask::from_logits(1, &[-16.0])));
                detail.clear();
            }
            _ => panic!("AI"),
        }
        pending.accept(selection, outcome(Ok(found))).unwrap();
        n += 1;
    }
    let commits = s.commits();
    pending.finish(&mut s).unwrap();
    assert!(s.activity.list().is_empty());
    assert_eq!(s.commits(), commits + 1);
    assert_eq!(*s.develop_of(target).unwrap(), expected[0]);
    assert_eq!(*s.develop_of(other).unwrap(), expected[1]);
    assert_eq!(s.develop_of(source).unwrap(), original, "clipboard source unchanged");
    s.undo_step().unwrap();
    assert_eq!(s.develop_of(target).unwrap(), before[0]);
    assert_eq!(s.develop_of(other).unwrap(), before[1]);
    s.redo_step().unwrap();
    assert_eq!(*s.develop_of(target).unwrap(), expected[0]);
    assert_eq!(*s.develop_of(other).unwrap(), expected[1]);
}

#[cfg(feature = "sam")]
#[test]
fn background_results_finish_after_navigation_but_never_overwrite_edits_or_cancellation() {
    for mode in ["success", "edited", "library", "cancelled", "failed"] {
        let (mut s, _, target) = copied();
        let mut d = settings();
        d.masks[0].components.truncate(1);
        let mut pending = Pending::new(&s, vec![(target, d)], "Paste Settings").unwrap();
        let selection = pending.queue.pop_front().unwrap();
        let (tx, rx) = mpsc::channel();
        pending.flight = Some(Flight { selection, rx, started: Instant::now() });
        let task = pending.task.id();
        s.segmenter.paste = Some(pending);
        assert!(s.segmenter.busy());
        if mode == "edited" {
            s.set_develop(target, DevelopSettings::default(), "New edit").unwrap();
        } else if mode == "library" {
            s.library_identity = Arc::new(());
        } else if mode == "cancelled" {
            s.activity.cancel(task).unwrap();
        }
        let before = s.develop_of(target).unwrap();
        let result = if mode == "failed" { Err("test inference failed".into()) } else { Ok(Some(SegMask::from_logits(1, &[7.0]))) };
        tx.send(outcome(result)).unwrap();
        // Active photo is still the source, not the destination.
        assert_ne!(s.active(), Some(target));
        let polled = s.segment_poll();
        assert_eq!(polled.changed, mode == "success", "{mode}");
        if mode == "success" {
            assert!(polled.messages.is_empty());
            let d = s.develop_of(target).unwrap();
            assert_eq!(d.masks[0].adjust.exposure, 1.3);
            assert!(matches!(&d.masks[0].components[0].shape, MaskShape::Prompt { seg: Some(_), detail, .. } if detail.is_empty()));
        } else {
            assert_eq!(s.develop_of(target).unwrap(), before, "{mode}");
            assert_eq!(polled.messages.len(), 1, "{mode}: {:?}", polled.messages);
        }
        assert!(!s.segmenter.busy());
        assert!(s.activity.list().is_empty());
    }
}

#[cfg(feature = "sam")]
fn completed_paste(s: &Session, ids: &[PhotoId]) -> Pending {
    let mut pending = Pending::new(s, ids.iter().map(|id| (*id, settings())).collect(), "Paste Settings").unwrap();
    complete(&mut pending);
    pending
}

#[cfg(feature = "sam")]
fn complete(pending: &mut Pending) {
    while let Some(selection) = pending.queue.pop_front() {
        pending.accept(selection, outcome(Ok(Some(SegMask::from_logits(1, &[7.0]))))).unwrap();
    }
}

#[cfg(feature = "sam")]
struct DamagedModel(std::path::PathBuf);

#[cfg(feature = "sam")]
impl DamagedModel {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("lc-sam3-paste-queue-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("model.safetensors"), b"\x10\x00\x00\x00\x00\x00\x00\x00{not json at all}").unwrap();
        std::fs::write(dir.join("vocab.json"), b"{}").unwrap();
        std::fs::write(dir.join("merges.txt"), b"#version: 0.2\n").unwrap();
        Self(dir)
    }
}

#[cfg(feature = "sam")]
impl Drop for DamagedModel {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(feature = "sam")]
#[test]
fn a_second_paste_queues_without_replacing_the_running_paste() {
    let model = DamagedModel::new();
    for command in ["develop.paste", "develop.pastePrevious"] {
        let (mut s, source, first) = copied();
        let second = s.catalog.photos().find(|p| p.id != source && p.id != first).unwrap().id;
        let before = [s.develop_of(first).unwrap(), s.develop_of(second).unwrap()];
        s.segmenter.background = true;
        s.segmenter.dir = Some(model.0.clone());
        s.segmenter.paste = Some(completed_paste(&s, &[first]));
        let commits = s.commits();
        let result = s.execute(command, &json!({"ids": [second.0]})).unwrap();
        assert_eq!(result["pending"], true);
        assert_eq!(result["queued"], true);
        assert_eq!(s.activity.list().len(), 2, "each paste click has its own cancellable operation");
        assert_eq!(result["task"], s.activity.list()[1].id);
        assert_eq!(s.activity.list()[1].label, "Queued AI mask paste");
        assert_eq!(s.activity.list()[1].detail, s.catalog.photo(second).unwrap().file_name);
        assert_eq!(s.develop_of(first).unwrap(), before[0]);
        assert_eq!(s.develop_of(second).unwrap(), before[1]);
        assert_eq!(s.commits(), commits);
    }
}

#[cfg(feature = "sam")]
#[test]
fn queued_pastes_capture_each_click_and_commit_in_order_with_separate_undo_steps() {
    let model = DamagedModel::new();
    let (mut s, source, first) = copied();
    let rest: Vec<_> = s.catalog.photos().filter(|p| p.id != source && p.id != first).take(2).map(|p| p.id).collect();
    let [second, third] = [rest[0], rest[1]];
    let ids = [first, second, third];
    let before = ids.map(|id| s.develop_of(id).unwrap());
    s.segmenter.background = true;
    s.segmenter.dir = Some(model.0.clone());
    let mut active = completed_paste(&s, &[first]);
    active.label = "First paste".into();
    s.segmenter.paste = Some(active);
    s.execute("develop.paste", &json!({"ids": [second.0]})).unwrap();
    let mut later = settings();
    later.light.exposure = 1.4;
    s.set_develop(source, later.clone(), "Later source").unwrap();
    s.execute("develop.pastePrevious", &json!({"ids": [third.0]})).unwrap();
    later.light.exposure = 2.1;
    s.set_develop(source, later, "After queue").unwrap();
    let latest_source = s.develop_of(source).unwrap();
    s.clipboard = Some(json!({"light": {"exposure": 9.0}}));
    let commits = s.commits();
    assert_eq!(s.segmenter.paste_queue.len(), 2);
    assert!(s.segment_poll().changed, "first click completes first");
    assert!(s.develop_of(second).unwrap().masks.is_empty());
    complete(s.segmenter.paste_queue.front_mut().unwrap());
    assert!(s.segment_poll().changed);
    assert_eq!(s.develop_of(second).unwrap().light.exposure, 0.7, "clipboard is captured at the click");
    assert!(s.develop_of(third).unwrap().masks.is_empty());
    complete(s.segmenter.paste_queue.front_mut().unwrap());
    assert!(s.segment_poll().changed);
    assert_eq!(s.develop_of(third).unwrap().light.exposure, 1.4, "Previous is captured at the click");
    assert_eq!(s.develop_of(source).unwrap(), latest_source);
    assert_eq!(s.commits(), commits + 3);
    assert!(s.activity.list().is_empty());
    assert!(!s.segmenter.busy());
    for (id, label) in [(third, "Paste from Previous"), (second, "Paste Settings"), (first, "First paste")] {
        assert_eq!(s.undo_step().unwrap(), label);
        assert_eq!(s.develop_of(id).unwrap(), before[ids.iter().position(|p| *p == id).unwrap()]);
    }
    for (id, exposure) in [(first, 0.7), (second, 0.7), (third, 1.4)] {
        s.redo_step().unwrap();
        assert_eq!(s.develop_of(id).unwrap().light.exposure, exposure);
    }
}

#[cfg(feature = "sam")]
#[test]
fn queued_cancellation_does_not_wait_for_or_cancel_the_active_paste() {
    let (mut s, source, first) = copied();
    let rest: Vec<_> = s.catalog.photos().filter(|p| p.id != source && p.id != first).take(2).map(|p| p.id).collect();
    let [second, third] = [rest[0], rest[1]];
    let before = [s.develop_of(first).unwrap(), s.develop_of(second).unwrap()];
    let mut active = Pending::new(&s, vec![(first, settings())], "First").unwrap();
    let selection = active.queue.pop_front().unwrap();
    let (tx, rx) = mpsc::channel();
    active.flight = Some(Flight { selection, rx, started: Instant::now() });
    let active_id = active.task.id();
    s.segmenter.paste = Some(active);
    let cancelled = completed_paste(&s, &[second]);
    s.activity.cancel(cancelled.task.id()).unwrap();
    s.segmenter.paste_queue.push_back(cancelled);
    s.segmenter.paste_queue.push_back(completed_paste(&s, &[third]));
    let polled = s.segment_poll();
    assert!(!polled.changed);
    assert_eq!(s.segmenter.paste_queue.len(), 1);
    assert_eq!(s.activity.list().len(), 2, "cancelled queued row disappears while the active worker waits");
    assert!(!s.segmenter.paste.as_ref().unwrap().task.is_cancelled());
    s.activity.cancel(active_id).unwrap();
    tx.send(outcome(Ok(Some(SegMask::from_logits(1, &[7.0]))))).unwrap();
    assert!(!s.segment_poll().changed);
    assert_eq!(s.develop_of(first).unwrap(), before[0]);
    assert_eq!(s.develop_of(second).unwrap(), before[1]);
    assert!(s.segment_poll().changed, "the uncancelled third click still completes");
    assert!(!s.develop_of(third).unwrap().masks.is_empty());
    assert!(s.activity.list().is_empty());
}

#[cfg(feature = "sam")]
#[test]
fn a_failed_paste_does_not_discard_the_next_queued_paste() {
    let (mut s, source, first) = copied();
    let second = s.catalog.photos().find(|p| p.id != source && p.id != first).unwrap().id;
    let before = s.develop_of(first).unwrap();
    let mut active = Pending::new(&s, vec![(first, settings())], "First").unwrap();
    let selection = active.queue.pop_front().unwrap();
    let (tx, rx) = mpsc::channel();
    active.flight = Some(Flight { selection, rx, started: Instant::now() });
    s.segmenter.paste = Some(active);
    s.segmenter.paste_queue.push_back(completed_paste(&s, &[second]));
    tx.send(outcome(Err("synthetic inference failure".into()))).unwrap();
    let failed = s.segment_poll();
    assert!(!failed.changed);
    assert_eq!(failed.messages, ["synthetic inference failure"]);
    assert_eq!(s.develop_of(first).unwrap(), before);
    assert!(s.segmenter.busy());
    assert!(s.segment_poll().changed);
    assert!(!s.develop_of(second).unwrap().masks.is_empty());
    assert!(s.activity.list().is_empty());
}

#[cfg(feature = "sam")]
#[test]
fn edited_queued_destinations_are_not_started_and_later_jobs_still_finish() {
    let (mut s, source, first) = copied();
    let second = s.catalog.photos().find(|p| p.id != source && p.id != first).unwrap().id;
    s.segmenter.paste_queue.push_back(Pending::new(&s, vec![(first, settings())], "Stale").unwrap());
    s.segmenter.paste_queue.push_back(completed_paste(&s, &[second]));
    let mut edited = DevelopSettings::default();
    edited.light.exposure = -0.2;
    s.set_develop(first, edited.clone(), "User edit").unwrap();
    let polled = s.segment_poll();
    assert!(!polled.changed);
    assert!(polled.messages[0].contains("destination photo changed"), "{:?}", polled.messages);
    assert_eq!(*s.develop_of(first).unwrap(), edited);
    assert!(s.segment_poll().changed);
    assert!(!s.develop_of(second).unwrap().masks.is_empty());
    assert!(s.activity.list().is_empty());
}

#[cfg(feature = "sam")]
#[test]
fn a_library_switch_discards_queued_results_even_when_photo_ids_are_reused() {
    let (mut s, _, target) = copied();
    let before = s.develop_of(target).unwrap();
    s.segmenter.paste_queue.push_back(completed_paste(&s, &[target]));
    s.library_identity = Arc::new(());
    let polled = s.segment_poll();
    assert!(!polled.changed);
    assert_eq!(polled.messages.len(), 1);
    assert_eq!(s.develop_of(target).unwrap(), before);
    assert!(!s.segmenter.busy());
    assert!(s.activity.list().is_empty());
}

#[cfg(feature = "sam")]
#[test]
fn overlapping_queue_requests_are_refused_as_a_whole() {
    let model = DamagedModel::new();
    let (mut s, source, first) = copied();
    let rest: Vec<_> = s.catalog.photos().filter(|p| p.id != source && p.id != first).take(2).map(|p| p.id).collect();
    let [second, third] = [rest[0], rest[1]];
    s.segmenter.dir = Some(model.0.clone());
    s.segmenter.background = true;
    s.segmenter.paste = Some(completed_paste(&s, &[first]));
    s.segmenter.paste_queue.push_back(completed_paste(&s, &[second]));
    let before = s.develop_of(third).unwrap();
    let err = s.execute("develop.paste", &json!({"ids": [second.0, third.0]})).unwrap_err();
    assert!(err.to_string().contains("already has a pending"), "{err}");
    assert_eq!(s.segmenter.paste_queue.len(), 1);
    assert_eq!(s.activity.list().len(), 2);
    assert_eq!(s.develop_of(third).unwrap(), before);
}

#[cfg(feature = "sam")]
#[test]
fn the_queue_bounds_all_requests_before_cloning_more_settings() {
    let model = DamagedModel::new();
    let (mut s, source, first) = copied();
    let second = s.catalog.photos().find(|p| p.id != source && p.id != first).unwrap().id;
    s.segmenter.dir = Some(model.0.clone());
    s.segmenter.background = true;
    let mut active = completed_paste(&s, &[first]);
    active.total = MAX_SELECTIONS as u64;
    s.segmenter.paste = Some(active);
    let err = s.execute("develop.paste", &json!({"ids": [second.0]})).unwrap_err();
    assert!(err.to_string().contains("10,000"), "{err}");
    assert_eq!(s.activity.list().len(), 1);
    s.segmenter.paste = None;
    for _ in 0..MAX_PASTES {
        s.segmenter.paste_queue.push_back(completed_paste(&s, &[first]));
    }
    let err = s.execute("develop.paste", &json!({"ids": [second.0]})).unwrap_err();
    assert!(err.to_string().contains("64"), "{err}");
    assert_eq!(s.segmenter.paste_queue.len(), MAX_PASTES);
    assert_eq!(s.activity.list().len(), MAX_PASTES);
}

#[cfg(feature = "sam")]
#[test]
fn a_new_click_does_not_jump_a_queue_between_polls() {
    let model = DamagedModel::new();
    let (mut s, source, first) = copied();
    let second = s.catalog.photos().find(|p| p.id != source && p.id != first).unwrap().id;
    s.segmenter.dir = Some(model.0.clone());
    s.segmenter.background = true;
    s.segmenter.paste_queue.push_back(completed_paste(&s, &[first]));
    assert!(s.segmenter.busy(), "queued jobs keep the UI polling even without an active flight");
    let r = s.execute("develop.paste", &json!({"ids": [second.0]})).unwrap();
    assert_eq!(r["queued"], true);
    assert!(s.segmenter.paste.is_none());
    assert_eq!(s.segmenter.paste_queue.front().unwrap().targets[0].id, first);
    assert!(s.segment_poll().changed);
    assert!(!s.develop_of(first).unwrap().masks.is_empty());
    assert!(s.develop_of(second).unwrap().masks.is_empty());
    complete(s.segmenter.paste_queue.front_mut().unwrap());
    assert!(s.segment_poll().changed);
    assert!(!s.develop_of(second).unwrap().masks.is_empty());
}

#[cfg(feature = "sam")]
#[test]
fn background_paste_rejects_open_destination_interactions_without_changing_history() {
    for (index, cancel) in [(0, false), (0, true), (1, false), (1, true)] {
        let (mut s, source, target) = copied();
        let other = s.catalog.photos().find(|p| p.id != source && p.id != target).unwrap().id;
        let ids = [target, other];
        let before = ids.map(|id| s.develop_of(id).unwrap());
        let pending = completed_paste(&s, &ids);
        let active = ids[index];
        s.execute("library.select", &json!({"ids": [active.0], "active": active.0})).unwrap();
        s.begin_interaction("Exposure").unwrap();
        assert!(Arc::ptr_eq(&s.develop_of(active).unwrap(), &before[index]), "an unchanged gesture passes the old stale check");
        let history = (s.commits(), s.undo.len(), s.redo.len(), s.pending_log.len());
        s.segmenter.paste = Some(pending);
        let polled = s.segment_poll();
        assert!(!polled.changed, "an open destination interaction must reject the entire batch");
        assert_eq!(polled.messages.len(), 1);
        assert!(polled.messages[0].contains("interaction"), "{:?}", polled.messages);
        assert_eq!(s.interaction.as_ref().unwrap().photo, active, "do not end the user's gesture");
        assert!(!s.segmenter.busy());
        assert!(s.activity.list().is_empty());
        assert_eq!(s.develop_of(target).unwrap(), before[0]);
        assert_eq!(s.develop_of(other).unwrap(), before[1]);
        if cancel {
            s.cancel_interaction().unwrap();
        } else {
            s.end_interaction().unwrap();
        }
        assert_eq!((s.commits(), s.undo.len(), s.redo.len(), s.pending_log.len()), history);
        assert_eq!(s.develop_of(target).unwrap(), before[0]);
        assert_eq!(s.develop_of(other).unwrap(), before[1]);
        // Retrying after the gesture ends still commits the whole batch as one undo step.
        s.segmenter.paste = Some(completed_paste(&s, &ids));
        assert!(s.segment_poll().changed);
        assert_eq!(s.commits(), history.0 + 1);
        assert_eq!(s.undo_step().unwrap(), "Paste Settings");
        assert_eq!(s.develop_of(target).unwrap(), before[0]);
        assert_eq!(s.develop_of(other).unwrap(), before[1]);
    }
}

#[cfg(feature = "sam")]
#[test]
fn background_paste_allows_interactions_on_unrelated_photos() {
    for cancel in [false, true] {
        let (mut s, source, target) = copied();
        let unrelated = s.catalog.photos().find(|p| p.id != source && p.id != target).unwrap().id;
        let before = s.develop_of(target).unwrap();
        let own = s.develop_of(unrelated).unwrap();
        let pending = completed_paste(&s, &[target]);
        s.execute("library.select", &json!({"ids": [unrelated.0], "active": unrelated.0})).unwrap();
        s.begin_interaction("Exposure").unwrap();
        let commits = s.commits();
        s.segmenter.paste = Some(pending);
        let polled = s.segment_poll();
        assert!(polled.changed);
        assert!(polled.messages.is_empty());
        assert!(s.activity.list().is_empty());
        assert_eq!(s.interaction.as_ref().unwrap().photo, unrelated);
        let pasted = s.develop_of(target).unwrap();
        assert_ne!(pasted, before);
        if cancel {
            s.cancel_interaction().unwrap();
        } else {
            s.end_interaction().unwrap();
        }
        assert_eq!(s.commits(), commits + 1);
        assert_eq!(s.develop_of(unrelated).unwrap(), own);
        assert_eq!(s.develop_of(target).unwrap(), pasted);
        assert_eq!(s.undo_step().unwrap(), "Paste Settings");
        assert_eq!(s.develop_of(target).unwrap(), before);
        s.redo_step().unwrap();
        assert_eq!(s.develop_of(target).unwrap(), pasted);
    }
}

#[cfg(feature = "sam")]
#[test]
fn background_paste_queues_real_worker_and_failed_inference_leaves_settings_unchanged() {
    let (mut s, _, target) = copied();
    let dir = std::env::temp_dir().join(format!("lc-sam3-paste-damaged-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("model.safetensors"), b"\x10\x00\x00\x00\x00\x00\x00\x00{not json at all}").unwrap();
    std::fs::write(dir.join("vocab.json"), b"{}").unwrap();
    std::fs::write(dir.join("merges.txt"), b"#version: 0.2\n").unwrap();
    s.segmenter.dir = Some(dir.clone());
    s.segmenter.background = true;
    let before = s.develop_of(target).unwrap();
    let commits = s.commits();
    let t = Instant::now();
    let out = s.execute("develop.paste", &json!({"ids": [target.0]})).unwrap();
    assert_eq!(out["pending"], true);
    assert_eq!(out["changed"], 0);
    assert!(t.elapsed() < std::time::Duration::from_secs(1), "UI does not wait");
    assert_eq!(s.activity.list()[0].total, 2);
    assert_eq!(s.activity.list()[0].kind, "segment");
    assert!(s.activity.list()[0].cancellable);
    assert_eq!(s.develop_of(target).unwrap(), before);
    let mut messages = Vec::new();
    while s.segmenter.busy() {
        messages.extend(s.segment_poll().messages);
        assert!(t.elapsed() < std::time::Duration::from_secs(60));
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!messages.is_empty());
    assert_eq!(s.develop_of(target).unwrap(), before);
    assert_eq!(s.commits(), commits);
    assert!(s.activity.list().is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(feature = "sam")]
#[test]
fn object_masks_without_points_or_with_too_many_points_are_actionable_errors() {
    let (s, _, target) = copied();
    for count in [0, super::super::MAX_CLICKS + 1] {
        let mut d = settings();
        let MaskShape::Object { hint, exclude, .. } = &mut d.masks[0].components[1].shape else { panic!("Object") };
        *hint = vec![Point::new(0.4, 0.5); count];
        exclude.clear();
        let err = match Pending::new(&s, vec![(target, d)], "Paste Settings") {
            Err(e) => e,
            Ok(_) => panic!("invalid prompts must not reach inference"),
        };
        assert!(err.contains(if count == 0 { "include point" } else { "at most" }), "{err}");
        assert!(s.activity.list().is_empty());
    }
}
