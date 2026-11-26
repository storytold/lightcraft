//! A date picker: a calendar button that opens a popup to pick a year, a month or a day.
//!
//! Standalone: it edits a [`PickedDate`] (a year, optionally a month, optionally a day), so a value
//! keeps its precision (`2026` means the whole year). Callers can limit the precisions offered and
//! the dates allowed ([`DatePicker::min`] / [`DatePicker::max`]: a value is allowed when any of its
//! days falls in range), and say what "today" is. Weekday and month names follow the UI language.
//! Widget ids for tests and agents: `datePicker:<id>` (the button),
//! `datePickerPrecision:<year|month|day>:<id>`, `datePickerPrev:<id>`, `datePickerNext:<id>` and
//! `datePickerCell:<iso>:<id>`.

use std::sync::Arc;

use craft::behavior::Metrics;
use craft::theme::{Stateful, States};
use craft::ui::contract::{Bind, Common, Component, Cx, MeasureCx, PaintUi, Props as _, Role, Show as _, ShowContainer as _, clicked};
use craft::ui::popover::Popover;
use craft::ui::runtime::focus::Initial;
use craft::ui::style::{ComponentStyle, FocusLook, StyleCx, SurfaceLook};
use egui::{Color32, CornerRadius, Galley, Response, RichText, Stroke, Ui, Vec2, vec2};

use crate::icons::Icon;

/// How much of a date a value names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Precision {
    Year,
    Month,
    Day,
}

impl Precision {
    pub const ALL: [Precision; 3] = [Precision::Year, Precision::Month, Precision::Day];

    fn id(self) -> &'static str {
        match self {
            Precision::Year => "year",
            Precision::Month => "month",
            Precision::Day => "day",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Precision::Year => "Year",
            Precision::Month => "Month",
            Precision::Day => "Day",
        }
    }
}

/// A year, a month of it or a day of that: `2026`, `2026-08`, `2026-08-14`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PickedDate {
    pub year: i32,
    pub month: Option<u8>,
    pub day: Option<u8>,
}

impl PickedDate {
    pub fn year(year: i32) -> Self {
        PickedDate { year, month: None, day: None }
    }

    /// Reads `2026`, `2026-08` or `2026-08-14` (anything after a day's `T` or space, such as a
    /// time, is read past); `None` for anything else or a date that doesn't exist.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let num = |a: usize, b: usize| s.get(a..b).filter(|t| t.bytes().all(|c| c.is_ascii_digit())).and_then(|t| t.parse::<u32>().ok());
        let year = i32::try_from(num(0, 4)?).ok()?;
        if s.len() == 4 {
            return Some(PickedDate::year(year));
        }
        let month = u8::try_from(num(5, 7)?).ok().filter(|m| (1..=12).contains(m))?;
        if s.get(4..5) != Some("-") {
            return None;
        }
        if s.len() == 7 {
            return Some(PickedDate { year, month: Some(month), day: None });
        }
        let day = u8::try_from(num(8, 10)?).ok().filter(|d| (1..=days_in_month(year, month)).contains(d))?;
        // a time after the day (read past) must be one: T10, T10:00 or T10:00:00 (or a space for the T)
        let time_ok = match s.len() {
            10 => true,
            13 | 16 | 19 => {
                matches!(s.get(10..11), Some("T" | " "))
                    && s.char_indices().skip(11).all(|(i, c)| if i == 13 || i == 16 { c == ':' } else { c.is_ascii_digit() })
            }
            _ => false,
        };
        (s.get(7..8) == Some("-") && time_ok).then_some(PickedDate { year, month: Some(month), day: Some(day) })
    }

    /// `2026`, `2026-08` or `2026-08-14`.
    pub fn iso(&self) -> String {
        match (self.month, self.day) {
            (Some(m), Some(d)) => format!("{:04}-{m:02}-{d:02}", self.year),
            (Some(m), None) => format!("{:04}-{m:02}", self.year),
            _ => format!("{:04}", self.year),
        }
    }

    pub fn precision(&self) -> Precision {
        match (self.month, self.day) {
            (Some(_), Some(_)) => Precision::Day,
            (Some(_), None) => Precision::Month,
            _ => Precision::Year,
        }
    }

    /// The first day the value covers: (year, month, day).
    pub fn first_day(&self) -> (i32, u8, u8) {
        (self.year, self.month.unwrap_or(1), self.day.unwrap_or(1))
    }

    /// The last day the value covers.
    pub fn last_day(&self) -> (i32, u8, u8) {
        let month = self.month.unwrap_or(12);
        (self.year, month, self.day.unwrap_or_else(|| days_in_month(self.year, month)))
    }
}

/// Days in `month` (1–12) of `year` in the Gregorian calendar; 0 for a month that doesn't exist.
pub fn days_in_month(year: i32, month: u8) -> u8 {
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => 0,
    }
}

/// The days of a month laid out in weeks from Monday: `None` before the 1st and after the last
/// day, so the length is a multiple of 7.
pub fn month_grid(year: i32, month: u8) -> Vec<Option<u8>> {
    let first = format!("{year:04}-{month:02}-01");
    // 1970-01-01 was a Thursday: three days after a Monday
    let lead = lightcraft_catalog::stacks::iso_seconds(&first).map_or(0, |s| (s.div_euclid(86_400) + 3).rem_euclid(7) as usize);
    let mut cells: Vec<Option<u8>> = std::iter::repeat_n(None, lead).chain((1..=days_in_month(year, month)).map(Some)).collect();
    while !cells.len().is_multiple_of(7) {
        cells.push(None);
    }
    cells
}

/// Whether any day of `date` lies between `min` and `max` (each optional, inclusive).
pub fn in_bounds(date: &PickedDate, min: Option<&PickedDate>, max: Option<&PickedDate>) -> bool {
    min.is_none_or(|m| date.last_day() >= m.first_day()) && max.is_none_or(|m| date.first_day() <= m.last_day())
}

/// Weekday column headers, Monday first (messages for translation catalogs).
pub const WEEKDAY_SHORT: [&str; 7] = ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"];

/// Where the open popup is: what it picks and which page it shows.
#[derive(Clone, Copy, Debug)]
struct View {
    precision: Precision,
    year: i32,
    month: u8,
}

impl View {
    /// The view opened on `at`: years kept to 0000–9999 and months to 1–12, whatever a caller's
    /// value holds.
    fn open(at: PickedDate, precision: Precision) -> View {
        View { precision, year: at.year.clamp(0, 9999), month: at.month.unwrap_or(1).clamp(1, 12) }
    }
}

/// The page before (`forward` false) or after `v`: a month, a year or twelve years. Paging stops at
/// years 0000 and 9999 rather than wrapping or leaving four-digit years.
fn step(v: View, forward: bool) -> View {
    let (year, month) = match v.precision {
        Precision::Day => {
            let m = i32::from(v.month) - 1 + if forward { 1 } else { -1 };
            (v.year.saturating_add(m.div_euclid(12)), u8::try_from(m.rem_euclid(12) + 1).unwrap_or(1))
        }
        Precision::Month => (v.year.saturating_add(if forward { 1 } else { -1 }), v.month),
        Precision::Year => {
            // pages of twelve years (2016–2027); the first and last pages hold the ends
            let page = v.year.div_euclid(12).saturating_add(if forward { 1 } else { -1 });
            (page.saturating_mul(12), v.month)
        }
    };
    let years = 0..=9999;
    let fits = match v.precision {
        Precision::Year => years.contains(&year) || years.contains(&year.saturating_add(11)),
        _ => years.contains(&year),
    };
    if fits { View { year, month, ..v } } else { v }
}

/// Whether picking `cell` would cover today: its day, its month or its year.
fn holds_today(cell: &PickedDate, today: Option<PickedDate>) -> bool {
    today.is_some_and(|t| t.first_day() >= cell.first_day() && t.first_day() <= cell.last_day())
}

// Calendar cells are intentionally not stock SDK buttons: their private padding cannot fit
// seven 26-point columns, and their plain labels cannot retain today's bold underline. This
// local component keeps only that compact face. The SDK owns all sensing, primary-release
// activation, cancellation, keyboard, disabled/read-only state, focus and accessibility.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct OptionPalette {
    hover: Color32,
    pressed: Color32,
    selected: Color32,
    edge: Color32,
    text: Color32,
    focus: Color32,
}

#[derive(Clone, Debug, PartialEq, ComponentStyle)]
struct OptionStyle {
    #[style(surface)]
    surface: SurfaceLook,
    #[style(focus)]
    focus: FocusLook,
    #[style(metrics)]
    metrics: Metrics,
}

#[derive(Component)]
struct CalendarOption {
    #[common]
    common: Common,
    #[style_key]
    palette: OptionPalette,
    #[style_key]
    selected: bool,
    galley: Arc<Galley>,
    size: Vec2,
    opacity: f32,
}

impl Component for CalendarOption {
    const ROLE: Role = Role::BUTTON;
    const PARTS: bool = false;
    const OVERLAYS: bool = false;
    type Style = OptionStyle;
    type Output = Response;

    fn style(cx: &StyleCx<'_>, key: CalendarOptionStyleKey) -> OptionStyle {
        let p = key.palette;
        let color = |c: Color32| cx.color(craft::theme::Rgba::from(c));
        let clear = color(Color32::TRANSPARENT);
        let fill = if key.selected {
            Stateful::new(color(p.selected))
        } else {
            Stateful::new(clear).off(States::new(clear, color(p.hover), color(p.pressed), clear))
        };
        let edge = if key.selected { color(p.edge) } else { clear };
        OptionStyle {
            surface: SurfaceLook::new(fill).radius(CornerRadius::same(4)).border(Stateful::new(edge), 1.0),
            focus: FocusLook::new(Stroke::new(2.0, p.focus), -2.0),
            metrics: Metrics::new(),
        }
    }

    fn measure(&mut self, _: &MeasureCx<'_, Self>) -> Vec2 {
        self.size
    }

    fn paint(&mut self, ui: &mut PaintUi<'_>, cx: &Cx<'_, Self>) {
        let shape = egui::epaint::TextShape::new(cx.rect.center() - self.galley.size() / 2.0, Arc::clone(&self.galley), self.palette.text)
            .with_opacity_factor(self.opacity);
        ui.reserve_slot(cx.visible).extend(&cx.response.ctx, vec![shape.into()]);
    }

    fn finish(self, cx: Cx<'_, Self>) -> Response {
        cx.response.clone()
    }
}

/// One compact option, with a semantic id so keyboard focus never follows a reused grid slot
/// onto an unrelated date after paging. Read activation with the SDK's `clicked`, not egui's.
fn option(ui: &mut Ui, id: String, text: RichText, label: String, selected: bool, enabled: bool, min_width: f32) -> Response {
    let t = crate::theme::Tokens::get(ui.ctx());
    let opacity = if enabled { 1.0 } else { ui.visuals().disabled_alpha() };
    let galley = egui::WidgetText::from(text).into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Button);
    let size = (galley.size() + ui.spacing().button_padding * 2.0).max(vec2(min_width, ui.spacing().interact_size.y));
    let (_, rect) = ui.allocate_space(size);
    let response = CalendarOption {
        common: Common::new(),
        palette: OptionPalette {
            hover: t.hover,
            pressed: t.pressed,
            selected: t.accent.gamma_multiply(0.6 * opacity),
            edge: t.accent.gamma_multiply(opacity),
            text: t.text_label,
            focus: t.accent,
        },
        selected,
        galley,
        size,
        opacity: ui.opacity() * opacity,
    }
    .id(ui.scope_id().with(("date-picker-option", id.as_str())))
    .label(label)
    .selected(selected)
    .enabled(enabled)
    .show_at(ui, rect);
    if !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx()) {
        crate::widgets::register_response(id, rect, &response);
    }
    response
}

/// A calendar button that edits a [`PickedDate`]; see the module docs.
pub struct DatePicker<'a> {
    id: String,
    value: &'a mut Option<PickedDate>,
    precisions: &'a [Precision],
    min: Option<PickedDate>,
    max: Option<PickedDate>,
    today: Option<PickedDate>,
}

impl<'a> DatePicker<'a> {
    /// `id` keeps pickers apart and names their widgets, so it must be unique among the pickers
    /// on screen (two with the same id share their open page); `value` is what they edit (`None`:
    /// no date yet, or one that couldn't be read).
    pub fn new(id: impl Into<String>, value: &'a mut Option<PickedDate>) -> Self {
        DatePicker { id: id.into(), value, precisions: &Precision::ALL, min: None, max: None, today: None }
    }

    /// The precisions offered (all three by default), in the order shown. Without a value at one
    /// of them, the picker opens on days when offered, else on the first.
    pub fn precisions(mut self, precisions: &'a [Precision]) -> Self {
        if !precisions.is_empty() {
            self.precisions = precisions;
        }
        self
    }

    /// The earliest date that can be picked.
    pub fn min(mut self, min: Option<PickedDate>) -> Self {
        self.min = min;
        self
    }

    /// The latest date that can be picked.
    pub fn max(mut self, max: Option<PickedDate>) -> Self {
        self.max = max;
        self
    }

    /// Where the calendar opens when there is no value, and the day it marks.
    pub fn today(mut self, today: Option<PickedDate>) -> Self {
        self.today = today;
        self
    }

    /// The button and, while open, the popup. The response is `changed()` when a date was picked.
    pub fn show(self, ui: &mut Ui) -> Response {
        let DatePicker { id, value, precisions, min, max, today } = self;
        let mut button = crate::widgets::icon_button(ui, &format!("datePicker:{id}"), Icon::Calendar, vec2(24.0, 22.0), false, true, "Pick a date");
        if !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx()) {
            crate::widgets::register_response(format!("datePicker:{id}"), button.rect, &button.response);
        }
        let env = craft::ui::runtime::env::Env::of(ui);
        let enabled = ui.is_enabled() && env.enabled;
        let read_only = env.read_only;
        let state = egui::Id::unique(("datePicker", id.as_str()));
        let open_state = state.with("open");
        let focus_state = state.with("focus-on-open");
        let mut open = ui.data(|d| d.get_temp::<bool>(open_state)).unwrap_or(false);
        if button.clicked() {
            open = !open;
            if open {
                // open on the value (or today), at its precision when it is one of those offered
                let at = value.or(today).unwrap_or(PickedDate::year(2026));
                let fallback =
                    if precisions.contains(&Precision::Day) { Precision::Day } else { precisions.first().copied().unwrap_or(Precision::Day) };
                let precision = value.map(|v| v.precision()).filter(|p| precisions.contains(p)).unwrap_or(fallback);
                ui.data_mut(|d| {
                    d.insert_temp(state, View::open(at, precision));
                    d.insert_temp(focus_state, true);
                });
            }
        }
        let mut focus_on_open = ui.data(|d| d.get_temp::<bool>(focus_state)).unwrap_or(false);
        let mut picked: Option<PickedDate> = None;
        let mut view = ui.data(|d| d.get_temp::<View>(state)).unwrap_or(View { precision: Precision::Day, year: 2026, month: 1 });
        // The framework popover hangs from the button, which opens and closes it by its own activation (a press, a long
        // press or a key); Escape, a press outside and the focus leaving close it, and the press outside reaches what it
        // landed on. Its focus goes to the selected (or today's) cell below, not to the first stop.
        Popover::new(crate::i18n::tr("Pick a date").to_owned(), Bind::bound(&mut open))
            .id(state.with("popover"))
            .opened_by(&button.response)
            // A picker that goes disabled or read-only while its calendar is open keeps the calendar open, its controls
            // inert (below): the opener going disabled is no anchor gone.
            .overlay(|overlay| {
                let mut dismiss = overlay.dismiss;
                dismiss.anchor_gone = false;
                overlay.dismiss(dismiss)
            })
            .initial_focus(Initial::Keep)
            .show(ui, |ui, cx| {
                craft::ui::runtime::env::with_env(
                    ui,
                    |set| {
                        // the popover is a layer of its own, not a child of the opener's disabled/read-only scope.
                        set.enabled = Some(enabled);
                        set.read_only = Some(read_only);
                    },
                    |ui| {
                        let real = !ui.is_sizing_pass() && !craft::ui::runtime::measure::active(ui.ctx());
                        // a disabled or read-only picker pages and switches nothing either: say so on each control, which
                        // would otherwise take its own `true` over the popover's scope
                        let live = enabled && !read_only;
                        // as wide as a month of days, however much room the window has
                        ui.set_min_width(220.0);
                        ui.set_max_width(240.0);
                        if precisions.len() > 1 {
                            ui.horizontal(|ui| {
                                for p in precisions {
                                    let label = crate::i18n::tr(p.label());
                                    let r = option(
                                        ui,
                                        format!("datePickerPrecision:{}:{id}", p.id()),
                                        RichText::new(label),
                                        label.to_owned(),
                                        view.precision == *p,
                                        live,
                                        0.0,
                                    );
                                    if clicked(&r) {
                                        view.precision = *p;
                                    }
                                }
                            });
                            ui.separator();
                        }
                        ui.horizontal(|ui| {
                            let prev = crate::widgets::icon_button(
                                ui,
                                &format!("datePickerPrev:{id}"),
                                Icon::ChevronLeft,
                                vec2(32.0, 22.0),
                                false,
                                live,
                                "Previous page",
                            );
                            if real {
                                crate::widgets::register_response(format!("datePickerPrev:{id}"), prev.rect, &prev.response);
                            }
                            let title = match view.precision {
                                Precision::Day => crate::i18n::date_group_label(&format!("{:04}-{:02}", view.year, view.month), false),
                                Precision::Month => format!("{:04}", view.year),
                                Precision::Year => {
                                    let first = view.year.div_euclid(12) * 12;
                                    format!("{first:04}–{:04}", first + 11)
                                }
                            };
                            ui.label(RichText::new(title).strong());
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let next = crate::widgets::icon_button(
                                    ui,
                                    &format!("datePickerNext:{id}"),
                                    Icon::ChevronRight,
                                    vec2(32.0, 22.0),
                                    false,
                                    live,
                                    "Next page",
                                );
                                if real {
                                    crate::widgets::register_response(format!("datePickerNext:{id}"), next.rect, &next.response);
                                }
                                if next.clicked() {
                                    view = step(view, true);
                                }
                                if prev.clicked() {
                                    view = step(view, false);
                                }
                            });
                        });
                        let mut first_enabled = None;
                        let mut preferred = None;
                        let mut cell = |ui: &mut Ui, date: PickedDate, text: String, picked: &mut Option<PickedDate>| {
                            let allowed = in_bounds(&date, min.as_ref(), max.as_ref());
                            let text = if holds_today(&date, today) { RichText::new(text).strong().underline() } else { RichText::new(text) };
                            let r = option(ui, format!("datePickerCell:{}:{id}", date.iso()), text, date.iso(), *value == Some(date), allowed, 26.0);
                            // A root SDK Response retains the host Ui's enabled flag; bounds
                            // are a component prop. Use that real prop when choosing the opening cell.
                            if allowed && enabled {
                                first_enabled.get_or_insert(r.id);
                                if *value == Some(date) || (value.is_none() && holds_today(&date, today)) {
                                    preferred = Some(r.id);
                                }
                            }
                            if clicked(&r) {
                                *picked = Some(date);
                            }
                        };
                        egui::Grid::new(("datePickerGrid", id.as_str())).spacing(vec2(2.0, 2.0)).show(ui, |ui| match view.precision {
                            Precision::Day => {
                                for name in WEEKDAY_SHORT {
                                    ui.label(RichText::new(crate::i18n::tr(name)).weak());
                                }
                                ui.end_row();
                                for week in month_grid(view.year, view.month).chunks(7) {
                                    for d in week {
                                        match d {
                                            Some(d) => cell(
                                                ui,
                                                PickedDate { year: view.year, month: Some(view.month), day: Some(*d) },
                                                d.to_string(),
                                                &mut picked,
                                            ),
                                            None => {
                                                ui.label("");
                                            }
                                        }
                                    }
                                    ui.end_row();
                                }
                            }
                            Precision::Month => {
                                for row in 0..3u8 {
                                    for col in 1..=4u8 {
                                        let m = row * 4 + col;
                                        let date = PickedDate { year: view.year, month: Some(m), day: None };
                                        cell(ui, date, crate::i18n::date_group_label(&date.iso(), true), &mut picked);
                                    }
                                    ui.end_row();
                                }
                            }
                            Precision::Year => {
                                let first = view.year.div_euclid(12) * 12;
                                for row in 0..3 {
                                    for col in 0..4 {
                                        let y = first + row * 4 + col;
                                        if (0..=9999).contains(&y) {
                                            cell(ui, PickedDate::year(y), format!("{y:04}"), &mut picked);
                                        }
                                    }
                                    ui.end_row();
                                }
                            }
                        });
                        // Open on the selected/today cell, or the first permitted one. Do this only in
                        // a real pass: a new popover can first run an invisible sizing pass.
                        if focus_on_open && real {
                            if let Some(target) = preferred.or(first_enabled) {
                                craft::ui::runtime::focus::request(ui.ctx(), craft::ui::runtime::focus::FocusTarget::Id(target));
                            }
                            focus_on_open = false;
                        }
                        // Esc closes the calendar and stops there: the dialog around it keeps its edits.
                        // Restore the opener on selection/cancel, not on outside clicks (which can focus
                        // another field in the caller).
                        if real && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                            crate::widgets::take_escape(ui.ctx());
                            craft::ui::runtime::focus::request(ui.ctx(), craft::ui::runtime::focus::FocusTarget::Id(button.id));
                            cx.request_close();
                        }
                        if real && picked.is_some() {
                            craft::ui::runtime::focus::request(ui.ctx(), craft::ui::runtime::focus::FocusTarget::Id(button.id));
                            cx.request_close();
                        }
                    },
                )
            });
        ui.data_mut(|d| {
            d.insert_temp(state, view);
            d.insert_temp(open_state, open);
            d.insert_temp(focus_state, focus_on_open);
        });
        if let Some(date) = picked {
            *value = Some(date);
            button.response.mark_changed();
        }
        button.response
    }
}

#[cfg(test)]
#[path = "date_picker/interaction_tests.rs"]
mod interaction_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_dates_read_and_write_iso() {
        let d = |s: &str| PickedDate::parse(s);
        assert_eq!(d("2026"), Some(PickedDate { year: 2026, month: None, day: None }));
        assert_eq!(d("2026-08"), Some(PickedDate { year: 2026, month: Some(8), day: None }));
        assert_eq!(d("2026-08-14"), Some(PickedDate { year: 2026, month: Some(8), day: Some(14) }));
        assert_eq!(d("2026-08-14T10:00").map(|p| p.iso()), Some("2026-08-14".to_string()), "a time is read past");
        for bad in ["", "26", "2026-13", "2026-02-30", "banana", "2026/08"] {
            assert_eq!(d(bad), None, "{bad}");
        }
        for s in ["2026", "2026-08", "2026-08-14", "0001-01-01", "9999-12-31"] {
            assert_eq!(d(s).map(|p| p.iso()).as_deref(), Some(s));
        }
        assert_eq!(d("2026-08").map(|p| p.precision()), Some(Precision::Month));
        // the first and last day a value covers, for bounds
        assert_eq!(d("2026-02").map(|p| (p.first_day(), p.last_day())), Some(((2026, 2, 1), (2026, 2, 28))));
        assert_eq!(d("2028").map(|p| (p.first_day(), p.last_day())), Some(((2028, 1, 1), (2028, 12, 31))));
    }

    #[test]
    fn month_lengths_and_grids() {
        assert_eq!((days_in_month(2026, 2), days_in_month(2028, 2), days_in_month(1900, 2), days_in_month(2000, 2)), (28, 29, 28, 29));
        assert_eq!((days_in_month(2026, 4), days_in_month(2026, 12), days_in_month(2026, 13)), (30, 31, 0));
        // August 2026 starts on a Saturday: five blanks in a Monday-first week, then 1..=31
        let g = month_grid(2026, 8);
        assert_eq!(g.len() % 7, 0);
        assert_eq!(&g[..6], &[None, None, None, None, None, Some(1)]);
        assert_eq!(g.iter().flatten().count(), 31);
        assert_eq!(month_grid(2026, 6).first(), Some(&Some(1)), "June 2026 starts on a Monday");
    }

    /// Paging stops at years 0000 and 9999 instead of wrapping (‹ on January 0000 stays there),
    /// and a view opened on a year beyond them starts at the nearest end.
    #[test]
    fn paging_stops_at_the_ends() {
        let v = |precision, year, month| View { precision, year, month };
        let at = |v: View| (v.year, v.month);
        assert_eq!(at(step(v(Precision::Day, 2026, 12), true)), (2027, 1));
        assert_eq!(at(step(v(Precision::Day, 2026, 1), false)), (2025, 12));
        assert_eq!(at(step(v(Precision::Day, 0, 1), false)), (0, 1));
        assert_eq!(at(step(v(Precision::Day, 9999, 12), true)), (9999, 12));
        assert_eq!(at(step(v(Precision::Month, 0, 5), false)), (0, 5));
        assert_eq!(at(step(v(Precision::Month, 9999, 5), true)), (9999, 5));
        assert_eq!(at(step(v(Precision::Year, 5, 1), false)), (5, 1), "the first page of years");
        assert_eq!(at(step(v(Precision::Year, 9990, 1), true)), (9996, 1), "the page holding 9996–9999");
        assert_eq!(at(step(v(Precision::Year, 9996, 1), true)), (9996, 1), "the last page");
        assert_eq!(at(step(v(Precision::Year, 2026, 1), false)), (2004, 1), "the page 2004–2015");
        let huge = PickedDate { year: i32::MAX, month: Some(13), day: None };
        assert_eq!(at(View::open(huge, Precision::Day)), (9999, 12));
        assert_eq!(at(View::open(PickedDate { year: i32::MIN, month: Some(0), day: None }, Precision::Day)), (0, 1));
    }

    /// The picker reads a date the way the rule check does: a time after it must be a time.
    #[test]
    fn a_trailing_time_must_be_a_time() {
        for ok in ["2026-08-14T10", "2026-08-14T10:00", "2026-08-14 10:00:00"] {
            assert!(PickedDate::parse(ok).is_some(), "{ok}");
        }
        for bad in ["2026-08-14Tjunk", "2026-08-14T", "2026-08-14T1", "2026-08-14T10:0", "2026-08-14T10:00Z"] {
            assert_eq!(PickedDate::parse(bad), None, "{bad}");
        }
    }

    /// Today is marked in every view: its day, its month, its year.
    #[test]
    fn today_is_marked_at_every_precision() {
        let d = |s: &str| PickedDate::parse(s).unwrap();
        let today = Some(d("2026-10-09"));
        assert!(holds_today(&d("2026-10-09"), today) && holds_today(&d("2026-10"), today) && holds_today(&d("2026"), today));
        assert!(!holds_today(&d("2026-10-08"), today) && !holds_today(&d("2026-09"), today) && !holds_today(&d("2025"), today));
        assert!(!holds_today(&d("2026"), None));
    }

    #[test]
    fn bounds_keep_whole_values_in_range() {
        let d = |s: &str| PickedDate::parse(s).unwrap();
        let (min, max) = (Some(d("2026-03")), Some(d("2026-06-15")));
        let ok = |s: &str| in_bounds(&d(s), min.as_ref(), max.as_ref());
        assert!(ok("2026-03-01") && ok("2026-04") && ok("2026-06-15"));
        assert!(!ok("2026-02-28") && !ok("2026-06-16") && !ok("2026-07"));
        assert!(ok("2026-06"), "a month that reaches the bound counts");
        assert!(ok("2026") && !ok("2027") && !ok("2025"));
        assert!(in_bounds(&d("1999"), None, None));
    }
}
