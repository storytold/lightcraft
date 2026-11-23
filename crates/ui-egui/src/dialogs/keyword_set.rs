//! Edit Keyword Set: framework chrome and controls over LightCraft's existing dialog draft.
//!
//! The draft remains `UiState::dialog`, so automation and Save use the same command boundary.
//! Cancel never writes a set; an engine refusal keeps the draft open for correction.

use craft::behavior::ColumnWidth;
use craft::ui::choice::Checkbox;
use craft::ui::contract::overlay::Outside;
use craft::ui::contract::{Bind, Props as _, Show as _, ShowContainer as _, Sizing};
use craft::ui::display::{Shade, Text};
use craft::ui::layout_components::{Grid, Stack};
use craft::ui::patterns::form_dialogs::FormDialog;
use craft::ui::runtime::env::ThemeScope;
use craft::ui::text::TextField;

use crate::LightcraftApp;
use crate::state::Dialog;
fn register(id: impl Into<String>, response: &egui::Response) {
    if !craft::ui::runtime::measure::active(&response.ctx) {
        crate::widgets::register_response(id, response.rect, response);
    }
}

#[derive(Clone, Default, PartialEq)]
struct Draft {
    name: String,
    slots: Vec<String>,
    as_new: bool,
}

const COLUMNS: &[ColumnWidth] = &[ColumnWidth::Fixed(130.0); 3];

/// The actual production body, also used by isolated visual tests.
fn body(ui: &mut egui::Ui, draft: &mut Draft, replacing: bool) -> bool {
    Text::new(crate::i18n::tr("Set name")).shade(Shade::Muted).show(ui);
    let name = TextField::new(crate::i18n::tr("Set name"), Bind::bound(&mut draft.name))
        .id_salt("keyword-set-name")
        .placeholder(crate::i18n::tr("Set name"))
        .select_on_focus()
        .limit(None)
        .sizing(Sizing::FillWidth)
        .show(ui);
    register("field:keywordSetName", &name.response);
    let mut escaped = name.escaped;
    Text::new(crate::i18n::tr("Keywords (⌥1–⌥9)")).shade(Shade::Muted).show(ui);
    draft.slots.resize(9, String::new());
    Grid::new(COLUMNS).show(ui, |ui, _| {
        for (i, slot) in draft.slots.iter_mut().enumerate() {
            let label = format!("⌥{}", i + 1);
            let shown = TextField::new(label.clone(), Bind::bound(slot))
                .id_salt(("keyword-set-slot", i))
                .placeholder(label)
                .limit(None)
                .sizing(Sizing::FillWidth)
                .show(ui);
            escaped |= shown.escaped;
            register(format!("field:keywordSetSlot:{}", i + 1), &shown.response);
        }
    });
    if replacing {
        let shown = Checkbox::new(Bind::bound(&mut draft.as_new)).id_salt("keyword-set-as-new").label(crate::i18n::tr("Save as a new set")).show(ui);
        register("check:keywordSetAsNew", &shown.response);
    }
    escaped
}

pub(super) fn show(app: &mut LightcraftApp, ctx: &egui::Context, blocked: bool) {
    let Some(mut dialog @ Dialog::KeywordSet { .. }) = app.ui.dialog.clone() else { return };
    let Dialog::KeywordSet { replaces, name, slots, as_new } = &dialog else { return };
    let replacing = replaces.is_some();
    let mut draft = Draft { name: name.clone(), slots: slots.clone(), as_new: *as_new };
    let kind = crate::theme::current(ctx).unwrap_or_else(|| app.ui.settings.theme(app.system_theme(ctx)));
    let mode = if kind.is_dark() { craft::theme::Mode::Dark } else { craft::theme::Mode::Light };
    let outside = app.ui.settings.close_dialogs_outside;
    let title_bar = super::title_bar_band(app);
    let popup_was_open =
        egui::Popup::is_any_open(ctx) || craft::ui::runtime::overlay::family(ctx, egui::Id::unique("lightcraft.keyword-set")).len() > 1;
    let mut output = None;
    egui::Area::new(egui::Id::unique("keyword-set-host")).fixed_pos(ctx.content_rect().min).show(ctx, |ui| {
        let key = egui::Id::unique("keyword-set-inspect");
        let (_, record) = craft::ui::runtime::inspect::record(ui, key, |ui| {
            Stack::new().theme(ThemeScope::mode(mode)).show(ui, |ui, _| {
                output = Some(
                    FormDialog::new(crate::i18n::tr("Edit Keyword Set"), Bind::bound(&mut draft))
                        .ok_label(crate::i18n::tr("Save"))
                        .cancel_label(crate::i18n::tr("Cancel"))
                        .alt_resets(false)
                        .closable(false)
                        .opened(Bind::given(true))
                        .shaped(|dialog| {
                            dialog.id(egui::Id::unique("lightcraft.keyword-set")).enabled(!blocked).overlay(|mut overlay| {
                                // LightCraft dismisses only a primary press-and-release outside,
                                // excluding the integrated titlebar; the shared helper below owns it.
                                overlay.dismiss = overlay.dismiss.outside(Outside::Keep);
                                overlay
                            })
                        })
                        .show(ui, |ui, cx, draft| {
                            if body(ui, draft, replacing) {
                                // Legacy Edit Keyword Set used one Escape to abandon the whole
                                // draft, not a second Escape after canceling the active field.
                                cx.request_close_as(craft::ui::runtime::overlay::CloseReason::Escape);
                            }
                            true
                        }),
                );
            });
        });
        // Keep existing automation ids, but derive their hit areas from the framework's actual
        // accessible footer buttons, never guessed coordinates or invisible surrogate controls.
        if let Some(record) = record {
            for element in &record.elements {
                let Some(facts) = &element.facts else { continue };
                if facts.role != "Button" {
                    continue;
                }
                let id = match facts.name.as_deref() {
                    Some(name) if name == crate::i18n::tr("Save") => "button:dialogOk",
                    Some(name) if name == crate::i18n::tr("Cancel") => "button:dialogCancel",
                    _ => continue,
                };
                let hit = record.hits.iter().find(|hit| hit.owner == element.id && hit.part == element.part);
                if let Some(shown) = &output {
                    let rect = hit.map_or(element.rect, |hit| hit.rect);
                    let mut response = shown.response.clone();
                    response.id = element.id;
                    response.rect = rect;
                    response.interact_rect = rect;
                    crate::widgets::register_response(id, rect, &response);
                }
            }
        }
    });
    let Some(output) = output else { return };
    crate::widgets::register_response("dialog:window", output.response.rect, &output.response);
    ctx.memory_mut(|memory| memory.set_modal_layer(output.response.layer_id));
    if !popup_was_open && let Some(band) = title_bar {
        super::title_bar_handle(ctx, band, output.response.rect);
    }
    if let Dialog::KeywordSet { name, slots, as_new, .. } = &mut dialog {
        *name = draft.name;
        *slots = draft.slots;
        *as_new = draft.as_new;
    }
    let backdrop = if blocked {
        ctx.data_mut(|data| data.remove_temp::<bool>(egui::Id::unique("dialog-backdrop-press")));
        false
    } else {
        outside && super::clicked_backdrop(ctx, output.response.rect, title_bar, popup_was_open)
    };
    let close = if blocked {
        false
    } else if backdrop {
        true
    } else if output.accepted.is_some() {
        super::confirmed(app, ctx, &mut dialog)
    } else {
        output.closed.is_some()
    };
    if !close && !blocked {
        let frame = ctx.cumulative_frame_nr();
        ctx.data_mut(|data| data.insert_temp(egui::Id::unique(super::BACKDROP_SHOWN), frame));
    }
    app.ui.dialog = (!close).then_some(dialog);
}
