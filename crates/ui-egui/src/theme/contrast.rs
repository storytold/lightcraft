//! The theme contrast gate: in every built-in theme, what the interface paints stands out from
//! what it is painted on (WCAG 2.x relative luminance), or is a listed gap.
//!
//! * **What is checked.** A table of the colour pairs the widgets paint together, built per theme
//!   by [`measure`]: text on its surface (4.5:1) and the marks that carry meaning on their surface
//!   (3:1): the outline of a control, a focus ring, a selection ring, an icon, a star, a flag, a
//!   slider's track and thumb, a progress bar. Each pair is named by what it is ("text: dim on
//!   grid cell") and says where it is painted. The pairs are read from the widgets, not from a
//!   list of tokens: a token pair nothing paints is not here, and a colour a widget makes up (a
//!   white ring, a fixed grey chip) is, as a constant. The button pairs come from
//!   [`crate::button::look`], so a change to the button's colours is checked too. Colours egui
//!   paints from the theme's visuals (a `.weak()` label, a hyperlink, a selected row) are read
//!   from the visuals [`crate::theme::apply_kind`] sets.
//! * **What is not checked.** Disabled controls (WCAG 1.4.3 and 1.4.11 leave them out), decorative
//!   lines (the dividers between panels), text over a picture (the picture decides, not the
//!   theme), and colours that a widget computes from a picture or from a label colour. Large text
//!   does not occur: nothing the interface paints is 24 px or more, or 18.66 px bold.
//! * **Known gaps.** Today's failures are in `tests/theme-contrast-gaps.json` (theme → the pairs
//!   that fail). Only a pair that is not listed fails. A pair that now stands the ratio, or no
//!   longer exists, is tolerated and printed as a hint. `LIGHTCRAFT_CONTRAST_BASELINE=shrink`
//!   deletes those entries; `=write` lists a theme that is not in the file yet with its failures
//!   (a new theme), and never adds a failure to a theme the file already has: for those, a new
//!   failing pair is fixed, or listed by hand in the same change.
//! * **What a listed gap does not say.** The file names the pair, not how far it is from the
//!   ratio: a listed pair that gets worse still passes. The failure of a pair that is not listed
//!   prints the measured ratio, and `cargo test -p lightcraft-ui-egui theme::contrast -- --nocapture`
//!   prints every listed one.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs,
    path::Path,
};

use egui::Color32;

use crate::{
    button::{Kind, State, contrast, look},
    theme::{ThemeKind, Tokens, apply_kind},
};

/// Where the known gaps live, relative to the crate.
const BASELINE: &str = "tests/theme-contrast-gaps.json";
/// The environment variable that rewrites the baseline (`shrink` or `write`).
const MODE: &str = "LIGHTCRAFT_CONTRAST_BASELINE";

/// The ratio a pair has to stand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Need {
    /// Text (WCAG 1.4.3): 4.5:1.
    Text,
    /// A mark that says something: an outline, an icon, a state (WCAG 1.4.11): 3:1.
    Graphic,
}

impl Need {
    fn ratio(self) -> f32 {
        match self {
            Need::Text => 4.5,
            Need::Graphic => 3.0,
        }
    }
}

/// One colour painted on another, in one theme.
#[derive(Debug, Clone, PartialEq)]
struct Pair {
    /// What it is: the key of the baseline, so it is never changed to reword.
    name: String,
    need: Need,
    ink: Color32,
    ground: Color32,
    /// Where it is painted, for the message.
    at: &'static str,
}

impl Pair {
    fn ratio(&self) -> f32 {
        // (an ink with some transparency, egui's weak text, lands on its ground as a blend)
        contrast(self.ground.blend(self.ink), self.ground)
    }

    fn stands(&self) -> bool {
        self.ratio() >= self.need.ratio()
    }
}

/// What a theme paints: its tokens and the colours egui takes from its visuals.
struct Scene {
    t: Tokens,
    /// `.weak()` text, and a text box's hint.
    weak: Color32,
    /// A hyperlink.
    link: Color32,
    /// What a selected row or selected text stands on, over its panel.
    selection: Color32,
    /// What egui paints selected text in (`selection.stroke`), whatever its own colour was.
    selection_stroke: Color32,
}

fn scene(kind: ThemeKind) -> Scene {
    let ctx = egui::Context::default();
    apply_kind(&ctx, kind);
    let style = ctx.style_of(if kind.is_dark() { egui::Theme::Dark } else { egui::Theme::Light });
    let visuals = &style.visuals;
    Scene {
        t: Tokens::get(&ctx),
        weak: visuals.weak_text_color(),
        link: visuals.hyperlink_color,
        selection: visuals.selection.bg_fill,
        selection_stroke: visuals.selection.stroke.color,
    }
}

/// A translucent `ink` over `ground`, as painted.
fn over(ground: Color32, ink: Color32, share: f32) -> Color32 {
    ground.blend(ink.gamma_multiply(share))
}

/// The table of what `kind` paints. The names are the same in every theme.
fn measure(kind: ThemeKind) -> Vec<Pair> {
    let Scene { t, weak, link, selection, selection_stroke } = scene(kind);
    let mut pairs = Vec::new();
    let mut add = |name: &str, need: Need, ink: Color32, ground: Color32, at: &'static str| {
        pairs.push(Pair { name: name.to_string(), need, ink, ground, at });
    };
    let (text, graphic) = (Need::Text, Need::Graphic);
    // Constants the widgets paint in every theme: the grid's selection rings and its chips.
    let (white_ring, grey_ring, chip, slab) = (Color32::WHITE, Color32::from_gray(170), Color32::from_gray(26), Color32::from_gray(38));
    // A row the pointer is over in the sidebar: the hover fill at 60 % over the panel.
    let hovered_row = over(t.chrome, t.hover, 0.6);

    // Text in the label colour: egui's `override_text_color`, so every label, button and menu row.
    add("text: label on panel", text, t.text_label, t.chrome, "every egui label, menu and dialog on the panel");
    add("text: label on canvas", text, t.text_label, t.canvas, "a pinned date header (grid.rs), the filmstrip and the bottom bar");
    add("text: label on grid", text, t.text_label, t.grid_bg, "a date header that is not pinned (grid.rs)");
    add("text: label on inset", text, t.text_label, t.inset, "mask tiles (masking.rs), import notes");
    add("text: label on text field", text, t.text_label, t.field, "typed text in an egui text box (extreme_bg_color)");
    add("text: label on button", text, t.text_label, t.button, "an egui button at rest (inactive widget fill)");
    add("text: label on hovered widget", text, t.text_label, t.hover, "an egui button, menu row or tab with the pointer on it");
    add("text: label on pressed widget", text, t.text_label, t.pressed, "an egui button being pressed");
    add("text: label on selected row", text, t.text_label, t.chrome.blend(selection), "a selected row of a list (egui selectable button)");
    // (egui repaints selected text in the selection stroke colour, not in the label colour)
    add("text: selection on selected text", text, selection_stroke, t.field.blend(selection), "selected text in a text box");
    add("text: label on format badge", text, t.text_label, chip, "the file format chip on a grid cell (grid.rs)");
    // Text in the strong colour.
    add("text: strong on panel", text, t.text, t.chrome, "panel and section titles (right.rs, presets.rs)");
    add("text: strong on shown row", text, t.text, t.canvas, "the sidebar row of what the grid shows (left.rs)");
    add("text: strong on chosen row", text, t.text, t.tool_active, "the chosen mask, preset or effect row (masking.rs, presets.rs, mask_effects.rs)");
    // Text in the dim colour.
    add("text: dim on panel", text, t.text_dim, t.chrome, "captions, counts and explanations");
    add("text: dim on canvas", text, t.text_dim, t.canvas, "the count on the shown sidebar row (left.rs), the filmstrip");
    add("text: dim on grid", text, t.text_dim, t.grid_bg, "the count of a date header that is not pinned (grid.rs)");
    add(
        "text: dim count of nothing on panel",
        text,
        t.text_dim.gamma_multiply(0.55),
        t.chrome,
        "the count 0 on a sidebar row (left.rs), drawn fainter on purpose",
    );
    add("text: dim on hovered row", text, t.text_dim, hovered_row, "the count on a sidebar row under the pointer (left.rs)");
    add("text: dim on grid cell", text, t.text_dim, t.cell, "the caption of a square-grid cell (grid.rs)");
    add("text: dim on selected grid cell", text, t.text_dim, t.cell_selected, "the caption of a selected square-grid cell (grid.rs)");
    add("text: dim on inset", text, t.text_dim, t.inset, "the import tag hint (import.rs), a theme card in Settings");
    add("text: dim on text field", text, t.text_dim, t.field, "the search box's placeholder (topbar.rs)");
    // egui's own colours.
    add("text: weak on panel", text, weak, t.chrome, "a `.weak()` label (the bottom bar's Sort by, the date picker's days)");
    add("text: weak on text field", text, weak, t.field, "a text box's hint text");
    add("text: link on panel", text, link, t.chrome, "a hyperlink (`ui.link`: the model pages, the SAM licence)");
    // Text in a colour that says something.
    add(
        "text: accent on panel",
        text,
        t.accent,
        t.chrome,
        "Auto Sync, \"in use\", the Activity panel's \"Show less\" (edit.rs, faces.rs, activity.rs)",
    );
    add("text: accent on inset", text, t.accent, t.inset, "\"In Use\" on the theme card (settings.rs)");
    add("text: caution on panel", text, t.caution, t.chrome, "warnings and notes (import.rs, denoise.rs, faces.rs, dialogs.rs, slide_*.rs)");
    add("text: reject on panel", text, t.reject, t.chrome, "an unavailable folder, a display error (settings.rs)");
    add("text: undone step on panel", text, t.text_disabled, t.chrome, "an undone history step (right.rs): readable, not disabled");
    // Text on a fill.
    add("text: white on accent", text, Color32::WHITE, t.accent, "a dialog's primary egui button (panels/mod.rs), the drag chips");
    add("text: badge count on accent", text, t.canvas, t.accent, "the filter count on the top bar (topbar.rs)");
    add("text: badge mark on reject", text, t.canvas, t.reject, "the \"!\" of an unsaved library on the top bar (topbar.rs)");
    // The button component's own colours, in every kind and state a person sees.
    for (kind_name, button) in [("primary", Kind::Primary), ("secondary", Kind::Secondary), ("danger", Kind::Danger), ("ghost", Kind::Ghost)] {
        for (state_name, state) in [("rest", State::Idle), ("hovered", State::Hovered), ("pressed", State::Pressed)] {
            let colours = look(&t, button, state);
            // (a button with no fill is on the panel)
            add(&format!("text: {kind_name} button, {state_name}"), text, colours.content, colours.fill.unwrap_or(t.chrome), "button.rs (look)");
        }
    }

    // What a control is outlined by.
    add(
        "outline: text box on panel",
        graphic,
        t.field_border,
        t.chrome,
        "the search box (topbar.rs), the keyword chips, the theme cards (settings.rs)",
    );
    add(
        "outline: control on panel",
        graphic,
        t.button_border,
        t.chrome,
        "buttons, text boxes, drop-downs, tick boxes (egui's inactive outline), text buttons (widgets.rs)",
    );
    add(
        "outline: accent on panel",
        graphic,
        t.accent,
        t.chrome,
        "the focus ring (button.rs, keywording.rs, right.rs), a drop target, the shown theme card",
    );
    add("outline: accent on chosen toggle", graphic, t.accent, t.tool_active, "the outline of a toggle that is on (filterbar.rs)");
    add(
        "outline: active filter on panel",
        graphic,
        over(t.chrome, t.accent, 0.7),
        t.chrome,
        "the outline of a filter drop-down that is on (filterbar.rs)",
    );
    // The grid's selection rings are the same two greys in every theme.
    add("ring: active photo on grid", graphic, white_ring, t.grid_bg, "the ring round the active photo, justified grid (grid.rs)");
    add("ring: active photo on selected cell", graphic, white_ring, t.cell_selected, "the ring round the active photo, square grid (grid.rs)");
    add("ring: selected photo on grid", graphic, grey_ring, t.grid_bg, "the ring round the other selected photos, justified grid (grid.rs)");
    add(
        "ring: selected photo on selected cell",
        graphic,
        grey_ring,
        t.cell_selected,
        "the ring round the other selected photos, square grid (grid.rs)",
    );
    // Icons and marks.
    add(
        "icon: chrome icon on panel",
        graphic,
        t.icon,
        t.chrome,
        "the top bar, the filter bar, the section chevrons (topbar.rs, filterbar.rs, right.rs)",
    );
    add("icon: chrome icon on canvas", graphic, t.icon, t.canvas, "the Copy Settings gear on the bottom bar (bottombar.rs)");
    add("icon: chrome icon on hovered widget", graphic, t.icon, t.hover, "the gear with the pointer on it (bottombar.rs)");
    add("mark: star on panel", graphic, t.star, t.chrome, "a lit star of the filter bar and of the rating row (filterbar.rs, widgets.rs)");
    add("mark: pick flag on chosen toggle", graphic, t.pick, t.tool_active, "the Pick flag of the filter bar when it is on (filterbar.rs)");
    add("mark: reject flag on chosen toggle", graphic, t.reject, t.tool_active, "the Reject flag of the filter bar when it is on (filterbar.rs)");
    add("mark: star on bottom pill", graphic, t.star, t.canvas, "the stars of the bottom bar's pill (bottombar.rs)");
    add("mark: pick flag on bottom pill", graphic, t.pick, t.canvas, "the Pick flag of the bottom bar's pill (bottombar.rs)");
    add("mark: reject flag on bottom pill", graphic, t.reject, t.canvas, "the Reject flag of the bottom bar's pill (bottombar.rs)");
    add("mark: caution icon on canvas", graphic, t.caution, t.canvas, "the Preview only card (widgets.rs)");
    add("mark: dim star on inset", graphic, t.text_dim, t.inset, "a theme card's star that is not a favourite (settings.rs)");
    add("mark: failure on thumbnail slab", graphic, t.text_dim, slab, "the \"!\" of a thumbnail that could not be read (grid.rs)");
    add("state: hidden eye on panel", graphic, t.text_disabled, t.chrome, "an effect's eye when it is off (widgets.rs, edit.rs)");
    // Sliders and progress.
    add("slider: track on panel", graphic, t.track, t.chrome, "the track of a Develop slider (widgets.rs)");
    add("slider: thumb on panel", graphic, t.thumb, t.chrome, "the thumb's ring (widgets.rs)");
    add("slider: thumb in use on panel", graphic, t.thumb_hover, t.chrome, "the thumb's ring under the pointer (widgets.rs)");
    add("progress: bar on its track", graphic, t.accent, t.field, "the Activity panel's progress bar (activity.rs)");
    pairs
}

/// A pair that is below its ratio.
#[derive(Debug, Clone, PartialEq)]
struct Failure {
    theme: String,
    name: String,
    ratio: f32,
    need: Need,
    at: &'static str,
}

/// Every pair of every theme that is below its ratio, in the table's order.
fn failures(themes: &[(ThemeKind, Vec<Pair>)]) -> Vec<Failure> {
    themes
        .iter()
        .flat_map(|(kind, pairs)| {
            pairs.iter().filter(|pair| !pair.stands()).map(|pair| Failure {
                theme: kind.id().to_string(),
                name: pair.name.clone(),
                ratio: pair.ratio(),
                need: pair.need,
                at: pair.at,
            })
        })
        .collect()
}

/// What the table and the known gaps say.
#[derive(Debug, Default, PartialEq)]
struct Report {
    /// Failures that are not listed.
    new_gaps: Vec<Failure>,
    /// A listed gap that stands now or is no longer in the table, as `(theme, pair)`.
    stale: Vec<(String, String)>,
}

fn check(found: &[Failure], known: &BTreeMap<String, BTreeSet<String>>) -> Report {
    let mut report = Report::default();
    for failure in found {
        if !known.get(&failure.theme).is_some_and(|listed| listed.contains(&failure.name)) {
            report.new_gaps.push(failure.clone());
        }
    }
    let failing: BTreeSet<(&str, &str)> = found.iter().map(|failure| (failure.theme.as_str(), failure.name.as_str())).collect();
    for (theme, listed) in known {
        for name in listed {
            if !failing.contains(&(theme.as_str(), name.as_str())) {
                report.stale.push((theme.clone(), name.clone()));
            }
        }
    }
    report
}

/// The baseline: theme id → the pairs that failed when it was written. A theme with no failures is
/// listed with none, so a theme that is not in the file is a new one.
#[derive(Debug, Default, PartialEq, serde::Deserialize)]
struct Baseline {
    gaps: BTreeMap<String, BTreeSet<String>>,
}

fn parse_baseline(text: &str) -> Baseline {
    serde_json::from_str(text).unwrap_or_else(|error| panic!("{BASELINE}: {error}"))
}

/// The baseline file: one pair per line, so a change to it reads as a diff of pairs.
fn render_baseline(baseline: &Baseline) -> String {
    let mut out = String::from("{\n  \"gaps\": {\n");
    let last_theme = baseline.gaps.len().saturating_sub(1);
    for (i, (theme, names)) in baseline.gaps.iter().enumerate() {
        let theme_key = serde_json::to_string(theme).unwrap_or_default();
        if names.is_empty() {
            let _ = write!(out, "    {theme_key}: []");
        } else {
            let _ = writeln!(out, "    {theme_key}: [");
            let last = names.len() - 1;
            for (j, name) in names.iter().enumerate() {
                let name = serde_json::to_string(name).unwrap_or_default();
                let _ = writeln!(out, "      {name}{}", if j == last { "" } else { "," });
            }
            out.push_str("    ]");
        }
        out.push_str(if i == last_theme { "\n" } else { ",\n" });
    }
    out.push_str("  }\n}\n");
    out
}

fn failure_message(report: &Report) -> String {
    let mut text = String::from("Colours the interface paints that do not stand out from what they are painted on (WCAG 2.x contrast):\n");
    for failure in &report.new_gaps {
        let _ = writeln!(
            text,
            "  [{}] {:?} is {:.2}:1, needs {:.1}:1 ({})",
            failure.theme,
            failure.name,
            failure.ratio,
            failure.need.ratio(),
            failure.at
        );
    }
    let _ = write!(
        text,
        "Change the token (crates/ui-egui/src/theme.rs, `Tokens::for_kind`) or the colour the widget paints so the pair reaches its ratio in every theme. \
         Do not add the pair to {BASELINE}: that file lists the failures that existed before this test, and it only shrinks (but for a new theme). \
         A pair that a widget paints and the table lacks is added in crates/ui-egui/src/theme/contrast.rs (`measure`)."
    );
    text
}

/// Every built-in theme's table.
fn all_themes() -> Vec<(ThemeKind, Vec<Pair>)> {
    ThemeKind::ALL.iter().map(|kind| (*kind, measure(*kind))).collect()
}

/// Apply `mode` to the baseline: `shrink` deletes what stands now, `write` lists a theme the file
/// does not have yet, with its failures, and never adds one to a theme it has.
fn rewrite(mode: &str, listed: &mut Baseline, found: &[Failure], themes: &[(ThemeKind, Vec<Pair>)]) {
    match mode {
        "shrink" => {
            let failing: BTreeSet<(&str, &str)> = found.iter().map(|failure| (failure.theme.as_str(), failure.name.as_str())).collect();
            for (theme, names) in listed.gaps.iter_mut() {
                names.retain(|name| failing.contains(&(theme.as_str(), name.as_str())));
            }
        }
        "write" => {
            for (kind, _) in themes {
                let id = kind.id().to_string();
                // (a theme the file has keeps what it lists, however many failures it has now)
                listed
                    .gaps
                    .entry(id.clone())
                    .or_insert_with(|| found.iter().filter(|failure| failure.theme == id).map(|failure| failure.name.clone()).collect());
            }
        }
        other => panic!("{MODE} is `write` or `shrink`, not {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(name: &str, need: Need, ink: Color32, ground: Color32) -> Pair {
        Pair { name: name.to_string(), need, ink, ground, at: "somewhere.rs" }
    }

    fn known(entries: &[(&str, &[&str])]) -> BTreeMap<String, BTreeSet<String>> {
        entries.iter().map(|(theme, names)| (theme.to_string(), names.iter().map(|name| name.to_string()).collect())).collect()
    }

    fn tiny(pairs: Vec<Pair>) -> Vec<(ThemeKind, Vec<Pair>)> {
        vec![(ThemeKind::Charcoal, pairs)]
    }

    #[test]
    fn text_needs_4_5_and_a_mark_3() {
        // 0x76 on white is 4.54:1, 0x78 is 4.44:1; 0x94 is 3.03:1, 0x96 is 2.95:1
        let (grey_76, grey_78, grey_94, grey_96) =
            (Color32::from_gray(0x76), Color32::from_gray(0x78), Color32::from_gray(0x94), Color32::from_gray(0x96));
        assert!(pair("a", Need::Text, grey_76, Color32::WHITE).stands());
        assert!(!pair("a", Need::Text, grey_78, Color32::WHITE).stands());
        assert!(pair("a", Need::Graphic, grey_94, Color32::WHITE).stands());
        assert!(!pair("a", Need::Graphic, grey_96, Color32::WHITE).stands());
        // and it does not matter which of the two is lighter
        assert_eq!(pair("a", Need::Text, Color32::BLACK, Color32::WHITE).ratio(), pair("a", Need::Text, Color32::WHITE, Color32::BLACK).ratio());
    }

    #[test]
    fn a_new_failure_fails_and_names_the_theme_the_ratio_and_the_place() {
        let themes = tiny(vec![pair("text: dim on panel", Need::Text, Color32::from_gray(0x80), Color32::from_gray(0x70))]);
        let found = failures(&themes);
        let report = check(&found, &known(&[]));
        assert_eq!(report.new_gaps.len(), 1);
        let message = failure_message(&report);
        assert!(
            message.contains("charcoal")
                && message.contains("text: dim on panel")
                && message.contains("needs 4.5:1")
                && message.contains("somewhere.rs"),
            "{message}"
        );
        assert!(message.contains(BASELINE), "{message}");
    }

    #[test]
    fn a_listed_failure_passes_but_the_same_pair_in_another_theme_fails() {
        let bad = pair("outline: control on panel", Need::Graphic, Color32::from_gray(0x70), Color32::from_gray(0x60));
        let themes = vec![(ThemeKind::Charcoal, vec![bad.clone()]), (ThemeKind::Silver, vec![bad])];
        let found = failures(&themes);
        let report = check(&found, &known(&[("charcoal", &["outline: control on panel"]), ("silver", &[])]));
        assert_eq!(report.new_gaps.len(), 1);
        assert_eq!(report.new_gaps[0].theme, "silver");
        assert!(report.stale.is_empty());
    }

    #[test]
    fn a_pair_that_stands_now_or_is_gone_is_stale_and_passes() {
        let themes = tiny(vec![pair("fixed", Need::Text, Color32::BLACK, Color32::WHITE)]);
        let found = failures(&themes);
        assert!(found.is_empty());
        let report = check(&found, &known(&[("charcoal", &["fixed", "gone"])]));
        assert!(report.new_gaps.is_empty());
        assert_eq!(report.stale, vec![("charcoal".to_string(), "fixed".to_string()), ("charcoal".to_string(), "gone".to_string())]);
    }

    #[test]
    fn shrink_removes_only_what_stands_and_never_adds() {
        let themes = tiny(vec![
            pair("still bad", Need::Text, Color32::from_gray(0x80), Color32::from_gray(0x70)),
            pair("new bad", Need::Text, Color32::from_gray(0x80), Color32::from_gray(0x70)),
            pair("fixed", Need::Text, Color32::BLACK, Color32::WHITE),
        ]);
        let found = failures(&themes);
        let mut listed = Baseline { gaps: known(&[("charcoal", &["still bad", "fixed", "gone"])]) };
        rewrite("shrink", &mut listed, &found, &themes);
        assert_eq!(listed.gaps, known(&[("charcoal", &["still bad"])]));
    }

    #[test]
    fn write_lists_a_new_theme_with_its_failures_but_never_adds_to_a_listed_one() {
        let bad = || pair("bad", Need::Text, Color32::from_gray(0x80), Color32::from_gray(0x70));
        let themes = vec![(ThemeKind::Charcoal, vec![bad()]), (ThemeKind::Paper, vec![bad()])];
        let found = failures(&themes);
        let mut listed = Baseline { gaps: known(&[("charcoal", &[])]) };
        rewrite("write", &mut listed, &found, &themes);
        // Paper is new: listed with its failure. Charcoal was listed clean: the new failure stays a failure.
        assert_eq!(listed.gaps, known(&[("charcoal", &[]), ("paper", &["bad"])]));
        let report = check(&found, &listed.gaps);
        assert_eq!(report.new_gaps.len(), 1);
        assert_eq!(report.new_gaps[0].theme, "charcoal");
    }

    #[test]
    fn the_baseline_round_trips_one_pair_per_line() {
        let baseline = Baseline { gaps: known(&[("charcoal", &["a \"quoted\" pair", "b"]), ("paper", &[])]) };
        let text = render_baseline(&baseline);
        assert_eq!(parse_baseline(&text), baseline);
        assert_eq!(text.lines().count(), 9, "{text}");
    }

    #[test]
    fn a_translucent_ink_is_measured_on_its_ground_not_on_black() {
        // 40 % black on white lands on a grey about 2.9:1 from white, not the 21:1 of solid black
        let faint = pair("a", Need::Text, Color32::BLACK.gamma_multiply(0.4), Color32::WHITE);
        assert!(faint.ratio() < 4.5 && faint.ratio() > 2.0, "{}", faint.ratio());
        assert!(!faint.stands());
    }

    #[test]
    fn a_translucent_ink_is_measured_where_it_lands() {
        // half-opaque white over black lands on mid grey, not on white
        let landed = over(Color32::BLACK, Color32::WHITE, 0.5);
        assert!(landed.r() > 100 && landed.r() < 200, "{landed:?}");
    }

    #[test]
    fn every_theme_has_the_same_named_pairs_and_every_name_is_unique() {
        let themes = all_themes();
        let names = |pairs: &[Pair]| pairs.iter().map(|pair| pair.name.clone()).collect::<Vec<_>>();
        let first = names(&themes[0].1);
        assert!(first.len() > 50, "the table has only {} pairs", first.len());
        for (kind, pairs) in &themes {
            assert_eq!(names(pairs), first, "{kind:?}: the pairs differ between themes");
        }
        let unique: BTreeSet<&String> = first.iter().collect();
        assert_eq!(unique.len(), first.len(), "a pair name is used twice");
        assert!(themes.iter().all(|(_, pairs)| pairs.iter().all(|pair| !pair.at.is_empty())), "a pair does not say where it is painted");
    }

    /// The repository's own themes: a pair that fails and is not listed in
    /// `tests/theme-contrast-gaps.json` is a failure.
    #[test]
    fn every_pair_the_interface_paints_stands_out_in_every_theme_or_is_a_listed_gap() {
        let themes = all_themes();
        let found = failures(&themes);
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(BASELINE);
        let mode = std::env::var(MODE).unwrap_or_default();
        let read = fs::read_to_string(&path);
        let mut listed = match &read {
            // `write` starts over only when there is no readable baseline yet.
            Ok(text) => parse_baseline(text),
            Err(_) if mode == "write" => Baseline::default(),
            Err(error) => panic!("{}: {error}", path.display()),
        };
        if !mode.is_empty() {
            rewrite(&mode, &mut listed, &found, &themes);
            fs::write(&path, render_baseline(&listed)).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        }
        let report = check(&found, &listed.gaps);
        let pairs = themes.iter().map(|(_, pairs)| pairs.len()).sum::<usize>();
        eprintln!("{pairs} pairs over {} themes, {} fail, {} of them listed", themes.len(), found.len(), found.len() - report.new_gaps.len());
        for failure in &found {
            eprintln!("  listed or not: [{}] {:?} {:.2}:1 (needs {:.1})", failure.theme, failure.name, failure.ratio, failure.need.ratio());
        }
        if !report.stale.is_empty() {
            eprintln!(
                "{} entries of {BASELINE} stand now or are gone; `{MODE}=shrink cargo test -p lightcraft-ui-egui theme::contrast` removes them",
                report.stale.len()
            );
        }
        assert!(report.new_gaps.is_empty(), "{}", failure_message(&report));
    }
}
