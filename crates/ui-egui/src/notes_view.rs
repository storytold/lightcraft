//! Release notes on screen: Help ▸ What's New and the update panel's notes.
//!
//! The notes are the app's Markdown subset: `## heading`, `### heading`, `- item`, prose, and inline `code`,
//! `**bold**` and `[text](url)`. [`flat`] draws a block of them; [`whats_new`] groups the joined What's New text
//! by its `## <date>` headings, one disclosure per date, and builds only as many as the window shows, adding a
//! batch when the scroll reaches the end of what is built.

use crate::theme::Tokens;
use egui::text::{LayoutJob, TextFormat};
use std::sync::OnceLock;

/// What a line of notes is made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Span<'a> {
    Text(&'a str),
    Code(&'a str),
    Bold(&'a str),
    Link { label: &'a str, url: &'a str },
}

/// `text` cut into its inline spans. Markup that isn't closed (a lone `**`, a backtick with no partner, `[a](`) stays
/// in the text as written.
pub fn spans(text: &str) -> Vec<Span<'_>> {
    let mut out = Vec::new();
    let (mut plain, mut i) = (0, 0);
    while i < text.len() {
        let found = match text.as_bytes().get(i) {
            Some(b'`') => closed(text, i + 1, "`").map(|(inner, end)| (Span::Code(inner), end)),
            Some(b'*') if text.get(i..).is_some_and(|r| r.starts_with("**")) => {
                closed(text, i + 2, "**").map(|(inner, end)| (Span::Bold(inner), end))
            }
            Some(b'[') => link_at(text, i),
            _ => None,
        };
        match found {
            Some((span, end)) => {
                if let Some(before) = text.get(plain..i).filter(|s| !s.is_empty()) {
                    out.push(Span::Text(before));
                }
                out.push(span);
                (plain, i) = (end, end);
            }
            None => i += 1,
        }
    }
    if let Some(rest) = text.get(plain..).filter(|s| !s.is_empty()) {
        out.push(Span::Text(rest));
    }
    out
}

/// The non-empty text from `from` up to the next `close`, and the index after `close`.
fn closed<'a>(text: &'a str, from: usize, close: &str) -> Option<(&'a str, usize)> {
    let rest = text.get(from..)?;
    let n = rest.find(close).filter(|n| *n > 0)?;
    Some((rest.get(..n)?, from + n + close.len()))
}

/// `[label](url)` starting at `i`, with a label and a URL that aren't empty. The URL has no spaces and may hold
/// balanced parentheses (`…/Foo_(bar)`); it ends at the first `)` that closes none of them.
fn link_at(text: &str, i: usize) -> Option<(Span<'_>, usize)> {
    let (label, after_label) = closed(text, i + 1, "](")?;
    let rest = text.get(after_label..)?;
    let mut depth = 0usize;
    let mut end = None;
    for (n, c) in rest.char_indices() {
        match c {
            '(' => depth += 1,
            ')' if depth == 0 => {
                end = Some(n);
                break;
            }
            ')' => depth -= 1,
            c if c.is_whitespace() => return None,
            _ => {}
        }
    }
    let n = end.filter(|n| *n > 0)?;
    Some((Span::Link { label, url: rest.get(..n)? }, after_label + n + 1))
}

/// `span`, or its label as text when it is a link the host won't open: not something that fails when clicked.
fn openable(span: Span<'_>) -> Span<'_> {
    match span {
        Span::Link { label, url } if crate::links::check_browser_url(url).is_err() => Span::Text(label),
        other => other,
    }
}

/// Opens a link the notes' reader clicked, through the host; a host that can't says so in a toast.
pub fn open_link(app: &mut crate::LccApp, ctx: &egui::Context, url: &str) {
    if let Err(e) = crate::links::open(app, url) {
        app.toast_error(ctx, e);
    }
}

/// Sizes and gaps a block of notes is drawn with.
#[derive(Debug, Clone, Copy)]
pub struct Look {
    /// `## ` heading size, and the gap above it.
    pub h2: (f32, f32),
    /// `### ` heading size, and the gap above it.
    pub h3: (f32, f32),
}

/// What's New's own look.
pub const WHATS_NEW_LOOK: Look = Look { h2: (14.0, 8.0), h3: (12.5, 6.0) };
/// The update panel's, a little tighter.
pub const UPDATE_LOOK: Look = Look { h2: (13.5, 6.0), h3: (12.5, 4.0) };

/// Draws lines of notes and collects the link a click opened.
struct Drawer<'a> {
    t: &'a Tokens,
    look: Look,
    /// How many links were drawn (their `link:notes:<n>` ids).
    links: &'a mut u32,
    clicked: Option<String>,
}

impl Drawer<'_> {
    fn line(&mut self, ui: &mut egui::Ui, line: &str) {
        use crate::i18n::tr;
        let t = self.t;
        let l = line.trim_end();
        if let Some(h) = l.strip_prefix("### ") {
            ui.add_space(self.look.h3.1);
            ui.label(egui::RichText::new(tr(h)).font(t.semibold(self.look.h3.0)).color(t.text));
        } else if let Some(h) = l.strip_prefix("## ") {
            ui.add_space(self.look.h2.1);
            ui.label(egui::RichText::new(tr(h)).font(t.semibold(self.look.h2.0)).color(t.text));
        } else if l.starts_with("# ") || l.is_empty() {
        } else if let Some(b) = l.strip_prefix("- ") {
            self.inline(ui, tr(b), true);
        } else {
            self.inline(ui, tr(l.trim()), false);
        }
    }

    /// One item or paragraph with its inline spans. Without a link it is one label; with one, a wrapped row of
    /// labels and links (a label can't hold a clickable part).
    fn inline(&mut self, ui: &mut egui::Ui, text: &str, bullet: bool) {
        let t = self.t;
        let body = egui::TextStyle::Body.resolve(ui.style());
        let parts: Vec<Span<'_>> = spans(text).into_iter().map(openable).collect();
        let bullet = bullet.then_some("•  ");
        if !parts.iter().any(|s| matches!(s, Span::Link { .. })) {
            let mut job = LayoutJob::default();
            let mut add = |s: &str, font: egui::FontId, mono: bool| {
                let mut f = TextFormat::simple(font, t.text_label);
                if mono {
                    f.background = t.text_dim.gamma_multiply(0.15);
                }
                job.append(s, 0.0, f);
            };
            if let Some(b) = bullet {
                add(b, body.clone(), false);
            }
            for s in &parts {
                match *s {
                    Span::Text(s) | Span::Link { label: s, .. } => add(s, body.clone(), false),
                    Span::Code(s) => add(s, egui::FontId::monospace(body.size * 0.92), true),
                    Span::Bold(s) => add(s, t.semibold(body.size), false),
                }
            }
            job.wrap.max_width = ui.available_width();
            ui.label(job);
            return;
        }
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            if let Some(b) = bullet {
                ui.label(egui::RichText::new(b).color(t.text_label));
            }
            for s in &parts {
                match *s {
                    Span::Text(s) => drop(ui.label(egui::RichText::new(s).color(t.text_label))),
                    Span::Code(s) => drop(
                        ui.label(
                            egui::RichText::new(s)
                                .monospace()
                                .size(body.size * 0.92)
                                .background_color(t.text_dim.gamma_multiply(0.15))
                                .color(t.text_label),
                        ),
                    ),
                    Span::Bold(s) => drop(ui.label(egui::RichText::new(s).font(t.semibold(body.size)).color(t.text_label))),
                    Span::Link { label, url } => {
                        let r = ui.link(label).on_hover_text(url);
                        crate::widgets::register(ui.ctx(), format!("link:notes:{}", *self.links), r.rect);
                        *self.links += 1;
                        if r.clicked() {
                            self.clicked = Some(url.to_string());
                        }
                    }
                }
            }
        });
    }
}

/// `lines` of notes drawn one after the other (the update panel's). Returns the URL of a link that was clicked.
pub fn flat<'a>(ui: &mut egui::Ui, t: &Tokens, look: Look, lines: impl Iterator<Item = &'a str>) -> Option<String> {
    let mut links = 0;
    let mut d = Drawer { t, look, links: &mut links, clicked: None };
    ui.scope(|ui| {
        for line in lines {
            d.line(ui, line);
        }
    });
    d.clicked
}

/// One `## <date>` section of the joined What's New: its heading and the lines under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section<'a> {
    /// The text after `## `: a date, or whatever upstream's file titled an undated section; empty for lines
    /// that come before any `## ` heading.
    pub heading: &'a str,
    pub lines: Vec<&'a str>,
}

/// `text` cut at its `## ` headings. The `# ` title and blank lines are left out; nothing else is lost.
pub fn sections(text: &str) -> Vec<Section<'_>> {
    let mut out: Vec<Section<'_>> = Vec::new();
    for line in text.lines().map(str::trim_end) {
        if let Some(h) = line.strip_prefix("## ") {
            out.push(Section { heading: h.trim(), lines: Vec::new() });
        } else if line.starts_with("# ") || line.is_empty() {
        } else {
            if out.is_empty() {
                out.push(Section { heading: "", lines: Vec::new() });
            }
            if let Some(s) = out.last_mut() {
                s.lines.push(line);
            }
        }
    }
    out
}

/// What's New's sections, cut once.
pub(crate) fn parsed() -> &'static [Section<'static>] {
    static SECTIONS: OnceLock<Vec<Section<'static>>> = OnceLock::new();
    SECTIONS.get_or_init(|| sections(crate::panels::dialogs::WHATS_NEW))
}

/// How many sections are built when the dialog opens.
pub const FIRST_BATCH: usize = 8;
/// How many more are built when the scroll reaches the end of the built ones.
pub const NEXT_BATCH: usize = 8;

fn loaded_id(open: u64) -> egui::Id {
    egui::Id::new(("whats-new-loaded", open))
}

/// How many What's New sections are built in the dialog opened as `open` (what [`whats_new`] last drew).
#[cfg(test)]
pub fn loaded(ctx: &egui::Context, open: u64) -> usize {
    ctx.data(|d| d.get_temp::<usize>(loaded_id(open))).unwrap_or(0)
}

/// How many sections What's New holds.
#[cfg(test)]
pub fn total() -> usize {
    parsed().len()
}

/// Help ▸ What's New: a disclosure per date, the newest open, built a batch at a time as the scroll reaches the
/// end. `open` tells one opening of the dialog from the next (each starts over). Returns the URL of a clicked link.
pub fn whats_new(ui: &mut egui::Ui, t: &Tokens, open: u64) -> Option<String> {
    sections_view(ui, t, open, parsed())
}

pub(crate) fn sections_view(ui: &mut egui::Ui, t: &Tokens, open: u64, all: &[Section<'_>]) -> Option<String> {
    let id = loaded_id(open);
    let built = ui.data(|d| d.get_temp::<usize>(id)).unwrap_or(FIRST_BATCH).min(all.len());
    let mut links = 0;
    let mut d = Drawer { t, look: WHATS_NEW_LOOK, links: &mut links, clicked: None };
    for (n, section) in all.iter().take(built).enumerate() {
        if section.heading.is_empty() {
            for line in &section.lines {
                d.line(ui, line);
            }
            continue;
        }
        let title = egui::RichText::new(crate::i18n::tr(section.heading)).font(t.semibold(WHATS_NEW_LOOK.h2.0)).color(t.text);
        let r = egui::CollapsingHeader::new(title).id_salt((id, n)).default_open(n == 0).show(ui, |ui| {
            for line in &section.lines {
                d.line(ui, line);
            }
        });
        crate::widgets::register(ui.ctx(), format!("disclosure:whatsNew-{n}"), r.header_response.rect);
    }
    let mut now = built;
    if built < all.len() {
        // the end of what is built: once it is in view, build more
        let (end, _) = ui.allocate_exact_size(egui::vec2(1.0, 1.0), egui::Sense::hover());
        if ui.is_rect_visible(end) {
            now = (built + NEXT_BATCH).min(all.len());
            ui.ctx().request_repaint();
        }
    }
    ui.data_mut(|data| data.insert_temp(id, now));
    d.clicked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_cut_code_bold_and_links() {
        assert_eq!(
            spans("Use `Cmd+K` for **quick** [help](https://example.com/a) now"),
            vec![
                Span::Text("Use "),
                Span::Code("Cmd+K"),
                Span::Text(" for "),
                Span::Bold("quick"),
                Span::Text(" "),
                Span::Link { label: "help", url: "https://example.com/a" },
                Span::Text(" now"),
            ]
        );
        assert_eq!(spans("plain"), vec![Span::Text("plain")]);
        assert!(spans("").is_empty());
    }

    #[test]
    fn unclosed_or_empty_markup_stays_as_written() {
        for s in ["a ** b", "a ` b", "[a](", "[a](b", "[](x)", "[a]()", "****", "``", "[a](b c)", "**", "`", "[", "]("] {
            let joined: String = spans(s)
                .iter()
                .map(|p| match p {
                    Span::Text(t) => t.to_string(),
                    other => panic!("{s:?} made {other:?}"),
                })
                .collect();
            assert_eq!(joined, s, "{s:?} is shown as written");
        }
    }

    /// The spans, written back with their markers.
    fn rewritten(s: &str) -> String {
        spans(s)
            .iter()
            .map(|p| match p {
                Span::Text(t) => t.to_string(),
                Span::Code(c) => format!("`{c}`"),
                Span::Bold(b) => format!("**{b}**"),
                Span::Link { label, url } => format!("[{label}]({url})"),
            })
            .collect()
    }

    #[test]
    fn spans_lose_nothing_and_never_split_a_character() {
        for s in [
            "é`é`é",
            "**é**é",
            "[é](é)",
            "日本`語",
            "`日本語`**語**[語](語)",
            "é[é]é(é)",
            "***é",
            "a **b** `c` [d](e) f",
            "[a](b)(c)",
            "[[a](b)",
            "[a]](b)",
            "`**a**`",
            "**`a`**",
            "[a](b)[c](d)",
            "(((",
            ")))",
            "[a](((b)",
            "x\ty",
            "**a** **",
            "` ` `",
        ] {
            assert_eq!(rewritten(s), s, "{s:?} comes back as written");
        }
    }

    #[test]
    fn a_link_url_may_hold_balanced_parentheses() {
        assert_eq!(
            spans("see [Foo](https://en.wikipedia.org/wiki/Foo_(bar)) now"),
            vec![Span::Text("see "), Span::Link { label: "Foo", url: "https://en.wikipedia.org/wiki/Foo_(bar)" }, Span::Text(" now")]
        );
        assert!(spans("[a](b(c)").iter().all(|p| matches!(p, Span::Text(_))), "an unbalanced URL is not a link");
    }

    #[test]
    fn sections_keep_every_line_under_their_heading() {
        let text = "# Title\n\n## 2026-11-02\n\n### A\n- one\n\n## 2026-11-01\n### B\n- two\nprose\n";
        let s = sections(text);
        assert_eq!(s.iter().map(|s| s.heading).collect::<Vec<_>>(), ["2026-11-02", "2026-11-01"]);
        assert_eq!(s[0].lines, ["### A", "- one"]);
        assert_eq!(s[1].lines, ["### B", "- two", "prose"]);
        let loose = sections("### A\n- x\n## Later\n- y");
        assert_eq!((loose[0].heading, loose[0].lines.len()), ("", 2), "lines before any heading still show");
        assert_eq!(loose[1].heading, "Later");
    }

    #[test]
    fn the_joined_whats_new_is_cut_into_dated_sections_without_loss() {
        let all = parsed();
        assert!(all.len() > FIRST_BATCH + NEXT_BATCH, "more than a few batches: {}", all.len());
        let shown: usize = all.iter().map(|s| s.lines.len()).sum();
        let written = crate::panels::dialogs::WHATS_NEW.lines().filter(|l| !l.is_empty() && !l.starts_with("# ") && !l.starts_with("## ")).count();
        assert_eq!(shown, written, "every line is in some section");
    }
}
