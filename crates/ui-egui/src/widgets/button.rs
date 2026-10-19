//! One button for the whole interface: what it is for ([`Kind`]: Primary, Secondary, Danger,
//! Ghost), how big ([`Size`]: Small, Medium, Large), and what it shows (text, an icon and text,
//! or an icon alone, which then needs a name for screen readers). It looks the same everywhere,
//! answers hover, press and the keyboard the same way, and its text always reads at WCAG AA on
//! its fill.
//!
//! ```ignore
//! if Button::new("Import Photos…").primary().widget("button:emptyImportPhotos").show(ui).clicked() { … }
//! Button::icon_only(Icon::Trash, "Delete mask").ghost().small().show(ui);
//! Button::new("Remove").icon(Icon::Trash).danger().show(ui);
//! ```

use egui::{Color32, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};

use crate::icons::{Icon, paint};
use crate::theme::Tokens;

/// What a button is for, which decides how loud it is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    /// The step to take: filled with the accent. One per place (a dialog's confirm, the welcome's
    /// Import Photos…).
    Primary,
    /// Any other action: a neutral fill with a border.
    #[default]
    Secondary,
    /// A step that removes or erases something, when it is the one being asked for: filled red.
    Danger,
    /// A quiet action among content (a toolbar, a banner's Hide): no fill until hovered.
    Ghost,
}

/// How big a button is: its height, padding, text and icon size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Size {
    /// 24 points: in toolbars, banners and dense rows.
    Small,
    /// 30 points: dialogs and panels.
    #[default]
    Medium,
    /// 36 points: a page's one main action (the welcome, About's Discord link).
    Large,
}

impl Size {
    pub const fn height(self) -> f32 {
        match self {
            Size::Small => 24.0,
            Size::Medium => 30.0,
            Size::Large => 36.0,
        }
    }
    fn padding(self) -> f32 {
        match self {
            Size::Small => 8.0,
            Size::Medium => 12.0,
            Size::Large => 16.0,
        }
    }
    fn text(self) -> f32 {
        match self {
            Size::Small => 12.0,
            Size::Medium => 13.0,
            Size::Large => 14.0,
        }
    }
    fn icon(self) -> f32 {
        match self {
            Size::Small => 14.0,
            Size::Medium => 16.0,
            Size::Large => 18.0,
        }
    }
    fn gap(self) -> f32 {
        match self {
            Size::Small => 4.0,
            Size::Medium => 6.0,
            Size::Large => 8.0,
        }
    }
    fn radius(self) -> f32 {
        match self {
            Size::Small => 4.0,
            Size::Medium => 5.0,
            Size::Large => 6.0,
        }
    }
}

/// Where the pointer and the keyboard are, for the colours.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Idle,
    Hovered,
    Pressed,
    Disabled,
}

/// The colours a button of `kind` takes in `state`: fill (if any), border (if any), and what is
/// drawn on it (text and icon).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    pub fill: Option<Color32>,
    pub border: Option<Color32>,
    pub content: Color32,
}

/// WCAG 2 contrast of two opaque colours.
pub fn contrast(a: Color32, b: Color32) -> f32 {
    let lum = |c: Color32| {
        let ch = |v: u8| {
            let v = f32::from(v) / 255.0;
            if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * ch(c.r()) + 0.7152 * ch(c.g()) + 0.0722 * ch(c.b())
    };
    let (x, y) = (lum(a), lum(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// `c` toward black by `amount` (0 to 1).
fn darker(c: Color32, amount: f32) -> Color32 {
    crate::widgets::lerp(c, Color32::BLACK, amount)
}

/// `fill`, darkened as little as needed for white to read on it at 4.5:1 (a light theme's or a
/// bright red's fill would otherwise be too pale for white text).
fn fill_for_white(fill: Color32) -> Color32 {
    let mut f = fill;
    for _ in 0..20 {
        if contrast(Color32::WHITE, f) >= 4.5 {
            break;
        }
        f = darker(f, 0.06);
    }
    f
}

/// The colours of a `kind` button in `state` with the theme `t`.
pub fn look(t: &Tokens, kind: Kind, state: State) -> Look {
    let filled = |base: Color32| {
        let base = fill_for_white(base);
        // hover and press go darker: white on them only reads better
        let fill = match state {
            State::Idle | State::Disabled => base,
            State::Hovered => darker(base, 0.12),
            State::Pressed => darker(base, 0.22),
        };
        Look { fill: Some(fill), border: None, content: Color32::WHITE }
    };
    let look = match kind {
        Kind::Primary => filled(t.accent),
        Kind::Danger => filled(t.reject),
        Kind::Secondary => Look {
            fill: Some(match state {
                State::Idle | State::Disabled => t.button,
                State::Hovered => t.hover,
                State::Pressed => t.pressed,
            }),
            border: Some(t.button_border),
            content: t.text,
        },
        Kind::Ghost => Look {
            fill: match state {
                State::Idle | State::Disabled => None,
                State::Hovered => Some(t.hover),
                State::Pressed => Some(t.pressed),
            },
            border: None,
            content: t.text,
        },
    };
    if state == State::Disabled {
        let fade = |c: Color32| c.gamma_multiply(0.45);
        Look { fill: look.fill.map(fade), border: look.border.map(fade), content: fade(look.content) }
    } else {
        look
    }
}

/// A button, built and then shown with [`Button::show`] (in the layout) or [`Button::show_at`]
/// (in a given rectangle).
#[derive(Clone, Debug)]
pub struct Button<'a> {
    text: Option<&'a str>,
    icon: Option<Icon>,
    name: Option<&'a str>,
    kind: Kind,
    size: Size,
    enabled: bool,
    busy: bool,
    widget: Option<String>,
    tooltip: Option<&'a str>,
    min_width: f32,
    id_salt: Option<egui::Id>,
}

impl<'a> Button<'a> {
    /// A button with `text` (an English string, translated when drawn).
    pub fn new(text: &'a str) -> Self {
        Button {
            text: Some(text),
            icon: None,
            name: None,
            kind: Kind::default(),
            size: Size::default(),
            enabled: true,
            busy: false,
            widget: None,
            tooltip: None,
            min_width: 0.0,
            id_salt: None,
        }
    }

    /// A button showing `icon` alone; `name` is what a screen reader says and the tooltip shows.
    pub fn icon_only(icon: Icon, name: &'a str) -> Self {
        Button { text: None, icon: Some(icon), name: Some(name), ..Button::new("") }
    }

    /// An icon before the text.
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn kind(mut self, kind: Kind) -> Self {
        self.kind = kind;
        self
    }
    pub fn primary(self) -> Self {
        self.kind(Kind::Primary)
    }
    pub fn secondary(self) -> Self {
        self.kind(Kind::Secondary)
    }
    pub fn danger(self) -> Self {
        self.kind(Kind::Danger)
    }
    pub fn ghost(self) -> Self {
        self.kind(Kind::Ghost)
    }

    pub fn size(mut self, size: Size) -> Self {
        self.size = size;
        self
    }
    pub fn small(self) -> Self {
        self.size(Size::Small)
    }
    pub fn large(self) -> Self {
        self.size(Size::Large)
    }

    /// Off: drawn faded, and a click does nothing.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Working on what it was pressed for: a spinner takes the icon's place (or sits before the
    /// text, inside the button's own padding, so the button keeps its width), a click does nothing,
    /// and screen readers hear it is busy.
    pub fn busy(mut self, busy: bool) -> Self {
        self.busy = busy;
        self
    }

    /// Registered under this id (`button:…`) for scripts, tests and `ui.clickWidget`; unique on
    /// screen, as every registered id is.
    pub fn widget(mut self, id: impl Into<String>) -> Self {
        self.widget = Some(id.into());
        self
    }

    /// Shown on hover (an icon-only button shows its name without one).
    pub fn tooltip(mut self, text: &'a str) -> Self {
        self.tooltip = Some(text);
        self
    }

    /// Told apart from buttons of the same widget id in other places (each dialog's confirm is
    /// `button:dialogOk`: salted with the dialog, the keyboard's place doesn't carry from one
    /// dialog to the next).
    pub fn id_salt(mut self, salt: impl std::hash::Hash + std::fmt::Debug) -> Self {
        self.id_salt = Some(egui::Id::NULL.with(salt));
        self
    }

    /// At least this wide (a row of buttons of one width).
    pub fn min_width(mut self, width: f32) -> Self {
        self.min_width = width;
        self
    }

    fn font(&self, t: &Tokens) -> egui::FontId {
        // the loud kinds speak a little bolder
        match self.kind {
            Kind::Primary | Kind::Danger => t.semibold(self.size.text()),
            Kind::Secondary | Kind::Ghost => t.font(self.size.text()),
        }
    }

    fn galley(&self, ui: &Ui) -> Option<std::sync::Arc<egui::Galley>> {
        let t = Tokens::get(ui.ctx());
        self.text.filter(|s| !s.is_empty()).map(|s| ui.painter().layout_no_wrap(crate::i18n::tr(s).to_string(), self.font(&t), Color32::WHITE))
    }

    /// How big it is drawn.
    pub fn desired_size(&self, ui: &Ui) -> Vec2 {
        let h = self.size.height();
        let text_w = self.galley(ui).map_or(0.0, |g| g.size().x);
        let w = match (self.icon.is_some(), text_w > 0.0) {
            // an icon alone: a square
            (true, false) => h,
            (true, true) => self.size.padding() * 2.0 + self.size.icon() + self.size.gap() + text_w,
            (false, _) => self.size.padding() * 2.0 + text_w,
        };
        vec2(w.max(self.min_width), h)
    }

    /// Show it where the layout puts it.
    pub fn show(self, ui: &mut Ui) -> Response {
        let size = self.desired_size(ui);
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        self.show_at(ui, rect)
    }

    /// Show it in `rect` (sized by the caller, e.g. a row of equal buttons).
    pub fn show_at(self, ui: &mut Ui, rect: Rect) -> Response {
        let t = Tokens::get(ui.ctx());
        // stable across frames and moves: its widget id (with its salt) when it has one, else its
        // place in the ui (moved on, so the next button in the same ui gets another)
        let id = match &self.widget {
            Some(w) => ui.scope_id().with(("lc-button", w.as_str(), self.id_salt)),
            None => {
                let id = ui.auto_id_with(("lc-button", self.text.or(self.name), self.id_salt));
                ui.skip_ahead_auto_ids(1);
                id
            }
        };
        // a button in a disabled part of the window is disabled too
        let enabled = self.enabled && ui.is_enabled();
        let resp = ui.interact(
            rect,
            id,
            if !enabled {
                Sense::hover()
            } else if self.busy {
                // still reachable by the keyboard, but nothing to press
                Sense::focusable_noninteractive()
            } else {
                Sense::click()
            },
        );
        let words = crate::i18n::tr(self.name.or(self.text).unwrap_or_default()).to_string();
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, enabled, words.clone()));
        if self.busy {
            ui.ctx().accesskit_node_builder(resp.id, |node| node.set_busy());
        }
        if let Some(w) = &self.widget {
            crate::widgets::register(ui, w.clone(), rect);
        }
        let state = if !enabled {
            State::Disabled
        } else if self.busy {
            State::Idle
        } else if resp.is_pointer_button_down_on() {
            State::Pressed
        } else if resp.hovered() {
            State::Hovered
        } else {
            State::Idle
        };
        let lk = look(&t, self.kind, state);
        // (its own rectangle and its focus ring: a long text never runs over its neighbours)
        let p = &ui.painter_at(rect.expand(5.0));
        let radius = self.size.radius();
        if let Some(fill) = lk.fill {
            p.rect_filled(rect, radius, fill);
        }
        if let Some(border) = lk.border {
            p.rect_stroke(rect, radius, Stroke::new(1.0, border), StrokeKind::Inside);
        }
        // where the keyboard is: an accent ring around it
        if resp.has_focus() {
            p.rect_stroke(rect.expand(2.0), radius + 2.0, Stroke::new(2.0, t.accent), StrokeKind::Outside);
        }
        let galley = self.galley(ui);
        let icon_s = self.size.icon();
        // busy: the spinner is the icon's slot; with text alone it is a smaller one in the button's
        // own padding (a little tighter), so the button never grows
        let spinner_only = self.busy && self.icon.is_none() && galley.is_some();
        let (slot, gap) = if spinner_only { ((self.size.padding() * 2.0 - 12.0).clamp(8.0, icon_s), 4.0) } else { (icon_s, self.size.gap()) };
        let has_slot = self.icon.is_some() || spinner_only;
        let content_w = match (&galley, has_slot) {
            (Some(g), true) => slot + gap + g.size().x,
            (Some(g), false) => g.size().x,
            (None, true) => slot,
            (None, false) => 0.0,
        };
        let mut x = rect.center().x - content_w / 2.0;
        if has_slot {
            let slot_rect = Rect::from_center_size(pos2(x + slot / 2.0, rect.center().y), vec2(slot, slot));
            if self.busy {
                if ui.is_rect_visible(rect) {
                    let reduced = crate::motion::reduced(ui.ctx());
                    crate::progress::paint_spinner(p, slot_rect, lk.content, ui.input(|i| i.time), reduced);
                    if !reduced {
                        ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
                    }
                }
            } else if let Some(icon) = self.icon {
                paint(p, slot_rect, icon, lk.content);
            }
            x += slot + gap;
        }
        if let Some(g) = galley {
            let at = pos2(x, rect.center().y - g.size().y / 2.0);
            p.galley_with_override_text_color(at, g, lk.content);
        }
        // the tooltip, or an icon's name, also while disabled (what the button would do). egui
        // calls a response disabled only in a disabled ui: a button turned off on its own still
        // answers hover, and takes the ordinary tooltip
        match self.tooltip.or(if self.text.is_none() { self.name } else { None }) {
            Some(tip) if resp.enabled() => resp.on_hover_text(crate::i18n::tr(tip)),
            Some(tip) => resp.on_disabled_hover_text(crate::i18n::tr(tip)),
            None => resp,
        }
    }
}
