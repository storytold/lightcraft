//! The keyboard-shortcut display: a shortcut drawn as keycaps, or as one compact key string.
//!
//! Replaces the shortcut texts every list built by hand (`⌘⇧Z` next to a menu row, the keys in
//! Help ▸ Keyboard Shortcuts, `format!("{label} ({shortcut})")` tooltips). The wording is
//! [`crate::menubar::shortcut_text`]'s: `⌘⇧Z` on macOS, `Ctrl+Shift+Z` elsewhere. It only shows;
//! nothing in it can be clicked. Colours and the caption font come from [`Tokens`].

use egui::{Align2, Color32, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};

use crate::theme::Tokens;

/// Size of the caption text on a key.
const CAPTION: f32 = 11.0;
/// Horizontal padding inside a keycap.
const CAP_PAD: f32 = 5.0;
/// Space between keycaps.
const CAP_GAP: f32 = 3.0;
/// Height of a keycap.
const CAP_H: f32 = 17.0;

/// Whether this egui context runs on macOS (shortcuts are written with ⌘⇧⌥⌃ there).
pub fn is_mac(ctx: &egui::Context) -> bool {
    ctx.os() == egui::os::OperatingSystem::Mac
}

/// The text of `shortcut` as this platform writes it, or `None` when it has none.
pub fn text_of(shortcut: Option<&str>, mac: bool) -> Option<String> {
    shortcut.filter(|s| !s.is_empty()).map(|s| crate::menubar::shortcut_text(s, mac))
}

/// A shortcut on screen.
#[derive(Clone, Copy, Debug)]
pub struct Kbd<'a> {
    shortcut: &'a str,
    mac: bool,
    caps: bool,
    color: Option<Color32>,
    strikethrough: bool,
}

impl<'a> Kbd<'a> {
    /// `shortcut` in the keymap's notation (`Cmd+Shift+Z`), written for macOS when `mac`.
    pub fn new(shortcut: &'a str, mac: bool) -> Self {
        Self { shortcut, mac, caps: false, color: None, strikethrough: false }
    }

    /// One bordered cap per key instead of one compact string.
    pub fn keycaps(mut self) -> Self {
        self.caps = true;
        self
    }

    /// In this colour instead of the label colour (a disabled row's dimmed keys).
    pub fn color(mut self, color: Color32) -> Self {
        self.color = Some(color);
        self
    }

    /// Struck through: a key that something else has taken over.
    pub fn struck(mut self) -> Self {
        self.strikethrough = true;
        self
    }

    /// The one compact string, `⌘⇧Z` or `Ctrl+Shift+Z`.
    pub fn text(&self) -> String {
        crate::menubar::shortcut_text(self.shortcut, self.mac)
    }

    /// The keys one by one: `["⌘", "⇧", "Z"]` or `["Ctrl", "Shift", "Z"]`.
    pub fn keys(&self) -> Vec<String> {
        let mut parts: Vec<&str> = self.shortcut.split('+').collect();
        // a "+" key itself (`Cmd++`) splits into empty parts
        let plus = parts.last() == Some(&"") && parts.len() > 1;
        parts.retain(|p| !p.is_empty());
        let mut keys: Vec<String> = parts.into_iter().map(|p| crate::menubar::shortcut_text(p, self.mac)).collect();
        if plus {
            keys.push("+".to_string());
        }
        keys
    }

    fn ink(&self, t: &Tokens) -> Color32 {
        self.color.unwrap_or(t.text_label)
    }

    fn layout(&self, ui: &Ui) -> Vec<std::sync::Arc<egui::Galley>> {
        let t = Tokens::get(ui.ctx());
        let font = t.font(CAPTION);
        let texts = if self.caps { self.keys() } else { vec![self.text()] };
        texts.into_iter().map(|s| ui.painter().layout_no_wrap(s, font.clone(), self.ink(&t))).collect()
    }

    /// How big it is drawn.
    pub fn size(&self, ui: &Ui) -> Vec2 {
        let galleys = self.layout(ui);
        if !self.caps {
            return galleys.first().map_or(Vec2::ZERO, |g| g.size());
        }
        let width: f32 = galleys.iter().map(|g| cap_width(g)).sum::<f32>() + CAP_GAP * galleys.len().saturating_sub(1) as f32;
        vec2(width, CAP_H)
    }

    /// Draw it with its right edge at `right_center`, centred on that height (a menu row's end).
    pub fn paint_right(&self, ui: &Ui, right_center: Pos2) {
        let size = self.size(ui);
        self.paint_in(ui, Rect::from_min_size(pos2(right_center.x - size.x, right_center.y - size.y / 2.0), size));
    }

    fn paint_in(&self, ui: &Ui, rect: Rect) {
        let t = Tokens::get(ui.ctx());
        let ink = self.ink(&t);
        let p = ui.painter();
        let galleys = self.layout(ui);
        if !self.caps {
            for g in galleys {
                let at = Align2::LEFT_CENTER.align_size_within_rect(g.size(), rect).min;
                p.galley(at, g, ink);
                if self.strikethrough {
                    let y = rect.center().y;
                    p.hline(rect.x_range(), y, Stroke::new(1.0, ink));
                }
            }
            return;
        }
        let mut x = rect.left();
        for g in galleys {
            let w = cap_width(&g);
            let cap = Rect::from_min_size(pos2(x, rect.center().y - CAP_H / 2.0), vec2(w, CAP_H));
            p.rect_filled(cap, 4.0, t.button);
            p.rect_stroke(cap, 4.0, Stroke::new(1.0, t.button_border), StrokeKind::Inside);
            p.galley(Align2::CENTER_CENTER.align_size_within_rect(g.size(), cap).min, g, ink);
            if self.strikethrough {
                p.hline(cap.shrink2(vec2(2.0, 0.0)).x_range(), cap.center().y, Stroke::new(1.0, ink));
            }
            x += w + CAP_GAP;
        }
    }

    /// Show it where the layout puts it.
    pub fn show(self, ui: &mut Ui) -> Response {
        let size = self.size(ui);
        let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
        let text = self.text();
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, text.clone()));
        self.paint_in(ui, rect);
        resp
    }
}

fn cap_width(g: &egui::Galley) -> f32 {
    (g.size().x + CAP_PAD * 2.0).max(CAP_H)
}

/// `text` with the current shortcut of command `command_id` appended, `Reset all edits (⌘⇧R)`.
/// Reads the keymap, so a key the user has remapped shows as remapped; just `text` when the
/// command has no shortcut (or none any more).
pub fn tip_with_shortcut(text: &str, command_id: &str, keymap: &crate::shortcuts::Keymap, mac: bool) -> String {
    match text_of(crate::shortcuts::shortcut_of(keymap, command_id), mac) {
        Some(keys) => format!("{text} ({keys})"),
        None => text.to_string(),
    }
}

/// `resp` with the hover tooltip `text` and the shortcut of command `command_id` as the keymap
/// has it now (see [`tip_with_shortcut`]). A response that is off says it as its disabled tip.
pub fn tooltip_with_shortcut(resp: Response, text: &str, command_id: &str, keymap: &crate::shortcuts::Keymap, mac: bool) -> Response {
    let tip = tip_with_shortcut(text, command_id, keymap, mac);
    if resp.enabled() { resp.on_hover_text(tip) } else { resp.on_disabled_hover_text(tip) }
}
