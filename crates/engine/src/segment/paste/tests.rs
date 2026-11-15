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
