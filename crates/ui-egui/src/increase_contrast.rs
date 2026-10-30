//! Increase Contrast (Settings ▸ Interface ▸ Increase contrast): borders, separators and text stand
//! out more in every theme.
//!
//! [`LightcraftApp::sync_theme`](crate::LightcraftApp::sync_theme) stores the setting in the egui
//! context ([`set`]) and restyles it; [`crate::theme::apply_kind`] reads it back ([`increased`]).

use egui::{Color32, Context, Id};

use crate::button::contrast;
use crate::theme::{ThemeKind, Tokens};

fn id() -> Id {
    Id::unique("lightcraft_increase_contrast")
}

/// Record the setting for the next [`crate::theme::apply_kind`].
pub fn set(ctx: &Context, increased: bool) {
    ctx.data_mut(|d| d.insert_temp(id(), increased));
}

/// Whether the theme the context shows has increased contrast.
pub fn increased(ctx: &Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(id())).unwrap_or(false)
}

/// How much of a disabled control's colors stays: egui fades them to half, which leaves a
/// light theme's labels under 3:1.
pub fn disabled_alpha(increased: bool) -> f32 {
    if increased { 0.7 } else { 0.5 }
}

/// Stroke width of window, panel and control outlines: thicker with the setting on.
pub fn line_width(increased: bool) -> f32 {
    if increased { 1.5 } else { 1.0 }
}

/// `from`, moved toward `to` in small steps until it stands `ratio`:1 against every color in
/// `against`; `to` itself when nothing on the way does.
fn reach(from: Color32, to: Color32, against: &[Color32], ratio: f32) -> Color32 {
    const STEPS: u8 = 64;
    (0..=STEPS)
        .map(|i| crate::widgets::lerp(from, to, f32::from(i) / f32::from(STEPS)))
        .find(|c| against.iter().all(|on| contrast(*c, *on) >= ratio))
        .unwrap_or(to)
}

impl Tokens {
    /// The tokens of `kind`, with increased contrast when `increased`.
    pub fn for_kind_with(kind: ThemeKind, increased: bool) -> Tokens {
        let t = Tokens::for_kind(kind);
        if increased { t.increased_contrast() } else { t }
    }

    /// These tokens with the ink and the lines made stronger, the surfaces untouched: text 7:1 on
    /// every surface it is written on (WCAG AAA), icons 4.5:1, disabled text 3:1, and borders,
    /// separators and slider parts 3:1 against what they part. Never weaker than before.
    ///
    /// Not covered: text on a photo (that has its own ink and shade), on a cell tinted by a color
    /// label, on a filled Primary or Danger button (4.5:1 as it was), text written in the accent,
    /// caution or reject color, a selected row of a list or a drop-down (egui's selection fill and its text), and
    /// the faded look of a disabled control.
    fn increased_contrast(self) -> Tokens {
        // (at rest, and a control hovered, pressed or active, and a selected cell: text is drawn on those too)
        let surfaces = [
            self.chrome,
            self.canvas,
            self.grid_bg,
            self.cell,
            self.inset,
            self.field,
            self.button,
            self.hover,
            self.pressed,
            self.tool_active,
            self.cell_selected,
        ];
        // (toward the text color, which stands furthest from every surface of the theme)
        let ink = |color: Color32, against: &[Color32], ratio: f32| reach(color, self.text, against, ratio);
        let text = ink(self.text, &surfaces, 7.0);
        let track = ink(self.track, &[self.chrome, self.inset, self.canvas], 3.0);
        let thumb = ink(self.thumb, &[self.chrome, track], 3.0);
        let text_label = ink(self.text_label, &surfaces, 7.0);
        // (dim text stays no stronger than the label's: at 7:1 both reach the same floor)
        let text_dim = ink(self.text_dim, &surfaces, 7.0);
        let text_dim = if contrast(text_dim, self.chrome) > contrast(text_label, self.chrome) { text_label } else { text_dim };
        Tokens {
            text,
            text_label,
            text_dim,
            text_disabled: ink(self.text_disabled, &surfaces, 3.0),
            icon: ink(self.icon, &surfaces, 4.5),
            field_border: ink(self.field_border, &[self.field, self.chrome, self.inset], 3.0),
            button_border: ink(self.button_border, &[self.button, self.chrome, self.inset, self.hover, self.pressed], 3.0),
            divider: ink(self.divider, &[self.chrome, self.canvas, self.grid_bg], 3.0),
            track,
            thumb,
            // (the end of the way: the raised resting thumb stands somewhere short of it)
            thumb_hover: self.text,
            ..self
        }
    }
}
