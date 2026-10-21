//! Reordering rules and groups in the smart-album rule editor: the grip at the start of each row,
//! dragged with the pointer or moved with the arrow keys. Every move here is made with real pointer
//! and key events, and each test says what the person sees.

use std::time::Duration;

use egui::{Event, Modifiers, PointerButton, Pos2, Rect, pos2, vec2};
use lightcraft_catalog::{Rule, RuleSet};
use serde_json::{Value, json};

use crate::headless::Headless;
use crate::panels::rules_reorder::grip_id;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn rule(n: u32) -> Value {
    json!({"field": "rating", "op": "is", "value": n})
}

fn group(rules: Vec<Value>) -> Value {
    json!({"group": {"match": "any", "rules": rules}})
}

/// The rule editor of a new smart album holding `rules`, in a window with room for them.
fn editor(rules: Vec<Value>) -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 1000.0], 1.0);
    let r = h.request("engine.execute", json!({"command": "dialog.smartAlbum", "params": {"name": "Order"}}), T);
    assert_eq!(r["ok"], true, "{r}");
    set_rules(&mut h, rules);
    h
}

fn set_rules(h: &mut Headless, rules: Vec<Value>) {
    let Some(crate::state::Dialog::SmartRules { rules: r, .. }) = &mut h.app.ui.dialog else { panic!("no rule editor") };
    *r = serde_json::from_value(json!({"rules": rules})).unwrap();
    h.settle(SETTLE);
}

fn rules(h: &Headless) -> RuleSet {
    let Some(crate::state::Dialog::SmartRules { rules, .. }) = &h.app.ui.dialog else { panic!("no rule editor") };
    rules.clone()
}

/// The rules as the editor shows them, each by its rating, each group in parentheses: `1 (2 3) 4`.
fn shape(h: &Headless) -> String {
    fn of(rs: &RuleSet) -> String {
        let parts: Vec<String> = rs
            .rules
            .iter()
            .map(|r| match r {
                Rule::Field { value, .. } => value.to_string(),
                Rule::Group { group } => format!("({})", of(group)),
            })
            .collect();
        parts.join(" ")
    }
    of(&rules(h))
}

fn widget(h: &Headless, id: &str) -> Rect {
    h.app.widgets.iter().rev().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"))
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

/// The grip of the row with salt `salt` (`rules-1-0`: the first rule of the second row's group).
fn grip(h: &Headless, salt: &str) -> Rect {
    widget(h, &format!("ruleGrip:{salt}"))
}

/// A point on the upper half of a row, where a drop goes before it.
fn above(r: Rect) -> Pos2 {
    r.center() + vec2(40.0, -6.0)
}

/// A point on the lower half of a row, where a drop goes after it.
fn below(r: Rect) -> Pos2 {
    r.center() + vec2(40.0, 6.0)
}

fn pointer(h: &mut Headless, pos: Pos2, pressed: Option<bool>) {
    h.app.synthetic.push(Event::PointerMoved(pos));
    if let Some(pressed) = pressed {
        h.app.synthetic.push(Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
    }
}

/// Press on `from` and move to `to` over a few frames, holding the button down.
fn hold(h: &mut Headless, from: Pos2, to: Pos2) {
    pointer(h, from, Some(true));
    h.step();
    for i in 1..=8 {
        pointer(h, from + (to - from) * (i as f32 / 8.0), None);
        h.step();
    }
    h.step();
}

fn release(h: &mut Headless, at: Pos2) {
    pointer(h, at, Some(false));
    h.step();
    h.settle(SETTLE);
}

/// A whole drag: press the grip of `from`, move to the lower (`after`) or upper half of the row
/// of `to`, release.
fn drag(h: &mut Headless, from: &str, to: &str, after: bool) {
    let from = grip(h, from).center();
    let to = if after { below(grip(h, to)) } else { above(grip(h, to)) };
    hold(h, from, to);
    release(h, to);
}

fn key(h: &mut Headless, key: &str) {
    let r = h.request("ui.key", json!({"key": key}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
}

fn focus(h: &mut Headless, salt: &str) {
    h.view.ctx.memory_mut(|m| m.request_focus(grip_id(salt)));
    h.settle(SETTLE);
}

fn focused_is(h: &Headless, salt: &str) -> bool {
    h.view.ctx.memory(|m| m.focused()) == Some(grip_id(salt))
}

/// The line the editor draws where a dragged row would land (accent, 2 pt wide), if it is drawn.
fn drop_line(h: &Headless) -> Option<(Pos2, Pos2)> {
    let accent = crate::theme::Tokens::get(&h.view.ctx).accent;
    h.view.painted_shapes().into_iter().find_map(|s| match s {
        egui::epaint::Shape::LineSegment { points, stroke } if stroke.width == 2.0 && stroke.color == accent => Some((points[0], points[1])),
        _ => None,
    })
}

fn pill(h: &Headless, text: &str) -> bool {
    h.painted_text().iter().any(|t| t == text)
}

/// Given three rules, when the first rule's grip is dragged below the third, a line shows the drop
/// between the third and the fourth, the dragged rule's name follows the pointer, and releasing
/// moves it there; and dragging a rule up works the same.
#[test]
fn dragging_a_grip_moves_the_rule() {
    let mut h = editor(vec![rule(1), rule(2), rule(3), rule(4)]);
    for i in 0..4 {
        assert!(has(&h, &format!("ruleGrip:rules-{i}")), "row {i} has a grip");
    }
    let from = grip(&h, "rules-0").center();
    let to = below(grip(&h, "rules-2"));
    hold(&mut h, from, to);
    let (a, b) = drop_line(&h).expect("a line shows where the rule would land");
    assert!(a.y > grip(&h, "rules-2").center().y && a.y < grip(&h, "rules-3").center().y, "between the third and fourth rule: {a:?}");
    assert_eq!(a.y, b.y, "a level line");
    assert!(pill(&h, "Rating is 1"), "the rule's name follows the pointer: {:?}", h.painted_text());
    assert_eq!(h.last_cursor, egui::CursorIcon::Grabbing);
    assert_eq!(shape(&h), "1 2 3 4", "nothing moves before the release");
    release(&mut h, to);
    assert_eq!(shape(&h), "2 3 1 4");
    assert!(drop_line(&h).is_none() && !pill(&h, "Rating is 1"), "the line and the name are gone");
    // up: the last rule above the first
    drag(&mut h, "rules-3", "rules-0", false);
    assert_eq!(shape(&h), "4 2 3 1");
}

/// Given a grip has the keyboard focus, when the mouse drags a rule's grip below the third, the
/// focus ends on the dragged rule's grip at its new place, whether the focus was on that grip, on
/// another row's grip (which would otherwise keep its place and find another rule there) or
/// nowhere; so the next arrow key moves the same rule.
#[test]
fn a_drop_keeps_the_focus_on_the_moved_rule() {
    for start in [Some("rules-0"), Some("rules-1"), None] {
        let mut h = editor(vec![rule(1), rule(2), rule(3)]);
        if let Some(salt) = start {
            focus(&mut h, salt);
        }
        drag(&mut h, "rules-0", "rules-2", true);
        assert_eq!(shape(&h), "2 3 1");
        assert!(focused_is(&h, "rules-2"), "the focus went with the rule (started on {start:?})");
        key(&mut h, "ArrowUp");
        assert_eq!(shape(&h), "2 1 3", "the next key moves the same rule (started on {start:?})");
    }
}

/// Dropping a rule where it already is changes nothing and shows no line.
#[test]
fn dropping_a_rule_where_it_is_does_nothing() {
    let mut h = editor(vec![rule(1), rule(2), rule(3)]);
    let from = grip(&h, "rules-1").center();
    let to = below(grip(&h, "rules-1"));
    hold(&mut h, from, to);
    assert!(drop_line(&h).is_none(), "no line for a drop that changes nothing");
    release(&mut h, to);
    assert_eq!(shape(&h), "1 2 3");
}

/// Given a group among rules, a rule dropped on a rule inside the group joins it there, and one
/// dropped beside the group leaves it.
#[test]
fn a_rule_is_dragged_into_and_out_of_a_group() {
    let mut h = editor(vec![rule(1), group(vec![rule(2), rule(3)]), rule(4)]);
    drag(&mut h, "rules-0", "rules-1-0", true);
    assert_eq!(shape(&h), "(2 1 3) 4", "into the group, after its first rule");
    drag(&mut h, "rules-0-2", "rules-1", false);
    assert_eq!(shape(&h), "(2 1) 3 4", "out of it, before the next rule");
    drag(&mut h, "rules-1", "rules-0-1", true);
    assert_eq!(shape(&h), "(2 1 3) 4", "and in again, after its last rule");
}

/// A group dropped on the rules of another group nests inside it, with its own rules.
#[test]
fn a_group_is_dragged_into_another_group() {
    let mut h = editor(vec![group(vec![rule(1)]), group(vec![rule(2)]), rule(3)]);
    drag(&mut h, "rules-0", "rules-1-0", true);
    assert_eq!(shape(&h), "(2 (1)) 3", "the first group sits in the second, after its rule");
    assert!(has(&h, "ruleGrip:rules-0-1") && has(&h, "ruleGrip:rules-0-1-0"), "drawn nested");
}

/// A group left empty by dragging its only rule out goes away, as the rule's own group would.
#[test]
fn a_group_whose_last_rule_is_dragged_out_goes_away() {
    let mut h = editor(vec![rule(1), group(vec![rule(2)]), rule(3)]);
    drag(&mut h, "rules-1-0", "rules-0", false);
    assert_eq!(shape(&h), "2 1 3");
    assert!(!has(&h, "ruleGrip:rules-3"), "and no empty group is left");
}

/// A group can't be dropped inside itself: the pointer says so (a not-allowed cursor, a line and a
/// note in the caution colour), the release changes nothing and the dialog stays.
#[test]
fn a_group_cant_be_dropped_inside_itself() {
    let mut h = editor(vec![group(vec![rule(1), rule(2)]), rule(3)]);
    let from = grip(&h, "rules-0").center();
    let to = below(grip(&h, "rules-0-0"));
    hold(&mut h, from, to);
    assert_eq!(h.last_cursor, egui::CursorIcon::NotAllowed);
    assert!(pill(&h, "A group can't go inside itself"), "{:?}", h.painted_text());
    assert!(drop_line(&h).is_none(), "the line is not the accent one");
    release(&mut h, to);
    assert_eq!(shape(&h), "(1 2) 3");
    assert!(h.app.ui.dialog.is_some());
}

/// Groups nest three deep at most (as the + Group button allows): a group whose rules would end up
/// deeper is refused with the reason, and one that fits is not.
#[test]
fn a_drop_that_would_nest_groups_too_deep_is_refused() {
    let tall = group(vec![group(vec![group(vec![rule(1)])])]);
    let mut h = editor(vec![group(vec![rule(2)]), tall]);
    let from = grip(&h, "rules-1").center();
    let to = below(grip(&h, "rules-0-0"));
    hold(&mut h, from, to);
    assert_eq!(h.last_cursor, egui::CursorIcon::NotAllowed);
    assert!(pill(&h, "Groups can't nest any deeper"), "{:?}", h.painted_text());
    release(&mut h, to);
    assert_eq!(shape(&h), "(2) (((1)))", "nothing moved");
    // beside the other group, the same drag is fine
    let from = grip(&h, "rules-1").center();
    // (above the group's frame, not on its first line)
    let to = above(grip(&h, "rules-0")) - vec2(0.0, 7.0);
    hold(&mut h, from, to);
    assert_ne!(h.last_cursor, egui::CursorIcon::NotAllowed);
    release(&mut h, to);
    assert_eq!(shape(&h), "(((1))) (2)");
}

/// Esc gives a drag up: nothing moves on the release, the dialog stays open, and the Esc after
/// that closes it as usual.
#[test]
fn escape_cancels_a_drag() {
    let mut h = editor(vec![rule(1), rule(2), rule(3)]);
    let from = grip(&h, "rules-0").center();
    let to = below(grip(&h, "rules-2"));
    hold(&mut h, from, to);
    assert!(drop_line(&h).is_some(), "dragging");
    key(&mut h, "Escape");
    assert!(h.app.ui.dialog.is_some(), "the Esc was the drag's");
    assert!(drop_line(&h).is_none() && !pill(&h, "Rating is 1"), "the drag is over");
    release(&mut h, to);
    assert_eq!(shape(&h), "1 2 3", "the release dropped nothing");
    key(&mut h, "Escape");
    assert!(h.app.ui.dialog.is_none(), "the next Esc closes the dialog");
}

/// A drag released away from the list drops nothing.
#[test]
fn a_drag_released_outside_the_list_drops_nothing() {
    let mut h = editor(vec![rule(1), rule(2)]);
    let from = grip(&h, "rules-0").center();
    let to = pos2(40.0, 980.0);
    hold(&mut h, from, to);
    assert!(drop_line(&h).is_none());
    release(&mut h, to);
    assert_eq!(shape(&h), "1 2");
}

/// Tab reaches a grip, ↓ and ↑ move its row one place, the focus stays on that row's grip, and at
/// the ends of the list nothing happens.
#[test]
fn arrow_keys_move_the_focused_row_and_the_focus_follows() {
    let mut h = editor(vec![rule(1), rule(2), rule(3)]);
    let mut reached = false;
    for _ in 0..4 {
        key(&mut h, "Tab");
        if focused_is(&h, "rules-0") {
            reached = true;
            break;
        }
    }
    assert!(reached, "Tab gets to the first row's grip");
    key(&mut h, "ArrowUp");
    assert_eq!((shape(&h).as_str(), focused_is(&h, "rules-0")), ("1 2 3", true), "the top stays");
    key(&mut h, "ArrowDown");
    assert_eq!((shape(&h).as_str(), focused_is(&h, "rules-1")), ("2 1 3", true), "moved down, focus with it");
    key(&mut h, "ArrowDown");
    assert_eq!((shape(&h).as_str(), focused_is(&h, "rules-2")), ("2 3 1", true));
    key(&mut h, "ArrowDown");
    assert_eq!((shape(&h).as_str(), focused_is(&h, "rules-2")), ("2 3 1", true), "the end stays");
    key(&mut h, "ArrowUp");
    assert_eq!((shape(&h).as_str(), focused_is(&h, "rules-1")), ("2 1 3", true));
    // the keys didn't also move the focus on to another widget, and don't leak to the photo grid
    assert!(h.app.ui.dialog.is_some());
}

/// At the ends of a group ↑ and ↓ carry the row out of it, one place at a time otherwise.
#[test]
fn arrow_keys_carry_a_row_out_at_the_ends_of_a_group() {
    let mut h = editor(vec![rule(1), group(vec![rule(2), rule(3)]), rule(4)]);
    focus(&mut h, "rules-1-0");
    key(&mut h, "ArrowUp");
    assert_eq!((shape(&h).as_str(), focused_is(&h, "rules-1")), ("1 2 (3) 4", true), "out, before the group");
    focus(&mut h, "rules-2-0");
    key(&mut h, "ArrowDown");
    assert_eq!((shape(&h).as_str(), focused_is(&h, "rules-2")), ("1 2 3 4", true), "its last rule: out, after the group, which is gone");
    let mut h = editor(vec![rule(1), group(vec![rule(2), rule(3)]), rule(4)]);
    focus(&mut h, "rules-1-0");
    key(&mut h, "ArrowDown");
    assert_eq!((shape(&h).as_str(), focused_is(&h, "rules-1-1")), ("1 (3 2) 4", true), "inside, one place");
    key(&mut h, "ArrowDown");
    assert_eq!((shape(&h).as_str(), focused_is(&h, "rules-2")), ("1 (3) 2 4", true), "then out, after the group");
}

/// → moves a row into the group above it (at its end) or below it (at its start); ← moves it out
/// of its group, to just after it.
#[test]
fn right_and_left_arrows_move_a_row_into_and_out_of_a_group() {
    let mut h = editor(vec![rule(1), group(vec![rule(2), rule(3)]), rule(4)]);
    focus(&mut h, "rules-2");
    key(&mut h, "ArrowRight");
    assert_eq!((shape(&h).as_str(), focused_is(&h, "rules-1-2")), ("1 (2 3 4)", true), "into the group above, at its end");
    key(&mut h, "ArrowLeft");
    assert_eq!((shape(&h).as_str(), focused_is(&h, "rules-2")), ("1 (2 3) 4", true), "out, after the group");
    focus(&mut h, "rules-0");
    key(&mut h, "ArrowRight");
    assert_eq!((shape(&h).as_str(), focused_is(&h, "rules-0-0")), ("(1 2 3) 4", true), "into the group below, at its start");
    key(&mut h, "ArrowLeft");
    assert_eq!((shape(&h).as_str(), focused_is(&h, "rules-1")), ("(2 3) 1 4", true));
    key(&mut h, "ArrowLeft");
    assert_eq!((shape(&h).as_str(), focused_is(&h, "rules-1")), ("(2 3) 1 4", true), "at the top level ← does nothing");
    // a group moves the same way
    let mut h = editor(vec![group(vec![rule(1)]), group(vec![rule(2)])]);
    focus(&mut h, "rules-1");
    key(&mut h, "ArrowRight");
    assert_eq!((shape(&h).as_str(), focused_is(&h, "rules-0-1")), ("(1 (2))", true), "a group into the group above");
}

/// → doesn't push a group deeper than groups go; nothing moves.
#[test]
fn right_arrow_respects_the_depth_limit() {
    let tall = group(vec![group(vec![group(vec![rule(1)])])]);
    let mut h = editor(vec![tall, group(vec![rule(2)])]);
    focus(&mut h, "rules-0");
    key(&mut h, "ArrowRight");
    assert_eq!(shape(&h), "(((1))) (2)");
    assert!(focused_is(&h, "rules-0"));
}

/// A group's rows keep their problems wherever they go: the mark under a rule that can't mean
/// anything follows the rule, and OK stays refused until it is fixed.
#[test]
fn a_rules_problem_follows_it() {
    let bad = json!({"field": "captureDate", "op": "is", "value": "banana"});
    let mut h = editor(vec![rule(1), bad, rule(3)]);
    assert!(has(&h, "ruleProblem:rules-1") && !has(&h, "ruleProblem:rules-0"));
    drag(&mut h, "rules-1", "rules-0", false);
    assert_eq!(rules(&h).rules.len(), 3);
    h.settle(SETTLE);
    assert!(has(&h, "ruleProblem:rules-0") && !has(&h, "ruleProblem:rules-1"), "the mark moved with the rule");
    focus(&mut h, "rules-0");
    key(&mut h, "ArrowDown");
    h.settle(SETTLE);
    assert!(has(&h, "ruleProblem:rules-1") && !has(&h, "ruleProblem:rules-0"), "and with the arrow keys");
}

/// The rows that read as buttons to a screen reader, by name.
fn spoken(rules: Vec<Value>) -> Vec<String> {
    let view = crate::headless::HeadlessView::new();
    view.ctx.enable_accesskit();
    let mut rs: RuleSet = serde_json::from_value(json!({"rules": rules})).unwrap();
    let env = crate::panels::rules_editor::Env::default();
    let mut found = Vec::new();
    for _ in 0..3 {
        let raw = crate::headless::HeadlessView::raw_input(vec2(900.0, 700.0), 1.0, 0.0, vec![]);
        let mut out = view.ctx.run_ui(raw, |ui| crate::panels::rules_editor::edit(ui, &mut rs, "rules", 0, &[], &env));
        out.textures_delta.clear();
        if let Some(update) = out.platform_output.accesskit_update.take() {
            found = update
                .nodes
                .iter()
                .filter(|(_, n)| n.role() == egui::accesskit::Role::Button)
                .filter_map(|(_, n)| n.label().map(str::to_string))
                .filter(|l| l.ends_with("Move with the arrow keys"))
                .collect();
        }
    }
    found
}

/// A grip is a focusable button named with the row's place and what it says.
#[test]
fn a_grip_is_named_for_its_row() {
    let names = spoken(vec![rule(1), group(vec![rule(2), rule(3)]), rule(4)]);
    for n in [
        "Rule 1 of 3: Rating is 1. Move with the arrow keys",
        "Group 2 of 3: match any, 2 inside. Move with the arrow keys",
        "Rule 1 of 2: Rating is 2. Move with the arrow keys",
        "Rule 2 of 2: Rating is 3. Move with the arrow keys",
        "Rule 3 of 3: Rating is 4. Move with the arrow keys",
    ] {
        assert!(names.iter().any(|x| x == n), "no grip named {n:?} in {names:?}");
    }
}

/// OK saves the rules in the order the editor shows, as one undo step like any edit of the rules.
#[test]
fn ok_saves_the_new_order_in_one_undo_step() {
    let mut h = editor(vec![rule(1), group(vec![rule(2), rule(3)]), rule(4)]);
    drag(&mut h, "rules-0", "rules-2", true);
    assert_eq!(shape(&h), "(2 3) 4 1");
    focus(&mut h, "rules-0");
    key(&mut h, "ArrowDown");
    assert_eq!(shape(&h), "4 (2 3) 1");
    let undo = h.app.session.undo.len();
    let r = h.request("ui.clickWidget", json!({"id": "button:dialogOk"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(h.app.ui.dialog.is_none(), "saved");
    assert_eq!(h.app.session.undo.len(), undo + 1, "one undo step for the whole reorder");
    let album = h.app.session.catalog.albums().find(|a| a.name == "Order").expect("the album").clone();
    let saved = album.smart.as_ref().and_then(|f| f.rule_set.clone()).expect("rules");
    let order: Vec<String> = saved
        .rules
        .iter()
        .map(|r| match r {
            Rule::Field { value, .. } => value.to_string(),
            Rule::Group { group } => format!("g{}", group.rules.len()),
        })
        .collect();
    assert_eq!(order, ["4", "g2", "1"].map(String::from), "{order:?}");
}

/// A row moved out of nested groups may leave the group it goes into empty for a moment: ← on the
/// only rule of a group in a group moves it into the outer one.
#[test]
fn a_rule_alone_in_a_nested_group_can_leave_it() {
    let mut h = editor(vec![group(vec![group(vec![rule(1)])])]);
    focus(&mut h, "rules-0-0-0");
    key(&mut h, "ArrowLeft");
    assert_eq!(shape(&h), "(1)");
    assert!(focused_is(&h, "rules-0-0"));
}

/// A drag whose rules are replaced under it (another dialog opened by an agent) drops nothing.
#[test]
fn a_drag_whose_dialog_was_replaced_drops_nothing() {
    let mut h = editor(vec![rule(1), rule(2), rule(3)]);
    let from = grip(&h, "rules-0").center();
    let to = below(grip(&h, "rules-2"));
    hold(&mut h, from, to);
    let r = h.request("engine.execute", json!({"command": "dialog.smartAlbum", "params": {"name": "Other"}}), T);
    assert_eq!(r["ok"], true, "{r}");
    set_rules(&mut h, vec![rule(7), rule(8), rule(9)]);
    pointer(&mut h, to, None);
    h.step();
    release(&mut h, to);
    assert_eq!(shape(&h), "7 8 9");
}
