//! The visible-English scan: text handed to a widget as a string literal never goes through `tr()`.
//!
//! The `gaps` scan lists the `tr("…")` strings a catalog lacks. It cannot see text that never
//! reaches `tr` at all: `ui.label("Folder name")` builds, passes CI and shows English in every
//! language. This finds those literals, for the translation report (`i18n::tests::translation_gaps_report`),
//! which lists them and never fails on them.
//!
//! * **What is checked.** Every call that takes visible text in `crates/ui-egui/src` and
//!   `apps/lightcraft/src`, when its text argument is a literal: `"…"`, `"…".to_string()` (also
//!   `.to_owned()`, `.into()`), `String::from("…")`, `format!("…", …)` (its format string) or the
//!   literal branches of an `if`/`match`. Two kinds of call take text:
//!   1. egui's and a few of ours, listed in [`CALLS`]: `.label`, `.heading`, `.button`,
//!      `.small_button`, `.link`, `.hyperlink_to`, `.checkbox`, `.radio`, `.radio_value`,
//!      `.selectable_label`, `.selectable_value`, `.colored_label`, `.collapsing`, `.menu_button`,
//!      `.on_hover_text`, `.on_disabled_hover_text`, `.on_hover_text_at_pointer`, `.hint_text`,
//!      `.selected_text`, `RichText::new`, `Button::new`, `Label::new`, `Checkbox::new`,
//!      `Window::new`, `CollapsingHeader::new`, `ComboBox::from_label`, `PickRequest::{folder,
//!      file, files, save}`, `MenuItem::with_id`, the painter's `.text` and `.layout_no_wrap`, and
//!      the calls that keep text for a window to draw later (`.toast`, `.toast_for`,
//!      `.toast_error`, `.hint`, `.empty_text`), and `.toggle_value`, `Button::selectable`.
//!   2. This crate's own functions, found by reading the code (`Sinks::derive`): a function with a
//!      `&str`, `String`, `impl Into<String>` or egui text (`impl IntoAtoms`) parameter that the body hands to a widget call
//!      (or to another such function) without `tr`. A helper that translates its parameter itself
//!      (`ui.label(RichText::new(tr(label)))`, like `settings::row` or `text_button`) is not a
//!      sink: its callers pass the English key, as they should. So a new helper is checked from
//!      the day it is written, with no list to update.
//!
//!   A string with no word of two letters, or only capitals (`"{}%"`, `"—"`, `"R"`, `"PDF"`), is not
//!   text. Test code (`tests.rs`, `tests_*.rs` and `*_tests.rs` files, `tests/`, items under `#[cfg(test)]`,
//!   `#[cfg(debug_assertions)]` or `#[test]`) is skipped. The scanner is the `gaps` lexer, so a call
//!   in a comment or a string is not a call. `Button::new` is egui's only when the file does not
//!   use this crate's `Button` (which takes the English key) or the call says `egui::Button::new`.
//! * **Limits (what it cannot see).** Text held in a variable or a table (`const LABEL: &str`, a
//!   struct field such as `label:` or `title:` that is translated when drawn, the `(id, label)` pairs of `choices` and
//!   `segmented`) is not seen, and neither is a helper that stores text for a later draw (add it to
//!   [`CALLS`], as `.toast` is), nor a unit suffix (`.suffix(" rows")`), nor text assigned to a
//!   field (`ui.status = "…".into()`, a `toast` tuple) or given to `with_title`. A function name used
//!   twice is read per file: the file's own definition, else the public ones elsewhere. A
//!   function reached as `Type::f` or `Self::f` is not resolved, and a closure that wraps a widget
//!   call (`let item = |ui, label: &str| ui.button(label)`) is not read as a helper, so literals
//!   passed to it are missed. Screen-reader names
//!   (`announce`, `named`) belong to the accessibility gate. A text that should stay English (a
//!   product name) is written `tr("…")` with the English copied into every catalog, as the
//!   localization guide says.

use std::collections::{BTreeMap, BTreeSet};

use super::gaps::{Kind, Tok, is_ident, is_punct, lex, matching, read_sources, skip_item};

/// One egui call that takes visible text, and which of its arguments (from 0, without the
/// receiver) are that text.
struct Call {
    /// `Some("RichText")` for `RichText::new(…)`; `None` for a method, `x.name(…)`.
    owner: Option<&'static str>,
    name: &'static str,
    args: &'static [usize],
}

const fn method(name: &'static str, args: &'static [usize]) -> Call {
    Call { owner: None, name, args }
}

const fn assoc(owner: &'static str, name: &'static str, args: &'static [usize]) -> Call {
    Call { owner: Some(owner), name, args }
}

impl Call {
    /// How the report names the call.
    fn label(&self) -> String {
        match self.owner {
            None => format!(".{}()", self.name),
            Some(owner) => format!("{owner}::{}()", self.name),
        }
    }
}

/// egui's calls that take visible text: the `Ui` and `Response` methods, the widget constructors
/// and the native pickers and menus, taken from the calls the code already gives `tr(…)` to.
/// This crate's own helpers are not listed: `Sinks::derive` reads them off their signatures.
const CALLS: &[Call] = &[
    // egui `Ui` and `Response` methods, and the widget builders of this crate.
    method("label", &[0]),
    method("heading", &[0]),
    method("button", &[0]),
    method("small_button", &[0]),
    method("link", &[0]),
    method("hyperlink_to", &[0]),
    method("checkbox", &[1]),
    method("radio", &[1]),
    method("radio_value", &[2]),
    method("selectable_label", &[1]),
    method("selectable_value", &[2]),
    method("colored_label", &[1]),
    method("collapsing", &[0]),
    method("menu_button", &[0]),
    method("on_hover_text", &[0]),
    method("on_disabled_hover_text", &[0]),
    method("on_hover_text_at_pointer", &[0]),
    method("hint_text", &[0]),
    method("selected_text", &[0]),
    // egui widgets and the native pickers and menus.
    assoc("RichText", "new", &[0]),
    assoc("Button", "new", &[0]),
    assoc("Label", "new", &[0]),
    assoc("Checkbox", "new", &[1]),
    assoc("Window", "new", &[0]),
    assoc("CollapsingHeader", "new", &[0]),
    assoc("ComboBox", "from_label", &[0]),
    assoc("PickRequest", "folder", &[0]),
    assoc("PickRequest", "file", &[0, 1]),
    assoc("PickRequest", "files", &[1]),
    assoc("PickRequest", "save", &[0, 1]),
    assoc("MenuItem", "with_id", &[1]),
    // `painter.text(pos, anchor, text, font, color)` and `painter.layout_no_wrap(text, font, color)`.
    method("text", &[2]),
    method("layout_no_wrap", &[0]),
    // A toggle button.
    method("toggle_value", &[1]),
    assoc("Button", "selectable", &[1]),
    // Text a function stores and the window draws later.
    method("toast", &[1]),
    method("toast_for", &[1]),
    method("toast_error", &[1]),
    method("hint", &[0]),
    method("empty_text", &[0]),
];

/// One function definition with a text parameter (`&str`, `String`, `impl Into<String>` or egui's
/// `impl IntoAtoms`), and what its body does with each of those parameters.
#[derive(Debug)]
struct Def {
    file: String,
    name: String,
    public: bool,
    /// It has a `self` receiver: it is called as `x.name(…)`.
    method: bool,
    /// Per parameter (without the receiver): its name when it is a text type.
    params: Vec<Option<String>>,
    /// The calls in the body: their key and the token ranges of their arguments.
    calls: Vec<(String, Vec<std::ops::Range<usize>>)>,
    /// Per parameter: does it reach a widget (or a function that does) without going through `tr`?
    shows: Vec<bool>,
    /// Per parameter: does the body (or a function it passes it to) run it through `tr`?
    translates: Vec<bool>,
}

impl Def {
    fn key(&self) -> String {
        if self.method { format!(".{}", self.name) } else { self.name.clone() }
    }
}

/// This crate's own functions whose text parameter reaches the screen without `tr` inside: a
/// literal passed to one is visible English. A function that translates its parameter itself
/// (`ui.label(tr(label))`) is not listed: its callers pass the English key.
#[derive(Debug, Default)]
struct Sinks {
    /// Function name → its definitions with at least one such parameter; `args` are those positions.
    by_name: BTreeMap<String, Vec<Sig>>,
    /// Every function the sources define, a sink or not: `(file, name, is a method)`.
    defined: BTreeSet<(String, String, bool)>,
}

/// The text parameters of one definition that need a translated argument.
#[derive(Debug)]
struct Sig {
    file: String,
    public: bool,
    method: bool,
    args: Vec<usize>,
}

/// How a call is keyed in the flow analysis: `.name` (method), `Owner::name` (associated function)
/// or `name` (a function, bare or through its module).
fn call_key(tokens: &[Tok], at: usize) -> Option<String> {
    let Kind::Ident(name) = &tokens.get(at)?.kind else { return None };
    if !is_punct(tokens.get(at + 1), '(') || at == 0 || is_ident(tokens.get(at - 1), "fn") {
        return None;
    }
    if is_punct(tokens.get(at - 1), '.') {
        return Some(format!(".{name}"));
    }
    if at >= 3 && is_punct(tokens.get(at - 1), ':') && is_punct(tokens.get(at - 2), ':') {
        return match &tokens[at - 3].kind {
            Kind::Ident(owner) if owner.starts_with(|c: char| c.is_uppercase()) => Some(format!("{owner}::{name}")),
            Kind::Ident(_) => Some(name.clone()),
            _ => None,
        };
    }
    Some(name.clone())
}

/// `tr`, `tr_ctx`, `tr_in`, … : the functions that look an English key up in the catalog.
fn is_translator(word: &str) -> bool {
    word == "tr" || word.starts_with("tr_")
}

/// Does this argument use the parameter `name`?
fn uses_param(tokens: &[Tok], name: &str) -> bool {
    let (plain, inline, inline_spec) = (name.to_string(), format!("{{{name}}}"), format!("{{{name}:"));
    tokens.iter().enumerate().any(|(i, token)| match &token.kind {
        Kind::Ident(word) => *word == plain && !(i > 0 && is_punct(tokens.get(i - 1), '.')),
        // `format!("{label}: x")` names the parameter inside the string.
        Kind::Str(text) => text.contains(&inline) || text.contains(&inline_spec),
        _ => false,
    })
}

fn has_translator(tokens: &[Tok]) -> bool {
    tokens.iter().any(|token| matches!(&token.kind, Kind::Ident(word) if is_translator(word)))
}

/// Split the tokens of a parameter list at its top-level commas (commas in `HashMap<K, V>` and in
/// brackets do not split).
fn split_params(tokens: &[Tok], open: usize, close: usize) -> Vec<&[Tok]> {
    let mut params = Vec::new();
    let (mut start, mut at, mut angle) = (open + 1, open + 1, 0i32);
    while at < close {
        match tokens[at].kind {
            Kind::Punct('(' | '[' | '{') => at = matching(tokens, at),
            Kind::Punct('<') => angle += 1,
            Kind::Punct('>') if at > 0 && tokens[at - 1].kind != Kind::Punct('-') => angle -= 1,
            Kind::Punct(',') if angle == 0 => {
                params.push(&tokens[start..at]);
                start = at + 1;
            }
            _ => {}
        }
        at += 1;
    }
    if start < close {
        params.push(&tokens[start..close]);
    }
    params
}

/// Is this parameter's type a string the caller writes as text: `&str`, `&'a str`, `String`,
/// `impl Into<String>`, or the `impl` types egui takes text through (`IntoAtoms`, `Into<WidgetText>`)?
fn is_text_type(ty: &[Tok]) -> bool {
    let idents: Vec<&str> = ty
        .iter()
        .filter_map(|token| match &token.kind {
            Kind::Ident(word) => Some(word.as_str()),
            _ => None,
        })
        .collect();
    match idents.as_slice() {
        ["String"] => ty.len() == 1,
        ["impl", "Into", "String"] => true,
        // `impl egui::IntoAtoms<'a>`, `impl Into<egui::WidgetText>`, `impl Into<RichText>`: what the
        // widget wrappers take, and egui turns a literal into their text
        ["impl", rest @ ..] => {
            rest.contains(&"IntoAtoms") || (rest.first() == Some(&"Into") && rest.iter().any(|w| matches!(*w, "WidgetText" | "RichText")))
        }
        // `&str`, `&'a str`, `&'static str`
        ["str"] | [_, "str"] => is_punct(ty.first(), '&'),
        _ => false,
    }
}

/// The `(` of a signature's parameter list, `open` being the token after the name: past the generics (a `->` inside a bound is no bracket).
fn skip_generics(tokens: &[Tok], mut open: usize) -> usize {
    if !is_punct(tokens.get(open), '<') {
        return open;
    }
    let mut depth = 0;
    while let Some(token) = tokens.get(open) {
        match token.kind {
            Kind::Punct('(' | '[') => open = matching(tokens, open),
            Kind::Punct('<') => depth += 1,
            Kind::Punct('>') if tokens[open - 1].kind != Kind::Punct('-') => depth -= 1,
            _ => {}
        }
        open += 1;
        if depth == 0 {
            break;
        }
    }
    open
}

/// Per parameter (without the receiver): its name when it is a text type.
fn text_params(params: &[&[Tok]]) -> Vec<Option<String>> {
    params
        .iter()
        .map(|param| {
            let param: &[Tok] = if is_ident(param.first(), "mut") { &param[1..] } else { param };
            match param {
                [Tok { kind: Kind::Ident(name), .. }, Tok { kind: Kind::Punct(':'), .. }, ty @ ..] if is_text_type(ty) => Some(name.clone()),
                _ => None,
            }
        })
        .collect()
}

/// The calls in the body of the function whose parameter list closes at `close` (a `;` before the `{` means a declaration: none).
fn body_calls(tokens: &[Tok], close: usize, crate_button: bool) -> Vec<(String, Vec<std::ops::Range<usize>>)> {
    let mut body = close + 1;
    while let Some(token) = tokens.get(body) {
        match token.kind {
            Kind::Punct('{' | ';') => break,
            Kind::Punct('(' | '[') => body = matching(tokens, body),
            _ => {}
        }
        body += 1;
    }
    if !is_punct(tokens.get(body), '{') {
        return Vec::new();
    }
    let mut calls = Vec::new();
    for inside in body + 1..matching(tokens, body) {
        if let Some(key) = call_key(tokens, inside) {
            // (this crate's `Button` takes the English key: not a call that needs translated text)
            if key == "Button::new" && crate_button && !(inside >= 6 && is_ident(tokens.get(inside - 6), "egui")) {
                continue;
            }
            calls.push((key, arg_ranges(tokens, inside + 1, matching(tokens, inside + 1))));
        }
    }
    calls
}

/// Is the `fn` at `at` public (`pub`, `pub(crate)`), past any `const`, `async` or `unsafe`?
fn is_public_fn(tokens: &[Tok], at: usize) -> bool {
    let mut before = at;
    while before > 0 && matches!(&tokens[before - 1].kind, Kind::Ident(word) if word == "const" || word == "async" || word == "unsafe") {
        before -= 1;
    }
    before > 0 && (is_ident(tokens.get(before - 1), "pub") || is_punct(tokens.get(before - 1), ')'))
}

/// The function whose `fn` is at `at`: is it a method, and its [`Def`] when it has a text parameter. `None`: no signature follows.
fn parse_def(file: &str, name: &str, tokens: &[Tok], at: usize, crate_button: bool) -> Option<(bool, Option<Def>)> {
    let open = skip_generics(tokens, at + 2);
    if !is_punct(tokens.get(open), '(') {
        return None;
    }
    let close = matching(tokens, open);
    let mut params = split_params(tokens, open, close);
    let method = params.first().is_some_and(|first| first.iter().take(4).any(|token| is_ident(Some(token), "self")));
    if method {
        params.remove(0);
    }
    let params = text_params(&params);
    if params.iter().all(Option::is_none) {
        return Some((method, None));
    }
    let count = params.len();
    let (file, name, public, calls) = (file.to_string(), name.to_string(), is_public_fn(tokens, at), body_calls(tokens, close, crate_button));
    Some((method, Some(Def { file, name, public, method, params, calls, shows: vec![false; count], translates: vec![false; count] })))
}

/// Which definitions have a parameter that shows (or translates) its text: `(key, position)` → the `(file, public)` of each such
/// definition. egui's own calls are the entries with an empty file.
type Flow = BTreeMap<(String, usize), BTreeSet<(String, bool)>>;

/// Does a call to `key` written in `file` reach a parameter in `flow`? Like [`Sinks::args`]: a function the file defines itself
/// wins, otherwise the public ones elsewhere count; egui's calls always do.
fn reaches(flow: &Flow, defined: &BTreeSet<(String, String, bool)>, file: &str, key: &str, position: usize) -> bool {
    let Some(places) = flow.get(&(key.to_string(), position)) else { return false };
    let own = defined.contains(&(file.to_string(), key.trim_start_matches('.').to_string(), key.starts_with('.')));
    places.iter().any(|(at, public)| at.is_empty() || if own { at == file } else { *public })
}

/// Flow to a fixed point: a parameter shows when it is handed to a widget call (or to a function whose parameter shows) outside `tr`,
/// and translates when it is handed to `tr` (or to a function that translates it). `defs[n]` is from `files[tokens_of[n]]`;
/// `defined` is every function the sources define, a sink or not.
fn flow_params(defs: &mut [Def], tokens_of: &[usize], files: &[Vec<Tok>], defined: &BTreeSet<(String, String, bool)>) {
    let mut shown = Flow::new();
    for call in CALLS {
        let key = call.owner.map_or_else(|| format!(".{}", call.name), |owner| format!("{owner}::{}", call.name));
        for arg in call.args {
            shown.entry((key.clone(), *arg)).or_default().insert((String::new(), true));
        }
    }
    let mut translated = Flow::new();
    loop {
        let mut changed = false;
        for (def, ti) in defs.iter_mut().zip(tokens_of) {
            let tokens = &files[*ti];
            for (index, param) in def.params.iter().enumerate() {
                let Some(param) = param else { continue };
                for (key, ranges) in &def.calls {
                    for (position, range) in ranges.iter().enumerate() {
                        let arg = &tokens[range.clone()];
                        if !uses_param(arg, param) {
                            continue;
                        }
                        // (a slider's `.text(label)` has one argument; the painter's `.text(pos, anchor, text, …)` has more)
                        let slider_text = key == ".text" && ranges.len() == 1 && position == 0;
                        if is_translator(key.trim_start_matches('.')) || reaches(&translated, defined, &def.file, key, position) {
                            def.translates[index] = true;
                        } else if (slider_text || reaches(&shown, defined, &def.file, key, position))
                            && !has_translator(arg)
                            && shows_param(arg, param)
                        {
                            def.shows[index] = true;
                        }
                    }
                }
                let (me, place) = (def.key(), (def.file.clone(), def.public));
                if def.translates[index] {
                    changed |= translated.entry((me.clone(), index)).or_default().insert(place.clone());
                }
                if def.shows[index] {
                    changed |= shown.entry((me, index)).or_default().insert(place);
                }
            }
        }
        if !changed {
            break;
        }
    }
}

impl Sinks {
    fn derive(sources: &[(String, String)]) -> Self {
        let files: Vec<Vec<Tok>> = sources.iter().map(|(_, source)| lex(source)).collect();
        let mut defs = Vec::new();
        let mut tokens_of = Vec::new();
        let skipped: Vec<Vec<std::ops::Range<usize>>> = files.iter().map(|tokens| skipped_ranges(tokens)).collect();
        let crate_button: Vec<bool> = files.iter().map(|tokens| uses_crate_button(tokens)).collect();
        let mut defined = BTreeSet::new();
        for (ti, ((file, _), tokens)) in sources.iter().zip(&files).enumerate() {
            for at in 0..tokens.len() {
                if skipped[ti].iter().any(|range| range.contains(&at)) || !is_ident(tokens.get(at), "fn") {
                    continue;
                }
                let Some(Kind::Ident(name)) = tokens.get(at + 1).map(|token| &token.kind) else { continue };
                let Some((method, def)) = parse_def(file, name, tokens, at, crate_button[ti]) else { continue };
                defined.insert((file.clone(), name.clone(), method));
                if let Some(def) = def {
                    tokens_of.push(ti);
                    defs.push(def);
                }
            }
        }
        flow_params(&mut defs, &tokens_of, &files, &defined);
        let mut sinks = Sinks { defined, ..Sinks::default() };
        for def in defs {
            let args: Vec<usize> = (0..def.params.len()).filter(|index| def.shows[*index] && !def.translates[*index]).collect();
            if !args.is_empty() {
                sinks.by_name.entry(def.name).or_default().push(Sig { file: def.file, public: def.public, method: def.method, args });
            }
        }
        sinks
    }

    /// The text arguments of a call to the crate's function `name`, written in `file`. A function the
    /// file defines itself wins, whether or not it is a sink; otherwise the public ones of that name
    /// elsewhere count (the file imports one of them), their positions merged.
    fn args(&self, file: &str, name: &str, method: bool) -> Vec<usize> {
        let Some(sigs) = self.by_name.get(name) else { return Vec::new() };
        let own = self.defined.contains(&(file.to_string(), name.to_string(), method));
        let sigs = sigs.iter().filter(|sig| sig.method == method && if own { sig.file == file } else { sig.public });
        let mut args: Vec<usize> = sigs.flat_map(|sig| sig.args.iter().copied()).collect();
        args.sort_unstable();
        args.dedup();
        args
    }

    /// Every call the scan reads, as the report names them: egui's and the derived helpers.
    fn labels(&self) -> BTreeSet<String> {
        let mut labels: BTreeSet<String> = CALLS.iter().map(Call::label).collect();
        for (name, sigs) in &self.by_name {
            for sig in sigs {
                labels.insert(if sig.method { format!(".{name}()") } else { format!("{name}()") });
            }
        }
        labels
    }
}

/// Does this argument hand the parameter `name` to a widget as it is: the parameter alone (with a
/// `&` or a `.to_string()`-like call) or inside a `format!`? A parameter that only feeds another
/// call (`choice_text(field, …)`) is not shown by it.
fn shows_param(arg: &[Tok], name: &str) -> bool {
    let mut arg = arg;
    while let [Tok { kind: Kind::Punct('&' | '*'), .. }, rest @ ..] = arg {
        arg = rest;
    }
    let is_param = |token: &Tok| matches!(&token.kind, Kind::Ident(word) if word == name);
    match arg {
        [first, bang, ..] if is_ident(Some(first), "format") && is_punct(Some(bang), '!') => uses_param(arg, name),
        [param] => is_param(param),
        [param, dot, Tok { kind: Kind::Ident(method), .. }, open, close] => {
            is_param(param)
                && is_punct(Some(dot), '.')
                && ["to_string", "to_owned", "into", "as_str", "clone"].contains(&method.as_str())
                && is_punct(Some(open), '(')
                && is_punct(Some(close), ')')
        }
        _ => false,
    }
}

/// The token ranges of the items the scan skips (see `skipped_attribute`).
fn skipped_ranges(tokens: &[Tok]) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::new();
    let mut at = 0;
    while at < tokens.len() {
        if let Some(length) = skipped_attribute(tokens, at) {
            let end = skip_item(tokens, at + length).max(at + 1);
            ranges.push(at..end);
            at = end;
        } else {
            at += 1;
        }
    }
    ranges
}

/// Wrapper methods that turn a literal into the `String` a call takes.
const TO_STRING: [&str; 3] = ["to_string", "to_owned", "into"];

/// One literal found in a listed call.
#[derive(Debug, Clone, PartialEq)]
struct Found {
    file: String,
    line: usize,
    /// The call, as `Call::label` names it.
    call: String,
    text: String,
}

/// Does this literal show a word? Placeholders (`{n}`, `{0:.1}`) do not count; a lone letter
/// (`"R"`) and an all-capitals acronym (`"PDF"`, `"GPS"`, `"MP4"`) do not either: symbols and
/// names of formats are not text to translate.
fn has_text(literal: &str) -> bool {
    let (mut run, mut inside, mut word, mut lower) = (0, false, false, false);
    for c in literal.chars() {
        match c {
            '{' => inside = true,
            '}' => inside = false,
            c if !inside && c.is_alphabetic() => {
                run += 1;
                word |= run >= 2;
                lower |= !c.is_uppercase();
            }
            _ => run = 0,
        }
    }
    word && lower
}

/// Is `tokens[at..]` an attribute that takes the item after it out of the scan: `#[cfg(test)]`,
/// `#[cfg(debug_assertions)]` or `#[test]`? Returns the attribute's length in tokens.
fn skipped_attribute(tokens: &[Tok], at: usize) -> Option<usize> {
    if !(is_punct(tokens.get(at), '#') && is_punct(tokens.get(at + 1), '[')) {
        return None;
    }
    let cfg = is_ident(tokens.get(at + 2), "cfg")
        && is_punct(tokens.get(at + 3), '(')
        && (is_ident(tokens.get(at + 4), "test") || is_ident(tokens.get(at + 4), "debug_assertions"))
        && is_punct(tokens.get(at + 5), ')')
        && is_punct(tokens.get(at + 6), ']');
    if cfg {
        return Some(7);
    }
    (is_ident(tokens.get(at + 2), "test") && is_punct(tokens.get(at + 3), ']')).then_some(4)
}

/// The token ranges of a call's arguments: what lies between its parentheses, split at top-level commas.
fn arg_ranges(tokens: &[Tok], open: usize, close: usize) -> Vec<std::ops::Range<usize>> {
    let mut args = Vec::new();
    let (mut start, mut at) = (open + 1, open + 1);
    while at < close {
        match tokens[at].kind {
            Kind::Punct('(' | '[' | '{') => at = matching(tokens, at),
            Kind::Punct(',') => {
                args.push(start..at);
                start = at + 1;
            }
            _ => {}
        }
        at += 1;
    }
    if start < close {
        args.push(start..close);
    }
    args
}

fn split_args(tokens: &[Tok], open: usize, close: usize) -> Vec<&[Tok]> {
    arg_ranges(tokens, open, close).into_iter().map(|range| &tokens[range]).collect()
}

/// The text literals an argument is made of; see the module docs for the forms read.
fn literals(arg: &[Tok]) -> Vec<(&str, usize)> {
    let mut arg = arg;
    while let [Tok { kind: Kind::Punct('&'), .. }, rest @ ..] = arg {
        arg = rest;
    }
    fn text(token: &Tok) -> Option<(&str, usize)> {
        match &token.kind {
            Kind::Str(text) => Some((text.as_str(), token.line)),
            _ => None,
        }
    }
    let found = match arg {
        [one] => text(one),
        // "…".to_string(), "…".into()
        [literal, dot, Tok { kind: Kind::Ident(method), .. }, open, close]
            if text(literal).is_some()
                && dot.kind == Kind::Punct('.')
                && TO_STRING.contains(&method.as_str())
                && open.kind == Kind::Punct('(')
                && close.kind == Kind::Punct(')') =>
        {
            text(literal)
        }
        // String::from("…")
        [Tok { kind: Kind::Ident(owner), .. }, c1, c2, Tok { kind: Kind::Ident(from), .. }, open, literal, close]
            if owner == "String"
                && from == "from"
                && c1.kind == Kind::Punct(':')
                && c2.kind == Kind::Punct(':')
                && open.kind == Kind::Punct('(')
                && close.kind == Kind::Punct(')') =>
        {
            text(literal)
        }
        // format!("…", …): the format string is the text.
        [Tok { kind: Kind::Ident(format), .. }, bang, open, literal, ..]
            if format == "format" && bang.kind == Kind::Punct('!') && open.kind == Kind::Punct('(') =>
        {
            text(literal)
        }
        // if flag { "On" } else { "Off" } and match x { A => "a", B => { "b" } }: each literal that is a whole branch.
        [Tok { kind: Kind::Ident(word), .. }, ..] if word == "if" || word == "match" => {
            return arg
                .windows(3)
                .filter_map(|window| match window {
                    [before, Tok { kind: Kind::Str(text), line }, after] => {
                        let braced = before.kind == Kind::Punct('{') && after.kind == Kind::Punct('}');
                        let arm = before.kind == Kind::Punct('>') && matches!(after.kind, Kind::Punct(',' | '}'));
                        (braced || arm).then_some((text.as_str(), *line))
                    }
                    _ => None,
                })
                .collect();
        }
        _ => None,
    };
    found.into_iter().collect()
}

/// The call that `tokens[at]` starts, if it takes visible text: its label and the text arguments.
fn call_at(tokens: &[Tok], at: usize, file: &str, sinks: &Sinks, crate_button: bool) -> Option<(String, Vec<usize>)> {
    let Kind::Ident(name) = &tokens[at].kind else { return None };
    if !is_punct(tokens.get(at + 1), '(') || at == 0 || is_ident(tokens.get(at - 1), "fn") {
        return None;
    }
    let after_dot = is_punct(tokens.get(at - 1), '.');
    let after_path = at >= 3 && is_punct(tokens.get(at - 1), ':') && is_punct(tokens.get(at - 2), ':');
    let owner = if after_path { tokens.get(at - 3) } else { None };
    let owner_name = match owner.map(|token| &token.kind) {
        Some(Kind::Ident(owner)) => Some(owner.as_str()),
        _ => None,
    };
    if after_path && owner_name.is_some_and(|owner| owner.starts_with(|c: char| c.is_uppercase())) {
        // `RichText::new(…)`: only the listed associated functions take text.
        let call = CALLS.iter().find(|call| call.name == name && call.owner == owner_name)?;
        // This crate's own `Button` takes the English key and translates it when drawn: only
        // `egui::Button::new` is a call that needs a translated text.
        if owner_name == Some("Button") && name == "new" && crate_button && !(at >= 6 && is_ident(tokens.get(at - 6), "egui")) {
            return None;
        }
        return Some((call.label(), call.args.to_vec()));
    }
    if after_path && owner_name.is_none() {
        return None;
    }
    // A method is egui's (listed) or this crate's (derived); a function is this crate's, called
    // bare or through its module (`widgets::text_button(…)`).
    let mut args = sinks.args(file, name, after_dot);
    if after_dot {
        for call in CALLS.iter().filter(|call| call.owner.is_none() && call.name == name) {
            args.extend_from_slice(call.args);
        }
        args.sort_unstable();
        args.dedup();
    }
    let label = if after_dot { format!(".{name}()") } else { format!("{name}()") };
    (!args.is_empty()).then_some((label, args))
}

/// The file uses this crate's `Button` (`use crate::button::Button`, `crate::button::Button::new`).
fn uses_crate_button(tokens: &[Tok]) -> bool {
    tokens
        .windows(4)
        .any(|w| is_ident(Some(&w[0]), "button") && is_punct(Some(&w[1]), ':') && is_punct(Some(&w[2]), ':') && is_ident(Some(&w[3]), "Button"))
}

/// The visible literals in the calls of one source file that take text.
fn scan_source(file: &str, source: &str, sinks: &Sinks) -> Vec<Found> {
    let tokens = lex(source);
    // The file uses this crate's `Button` (`use crate::button::Button`, `crate::button::Button::new`).
    let crate_button = uses_crate_button(&tokens);
    let mut found = Vec::new();
    let mut at = 0;
    while at < tokens.len() {
        if let Some(length) = skipped_attribute(&tokens, at) {
            at = skip_item(&tokens, at + length);
            continue;
        }
        if let Some((call, text_args)) = call_at(&tokens, at, file, sinks, crate_button) {
            let close = matching(&tokens, at + 1);
            let args = split_args(&tokens, at + 1, close);
            // `slider.text("…")` has one argument; the painter's `.text(pos, anchor, text, …)` has more.
            let text_args = if call == ".text()" && args.len() == 1 { vec![0] } else { text_args };
            for index in text_args {
                for (text, line) in args.get(index).map(|arg| literals(arg)).unwrap_or_default() {
                    if has_text(text) {
                        found.push(Found { file: file.to_string(), line, call: call.clone(), text: text.to_string() });
                    }
                }
            }
        }
        at += 1;
    }
    found
}

/// Where English is shown without `tr`: one `(file:line, call, text)` per distinct text in a file,
/// at its first place, in file order.
pub(super) fn untranslated_places() -> Vec<(String, String, String)> {
    let sources = read_sources();
    let sinks = Sinks::derive(&sources);
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    let mut places = Vec::new();
    for (file, source) in &sources {
        for item in scan_source(file, source, &sinks) {
            if seen.insert((item.file.clone(), item.text.clone())) {
                places.push((format!("{}:{}", item.file, item.line), item.call, item.text));
            }
        }
    }
    places
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the scan finds in one source that also defines its own helpers.
    fn found_in(source: &str) -> Vec<(String, usize, String)> {
        let sinks = Sinks::derive(&[("x.rs".to_string(), source.to_string())]);
        scan_source("x.rs", source, &sinks).into_iter().map(|item| (item.call, item.line, item.text)).collect()
    }

    #[test]
    fn words_are_text_but_placeholders_symbols_and_lone_letters_are_not() {
        for text in ["Open", "{n} photos", "a bc", "{0:.1} px", "Größe", "写真", "WebM", "PDF export"] {
            assert!(has_text(text), "{text}");
        }
        for text in ["", "{}", "{n}%", "—", "…", "R", "{n} x {m}", "1/2", "{a}{b}", "a.b", "a b", "PDF", "GPS", "MP4", "ISO {n}"] {
            assert!(!has_text(text), "{text}");
        }
    }

    #[test]
    fn a_literal_in_a_listed_call_is_found_with_its_call_and_line() {
        let source = r##"
fn f(ui: &mut Ui, app: &mut App, ctx: &Context) {
    ui.label("Plain");
    ui.add(egui::RichText::new("Rich").strong());
    ui.checkbox(&mut on, "Check");
    ui.selectable_value(&mut mode, Mode::A, "Option");
    app.toast(ctx, "Toast");
    crate::widgets::text_button(ui, "id", "Button text", false);
    ui.label("Wrapped \
              line");
    ui.label(
        "Wrapped by rustfmt",
    );
    ui.label(format!("{n} items"));
    ui.label("From".to_string());
    ui.label(String::from("String from"));
    ui.label(&"Reference".to_string());
    ui.label(if on { "On" } else { "Off" });
    ui.label(match m { Mode::A => "Arm", Mode::B => { "Braced arm" }, Mode::C => other() });
    ui.label(r#"Raw "text""#);
}
fn text_button(ui: &mut Ui, id: &str, label: &str, active: bool) { ui.label(label); }
impl App {
    pub fn toast(&mut self, ctx: &Context, text: impl Into<String>) {}
}
"##;
        assert_eq!(
            found_in(source),
            [
                (".label()", 3, "Plain"),
                ("RichText::new()", 4, "Rich"),
                (".checkbox()", 5, "Check"),
                (".selectable_value()", 6, "Option"),
                (".toast()", 7, "Toast"),
                ("text_button()", 8, "Button text"),
                (".label()", 9, "Wrapped line"),
                (".label()", 12, "Wrapped by rustfmt"),
                (".label()", 14, "{n} items"),
                (".label()", 15, "From"),
                (".label()", 16, "String from"),
                (".label()", 17, "Reference"),
                (".label()", 18, "On"),
                (".label()", 18, "Off"),
                (".label()", 19, "Arm"),
                (".label()", 19, "Braced arm"),
                (".label()", 20, "Raw \"text\""),
            ]
            .map(|(call, line, text)| (call.to_string(), line, text.to_string()))
        );
    }

    #[test]
    fn translated_text_ids_variables_and_other_receivers_are_not_found() {
        let source = r#"
fn f(ui: &mut Ui, name: &str) {
    ui.label(tr("Translated"));
    ui.label(crate::i18n::tr("Qualified"));
    ui.label(tr_format!("{n} photos", n));
    ui.label(name);
    ui.label(format!("{}", tr("Inner")));
    ui.label(format!("{name}"));
    ui.label("—");
    ui.label("{}%");
    ui.label("R");
    ui.label(name.to_string());
    ui.button(tr(if on { "On" } else { "Off" }));
    egui::Id::new("Some salt");
    Command::new("git");
    Foo::text_button(ui, "id", "Assoc, not a helper", false);
    ui.other("Not a listed call");
    ui.checkbox(&mut on, tr("Check"));
    ui.checkbox("The checked value is argument 1", tr("Check"));
    // ui.label("In a comment");
    let s = "ui.label(\"In a string\")";
}
fn label(text: &str) {}
fn text_button(ui: &mut Ui, id: &str, label: &str, active: bool) { ui.label(label); }
"#;
        assert_eq!(found_in(source), []);
    }

    #[test]
    fn test_and_debug_items_are_skipped_and_the_code_after_them_is_not() {
        let source = r#"
fn a(ui: &mut Ui) {
    ui.label("Before");
}
#[cfg(test)]
mod tests {
    fn t(ui: &mut Ui) { ui.label("In a test module"); }
}
#[cfg(test)]
fn only_in_tests(ui: &mut Ui) { ui.label("In a test fn"); }
#[cfg(debug_assertions)]
fn only_in_debug(ui: &mut Ui) { ui.label("In a debug fn"); }
#[test]
fn a_test(ui: &mut Ui) { ui.label("In a #[test]"); }
#[cfg(not(test))]
fn kept(ui: &mut Ui) { ui.label("Not a test"); }
fn z(ui: &mut Ui) {
    let r = R {
        #[cfg(test)]
        extra: ui.label("Test field"),
        after: ui.label("After a test field"),
    };
}
"#;
        let texts: Vec<String> = found_in(source).into_iter().map(|(_, _, text)| text).collect();
        assert_eq!(texts, ["Before", "Not a test", "After a test field"]);
    }

    #[test]
    fn a_helper_is_a_sink_when_its_text_parameter_reaches_a_widget_without_tr() {
        let source = r#"
pub fn shows(ui: &mut Ui, id: &str, label: &str) { ui.label(label); }
pub fn translates(ui: &mut Ui, label: &str) { ui.label(tr(label)); }
fn forwards(ui: &mut Ui, caption: &str) { shows(ui, "id", caption); }
fn forwards_translated(ui: &mut Ui, caption: &str) { translates(ui, caption); }
fn forwards_late(ui: &mut Ui, caption: &str) { later(ui, caption); }
fn later(ui: &mut Ui, caption: &str) { ui.heading(caption); }
pub(crate) fn generic<'a, R: Fn(&mut Ui) -> R>(ui: &mut Ui, map: HashMap<String, u8>, text: String, add: R) { ui.heading(format!("{text}!")); }
pub fn wrapped(
    ui: &mut Ui,
    mut caption: &str,
) { RichText::new(caption); }
impl W {
    pub fn builder(mut self, hint: impl Into<String>) -> Self { self.edit.hint_text(hint); self }
    pub fn shown(&self, ctx: &Context, tip: &str) { ui.on_hover_text(tip); }
}
fn not_shown(ui: &mut Ui, text: &str) { measure(text); }
fn not_text(ui: &mut Ui, name: &str, text: &mut String, label: Option<&str>) { ui.label(name); ui.label(text); ui.label(label); }
pub fn atoms<'a>(ui: &mut Ui, atoms: impl egui::IntoAtoms<'a>) { ui.menu_button(atoms, |ui| {}); }
fn into_text(ui: &mut Ui, text: impl Into<egui::WidgetText>) { ui.label(text); }
"#;
        let sinks = Sinks::derive(&[("a.rs".to_string(), source.to_string())]);
        let derived = |name: &str, method: bool| sinks.args("a.rs", name, method);
        assert_eq!(derived("shows", false), [2]);
        assert_eq!(derived("translates", false), Vec::<usize>::new());
        assert_eq!(derived("forwards", false), [1]);
        assert_eq!(derived("forwards_translated", false), Vec::<usize>::new());
        assert_eq!(derived("forwards_late", false), [1], "found however the functions are ordered");
        assert_eq!(derived("generic", false), [2]);
        assert_eq!(derived("wrapped", false), [1]);
        assert_eq!(derived("builder", true), [0]);
        assert_eq!(derived("shown", true), [1]);
        assert_eq!(derived("shows", true), Vec::<usize>::new(), "a function is not a method");
        assert_eq!(derived("not_shown", false), Vec::<usize>::new());
        assert_eq!(derived("not_text", false), [1], "only the `&str` parameter");
        assert_eq!(derived("atoms", false), [1], "egui text types are text");
        assert_eq!(derived("into_text", false), [1]);
        // Another file sees the public helpers only.
        assert_eq!(sinks.args("b.rs", "shows", false), [2]);
        assert_eq!(sinks.args("b.rs", "forwards", false), Vec::<usize>::new());
    }

    #[test]
    fn a_helper_that_passes_its_text_to_this_crates_button_is_not_a_sink_but_egui_s_is() {
        let source = r#"
use crate::button::Button;
fn ours(ui: &mut Ui, label: &str) { ui.add(Button::new(label)); }
fn egui_s(ui: &mut Ui, label: &str) { ui.add(egui::Button::new(label)); }
"#;
        let sinks = Sinks::derive(&[("c.rs".to_string(), source.to_string())]);
        assert_eq!(sinks.args("c.rs", "ours", false), Vec::<usize>::new());
        assert_eq!(sinks.args("c.rs", "egui_s", false), [1]);
    }

    #[test]
    fn this_crates_button_and_the_one_argument_text_are_read_as_they_are() {
        // This crate's `Button` takes the English key; egui's does not.
        let crate_button = r#"
use crate::button::Button;
fn f(ui: &mut Ui) {
    ui.add(Button::new("Takes the key"));
    ui.add(egui::Button::new("Egui button"));
    slider.text("Slider caption");
    painter.text(pos, align, "Painted", font, color);
    ui.toggle_value(&mut on, "Toggle");
}
"#;
        let texts: Vec<(String, String)> = found_in(crate_button).into_iter().map(|(call, _, text)| (call, text)).collect();
        assert_eq!(
            texts,
            [("Button::new()", "Egui button"), (".text()", "Slider caption"), (".text()", "Painted"), (".toggle_value()", "Toggle")]
                .map(|(call, text)| (call.to_string(), text.to_string()))
        );
        // Without the import, `Button::new` is egui's.
        let egui_only = "fn f(ui: &mut Ui) { ui.add(Button::new(\"Egui button\")); }";
        assert_eq!(found_in(egui_only).len(), 1);
    }

    #[test]
    fn a_parameter_is_shown_only_when_it_reaches_the_widget_as_it_is() {
        let source = r#"
fn shows_ref(ui: &mut Ui, label: &str) { ui.label(&label.to_string()); }
fn shows_format(ui: &mut Ui, label: &str) { ui.label(format!("{label}: x")); }
fn only_feeds(ui: &mut Ui, field: &str) { ui.selected_text(choice_text(field, &cur)); }
fn own_row(ui: &mut Ui, label: &str) { ui.label(tr(label)); }
fn test_helper(ui: &mut Ui, label: &str) { ui.label(label); }
"#;
        let other = "pub fn own_row(ui: &mut Ui, label: &str) { ui.label(label); }\n#[cfg(test)]\nmod tests { pub fn in_tests(ui: &mut Ui, text: &str) { ui.label(text); } }";
        let sinks = Sinks::derive(&[("a.rs".to_string(), source.to_string()), ("b.rs".to_string(), other.to_string())]);
        assert_eq!(sinks.args("a.rs", "shows_ref", false), [1]);
        assert_eq!(sinks.args("a.rs", "shows_format", false), [1]);
        assert_eq!(sinks.args("a.rs", "only_feeds", false), Vec::<usize>::new());
        // `a.rs` has its own `own_row`, which translates: another file's public sink of that name does not apply to it.
        assert_eq!(sinks.args("a.rs", "own_row", false), Vec::<usize>::new());
        assert_eq!(sinks.args("c.rs", "own_row", false), [1]);
        // Items under `#[cfg(test)]` are not helpers.
        assert_eq!(sinks.args("c.rs", "in_tests", false), Vec::<usize>::new());
    }

    #[test]
    fn helpers_of_the_same_name_in_different_files_do_not_share_what_they_do_with_their_text() {
        // Both are public, so another file could reach them: what keeps a call from them is the file's own function of that name.
        // `a.rs` has its own `caption`, which translates; `b.rs`'s shows its text.
        let a = "pub fn caption(ui: &mut Ui, text: &str) { ui.label(tr(text)); }\nfn calls_a(ui: &mut Ui, text: &str) { caption(ui, text); }";
        let b = "pub fn caption(ui: &mut Ui, text: &str) { ui.label(text); }\nfn calls_b(ui: &mut Ui, text: &str) { caption(ui, text); }";
        // `c.rs`'s `row` shows its text; `d.rs` has a `row` that takes none, so `d.rs`'s call is not to it.
        let c = "pub fn row(ui: &mut Ui, label: &str) { ui.label(label); }";
        let d = "fn row(ui: &mut Ui, count: usize) {}\nfn wraps(ui: &mut Ui, label: &str) { row(ui, label); }";
        let files = ["a.rs", "b.rs", "c.rs", "d.rs"].map(String::from).into_iter().zip([a, b, c, d].map(String::from)).collect::<Vec<_>>();
        let sinks = Sinks::derive(&files);
        assert_eq!(sinks.args("a.rs", "calls_a", false), Vec::<usize>::new(), "its own `caption` translates");
        assert_eq!(sinks.args("b.rs", "calls_b", false), [1], "its own `caption` shows, whatever another file's does");
        assert_eq!(sinks.args("d.rs", "wraps", false), Vec::<usize>::new(), "`row` here shows nothing");
    }

    #[test]
    fn a_helper_that_gives_its_text_to_a_slider_is_a_sink() {
        let source = "fn slider(ui: &mut Ui, label: &str, v: &mut f32) { ui.add(Slider::new(v, 0.0..=1.0).text(label)); }";
        let sinks = Sinks::derive(&[("a.rs".to_string(), source.to_string())]);
        assert_eq!(sinks.args("a.rs", "slider", false), [1]);
    }

    /// The scan reads the repository's sources (a wrong folder would make the report empty and look clean).
    #[test]
    fn the_scan_reads_the_sources_and_derives_helpers_from_them() {
        let sources = read_sources();
        assert!(sources.len() > 60, "the scan read only {} files: it reads the wrong folders", sources.len());
        let sinks = Sinks::derive(&sources);
        let listed = CALLS.iter().map(Call::label).collect::<BTreeSet<_>>().len();
        assert!(sinks.labels().len() > listed, "only the {listed} listed calls are read: no helper was derived from the sources");
    }
}
