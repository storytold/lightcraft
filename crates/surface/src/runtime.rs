//! The live engine: input events in, [`Effect`]s out.
//!
//! Portions © RAD Films - Roman Alurkoff (from TactileBridge), MIT: dial focus by key (the
//! "adjustment dial toggle"), tool-following pages, the coalescing flush, echo suppression and
//! Auto Sync's idle timeout come from TactileBridge (`ToolContextFolder`, `CaptureOneAdjustment`,
//! `TipcBridgeService`).
//!
//! Time is passed in (milliseconds on any monotonic clock), so everything here is deterministic
//! and testable. The host:
//! 1. calls [`Runtime::set_context`] when what the app shows changes;
//! 2. feeds surface input to [`Runtime::input`];
//! 3. calls [`Runtime::flush`] every frame (and [`Runtime::interrupt`] when the active photo
//!    changes or the app takes over the edit);
//! 4. runs the effects: `Begin`/`End` bracket one undoable gesture, `Adjust` moves a value by a
//!    delta, `Command` runs a command by id.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::binding::{Binding, Modifier};
use crate::devices::{PageGroup, group_of, is_encoder};
use crate::motion::{Accelerator, delta, modifier_factor};
use crate::profile::{Page, Profile};
use crate::settings::SurfaceSettings;
use crate::targets::target_spec;

/// Input from a surface.
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    /// An encoder turned by `ticks` detents (positive = clockwise).
    Turn { control: String, ticks: i32 },
    /// A key or button went down. `page` (1-based) is the key page the device itself shows, for
    /// devices that page their keys in hardware (the MX Creative Keypad's page buttons).
    Press { control: String, page: Option<usize> },
    /// A key or button came up.
    Release { control: String },
}

/// What the host should do.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// Run a command by id (engine or UI command).
    Command { command: String, params: Value },
    /// A dial gesture starts on `target`: open one undo step (develop and mask values).
    Begin { target: String },
    /// Move `target` by `delta` (in the target's units, already clamped to its range span).
    Adjust { target: String, delta: f64 },
    /// The gesture on `target` ends: commit its undo step; with `apply_to_selected`, the change
    /// goes to every selected photo in that same step.
    End { target: String, apply_to_selected: bool },
    /// Put `target` back to its default (its own undo step).
    Reset { target: String },
    /// The surface's own state changed (page, profile, dial focus, a toggle): push the layout
    /// again; `notice` is an English line for the on-screen readout.
    Changed { notice: Option<String> },
    /// Settings changed from a key (profile, toggles): save them.
    Save,
}

/// What the on-screen readout shows.
#[derive(Clone, Debug, PartialEq)]
pub enum Shown {
    /// A value being turned (the host reads its current value).
    Value { target: String },
    /// A command that ran: English label and icon.
    Command { label: String, icon: Option<&'static str> },
    /// A surface notice (page or profile change, a toggle): English text.
    Notice { text: String },
}

#[derive(Clone, Debug)]
struct Gesture {
    target: String,
    last_input_ms: f64,
    /// The way the dial last turned (the sign of its ticks).
    direction: i32,
}

/// Learn mode: the next control touched is reported instead of acting.
#[derive(Clone, Debug, Default, PartialEq)]
enum Learn {
    #[default]
    Off,
    Armed,
    Got(String),
}

/// Commands a key runs only while it is held (pressed: `{on: true}`, released: `{on: false}`):
/// the brush's Erase works like holding Alt.
pub const MOMENTARY: &[&str] = &["brush.erase"];

/// Commands that destroy or discard something: a console key runs them only after being held
/// for [`HOLD_MS`] (a quick press shows "Hold to …" instead), so a stray press never deletes.
pub const HOLD_TO_RUN: &[&str] =
    &["photo.delete", "photo.deletePermanently", "history.clear", "mask.deleteAll", "develop.reset", "version.delete", "preset.delete"];

/// How long a destructive key must be held (ms).
pub const HOLD_MS: f64 = 700.0;

pub struct Runtime {
    settings: SurfaceSettings,
    /// Momentary commands running while their key is held: (control, command, params).
    momentary: Vec<(String, String, Value)>,
    /// A destructive key waiting to be held long enough: (control, binding, pressed at ms, label).
    armed: Option<(String, Binding, f64)>,
    context: String,
    /// The current page of each group.
    pages: BTreeMap<PageGroup, String>,
    /// The pages the context asks for itself (not inherited from `default`).
    context_pages: BTreeMap<PageGroup, String>,
    /// The pages last chosen by hand (or the profile's defaults): shown again when a context
    /// that asks for nothing special comes back.
    manual: BTreeMap<PageGroup, String>,
    /// The value the dials move instead of their own (a key bound to a value was pressed).
    focus: Option<String>,
    held: Vec<(String, Modifier)>,
    accel: Accelerator,
    pending: BTreeMap<String, f64>,
    gesture: Option<Gesture>,
    last_flush_ms: f64,
    learn: Learn,
    shown: Option<(Shown, f64)>,
    revision: u64,
}

impl Runtime {
    pub fn new(settings: SurfaceSettings) -> Runtime {
        let mut r = Runtime {
            settings,
            momentary: Vec::new(),
            armed: None,
            context: "default".into(),
            pages: BTreeMap::new(),
            context_pages: BTreeMap::new(),
            manual: BTreeMap::new(),
            focus: None,
            held: Vec::new(),
            accel: Accelerator::default(),
            pending: BTreeMap::new(),
            gesture: None,
            last_flush_ms: f64::NEG_INFINITY,
            learn: Learn::Off,
            shown: None,
            revision: 0,
        };
        r.settings.sanitize();
        r.reset_pages();
        r
    }

    pub fn settings(&self) -> &SurfaceSettings {
        &self.settings
    }

    /// Change settings (the editor), then [`Runtime::settings_changed`].
    pub fn settings_mut(&mut self) -> &mut SurfaceSettings {
        &mut self.settings
    }

    /// Settings were edited: clamp them, keep pages that still exist.
    pub fn settings_changed(&mut self) {
        self.settings.sanitize();
        let valid = |p: &Profile, g: PageGroup, n: &String| p.page(n).is_some_and(|pg| pg.in_group(g));
        if let Some(p) = self.settings.active_profile().cloned() {
            self.manual.retain(|g, n| valid(&p, *g, n));
            self.pages.retain(|g, n| valid(&p, *g, n));
        }
        let ctx = std::mem::take(&mut self.context);
        self.apply_context(&ctx);
        self.revision += 1;
    }

    /// Replace all settings (a file was imported or reloaded).
    pub fn replace_settings(&mut self, settings: SurfaceSettings) {
        self.settings = settings;
        self.settings.sanitize();
        self.reset_pages();
        self.revision += 1;
    }

    /// Bumped whenever the layout may have changed (pages, focus, profile, settings).
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn profile(&self) -> Option<&Profile> {
        self.settings.active_profile()
    }

    pub fn context(&self) -> &str {
        &self.context
    }

    /// The current page of each group.
    pub fn pages(&self) -> &BTreeMap<PageGroup, String> {
        &self.pages
    }

    /// The value the dials move instead of their own, if a key put one there.
    pub fn focus(&self) -> Option<&str> {
        self.focus.as_deref()
    }

    /// Modifiers held right now.
    pub fn held(&self) -> Vec<Modifier> {
        self.held.iter().map(|(_, m)| *m).collect()
    }

    /// The value being turned, if a gesture is in progress.
    pub fn turning(&self) -> Option<&str> {
        self.gesture.as_ref().map(|g| g.target.as_str())
    }

    /// While turning, values for `target` aren't sent back until every tick so far has been
    /// applied (TactileBridge's echo suppression; the readout never jumps back).
    pub fn holds_feedback(&self, target: &str) -> bool {
        self.settings.transport.echo_suppression && self.pending.get(target).is_some_and(|d| *d != 0.0)
    }

    /// What the on-screen readout shows at `now_ms`, and since when (it fades after the
    /// display time).
    pub fn shown(&self, now_ms: f64) -> Option<(&Shown, f64)> {
        let (s, at) = self.shown.as_ref()?;
        let age = now_ms - at;
        (self.settings.osd.enabled && age.is_finite() && age < f64::from(self.settings.osd.display_ms) + 400.0).then_some((s, *at))
    }

    fn show(&mut self, s: Shown, now_ms: f64) {
        let commands_ok = self.settings.osd.show_commands || matches!(s, Shown::Value { .. });
        if commands_ok {
            self.shown = Some((s, now_ms));
        }
    }

    // ------------------------------------------------------------------------------- learning

    /// The next control touched is reported by [`Runtime::take_learned`] instead of acting.
    pub fn arm_learn(&mut self) {
        self.learn = Learn::Armed;
    }

    pub fn cancel_learn(&mut self) {
        self.learn = Learn::Off;
    }

    pub fn learning(&self) -> bool {
        self.learn == Learn::Armed
    }

    /// The control learnt, once.
    pub fn take_learned(&mut self) -> Option<String> {
        match std::mem::take(&mut self.learn) {
            Learn::Got(c) => Some(c),
            other => {
                self.learn = other;
                None
            }
        }
    }

    // ------------------------------------------------------------------------------- pages

    fn reset_pages(&mut self) {
        self.manual = self.profile().map(|p| p.pages_for_context("default")).unwrap_or_default();
        self.pages.clear();
        self.focus = None;
        let ctx = std::mem::take(&mut self.context);
        self.apply_context(if ctx.is_empty() { "default" } else { &ctx });
    }

    fn apply_context(&mut self, key: &str) {
        self.context = key.to_string();
        let Some(p) = self.profile().cloned() else {
            self.pages.clear();
            self.context_pages.clear();
            return;
        };
        self.context_pages = p.context_pages(key, false);
        let defaults = p.pages_for_context("default");
        for g in PageGroup::ALL {
            let follow = self.settings.follow_context.then(|| self.context_pages.get(&g)).flatten();
            let keep = (!self.settings.follow_context).then(|| self.pages.get(&g)).flatten();
            let name =
                follow.or(keep).or(self.manual.get(&g)).or(defaults.get(&g)).cloned().or_else(|| p.pages_in(g).next().map(|pg| pg.name.clone()));
            match name {
                Some(n) => {
                    self.pages.insert(g, n);
                }
                None => {
                    self.pages.remove(&g);
                }
            }
        }
    }

    /// What the app shows changed (`crate::context::context_key`). Pages follow when Follow
    /// Context is on; the dial focus is dropped either way (as TactileBridge did on a tool
    /// change). Returns whether anything changed.
    pub fn set_context(&mut self, key: &str) -> bool {
        if key == self.context {
            return false;
        }
        let before = (self.pages.clone(), self.focus.clone());
        self.focus = None;
        self.apply_context(key);
        let changed = before != (self.pages.clone(), self.focus.clone());
        if changed {
            self.revision += 1;
        }
        changed
    }

    fn page_step(&mut self, group: Option<PageGroup>, step: i32, now_ms: f64) -> Vec<Effect> {
        let Some(p) = self.profile().cloned() else { return Vec::new() };
        let groups: Vec<PageGroup> = group.map_or(PageGroup::ALL.to_vec(), |g| vec![g]);
        let mut names = Vec::new();
        for g in groups {
            if let Some(next) = p.step_page(g, self.pages.get(&g).map(String::as_str), step) {
                self.pages.insert(g, next.clone());
                self.manual.insert(g, next.clone());
                names.push(next);
            }
        }
        if names.is_empty() {
            return Vec::new();
        }
        names.dedup();
        self.focus = None;
        self.revision += 1;
        let text = names.join(" · ");
        self.show(Shown::Notice { text: text.clone() }, now_ms);
        vec![Effect::Changed { notice: Some(text) }]
    }

    /// Show page `name` on the surface without a notice (the editor in Settings ▸ Controls shows
    /// the device the page being edited). False when it's already shown or doesn't exist.
    pub fn show_page(&mut self, name: &str) -> bool {
        let Some(page) = self.profile().and_then(|p| p.page(name)).cloned() else { return false };
        let mut changed = false;
        for g in PageGroup::ALL {
            if page.in_group(g) && self.pages.get(&g) != Some(&page.name) {
                self.pages.insert(g, page.name.clone());
                self.manual.insert(g, page.name.clone());
                changed = true;
            }
        }
        if changed {
            self.revision += 1;
        }
        changed
    }

    fn goto_page(&mut self, name: &str, now_ms: f64) -> Vec<Effect> {
        let Some(page) = self.profile().and_then(|p| p.page(name)).cloned() else { return Vec::new() };
        for g in PageGroup::ALL {
            if page.in_group(g) {
                self.pages.insert(g, page.name.clone());
                self.manual.insert(g, page.name.clone());
            }
        }
        self.focus = None;
        self.revision += 1;
        self.show(Shown::Notice { text: page.name.clone() }, now_ms);
        vec![Effect::Changed { notice: Some(page.name) }]
    }

    fn switch_profile(&mut self, name: Option<&str>, now_ms: f64) -> Vec<Effect> {
        let names: Vec<String> = self.settings.profiles.iter().map(|p| p.name.clone()).collect();
        let target = match name {
            Some(n) => names.iter().find(|p| *p == n).cloned(),
            None => {
                let i = names.iter().position(|p| *p == self.settings.profile).map_or(0, |i| i + 1);
                names.get(i % names.len().max(1)).cloned()
            }
        };
        let Some(target) = target else { return Vec::new() };
        let mut out = self.interrupt();
        self.settings.profile = target.clone();
        self.reset_pages();
        self.revision += 1;
        self.show(Shown::Notice { text: target.clone() }, now_ms);
        out.push(Effect::Save);
        out.push(Effect::Changed { notice: Some(target) });
        out
    }

    // ------------------------------------------------------------------------------- bindings

    /// The page the device shows as its key page `n` (1-based): the context's own key page while
    /// one is active, else the profile's `n`-th key page.
    pub fn device_key_page(&self, n: usize) -> Option<&Page> {
        let p = self.profile()?;
        if self.settings.follow_context
            && let Some(name) = self.context_pages.get(&PageGroup::Keys)
        {
            return p.page(name);
        }
        p.pages_in(PageGroup::Keys).nth(n.checked_sub(1)?)
    }

    /// How many key pages the profile has.
    pub fn key_page_count(&self) -> usize {
        self.profile().map_or(0, |p| p.pages_in(PageGroup::Keys).count())
    }

    /// The binding of `control` now (`page`: the device's own key page, see [`Input::Press`]).
    /// Encoders bound to nothing, or a value, follow the dial focus.
    pub fn binding(&self, control: &str, page: Option<usize>) -> Option<Binding> {
        let p = self.profile()?;
        if is_encoder(control)
            && let Some(f) = &self.focus
        {
            let own = p.binding(control, &self.pages).cloned().or_else(|| crate::loupedeck::default_binding(control));
            // a dial focus moves the dial (the first encoder) only; the others keep their values
            if control == "dial" || own.is_none() {
                return Some(Binding::value(f));
            }
        }
        if let (Some(n), PageGroup::Keys) = (page, group_of(control)) {
            return self.device_key_page(n).and_then(|pg| pg.bindings.get(control)).or_else(|| p.base.get(control)).cloned();
        }
        // a Loupedeck+ control nothing binds does what its label says
        p.binding(control, &self.pages).cloned().or_else(|| crate::loupedeck::default_binding(control))
    }

    // ------------------------------------------------------------------------------- input

    /// Handle one input event at `now_ms`.
    pub fn input(&mut self, input: Input, now_ms: f64) -> Vec<Effect> {
        if !self.settings.enabled {
            return Vec::new();
        }
        // learning, or learnt and not yet taken: input is captured, never applied (the host takes
        // the control at its next frame; the last one touched wins)
        if self.learn != Learn::Off {
            let control = match &input {
                Input::Turn { control, .. } | Input::Press { control, .. } => Some(control.clone()),
                Input::Release { .. } => None,
            };
            if let Some(c) = control {
                self.learn = Learn::Got(c);
                self.revision += 1;
                return vec![Effect::Changed { notice: None }];
            }
            return Vec::new();
        }
        match input {
            Input::Turn { control, ticks } => self.turn(&control, ticks, now_ms),
            Input::Press { control, page } => self.press(&control, page, now_ms),
            Input::Release { control } => {
                let mut out = Vec::new();
                // a destructive key let go too soon: say how to use it, run nothing
                if let Some((c, b, start)) = self.armed.take() {
                    if c == control {
                        let text = format!("Hold the key: {}", b.describe());
                        self.show(Shown::Notice { text: text.clone() }, now_ms);
                        out.push(Effect::Changed { notice: Some(text) });
                    } else {
                        self.armed = Some((c, b, start));
                    }
                }
                // a momentary command ends with its key
                if let Some(i) = self.momentary.iter().position(|(c, _, _)| *c == control) {
                    let (_, command, params) = self.momentary.remove(i);
                    out.push(Effect::Command { command, params: with_on(&params, false) });
                    self.revision += 1;
                    out.push(Effect::Changed { notice: None });
                }
                let before = self.held.len();
                self.held.retain(|(c, _)| *c != control);
                if self.held.len() != before {
                    self.revision += 1;
                    out.push(Effect::Changed { notice: None });
                }
                out
            }
        }
    }

    /// An action a surface's own app put on a control (a plugin's action list: see
    /// [`crate::catalog`]): turning a value moves it, pressing a value resets it, pressing anything
    /// else does what a key bound to it does.
    pub fn act(&mut self, b: &Binding, ticks: Option<i32>, now_ms: f64) -> Vec<Effect> {
        if !self.settings.enabled {
            return Vec::new();
        }
        match (b, ticks) {
            (Binding::Value { target }, Some(t)) if t != 0 => self.turn_value(target, t, now_ms),
            (Binding::Value { target }, None) => self.reset(target, now_ms),
            (_, None) => self.perform("action", b, now_ms),
            // other actions act once per turn event, clockwise only
            (_, Some(t)) if t > 0 => self.perform("action", b, now_ms),
            _ => Vec::new(),
        }
    }

    fn turn(&mut self, control: &str, ticks: i32, now_ms: f64) -> Vec<Effect> {
        if ticks == 0 {
            return Vec::new();
        }
        match self.binding(control, None) {
            Some(Binding::Value { target }) => self.turn_value(&target, ticks, now_ms),
            Some(Binding::PageNext { group }) => self.page_step(group, ticks.signum(), now_ms),
            Some(Binding::PagePrevious { group }) => self.page_step(group, -ticks.signum(), now_ms),
            // other bindings act once per turn event, clockwise only
            Some(b) if ticks > 0 => self.perform(control, &b, now_ms),
            _ => Vec::new(),
        }
    }

    fn turn_value(&mut self, target: &str, ticks: i32, now_ms: f64) -> Vec<Effect> {
        let Some(spec) = target_spec(target) else { return Vec::new() };
        let mut out = Vec::new();
        if self.gesture.as_ref().is_some_and(|g| g.target != target) {
            out.extend(self.finish_gesture());
        }
        if self.gesture.is_none() {
            out.push(Effect::Begin { target: target.to_string() });
        }
        // The dial turned back (TactileBridge's `LastInputDirection`): the new way is followed at
        // once. What the old way gathered but hasn't applied yet is dropped, and the speed starts
        // again from the floor, so a fast spin turned back doesn't carry on (or overshoot) first.
        let direction = ticks.signum();
        let reversed = self.gesture.as_ref().is_some_and(|g| g.direction != 0 && g.direction != direction);
        if reversed {
            self.accel.reset(target);
            if let Some(p) = self.pending.get_mut(target) {
                *p = 0.0;
            }
        }
        self.gesture = Some(Gesture { target: target.to_string(), last_input_ms: now_ms, direction });
        let tuning = self.settings.tuning_of(target);
        let motion = self.settings.motion.clone();
        let acc = self.accel.acceleration(target, now_ms, &tuning, &motion);
        let held = self.held();
        let d = delta(ticks, spec.step, acc, modifier_factor(&held, &motion), &tuning, &motion, spec.max_delta());
        let lim = spec.max_delta();
        let p = self.pending.entry(target.to_string()).or_insert(0.0);
        *p = (*p + d).clamp(-lim, lim);
        self.show(Shown::Value { target: target.to_string() }, now_ms);
        out
    }

    fn press(&mut self, control: &str, page: Option<usize>, now_ms: f64) -> Vec<Effect> {
        if is_encoder(control) {
            // a dial press: its own binding (`dial.press`), else reset what the dial moves
            let own = format!("{control}.press");
            // (its own binding only: the dial focus moves the dial, it doesn't bind its press)
            let bound = self.profile().and_then(|p| p.binding(&own, &self.pages).cloned()).or_else(|| crate::loupedeck::default_binding(&own));
            if let Some(b) = bound {
                return self.perform(&own, &b, now_ms);
            }
            return match self.binding(control, None) {
                Some(Binding::Value { target }) => self.reset(&target, now_ms),
                _ => Vec::new(),
            };
        }
        match self.binding(control, page) {
            Some(b) => self.perform(control, &b, now_ms),
            None => Vec::new(),
        }
    }

    fn reset(&mut self, target: &str, now_ms: f64) -> Vec<Effect> {
        let mut out = self.interrupt();
        if target_spec(target).is_some() {
            self.show(Shown::Value { target: target.to_string() }, now_ms);
            out.push(Effect::Reset { target: target.to_string() });
        }
        out
    }

    /// Do what `b` does, as if a key bound to it (`control`) were pressed (commands that page,
    /// switch profiles or toggle without a key).
    pub fn perform(&mut self, control: &str, b: &Binding, now_ms: f64) -> Vec<Effect> {
        match b {
            Binding::Value { target } => {
                // the adjustment dial toggle: press to put this value on the dial, again to give
                // the dial back
                self.focus = if self.focus.as_deref() == Some(target) { None } else { Some(target.clone()) };
                self.revision += 1;
                let text = match &self.focus {
                    Some(t) => format!("Dial: {}", target_spec(t).map_or(t.as_str(), |s| s.label)),
                    None => "Dial: page".to_string(),
                };
                // no readout: putting a value on the dial changes nothing in the photo (the lit key
                // says it); the readout appears once the dial turns
                let _ = now_ms;
                vec![Effect::Changed { notice: Some(text) }]
            }
            Binding::Command { command, params } if MOMENTARY.contains(&command.as_str()) => {
                // held: on now, off at the release (no readout: the key itself lights up)
                let mut out = self.interrupt();
                self.momentary.retain(|(c, _, _)| c != control);
                self.momentary.push((control.to_string(), command.clone(), params.clone()));
                self.revision += 1;
                out.push(Effect::Command { command: command.clone(), params: with_on(params, true) });
                out.push(Effect::Changed { notice: None });
                out
            }
            Binding::Command { command, .. } if HOLD_TO_RUN.contains(&command.as_str()) && control != "command" => {
                // destructive: armed now, run by flush once held long enough
                self.armed = Some((control.to_string(), b.clone(), now_ms));
                Vec::new()
            }
            Binding::Command { command, params } => {
                self.show(Shown::Command { label: b.describe(), icon: crate::icons::icon_for(b) }, now_ms);
                // a command may change the photo: what the dial did so far goes to this one first
                let mut out = self.interrupt();
                out.push(Effect::Command { command: command.clone(), params: params.clone() });
                out
            }
            Binding::Modifier { modifier } => {
                if !self.held.iter().any(|(c, m)| c == control && m == modifier) {
                    self.held.push((control.to_string(), *modifier));
                }
                self.revision += 1;
                vec![Effect::Changed { notice: None }]
            }
            Binding::PageNext { group } => self.page_step(*group, 1, now_ms),
            Binding::PagePrevious { group } => self.page_step(*group, -1, now_ms),
            Binding::Page { page } => self.goto_page(page, now_ms),
            Binding::Profile { profile } => self.switch_profile(Some(profile), now_ms),
            Binding::ProfileNext => self.switch_profile(None, now_ms),
            Binding::Reset => match self.binding("dial", None) {
                Some(Binding::Value { target }) => self.reset(&target, now_ms),
                _ => Vec::new(),
            },
            Binding::ApplyToSelected => {
                self.settings.apply_to_selected = !self.settings.apply_to_selected;
                self.revision += 1;
                let text = format!("Apply to all selected: {}", if self.settings.apply_to_selected { "On" } else { "Off" });
                self.show(Shown::Notice { text: text.clone() }, now_ms);
                vec![Effect::Save, Effect::Changed { notice: Some(text) }]
            }
            Binding::FollowContext => {
                self.settings.follow_context = !self.settings.follow_context;
                let ctx = self.context.clone();
                self.apply_context(&ctx);
                self.revision += 1;
                let text = format!("Pages follow the app: {}", if self.settings.follow_context { "On" } else { "Off" });
                self.show(Shown::Notice { text: text.clone() }, now_ms);
                vec![Effect::Save, Effect::Changed { notice: Some(text) }]
            }
        }
    }

    // ------------------------------------------------------------------------------- flushing

    fn finish_gesture(&mut self) -> Vec<Effect> {
        let Some(g) = self.gesture.take() else { return Vec::new() };
        let mut out = Vec::new();
        if let Some(d) = self.pending.remove(&g.target)
            && d != 0.0
        {
            out.push(Effect::Adjust { target: g.target.clone(), delta: d });
        }
        self.accel.reset(&g.target);
        out.push(Effect::End { target: g.target, apply_to_selected: self.settings.apply_to_selected });
        out
    }

    /// Apply gathered dial movement (at most once per flush interval) and end a gesture that has
    /// been idle for the gesture time. Call every frame.
    pub fn flush(&mut self, now_ms: f64) -> Vec<Effect> {
        let mut out = Vec::new();
        if !now_ms.is_finite() {
            return out;
        }
        let interval = f64::from(self.settings.transport.flush_ms);
        if !self.pending.is_empty() && (now_ms - self.last_flush_ms >= interval || now_ms < self.last_flush_ms) {
            for (target, d) in std::mem::take(&mut self.pending) {
                if d != 0.0 {
                    out.push(Effect::Adjust { target, delta: d });
                }
            }
            self.last_flush_ms = now_ms;
        }
        // a destructive key held long enough: run it now (once)
        if self.armed.as_ref().is_some_and(|(_, _, start)| now_ms - start >= HOLD_MS || now_ms < *start)
            && let Some((_, b, _)) = self.armed.take()
            && let Binding::Command { command, params } = &b
        {
            self.show(Shown::Command { label: b.describe(), icon: crate::icons::icon_for(&b) }, now_ms);
            out.extend(self.interrupt());
            out.push(Effect::Command { command: command.clone(), params: params.clone() });
        }
        let idle = f64::from(self.settings.transport.gesture_end_ms);
        if self.pending.is_empty() && self.gesture.as_ref().is_some_and(|g| now_ms - g.last_input_ms >= idle) {
            out.extend(self.finish_gesture());
        }
        out
    }

    /// End any gesture now, applying what is pending (the active photo changed, the app took
    /// over the edit, the surface went away).
    /// A momentary command (see [`MOMENTARY`]) is running on `control`: its key is held.
    pub fn momentary_on(&self, control: &str) -> bool {
        self.momentary.iter().any(|(c, _, _)| c == control)
    }

    /// A destructive key is being held, waiting to run (see [`HOLD_TO_RUN`]).
    pub fn arming(&self) -> bool {
        self.armed.is_some()
    }

    pub fn interrupt(&mut self) -> Vec<Effect> {
        let mut out: Vec<Effect> = std::mem::take(&mut self.pending)
            .into_iter()
            .filter(|(_, d)| *d != 0.0)
            .map(|(target, delta)| Effect::Adjust { target, delta })
            .collect();
        // nothing is pending now: this only ends the gesture
        out.extend(self.finish_gesture());
        out
    }

    /// Does anything still need frames (pending movement, an open gesture, a fading readout)?
    pub fn busy(&self, now_ms: f64) -> bool {
        !self.pending.is_empty() || self.gesture.is_some() || self.shown(now_ms).is_some() || self.armed.is_some()
    }

    /// The modifier held on a control, if any (keys show it lit).
    pub fn held_on(&self, control: &str) -> Option<Modifier> {
        self.held.iter().find(|(c, _)| c == control).map(|(_, m)| *m)
    }
}

/// `params` with `on` set (an object; anything else becomes `{on}`).
fn with_on(params: &Value, on: bool) -> Value {
    let mut m = params.as_object().cloned().unwrap_or_default();
    m.insert("on".into(), Value::Bool(on));
    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::PageGroup;
    use crate::profile::Page;

    fn settings() -> SurfaceSettings {
        let mut p = Profile { name: "T".into(), ..Default::default() };
        p.pages.push(Page::new(
            "Cull",
            &[PageGroup::Keys],
            [("key.1", Binding::command("library.next")), ("key.2", Binding::value("light.contrast"))],
        ));
        p.pages.push(Page::new("Edit", &[PageGroup::Keys], [("key.1", Binding::command("develop.copy"))]));
        p.pages.push(Page::new("Light", &[PageGroup::Dials], [("dial", Binding::value("light.exposure")), ("roller", Binding::value("wb.temp"))]));
        p.pages.push(Page::new("HDR", &[PageGroup::Dials], [("dial", Binding::value("light.highlights"))]));
        p.pages.push(Page::new("Crop", &[], [("key.1", Binding::command("crop.rotateAspect")), ("dial", Binding::value("crop.angle"))]));
        p.base.insert("pad.1".into(), Binding::Modifier { modifier: Modifier::Fine });
        p.base.insert("pad.2".into(), Binding::PageNext { group: Some(PageGroup::Dials) });
        p.base.insert("pad.3".into(), Binding::ApplyToSelected);
        p.base.insert("pad.4".into(), Binding::Reset);
        p.contexts.insert("default".into(), vec!["Cull".into(), "Light".into()]);
        p.contexts.insert("crop".into(), vec!["Crop".into()]);
        SurfaceSettings { profiles: vec![p], profile: "T".into(), enabled: true, ..SurfaceSettings::default() }
    }

    fn turn(c: &str, t: i32) -> Input {
        Input::Turn { control: c.into(), ticks: t }
    }
    fn press(c: &str) -> Input {
        Input::Press { control: c.into(), page: None }
    }
    fn release(c: &str) -> Input {
        Input::Release { control: c.into() }
    }
    fn adjusts(e: &[Effect]) -> f64 {
        e.iter().map(|e| if let Effect::Adjust { delta, .. } = e { *delta } else { 0.0 }).sum()
    }

    #[test]
    fn ticks_coalesce_into_one_adjust_per_flush_and_one_gesture() {
        let mut r = Runtime::new(settings());
        let e = r.input(turn("dial", 1), 0.0);
        assert_eq!(e, [Effect::Begin { target: "light.exposure".into() }]);
        assert!(r.input(turn("dial", 1), 300.0).is_empty(), "no second Begin");
        assert!(r.holds_feedback("light.exposure"));
        let f = r.flush(300.0);
        assert!((adjusts(&f) - 0.02).abs() < 1e-12, "{f:?}");
        assert!(!r.holds_feedback("light.exposure"));
        // within the flush interval nothing more goes out
        r.input(turn("dial", 1), 305.0); // 5 ms after the last tick: the full 10× ceiling
        assert!(r.flush(306.0).is_empty());
        assert!((adjusts(&r.flush(320.0)) - 0.1).abs() < 1e-12);
        // idle past the gesture time: one End
        assert!(r.flush(1000.0).is_empty());
        let end = r.flush(305.0 + 800.0);
        assert_eq!(end, [Effect::End { target: "light.exposure".into(), apply_to_selected: false }]);
        assert!(r.flush(5000.0).is_empty());
        assert!(r.turning().is_none());
    }

    /// TactileBridge's direction rule: a dial turned back follows the new way at once — the old
    /// way's ticks not applied yet are dropped and the speed starts from the floor again.
    #[test]
    fn a_dial_turned_back_follows_the_new_way_at_once() {
        let mut r = Runtime::new(settings());
        r.input(turn("dial", 1), 0.0);
        r.flush(0.0);
        // a fast spin up: the ceiling (10×)
        r.input(turn("dial", 1), 5.0);
        r.input(turn("dial", 1), 10.0);
        let up = adjusts(&r.flush(20.0));
        assert!((up - 0.2).abs() < 1e-12, "{up}");
        // more up, then back down before the next frame: down wins, at the floor speed
        r.input(turn("dial", 3), 22.0);
        r.input(turn("dial", -1), 25.0);
        let f = r.flush(40.0);
        assert!((adjusts(&f) + 0.01).abs() < 1e-12, "down at once, one step: {f:?}");
        // still the same gesture (one undo step)
        assert!(f.iter().all(|e| !matches!(e, Effect::Begin { .. } | Effect::End { .. })), "{f:?}");
        assert_eq!(r.turning(), Some("light.exposure"));
    }

    /// Hundreds of ticks in bursts between frames: every frame applies what came in once, nothing
    /// is left behind (no backlog), and the value moves by exactly the sum of the ticks.
    #[test]
    fn hundreds_of_ticks_apply_once_per_frame_and_sum_exactly() {
        let mut s = settings();
        // no acceleration: one step a tick
        s.motion.acceleration = 0.1;
        let mut r = Runtime::new(s);
        let (mut total, mut ticks, mut now) = (0.0, 0i64, 0.0);
        for frame in 0..100 {
            // 1–7 ticks arrive within each 16 ms frame, the same way
            for k in 0..(1 + frame % 7) {
                r.input(turn("dial", 1), now + f64::from(k));
                ticks += 1;
            }
            now += 16.0;
            let f = r.flush(now);
            let adjust_count = f.iter().filter(|e| matches!(e, Effect::Adjust { .. })).count();
            assert_eq!(adjust_count, 1, "one apply per frame: {f:?}");
            total += adjusts(&f);
            assert!(!r.holds_feedback("light.exposure"), "nothing pending after a frame");
        }
        assert!(ticks >= 300);
        assert!((total - 0.01 * ticks as f64).abs() < 1e-9, "{total} for {ticks} ticks");
    }

    /// A key pressed while the dial is turning runs at once (what the dial gathered goes first).
    #[test]
    fn a_key_press_runs_at_once_even_mid_turn() {
        let mut r = Runtime::new(settings());
        r.input(turn("dial", 1), 0.0);
        r.input(turn("dial", 1), 300.0);
        let e = r.input(press("key.1"), 301.0);
        let cmd = e.iter().position(|e| matches!(e, Effect::Command { .. })).expect("the command");
        let adj = e.iter().position(|e| matches!(e, Effect::Adjust { .. })).expect("the pending turn");
        assert!(adj < cmd, "{e:?}");
    }

    #[test]
    fn erase_works_while_held_and_delete_needs_a_hold() {
        let mut s = settings();
        let p = s.profiles.get_mut(0).unwrap();
        p.base.insert("key.7".into(), Binding::command("brush.erase"));
        p.base.insert("key.8".into(), Binding::command("photo.delete"));
        let mut r = Runtime::new(s);
        // Erase: on while the key is down, off when it comes up
        let e = r.input(press("key.7"), 0.0);
        assert!(e.contains(&Effect::Command { command: "brush.erase".into(), params: serde_json::json!({"on": true}) }), "{e:?}");
        assert!(r.momentary_on("key.7"));
        let e = r.input(release("key.7"), 300.0);
        assert!(e.contains(&Effect::Command { command: "brush.erase".into(), params: serde_json::json!({"on": false}) }), "{e:?}");
        assert!(!r.momentary_on("key.7"));
        // Delete: a quick press runs nothing and says to hold
        assert!(r.input(press("key.8"), 1000.0).is_empty());
        assert!(r.flush(1200.0).iter().all(|e| !matches!(e, Effect::Command { .. })));
        let e = r.input(release("key.8"), 1300.0);
        assert!(e.iter().all(|e| !matches!(e, Effect::Command { .. })), "{e:?}");
        assert!(matches!(r.shown(1310.0), Some((Shown::Notice { .. }, _))), "Hold to Delete");
        // held long enough: it runs, once
        r.input(press("key.8"), 2000.0);
        assert!(r.busy(2100.0), "frames keep coming while it is held");
        let e = r.flush(2000.0 + HOLD_MS + 1.0);
        assert!(e.contains(&Effect::Command { command: "photo.delete".into(), params: Value::Null }), "{e:?}");
        assert!(r.flush(4000.0).iter().all(|e| !matches!(e, Effect::Command { .. })));
        assert!(r.input(release("key.8"), 4100.0).iter().all(|e| !matches!(e, Effect::Command { .. })));
    }

    #[test]
    fn putting_a_value_on_the_dial_shows_no_readout_turning_does() {
        let mut r = Runtime::new(settings());
        r.input(press("key.2"), 0.0);
        r.input(release("key.2"), 10.0);
        assert_eq!(r.focus(), Some("light.contrast"));
        assert!(r.shown(20.0).is_none(), "no readout for picking the value");
        r.input(turn("dial", 1), 30.0);
        assert!(matches!(r.shown(40.0), Some((Shown::Value { .. }, _))), "the turn shows it");
    }

    #[test]
    fn actions_from_a_plugin_turn_reset_and_run() {
        let mut r = Runtime::new(settings());
        let b = Binding::value("light.whites");
        assert_eq!(r.act(&b, Some(2), 0.0), [Effect::Begin { target: "light.whites".into() }]);
        assert!(adjusts(&r.flush(20.0)) > 0.0);
        let e = r.act(&b, None, 30.0);
        assert!(e.contains(&Effect::Reset { target: "light.whites".into() }), "{e:?}");
        let e = r.act(&Binding::command("edit.undo"), None, 40.0);
        assert!(e.contains(&Effect::Command { command: "edit.undo".into(), params: Value::Null }), "{e:?}");
        assert!(r.act(&Binding::command("edit.undo"), Some(-1), 50.0).is_empty(), "counter-clockwise does nothing");
        // a held modifier from an action lets go with the action's release
        r.act(&Binding::Modifier { modifier: Modifier::Fine }, None, 60.0);
        assert_eq!(r.held(), [Modifier::Fine]);
        r.input(release("action"), 70.0);
        assert!(r.held().is_empty());
        r.settings_mut().enabled = false;
        assert!(r.act(&b, Some(1), 80.0).is_empty());
    }

    #[test]
    fn fast_turning_accelerates() {
        let mut r = Runtime::new(settings());
        r.input(turn("dial", 1), 0.0);
        r.input(turn("dial", 1), 5.0); // 5 ms after: full ceiling (10×)
        let f = r.flush(10.0);
        assert!((adjusts(&f) - 0.11).abs() < 1e-9, "{f:?}");
    }

    #[test]
    fn switching_dials_ends_the_first_gesture() {
        let mut r = Runtime::new(settings());
        r.input(turn("dial", 2), 0.0);
        let e = r.input(turn("roller", 1), 10.0);
        assert_eq!(
            e,
            [
                Effect::Adjust { target: "light.exposure".into(), delta: 0.02 },
                Effect::End { target: "light.exposure".into(), apply_to_selected: false },
                Effect::Begin { target: "wb.temp".into() },
            ]
        );
    }

    #[test]
    fn modifiers_scale_steps_while_held() {
        let mut r = Runtime::new(settings());
        r.input(press("pad.1"), 0.0);
        assert_eq!(r.held(), [Modifier::Fine]);
        r.input(turn("dial", 4), 1000.0);
        assert!((adjusts(&r.flush(1000.0)) - 0.01).abs() < 1e-12, "4 ticks × 0.01 × ¼");
        r.input(release("pad.1"), 1001.0);
        assert!(r.held().is_empty());
    }

    #[test]
    fn a_key_bound_to_a_value_puts_it_on_the_dial_and_back() {
        let mut r = Runtime::new(settings());
        let rev = r.revision();
        r.input(press("key.2"), 0.0);
        assert_eq!(r.focus(), Some("light.contrast"));
        assert!(r.revision() > rev);
        assert_eq!(r.binding("dial", None), Some(Binding::value("light.contrast")));
        assert_eq!(r.binding("roller", None), Some(Binding::value("wb.temp")), "the roller keeps its own");
        assert_eq!(r.input(turn("dial", 1), 10.0), [Effect::Begin { target: "light.contrast".into() }]);
        r.input(press("key.2"), 20.0);
        assert_eq!(r.focus(), None);
        // pressing the dial resets the value on it, and keeps it there
        r.input(press("key.2"), 25.0);
        // (the turn still open goes to the photo first)
        assert_eq!(r.input(press("dial"), 26.0).last(), Some(&Effect::Reset { target: "light.contrast".into() }));
        assert_eq!(r.focus(), Some("light.contrast"));
        r.input(press("key.2"), 27.0);
        // a context change drops the focus too
        r.input(press("key.2"), 30.0);
        r.set_context("crop");
        assert_eq!(r.focus(), None);
    }

    #[test]
    fn pages_follow_context_and_come_back_to_the_hand_picked_ones() {
        let mut r = Runtime::new(settings());
        assert_eq!(r.pages()[&PageGroup::Dials], "Light");
        r.input(press("pad.2"), 0.0); // next dial page
        assert_eq!(r.pages()[&PageGroup::Dials], "HDR");
        assert!(r.set_context("crop"));
        assert_eq!((r.pages()[&PageGroup::Keys].as_str(), r.pages()[&PageGroup::Dials].as_str()), ("Crop", "Crop"));
        assert_eq!(r.binding("key.1", Some(2)), Some(Binding::command("crop.rotateAspect")), "a context page fills every device page");
        r.set_context("develop.light");
        assert_eq!((r.pages()[&PageGroup::Keys].as_str(), r.pages()[&PageGroup::Dials].as_str()), ("Cull", "HDR"));
        assert_eq!(r.binding("key.1", Some(2)), Some(Binding::command("develop.copy")), "device page 2 = second key page");
        assert_eq!(r.binding("key.1", Some(9)), None);
        // follow off: contexts don't move pages
        r.input(Input::Press { control: "x".into(), page: None }, 0.0);
        r.settings_mut().follow_context = false;
        r.settings_changed();
        r.set_context("crop");
        assert_eq!(r.pages()[&PageGroup::Keys], "Cull");
    }

    #[test]
    fn commands_toggles_and_dial_press() {
        let mut r = Runtime::new(settings());
        assert_eq!(r.input(press("key.1"), 0.0), [Effect::Command { command: "library.next".into(), params: Value::Null }]);
        assert!(matches!(r.shown(100.0), Some((Shown::Command { .. }, _))));
        assert!(r.shown(100_000.0).is_none(), "the readout times out");
        let e = r.input(press("pad.3"), 0.0);
        assert!(r.settings().apply_to_selected && e.contains(&Effect::Save));
        r.input(turn("dial", 1), 10.0);
        let e = r.interrupt();
        assert_eq!(e.last(), Some(&Effect::End { target: "light.exposure".into(), apply_to_selected: true }));
        assert_eq!(r.input(press("dial"), 20.0), [Effect::Reset { target: "light.exposure".into() }]);
        assert_eq!(r.input(press("pad.4"), 30.0), [Effect::Reset { target: "light.exposure".into() }]);
        assert!(r.input(press("key.9"), 0.0).is_empty(), "unbound");
    }

    #[test]
    fn interrupt_applies_pending_and_ends() {
        let mut r = Runtime::new(settings());
        r.input(turn("dial", 3), 0.0);
        let e = r.interrupt();
        assert_eq!(e.len(), 2);
        assert!((adjusts(&e) - 0.03).abs() < 1e-12);
        assert!(r.interrupt().is_empty());
        assert!(!r.busy(1e9));
    }

    #[test]
    fn learn_mode_reports_the_control_instead_of_acting() {
        let mut r = Runtime::new(settings());
        r.arm_learn();
        assert!(r.learning());
        assert_eq!(r.input(press("key.1"), 0.0), [Effect::Changed { notice: None }]);
        assert_eq!(r.take_learned().as_deref(), Some("key.1"));
        assert_eq!(r.take_learned(), None);
        assert_eq!(r.input(press("key.1"), 0.0).len(), 1, "acts again");
    }

    #[test]
    fn learnt_and_not_yet_taken_still_captures_input() {
        // Touch to find: a dial still turning after it was learnt, before the editor's next frame,
        // must not move the photo; the last control touched is the one reported
        let mut r = Runtime::new(settings());
        r.arm_learn();
        r.input(Input::Turn { control: "dial".into(), ticks: 1 }, 0.0);
        let more = r.input(Input::Turn { control: "dial".into(), ticks: 5 }, 10.0);
        assert!(!more.iter().any(|e| matches!(e, Effect::Command { .. } | Effect::Begin { .. } | Effect::Adjust { .. })), "{more:?}");
        r.input(press("key.4"), 20.0);
        assert_eq!(r.take_learned().as_deref(), Some("key.4"));
        assert!(r.turning().is_none(), "nothing was applied");
    }

    #[test]
    fn disabled_surfaces_do_nothing_and_hostile_input_is_harmless() {
        let mut r = Runtime::new(settings());
        r.input(turn("dial", i32::MAX), 0.0);
        let f = r.flush(0.0);
        assert!(adjusts(&f).abs() <= 10.0, "limited to the exposure range span");
        assert!(r.input(turn("nope", 5), 0.0).is_empty());
        assert!(r.input(turn("dial", 0), 0.0).is_empty());
        assert!(r.flush(f64::NAN).is_empty());
        r.settings_mut().enabled = false;
        assert!(r.input(press("key.1"), 0.0).is_empty());
    }

    #[test]
    fn profiles_switch_and_pages_reset() {
        let mut s = settings();
        let mut q = s.profiles[0].clone();
        q.name = "Q".into();
        s.profiles.push(q);
        let mut r = Runtime::new(s);
        r.input(press("pad.2"), 0.0);
        let e = r.perform("x", &Binding::ProfileNext, 10.0);
        assert!(e.contains(&Effect::Save));
        assert_eq!(r.settings().profile, "Q");
        assert_eq!(r.pages()[&PageGroup::Dials], "Light", "a new profile starts on its default pages");
        r.perform("x", &Binding::Profile { profile: "T".into() }, 20.0);
        assert_eq!(r.settings().profile, "T");
        assert!(r.perform("x", &Binding::Profile { profile: "missing".into() }, 30.0).is_empty());
        let e = r.perform("x", &Binding::Page { page: "Edit".into() }, 40.0);
        assert_eq!(e, [Effect::Changed { notice: Some("Edit".into()) }]);
        assert_eq!(r.pages()[&PageGroup::Keys], "Edit");
    }
}
