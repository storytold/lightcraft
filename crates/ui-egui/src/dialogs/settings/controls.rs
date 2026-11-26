//! The framework controls the Settings pages share besides checkboxes, choices and text fields
//! ([`super::widgets`]): a push button, a slider with its number field, and a colour well that
//! opens a small colour editor.
//!
//! Every control is a framework component, so Tab and Shift+Tab reach it like any other stop of the
//! page. Each is addressable by the automation name its caller gives it (`button:{name}`,
//! `slider:{name}` / `sliderValue:{name}`, `button:{name}` for a colour well) and does nothing while
//! the framework is only measuring the page.

use craft::behavior::{EditEvent, Mixed};
use craft::ui::buttons::Button;
use craft::ui::contract::{Bind, Props as _, Show as _, ShowContainer as _, Sizing};
use craft::ui::form::{SliderField, SliderFieldLayout};
use craft::ui::popover::Popover;
use craft::ui::runtime::env::Label;
use craft::ui::value_drag::SliderScale;
use egui::{Color32, Sense, vec2};

/// Whether this pass acts: never a sizing pass or the framework's measuring pass.
pub(crate) fn live(ui: &egui::Ui) -> bool {
    !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx())
}

/// The identity a control called `name` keeps its framework state under.
pub(crate) fn owner(name: &str) -> egui::Id {
    egui::Id::new(("settings-control", name))
}

/// A push button (`button:{name}`); true when it was activated by the pointer, the keyboard or
/// assistive technology. A disabled button is no focus stop and never activates.
pub(crate) fn button(ui: &mut egui::Ui, name: &str, label: &str, enabled: bool) -> bool {
    let real = live(ui);
    let shown = Button::new(crate::i18n::tr(label).to_owned()).id_salt(("settings-button", name)).enabled(enabled).show(ui);
    if real {
        crate::widgets::register_response(format!("button:{name}"), shown.response.rect, &shown.response);
    }
    real && enabled && ui.is_enabled() && shown.clicked()
}

/// How a slider reads: its caption (the accessible name; the row shows the visible one), its span
/// and its unit in the numbers the person sees, the least change one step makes, and whether the
/// track is logarithmic.
pub(crate) struct Shape<'a> {
    pub label: &'a str,
    pub lo: f64,
    pub hi: f64,
    pub unit: &'a str,
    pub step: f64,
    pub log: bool,
}

/// What a slider asks its owner to do, in the numbers it shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Step {
    /// A gesture began at `value`: the control's own value then (`None` when it had no single one).
    /// An owner that needs the settings the gesture started from keeps its own copy here.
    Begin(Option<f64>),
    /// Show `value` now (a drag in progress, or an abandoned gesture's value going back) without saving it.
    Preview(f64),
    /// The change is done: save `value`.
    Save(f64),
    /// The gesture is over (`Save` has come, or a cancel gave its value back): whatever an owner kept
    /// for it is dropped here, so nothing of it can colour the next change.
    End,
}

/// A slider with its number field in one line (`slider:{name}`, `sliderValue:{name}`), over `value`.
/// A drag previews as it goes and saves when let go; a typed value, a key or a click on the track
/// saves at once; Escape in a drag gives the value back.
pub(crate) fn slider(ui: &mut egui::Ui, owner: egui::Id, name: &str, shape: &Shape<'_>, value: f64) -> Vec<Step> {
    let real = live(ui);
    let id = owner.with(("slider", name));
    let (out, record) = craft::ui::runtime::inspect::record(ui, id.with("inspect"), |ui| {
        let mut field = SliderField::new(crate::i18n::tr(shape.label).to_owned(), Bind::given(value), shape.lo..=shape.hi)
            .id(id)
            .layout(SliderFieldLayout::Inline)
            .field_sizing(Sizing::Content)
            .step(shape.step)
            .sizing(Sizing::FillWidth);
        if !shape.unit.is_empty() {
            field = field.unit(shape.unit.to_owned());
        }
        if shape.log {
            field = field.scale(SliderScale::Log);
        }
        field.show(ui)
    });
    let mut steps = Vec::new();
    if !real {
        return steps;
    }
    if let Some(record) = record {
        for (index, element) in record.elements.iter().enumerate() {
            let Some(facts) = &element.facts else { continue };
            let prefix = match facts.role.as_str() {
                "Slider" => "slider",
                "SpinButton" => "sliderValue",
                _ => continue,
            };
            // A composite child may have an SDK arena response rather than an egui read_response.
            // Use that recorded child's real owner and stop bounds; this registers an alias only,
            // never another interactive widget or a guessed target.
            let mut response = ui.ctx().read_response(element.id).unwrap_or_else(|| {
                let mut response = out.response.clone();
                response.id = element.id;
                response.rect = record.stop_rect(index).unwrap_or(element.rect);
                response.interact_rect = response.rect.intersect(ui.clip_rect());
                response
            });
            response.interact_rect = response.interact_rect.intersect(ui.clip_rect());
            crate::widgets::register_response(format!("{prefix}:{name}"), response.rect, &response);
        }
    }
    if !ui.is_enabled() {
        return steps;
    }
    for event in &out.events {
        match event {
            EditEvent::Begin { before: Mixed::Same(before), .. } => steps.push(Step::Begin(Some(*before))),
            EditEvent::Begin { .. } => steps.push(Step::Begin(None)),
            EditEvent::Update { value, .. } => steps.push(Step::Preview(*value)),
            EditEvent::Commit { before: Mixed::Same(before), after, .. } => {
                steps.push(Step::Preview(*before));
                if after != before {
                    steps.push(Step::Save(*after));
                }
                steps.push(Step::End);
            }
            // A commit with no single value before it changes nothing to preview or save, but it still
            // ends a gesture.
            EditEvent::Commit { .. } => steps.push(Step::End),
            // A value that arrived in one pass (a press on the track, a typed number, the wheel) is a
            // gesture of its own: it begins where it began, so an owner that keeps the settings a
            // change started from hears both ends of it rather than a save out of nowhere.
            EditEvent::Set { before, after, .. } => {
                if !matches!(before, Mixed::Same(value) if value == after) {
                    match before {
                        Mixed::Same(before) => steps.push(Step::Begin(Some(*before))),
                        _ => steps.push(Step::Begin(None)),
                    }
                    steps.push(Step::Save(*after));
                    steps.push(Step::End);
                }
            }
            EditEvent::Cancel { before: Some(Mixed::Same(before)), .. } => {
                steps.push(Step::Preview(*before));
                steps.push(Step::End);
            }
            EditEvent::Cancel { .. } => steps.push(Step::End),
            _ => {}
        }
    }
    steps
}

/// `#rrggbb` (or `rrggbb`, `#rgb`) as a colour.
pub(crate) fn parse_hex(text: &str) -> Option<[u8; 3]> {
    let digits = text.trim().trim_start_matches('#');
    let value = |hex: &str| u8::from_str_radix(hex, 16).ok();
    match digits.len() {
        6 if digits.is_ascii() => Some([value(&digits[0..2])?, value(&digits[2..4])?, value(&digits[4..6])?]),
        3 if digits.is_ascii() => Some([value(&digits[0..1])? * 17, value(&digits[1..2])? * 17, value(&digits[2..3])? * 17]),
        _ => None,
    }
}

/// A colour well (`button:{name}`): a button showing the colour that opens a small editor of red, green
/// and blue sliders and a hex field. Returns the colour once an edit is done (a slider let go, a hex value
/// entered); a drag shows its colour in the well meanwhile and saves nothing.
pub(crate) fn colour(ui: &mut egui::Ui, owner: egui::Id, name: &str, label: &str, value: [u8; 3]) -> Option<[u8; 3]> {
    let real = live(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(44.0, 24.0), Sense::hover());
    if !real {
        return None;
    }
    let words = crate::i18n::tr(label).to_owned();
    let draft_key = owner.with("colour-draft");
    let open_key = owner.with("colour-open");
    let stamp_key = open_key.with("pass");
    let pass = ui.ctx().cumulative_pass_nr();
    let (kept, previous) = ui.data(|d| (d.get_temp::<bool>(open_key).unwrap_or(false), d.get_temp::<u64>(stamp_key)));
    // (an editor left open by a page that went away is not open when the page comes back)
    let mut open = kept && previous.is_some_and(|last| last == pass || last.checked_add(1) == Some(pass));
    let mut rgb: [u8; 3] = ui.data(|d| d.get_temp(draft_key)).unwrap_or(value);

    let chip = Button::new(words.clone()).id(owner.with("chip")).painted(Label::Empty).show_at(ui, rect);
    crate::widgets::register_response(format!("button:{name}"), rect, &chip.response);
    let face = rect.shrink2(vec2(7.0, 5.0));
    ui.painter().rect_filled(face, 3.0, Color32::from_rgb(rgb[0], rgb[1], rgb[2]));
    ui.painter().rect_stroke(face, 3.0, egui::Stroke::new(1.0, Color32::from_gray(128)), egui::StrokeKind::Inside);
    if ui.is_enabled() && chip.clicked() {
        open = !open;
    }

    let mut saved = false;
    let _editor = Popover::new(words, Bind::bound(&mut open)).id_salt(("settings-colour-editor", name)).opened_by(&chip.response).show(ui, |ui, _| {
        ui.set_min_width(240.0);
        for (i, channel) in ["Red", "Green", "Blue"].into_iter().enumerate() {
            let shape = Shape { label: channel, lo: 0.0, hi: 255.0, unit: "", step: 1.0, log: false };
            let steps = ui
                .horizontal(|ui| {
                    ui.add_sized(vec2(48.0, 24.0), egui::Label::new(crate::i18n::tr(channel)));
                    slider(ui, owner, &format!("{name}{channel}"), &shape, f64::from(rgb[i]))
                })
                .inner;
            for step in steps {
                match step {
                    Step::Begin(_) | Step::End => {}
                    Step::Preview(v) | Step::Save(v) => {
                        rgb[i] = v.round().clamp(0.0, 255.0) as u8;
                        saved |= matches!(step, Step::Save(_));
                    }
                }
            }
        }
        if let Some(typed) = hex_field(ui, owner, name, rgb) {
            rgb = typed;
            saved = true;
        }
    });
    let editing = open;
    ui.data_mut(|d| {
        d.insert_temp(open_key, open);
        d.insert_temp(stamp_key, pass);
        if editing && !saved && rgb != value {
            d.insert_temp(draft_key, rgb);
        } else {
            d.remove::<[u8; 3]>(draft_key);
        }
    });
    (saved && ui.is_enabled() && rgb != value).then_some(rgb)
}

/// The hex text of a colour well's editor (`field:{name}Hex`): the colour typed in, once Return or leaving the
/// field enters it.
fn hex_field(ui: &mut egui::Ui, owner: egui::Id, name: &str, rgb: [u8; 3]) -> Option<[u8; 3]> {
    use craft::ui::text::TextField;
    let key = owner.with("colour-hex");
    let mut text: String = ui.data(|d| d.get_temp(key)).unwrap_or_else(|| format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]));
    let (rect, _) = ui.allocate_exact_size(vec2(96.0, 24.0), Sense::hover());
    if !live(ui) {
        return None;
    }
    let out = TextField::new(crate::i18n::tr("Hex"), Bind::bound(&mut text)).id(key.with("field")).submits().show_at(ui, rect);
    crate::widgets::register_response(format!("field:{name}Hex"), out.response.rect, &out.response);
    // (what is typed is kept while the field has the keyboard; otherwise it shows the colour)
    let committed = out.events.committed().cloned();
    if out.response.has_focus() && committed.is_none() {
        ui.data_mut(|d| d.insert_temp(key, text));
    } else {
        ui.data_mut(|d| d.remove::<String>(key));
    }
    committed.as_deref().and_then(parse_hex).filter(|typed| *typed != rgb)
}

#[cfg(test)]
mod tests {
    use super::parse_hex;

    #[test]
    fn hex_colours_read_with_or_without_the_hash_and_in_short_form() {
        assert_eq!(parse_hex("#203040"), Some([0x20, 0x30, 0x40]));
        assert_eq!(parse_hex("ff8000"), Some([0xff, 0x80, 0x00]));
        assert_eq!(parse_hex(" #f80 "), Some([0xff, 0x88, 0x00]));
        assert_eq!(parse_hex("#12345"), None);
        assert_eq!(parse_hex("#gg0000"), None);
        assert_eq!(parse_hex("#é0000"), None);
        assert_eq!(parse_hex(""), None);
    }
}
