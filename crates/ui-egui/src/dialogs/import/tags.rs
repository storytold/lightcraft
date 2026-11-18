use super::{Tokens, register};

/// Reserve the actual translated tag button width beside a template field.
pub(crate) fn tag_field_width(ui: &egui::Ui) -> f32 {
    let t = Tokens::get(ui.ctx());
    let galley = ui.painter().layout_no_wrap(crate::i18n::tr("Tags").to_string(), t.semibold(11.5), t.text);
    (ui.available_width() - (galley.size().x + 18.0).max(37.0) - ui.spacing().item_spacing.x).max(80.0)
}

/// The "Tags" toggle beside a template field (`button:<key>Tags`); returns whether the tag help
/// ([`tag_help`]) is open.
pub(crate) fn tag_toggle(ui: &mut egui::Ui, key: &str) -> bool {
    let id = egui::Id::unique((key, "tags-open"));
    let mut open = ui.ctx().data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    let r = crate::widgets::text_button(ui, &format!("{key}Tags"), crate::i18n::tr("Tags"), open)
        .on_hover_text(crate::i18n::tr("Show the template tags; click one to insert it"));
    if r.clicked() {
        open = !open;
        ui.ctx().data_mut(|d| d.insert_temp(id, open));
    }
    open
}

/// "Unknown tag {x} stays as typed" under a template field, when it has one.
pub(crate) fn unknown_tags_warning(ui: &mut egui::Ui, template: &str) {
    let unknown = lightcraft_engine::rename::unknown_tokens(template);
    if !unknown.is_empty() {
        let s = if unknown.len() == 1 { "" } else { "s" };
        let text = crate::i18n::tr_format!(
            "Unknown tag{s} {} stay{} as typed (see Tags)",
            unknown.join(" "),
            if unknown.len() == 1 { "s" } else { "" },
            s = s
        );
        ui.label(egui::RichText::new(text).color(Tokens::get(ui.ctx()).caution));
    }
}

/// Insert `tag` into `text` at the text cursor of the field `edit_id` (replacing its selection);
/// appended when the field never had a cursor. The cursor ends up after the tag.
pub(crate) fn insert_at_cursor(ctx: &egui::Context, edit_id: egui::Id, text: &mut String, tag: &str) {
    use egui::text::{CCursor, CCursorRange};
    let mut state = egui::text_edit::TextEditState::load(ctx, edit_id).unwrap_or_default();
    let n = text.chars().count();
    let (a, b) = state.cursor.char_range().map_or((n, n), |r| {
        let (x, y) = (r.primary.index.0.min(n), r.secondary.index.0.min(n));
        (x.min(y), x.max(y))
    });
    let byte = |c: usize| text.char_indices().nth(c).map_or(text.len(), |(i, _)| i);
    let (ba, bb) = (byte(a), byte(b));
    text.replace_range(ba..bb, tag);
    state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(a + tag.chars().count()))));
    state.store(ctx, edit_id);
    ctx.memory_mut(|m| m.request_focus(edit_id));
}

/// The template tag help: every tag (from the engine's one list, `rename::TOKENS`) with its
/// meaning and an example, the `{date:…}` directives and how templates behave. Clicking a tag
/// (`button:<key>Tag-<i>`) inserts it at the cursor of the field `edit_id`.
pub(crate) fn tag_help(ui: &mut egui::Ui, key: &str, text: &mut String, edit_id: egui::Id) {
    use lightcraft_engine::rename::{DATE_DIRECTIVES, TEMPLATE_NOTES, TOKENS, token_example};
    let t = Tokens::get(ui.ctx());
    egui::Frame::new().fill(t.inset).corner_radius(4.0).inner_margin(6.0).show(ui, |ui| {
        ui.label(
            egui::RichText::new(crate::i18n::tr("Click a tag to insert it. Examples are for IMG_0042.CR3 taken 14 Jan 2026, 05:58:48."))
                .color(t.text_dim)
                .small(),
        );
        egui::ScrollArea::vertical().id_salt((key, "tags")).max_height(170.0).show(ui, |ui| {
            egui::Grid::new((key, "tag-grid")).num_columns(3).spacing([10.0, 2.0]).show(ui, |ui| {
                for (i, tok) in TOKENS.iter().enumerate() {
                    let r = ui.add(egui::Button::new(egui::RichText::new(tok.tag).monospace().color(t.text)).small()).on_hover_text(
                        if tok.aliases.is_empty() {
                            crate::i18n::tr("Insert at the cursor").to_string()
                        } else {
                            crate::i18n::tr_format!("Insert at the cursor (also written {})", tok.aliases.join(", "))
                        },
                    );
                    register(ui, format!("button:{key}Tag-{i}"), r.rect);
                    if r.clicked() {
                        insert_at_cursor(ui.ctx(), edit_id, text, tok.tag);
                    }
                    ui.label(egui::RichText::new(crate::i18n::tr(tok.meaning)).color(t.text_label));
                    ui.label(egui::RichText::new(token_example(tok.tag)).monospace().color(t.text_dim));
                    ui.end_row();
                }
            });
            let directives: Vec<String> = DATE_DIRECTIVES.iter().map(|(d, m)| format!("{d} {}", crate::i18n::tr(m))).collect();
            ui.label(egui::RichText::new(crate::i18n::tr_format!("{{date:…}} directives: {}", directives.join(" · "))).color(t.text_dim).small());
            for n in TEMPLATE_NOTES {
                ui.label(egui::RichText::new(format!("• {}", crate::i18n::tr(n))).color(t.text_dim).small());
            }
        });
    });
}
