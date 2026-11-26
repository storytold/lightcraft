//! The Info panel's field drafts belong to the passes that draw them: a draft left behind by a panel
//! that stopped drawing is given up rather than shown again on another photo (CodeRabbit #4750, t11).
use super::{FieldEdit, current, draft};

/// A draft as `meta_field` keeps it while a field is edited, last under way in pass `seen`.
fn edit(seen: u64) -> FieldEdit {
    FieldEdit { text: "A's title".into(), start: String::new(), ids: vec![7], mixed: false, typed: true, seen }
}

/// A draft kept in pass N is the field's in passes N and N+1 (egui may run a pass again) and in no
/// later one, so a field the panel stopped drawing never shows its edit on another photo.
#[test]
fn a_draft_is_the_fields_in_its_own_pass_and_the_next_only() {
    let ctx = egui::Context::default();
    let id = egui::Id::unique(("info-field", "title"));
    let mut seen: Option<u64> = None;
    let mut observed: Vec<(u64, bool)> = Vec::new();
    for _ in 0..4 {
        let frame = ctx.run_pass(egui::RawInput::default(), |ctx| {
            let pass = ctx.cumulative_pass_nr();
            if seen.is_none() {
                // the pass a field was edited in, as `keep_edit` stores it
                seen = Some(pass);
                ctx.data_mut(|d| d.insert_temp(id, edit(pass)));
            }
            observed.push((pass, draft(ctx, id).is_some()));
        });
        // A pass hands out a font-atlas delta, and a dropped FullOutput panics on unapplied deltas:
        // the app's harness applies or clears them, so the test does too.
        frame.drop_without_applying_deltas();
    }
    let seen = seen.unwrap();
    assert!(observed.len() > 2, "the passes this test needs: {observed:?}");
    for (pass, kept) in &observed {
        assert_eq!(*kept, current(seen, *pass), "draft last seen in pass {seen}, asked in pass {pass}");
    }
    assert!(observed.iter().any(|(pass, kept)| *pass == seen + 1 && *kept), "a field that keeps drawing keeps its draft");
    assert!(observed.iter().any(|(pass, kept)| *pass > seen + 1 && !*kept), "a field that stopped drawing gives its draft up: {observed:?}");
}
