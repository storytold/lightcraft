//! The menu row: one component for every row of a dropdown, context menu or submenu.
//!
//! Replaces `ui.button`, `selectable_label`, `egui::Button::new(..).frame(false)`, the
//! string-padding check gutter of the in-window menu bar and `widgets::menu_button` inside menus.
//! One row height, one hover look, a reserved check slot (a vector check, announced as checked),
//! an optional icon, a right-aligned shortcut ([`Kbd`]) and, for a submenu, a chevron. Callers pass
//! translated text. A click closes the menu as for any egui menu row (the popup's close
//! behaviour decides).

use egui::{Response, Sense, Stroke, StrokeKind, Ui, pos2, vec2};

use crate::icons::{Icon, paint};
use crate::kbd::Kbd;
use crate::theme::Tokens;

/// Height of every row.
pub const ROW_H: f32 = 22.0;
const PAD: f32 = 8.0;
const SLOT: f32 = 16.0;
const GAP: f32 = 6.0;
const KEYS_GAP: f32 = 24.0;
const MARK: f32 = 13.0;

/// A row of a menu.
#[derive(Clone, Debug)]
pub struct MenuItem<'a> {
    label: &'a str,
    icon: Option<Icon>,
    swatch: Option<egui::Color32>,
    checked: Option<bool>,
    /// One-of-N choice: `Some(on)`.
    selected: Option<bool>,
    inert: bool,
    slot: bool,
    shortcut: Option<&'a str>,
    enabled: bool,
    reason: Option<&'a str>,
    widget: Option<String>,
}

impl<'a> MenuItem<'a> {
    /// A row saying `label` (already translated).
    pub fn new(label: &'a str) -> Self {
        Self {
            label,
            icon: None,
            swatch: None,
            checked: None,
            selected: None,
            inert: false,
            slot: false,
            shortcut: None,
            enabled: true,
            reason: None,
            widget: None,
        }
    }

    /// An icon before the text.
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// A round colour dot before the text (a colour label's row).
    pub fn swatch(mut self, color: egui::Color32) -> Self {
        self.swatch = Some(color);
        self
    }

    /// A row that is on or off: reserves the check slot, draws a check while `on`, and tells
    /// screen readers whether it is checked.
    pub fn checked(mut self, on: bool) -> Self {
        self.checked = Some(on);
        self.slot = true;
        self
    }

    /// A row that is the current one of several exclusive choices (a combo row, a sort key): the
    /// check slot as for [`Self::checked`], announced as a selected radio button.
    pub fn selected(mut self, on: bool) -> Self {
        self.selected = Some(on);
        self.slot = true;
        self
    }

    /// Not clickable, but drawn at full strength (the current choice that has nothing to choose).
    pub fn inert(mut self) -> Self {
        self.inert = true;
        self
    }

    /// Reserve the check slot without being checkable, so the rows of a menu where some are
    /// checkable line up.
    pub fn check_slot(mut self, reserve: bool) -> Self {
        self.slot = reserve || self.checked.is_some() || self.selected.is_some();
        self
    }

    /// The shortcut, in the keymap's notation (`Cmd+Shift+Z`), shown right-aligned as this
    /// platform writes it.
    pub fn shortcut(mut self, shortcut: &'a str) -> Self {
        self.shortcut = Some(shortcut);
        self
    }

    /// Off: drawn faded, and a click does nothing.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Why it is off, shown on hover while it is.
    pub fn disabled_reason(mut self, reason: &'a str) -> Self {
        self.reason = Some(reason);
        self
    }

    /// Registered under this id (`button:…`, `menu:…`) for scripts, tests and `ui.clickWidget`.
    pub fn widget(mut self, id: impl Into<String>) -> Self {
        self.widget = Some(id.into());
        self
    }

    /// Show the row where the layout puts it; `clicked()` says it was chosen.
    pub fn show(self, ui: &mut Ui) -> Response {
        self.row(ui, false)
    }

    /// Show the row as a submenu entry (a chevron at its end) with `contents` in the submenu
    /// beside it. egui keeps the submenu's open/close interaction.
    pub fn submenu<R>(self, ui: &mut Ui, contents: impl FnOnce(&mut Ui) -> R) -> egui::InnerResponse<Option<R>> {
        let enabled = self.enabled && ui.is_enabled();
        let response = self.row(ui, true);
        let inner = if enabled { egui::containers::menu::SubMenu::new().show(ui, &response, contents).map(|r| r.inner) } else { None };
        egui::InnerResponse::new(inner, response)
    }

    /// The columns every row of this menu reserves: those any row of it uses, as drawn on the
    /// previous frame (a menu is redrawn at once when a column appears), so the labels of a
    /// mixed menu start at one x without callers keeping count.
    fn columns(&self, ui: &Ui) -> (bool, bool) {
        let mine = (self.slot || self.checked.is_some() || self.selected.is_some(), self.icon.is_some() || self.swatch.is_some());
        let ctx = ui.ctx();
        let key = egui::Id::unique(("lc-menu-columns", ui.scope_id()));
        let pass = ctx.cumulative_pass_nr();
        // (previous frame's columns, this frame's so far, the pass they were last touched on)
        let (mut prev, mut cur, at) = ctx.data(|d| d.get_temp::<((bool, bool), (bool, bool), u64)>(key)).unwrap_or_default();
        if at != pass {
            prev = cur;
            cur = (false, false);
        }
        cur = (cur.0 || mine.0, cur.1 || mine.1);
        ctx.data_mut(|d| d.insert_temp(key, (prev, cur, pass)));
        if (mine.0 || cur.0) && !prev.0 || (mine.1 || cur.1) && !prev.1 {
            ctx.request_repaint();
        }
        (mine.0 || prev.0, mine.1 || prev.1)
    }

    fn row(mut self, ui: &mut Ui, submenu: bool) -> Response {
        let (slot, lead) = self.columns(ui);
        self.slot = slot;
        let t = Tokens::get(ui.ctx());
        let font = t.font(13.0);
        let galley = ui.painter().layout_no_wrap(self.label.to_string(), font, t.text);
        let mac = crate::kbd::is_mac(ui.ctx());
        let keys = self.shortcut.filter(|s| !s.is_empty()).map(|s| Kbd::new(s, mac));
        let mut width = PAD * 2.0 + galley.size().x;
        if self.slot {
            width += SLOT + GAP;
        }
        if lead {
            width += SLOT + GAP;
        }
        if let Some(k) = &keys {
            width += KEYS_GAP + k.size(ui).x;
        }
        if submenu {
            width += KEYS_GAP / 2.0 + SLOT;
        }
        // the layout stretches it to the menu's width
        let (rect, _) = ui.allocate_at_least(vec2(width, ROW_H), Sense::hover());
        let id = match &self.widget {
            Some(w) => ui.id().with(("lc-menu-item", w.as_str())),
            None => {
                let id = ui.auto_id_with(("lc-menu-item", self.label));
                ui.skip_ahead_auto_ids(1);
                id
            }
        };
        let enabled = self.enabled && ui.is_enabled();
        let clickable = enabled && !self.inert;
        let resp = ui.interact(rect, id, if clickable { Sense::click() } else { Sense::hover() });
        let words = self.label.to_string();
        match self.selected.or(self.checked) {
            // (a submenu row marking the current choice is still a button that opens a menu)
            // (a submenu row marking the current group is a plain button: its check is visual only)
            Some(_) if submenu => resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, words.clone())),
            Some(on) => {
                let kind = if self.selected.is_some() { egui::Role::RadioButton } else { egui::Role::CheckBox };
                resp.widget_info(|| egui::WidgetInfo::selected(kind, enabled, on, words.clone()))
            }
            None => resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, words.clone())),
        }
        if let Some(w) = &self.widget {
            crate::widgets::register(ui, w.clone(), rect);
        }
        // the row whose submenu is open stays lit
        let open = submenu && egui::Popup::is_id_open(ui.ctx(), egui::containers::menu::SubMenu::id_from_widget_id(id));
        let p = ui.painter();
        let fill = if !clickable {
            None
        } else if resp.is_pointer_button_down_on() {
            Some(t.pressed)
        } else if resp.hovered() || open {
            Some(t.hover)
        } else {
            None
        };
        if let Some(fill) = fill {
            p.rect_filled(rect, 3.0, fill);
        }
        if resp.has_focus() {
            p.rect_stroke(rect, 3.0, Stroke::new(1.5, t.accent), StrokeKind::Inside);
        }
        let ink = if enabled { t.text } else { t.text_disabled };
        let cy = rect.center().y;
        let mut x = rect.left() + PAD;
        if self.slot {
            if self.selected.or(self.checked) == Some(true) {
                paint(p, egui::Rect::from_center_size(pos2(x + SLOT / 2.0, cy), vec2(MARK, MARK)), Icon::Check, ink);
            }
            x += SLOT + GAP;
        }
        if let Some(icon) = self.icon {
            paint(p, egui::Rect::from_center_size(pos2(x + SLOT / 2.0, cy), vec2(SLOT, SLOT)), icon, ink);
        }
        if let Some(color) = self.swatch {
            p.circle_filled(pos2(x + SLOT / 2.0, cy), 5.0, color);
        }
        if lead {
            x += SLOT + GAP;
        }
        p.galley_with_override_text_color(pos2(x, cy - galley.size().y / 2.0), galley, ink);
        let mut right = rect.right() - PAD;
        if submenu {
            paint(p, egui::Rect::from_center_size(pos2(right - SLOT / 2.0, cy), vec2(SLOT, SLOT)), Icon::ChevronRight, ink);
            right -= SLOT + KEYS_GAP / 2.0;
        }
        if let Some(k) = keys {
            let k = if enabled { k } else { k.color(t.text_disabled) };
            k.paint_right(ui, pos2(right, cy));
        }
        match self.reason {
            Some(reason) if !enabled && ui.is_enabled() => resp.on_hover_text(reason),
            Some(reason) if !enabled => resp.on_disabled_hover_text(reason),
            _ => resp,
        }
    }
}
