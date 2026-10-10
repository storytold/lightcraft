//! The smart-album rule editor: match all / any / none of a list of rules (field, operator,
//! value), with nested groups. Field and operator lists come from `lightcraft_catalog::rules`.

use egui::RichText;
use lightcraft_catalog::rules::{FIELD_GROUPS, Kind, Problem, TOP_LEVEL_FIELDS, bool_value, field_kind, field_label, ops_for};
use lightcraft_catalog::{Match, Rule, RuleSet};
use serde_json::{Value, json};

use crate::album_picker::{AlbumEntry, AlbumPicker};
use crate::date_picker::{DatePicker, PickedDate};
use crate::theme::Tokens;
use crate::widgets::register;

/// A sensible starting value for `field` / `op`.
pub fn default_value(field: &str, op: &str) -> Value {
    match (field_kind(field), op) {
        (_, "isEmpty" | "isNotEmpty") => Value::Null,
        (Some(Kind::Date), "inLast" | "notInLast") => json!({"n": 30, "unit": "days"}),
        // this year (the catalog's clock): a date that means something, not an empty one that
        // matches nothing ("is") or every date ("between")
        (Some(Kind::Date), "between") => {
            let year = this_year();
            json!([format!("{year}-01"), format!("{year}-12")])
        }
        (Some(Kind::Date), _) => json!(this_year()),
        (Some(Kind::Number), "between") if field == "shutterSpeed" => json!(["1/1000", "1/125"]),
        (Some(Kind::Number), _) if field == "shutterSpeed" => json!("1/250"),
        (Some(Kind::Number), "between") => json!([0, 0]),
        (Some(Kind::Number), _) => json!(if field == "rating" { 3 } else { 0 }),
        (Some(Kind::Choice(c)), _) => json!(c.first().map_or("", |c| c.0)),
        (Some(Kind::Bool), _) => json!(true),
        // picked from the list
        (Some(Kind::Album), _) => Value::Null,
        _ => json!(""),
    }
}

/// The current year, `2026`.
fn this_year() -> String {
    lightcraft_catalog::rules::now().get(..4).unwrap_or("2026").to_string()
}

/// A new rule (Rating ≥ 3).
pub fn new_rule() -> Rule {
    Rule::Field { field: "rating".into(), op: "gte".into(), value: json!(3) }
}

fn match_combo(ui: &mut egui::Ui, salt: &str, m: &mut Match) {
    let label = |m: Match| match m {
        Match::All => "all",
        Match::Any => "any",
        Match::None => "none",
    };
    egui::ComboBox::from_id_salt(format!("{salt}-match")).width(64.0).selected_text(crate::i18n::tr(label(*m))).show_ui(ui, |ui| {
        for x in [Match::All, Match::Any, Match::None] {
            ui.selectable_value(m, x, crate::i18n::tr(label(x)));
        }
    });
}

fn text_value(ui: &mut egui::Ui, v: &mut Value, width: f32, hint: &str, salt: &str) {
    let mut s = match &*v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    };
    let r = ui.add(egui::TextEdit::singleline(&mut s).hint_text(crate::i18n::tr(hint)).desired_width(width));
    register(ui.ctx(), format!("field:{salt}"), r.rect);
    if r.changed() {
        *v = json!(s);
    }
}

fn number_value(ui: &mut egui::Ui, v: &mut Value, field: &str) {
    let mut n = v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())).unwrap_or(0.0);
    let (lo, hi, speed) = match field {
        "rating" => (0.0, 5.0, 0.05),
        "iso" => (0.0, 409_600.0, 10.0),
        "aperture" => (0.0, 64.0, 0.05),
        "keywordCount" | "personCount" => (0.0, 1000.0, 0.05),
        "longEdge" | "shortEdge" => (0.0, 100_000.0, 10.0),
        _ => (0.0, 100_000.0, 0.5),
    };
    let whole = matches!(field, "rating" | "iso" | "keywordCount" | "personCount" | "longEdge" | "shortEdge");
    // the range limits dragging; a stored value outside it (a rating of 9 from an agent) is shown
    // as it is, marked by the check, not quietly clamped to something that matches
    let dv = egui::DragValue::new(&mut n).range(lo..=hi).clamp_existing_to_range(false).speed(speed);
    let dv = if whole { dv.fixed_decimals(0) } else { dv.max_decimals(2) };
    if ui.add(dv).changed() {
        *v = if whole { json!(n.round() as i64) } else { json!(n) };
    }
}

/// A rule date's calendar ([`DatePicker`]): opens on the date as written (or today), keeps its
/// precision, and writes what is picked as `2026`, `2026-08` or `2026-08-14`. It picks dates, not
/// times: picking a day for "2026-08-14T10:00" leaves "2026-08-14" (a time is typed).
fn date_picker(ui: &mut egui::Ui, v: &mut Value, salt: &str, min: Option<PickedDate>, max: Option<PickedDate>, env: &Env) {
    let mut date = v.as_str().and_then(PickedDate::parse);
    let today = PickedDate::parse(&env.today);
    if DatePicker::new(salt, &mut date).min(min).max(max).today(today).show(ui).changed()
        && let Some(date) = date
    {
        *v = json!(date.iso());
    }
}

/// What the editor shows besides the rules: the problems `RuleSet::check` found in them (each
/// row marks its own) and the albums an Album rule can test.
#[derive(Default)]
pub struct Env {
    pub problems: Vec<Problem>,
    /// Today (`YYYY-MM-DD…`, the session's clock): where a date picker opens when a rule has no date.
    pub today: String,
    /// The albums, smart albums and folders an Album rule picks from, in the sidebar's order
    /// ([`crate::album_picker::entries_from`]); the album being edited and those that would loop
    /// back to it are blocked, with the reason.
    pub albums: Vec<AlbumEntry>,
}

/// An Album rule's value: an album or a smart album, picked from the tree or by typing
/// ([`AlbumPicker`]). The id is read as the catalog reads it (`3`, `"3"`) and written as a number.
fn album_value(ui: &mut egui::Ui, v: &mut Value, salt: &str, env: &Env) {
    let mut chosen = lightcraft_catalog::rules::album_rule_id(v).map(|a| a.0);
    if AlbumPicker::new(salt, &mut chosen, &env.albums).width(200.0).show(ui).changed()
        && let Some(id) = chosen
    {
        *v = json!(id);
    }
}

/// The value editor for one rule.
fn value_editor(ui: &mut egui::Ui, field: &str, op: &str, v: &mut Value, salt: &str, env: &Env) {
    match (field_kind(field), op) {
        (_, "isEmpty" | "isNotEmpty") => {}
        (Some(Kind::Album), _) => album_value(ui, v, salt, env),
        (Some(Kind::Date), "inLast" | "notInLast") => {
            // shown as stored ({n, unit}, or a plain number of days) and only written when changed:
            // drawing never turns "0 days" into 1 or 20,000 into the spinner's 10,000
            let (stored_n, stored_unit) = match &*v {
                // n as the catalog reads it: a number, or one written as text ("7")
                Value::Object(o) => (
                    o.get("n").and_then(|n| n.as_f64().or_else(|| n.as_str().and_then(|t| t.trim().parse().ok()))),
                    o.get("unit").and_then(Value::as_str).unwrap_or("days").to_string(),
                ),
                Value::Number(n) => (n.as_f64(), "days".to_string()),
                _ => (None, "days".to_string()),
            };
            let mut n = stored_n.unwrap_or(30.0);
            let mut unit = stored_unit.to_lowercase();
            let mut changed =
                ui.add(egui::DragValue::new(&mut n).range(1.0..=10_000.0).clamp_existing_to_range(false).speed(0.2).fixed_decimals(0)).changed();
            egui::ComboBox::from_id_salt(format!("{salt}-unit")).width(70.0).selected_text(crate::i18n::tr(&unit)).show_ui(ui, |ui| {
                for u in ["hours", "days", "weeks", "months", "years"] {
                    if ui.selectable_label(unit.trim_end_matches('s') == u.trim_end_matches('s'), crate::i18n::tr(u)).clicked() {
                        unit = u.to_string();
                        changed = true;
                    }
                }
            });
            if changed {
                *v = json!({"n": n.round(), "unit": unit});
            }
        }
        (Some(k), "between") => {
            // a value that isn't a pair is shown as the default pair but kept until one is edited,
            // so the check can mark it
            let mut pair = v.as_array().filter(|a| a.len() == 2).cloned().map_or_else(|| default_value(field, op), Value::Array);
            let before = pair.clone();
            let Some([lo, hi]) = pair.as_array_mut().map(Vec::as_mut_slice) else { return };
            if field == "shutterSpeed" {
                text_value(ui, lo, 60.0, "1/1000", &format!("{salt}-a"));
                ui.label(crate::i18n::tr("and"));
                text_value(ui, hi, 60.0, "1/125", &format!("{salt}-b"));
            } else if k == Kind::Number {
                number_value(ui, lo, field);
                ui.label(crate::i18n::tr("and"));
                number_value(ui, hi, field);
            } else if k == Kind::Date {
                // each date's picker stops at the other one, so the two stay in order
                let (lo_date, hi_date) = (lo.as_str().and_then(PickedDate::parse), hi.as_str().and_then(PickedDate::parse));
                text_value(ui, lo, 86.0, "2026-01-01", &format!("{salt}-a"));
                date_picker(ui, lo, &format!("{salt}-a"), None, hi_date, env);
                ui.label(crate::i18n::tr("and"));
                text_value(ui, hi, 86.0, "2026-12", &format!("{salt}-b"));
                date_picker(ui, hi, &format!("{salt}-b"), lo_date, None, env);
            } else {
                // a field without "between" (the check marks it): its two values, as text
                text_value(ui, lo, 86.0, "", &format!("{salt}-a"));
                ui.label(crate::i18n::tr("and"));
                text_value(ui, hi, 86.0, "", &format!("{salt}-b"));
            }
            if pair != before {
                *v = pair;
            }
        }
        // camera notation: 1/250, 0.5, 2"
        (Some(Kind::Number), _) if field == "shutterSpeed" => text_value(ui, v, 70.0, "1/250", salt),
        (Some(Kind::Number), _) => number_value(ui, v, field),
        (Some(Kind::Choice(c)), _) => {
            let cur = v.as_str().unwrap_or("").to_string();
            egui::ComboBox::from_id_salt(format!("{salt}-choice")).width(120.0).selected_text(crate::i18n::choice_text(field, &cur)).show_ui(
                ui,
                |ui| {
                    for (id, _) in c.iter() {
                        if ui.selectable_label(cur == *id, crate::i18n::choice_text(field, id)).clicked() {
                            *v = json!(id);
                        }
                    }
                },
            );
        }
        (Some(Kind::Bool), _) => {
            // the catalog's reading of the value ("false" is no); one that is neither yes nor no is
            // shown as written and kept until a choice replaces it, so the dialog reports it
            let cur = bool_value(v);
            let text = cur.map_or_else(|| v.to_string(), |b| crate::i18n::bool_text(b).to_string());
            egui::ComboBox::from_id_salt(format!("{salt}-bool")).width(60.0).selected_text(text).show_ui(ui, |ui| {
                for b in [true, false] {
                    if ui.selectable_label(cur == Some(b), crate::i18n::bool_text(b)).clicked() {
                        *v = json!(b);
                    }
                }
            });
        }
        (Some(Kind::Date), _) => {
            text_value(ui, v, 120.0, "2026-04 or 2026-04-12", salt);
            date_picker(ui, v, salt, None, None, env);
        }
        _ => text_value(ui, v, 140.0, "", salt),
    }
}

/// The field menu of one rule: the top-level fields, then one submenu per field group. Picking a
/// field keeps the operator when the new field has it, else takes the field's first operator.
fn field_menu(ui: &mut egui::Ui, salt: &str, field: &mut String, op: &mut String, value: &mut Value) {
    let label = field_label(field).unwrap_or(field.as_str()).to_string();
    let current = field.clone();
    let r =
        egui::ComboBox::from_id_salt(format!("{salt}-field")).width(150.0).height(400.0).selected_text(crate::i18n::tr(&label)).show_ui(ui, |ui| {
            let mut pick = None;
            let mut item = |ui: &mut egui::Ui, id: &'static str| {
                let r = ui.selectable_label(current == id, crate::i18n::tr(field_label(id).unwrap_or(id)));
                register(ui.ctx(), format!("ruleFieldItem:{id}:{salt}"), r.rect);
                if r.clicked() {
                    pick = Some(id);
                    ui.close();
                }
            };
            for id in TOP_LEVEL_FIELDS {
                item(ui, id);
            }
            ui.separator();
            // submenu rows look like the items above them; the one holding the current field stands out
            // (a transparent frame, not none: a frameless button drops its padding and hover fill)
            let inactive = &mut ui.visuals_mut().widgets.inactive;
            inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
            inactive.bg_stroke = egui::Stroke::NONE;
            for (group, ids) in FIELD_GROUPS {
                let text = RichText::new(crate::i18n::tr(group));
                let text = if ids.contains(&current.as_str()) { text.strong() } else { text };
                let r = ui.menu_button(text, |ui| {
                    for id in *ids {
                        item(ui, id);
                    }
                });
                register(ui.ctx(), format!("ruleFieldGroup:{group}:{salt}"), r.response.rect);
            }
            pick
        });
    register(ui.ctx(), format!("ruleField:{salt}"), r.response.rect);
    let Some(Some(id)) = r.inner else { return };
    if field == id {
        return;
    }
    *field = id.to_string();
    let ops = ops_for(field_kind(id).unwrap_or(Kind::Text));
    if !ops.iter().any(|o| o.0 == op.as_str()) {
        *op = ops.first().map_or("is", |o| o.0).to_string();
    }
    *value = default_value(field, op);
}

/// A rule's problem, shown under it in the caution colour (`ruleProblem:<salt>` for tests and
/// agents).
fn problem_note(ui: &mut egui::Ui, problem: Option<&Problem>, salt: &str) {
    let Some(p) = problem else { return };
    let t = Tokens::get(ui.ctx());
    let r = ui.label(RichText::new(format!("⚠ {}", crate::i18n::problem_text(p))).color(t.caution).small());
    register(ui.ctx(), format!("ruleProblem:{salt}"), r.rect);
}

/// Edit `rs` in place; `salt` keeps widget ids apart between groups, `path` is where `rs` sits
/// (empty at the top) so each row finds its own problem in `env`.
pub fn edit(ui: &mut egui::Ui, rs: &mut RuleSet, salt: &str, depth: usize, path: &[usize], env: &Env) {
    let t = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        ui.label(RichText::new(crate::i18n::tr("Match")).color(t.text_label));
        match_combo(ui, salt, &mut rs.mode);
        ui.label(RichText::new(crate::i18n::tr("of the following rules:")).color(t.text_label));
    });
    let mut remove = None;
    let mut insert: Option<(usize, Rule)> = None;
    for (i, rule) in rs.rules.iter_mut().enumerate() {
        let rsalt = format!("{salt}-{i}");
        let here: Vec<usize> = path.iter().copied().chain([i]).collect();
        let problem = env.problems.iter().find(|p| p.path == here);
        match rule {
            Rule::Group { group } => {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if ui.small_button("−").on_hover_text(crate::i18n::tr("Remove this group")).clicked() {
                            remove = Some(i);
                        }
                        ui.vertical(|ui| edit(ui, group, &rsalt, depth + 1, &here, env));
                    });
                    problem_note(ui, problem, &rsalt);
                });
            }
            Rule::Field { field, op, value } => {
                ui.horizontal(|ui| {
                    field_menu(ui, &rsalt, field, op, value);
                    let kind = field_kind(field).unwrap_or(Kind::Text);
                    let op_label = ops_for(kind).iter().find(|o| o.0 == op.as_str()).map_or(op.as_str(), |o| o.1).to_string();
                    egui::ComboBox::from_id_salt(format!("{rsalt}-op")).width(110.0).selected_text(crate::i18n::tr(&op_label)).show_ui(ui, |ui| {
                        for (id, l) in ops_for(kind) {
                            if ui.selectable_label(op == id, crate::i18n::tr(l)).clicked() && op != id {
                                let was_special = matches!(op.as_str(), "between" | "inLast" | "notInLast" | "isEmpty" | "isNotEmpty");
                                *op = id.to_string();
                                if was_special || matches!(*id, "between" | "inLast" | "notInLast" | "isEmpty" | "isNotEmpty") {
                                    *value = default_value(field, op);
                                }
                            }
                        }
                    });
                    value_editor(ui, field, op, value, &rsalt, env);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let add = ui.small_button("+").on_hover_text(crate::i18n::tr("Add a rule (⌥-click: a group)"));
                        register(ui.ctx(), format!("button:ruleAdd-{rsalt}"), add.rect);
                        if add.clicked() {
                            let group = ui.input(|i| i.modifiers.alt) && depth < 3;
                            insert = Some((
                                i + 1,
                                if group { Rule::Group { group: RuleSet { mode: Match::Any, rules: vec![new_rule()] } } } else { new_rule() },
                            ));
                        }
                        let del = ui.small_button("−").on_hover_text(crate::i18n::tr("Remove this rule"));
                        register(ui.ctx(), format!("button:ruleRemove-{rsalt}"), del.rect);
                        if del.clicked() {
                            remove = Some(i);
                        }
                    });
                });
                problem_note(ui, problem, &rsalt);
            }
        }
    }
    if let Some(i) = remove {
        rs.rules.remove(i);
    }
    if let Some((i, r)) = insert {
        rs.rules.insert(i.min(rs.rules.len()), r);
    }
    if rs.rules.is_empty() || depth == 0 {
        ui.horizontal(|ui| {
            let add = ui.small_button(crate::i18n::tr("+ Rule"));
            register(ui.ctx(), format!("button:ruleAddEnd-{salt}"), add.rect);
            if add.clicked() {
                rs.rules.push(new_rule());
            }
            if depth < 3 && ui.small_button(crate::i18n::tr("+ Group")).clicked() {
                rs.rules.push(Rule::Group { group: RuleSet { mode: Match::Any, rules: vec![new_rule()] } });
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_catalog::rules::FIELDS;
    use lightcraft_catalog::{Catalog, Photo, PhotoId, Source};

    /// Every field × operator starts with a known operator and a value of the right shape: none for
    /// the empty tests, a pair for "between", one of the choices, a number for number fields (a
    /// real time in camera notation for shutter speed); and evaluating it never panics.
    #[test]
    fn default_values_make_valid_rules() {
        let cat = Catalog::new();
        let p = Photo::new(PhotoId(1), Source::Demo { scene: 0 }, "a.jpg", "JPEG", 10, 10, "2026-10-01T00:00:00");
        for (field, _, kind) in FIELDS {
            for (op, _) in ops_for(*kind) {
                let rs = RuleSet {
                    mode: Match::All,
                    rules: vec![Rule::Field { field: field.to_string(), op: op.to_string(), value: default_value(field, op) }],
                };
                // only a text rule (nothing to look for yet) and Album (no album picked yet) wait for
                // input; every other default is a rule that means something
                let problems = rs.check(&cat);
                let waits = matches!(kind, Kind::Text | Kind::Keywords) || *field == "album";
                assert!(problems.is_empty() || waits && !matches!(*op, "isEmpty" | "isNotEmpty"), "{field} {op}: {problems:?}");
                let _ = rs.matches(&p, &cat);
                let v = default_value(field, op);
                let one = |v: &Value| match kind {
                    Kind::Choice(c) => v.as_str().is_some_and(|s| c.iter().any(|c| c.0 == s)),
                    Kind::Number if *field == "shutterSpeed" => v.as_str().and_then(lightcraft_catalog::parse_shutter_seconds).is_some(),
                    Kind::Number => v.is_number(),
                    Kind::Bool => v.is_boolean(),
                    Kind::Album => v.is_null(),
                    Kind::Text | Kind::Keywords | Kind::Date => v.is_string(),
                };
                match *op {
                    "isEmpty" | "isNotEmpty" => assert!(v.is_null(), "{field} {op}: {v}"),
                    "between" => assert!(v.as_array().is_some_and(|a| a.len() == 2 && a.iter().all(one)), "{field} {op}: {v}"),
                    "inLast" | "notInLast" => assert!(v["n"].is_number(), "{field} {op}: {v}"),
                    _ => assert!(one(&v), "{field} {op}: {v}"),
                }
            }
        }
        // a new date rule starts at this year, not at "" (and a between doesn't match every date)
        lightcraft_catalog::rules::set_now(Some("2026-10-09T12:00:00".into()));
        assert_eq!(default_value("captureDate", "is"), json!("2026"));
        assert_eq!(default_value("captureDate", "between"), json!(["2026-01", "2026-12"]));
        lightcraft_catalog::rules::set_now(None);
        assert_eq!(default_value("shutterSpeed", "gte"), json!("1/250"));
        assert_eq!(default_value("shutterSpeed", "between"), json!(["1/1000", "1/125"]));
    }
}
