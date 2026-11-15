//! Help links (fork-owned): the small **?** beside a setting or a group of settings that opens the
//! online manual at the part about it.
//!
//! Every link names a *topic* from [`TOPICS`], one table of `topic → page#anchor` in the manual
//! (`docs/manual-site/src/content/docs/<page>.md(x)`, heading anchors as the site makes them). The
//! URL is the brand's docs address (`lcc_brand::DOCS`) + page + anchor, never a hard-coded product
//! URL. A test reads the manual's sources and checks that each topic's page and heading exist, so
//! a renamed heading fails the build instead of leaving a dead link.
//!
//! A click runs the `app.openHelp {topic}` command (so the CLI, the control channel and MCP can
//! open the same pages; without a topic it lists them). The icon is drawn where there is no
//! `LccApp` at hand (inside a group heading), so it only records the click; [`poll`] runs the
//! command once a frame.
//!
//! The icon (a ring with a question mark) is drawn in code: original work.

use egui::{Rect, Sense, Stroke, Ui, pos2, vec2};
use serde_json::{Value, json};

use crate::LccApp;
use crate::menus::UiCommand;
use crate::theme::Tokens;

/// The command a help link runs.
pub const COMMANDS: &[UiCommand] = &[("app.openHelp", "Open Help Topic", None, "")];

/// (topic, manual page `#` heading anchor). The page is the path under the manual's content folder
/// without its extension; an empty anchor opens the page at its top.
pub const TOPICS: &[(&str, &str)] = &[
    // Settings ▸ General
    ("layout", "reference/settings#layout-and-settings-categories"),
    ("gradingMode", "develop/colour#two-grading-modes"),
    ("catalog", "reference/settings#catalog"),
    ("startup", "reference/settings#startup-and-updates"),
    ("culling", "reference/settings#culling-and-dialogs"),
    ("externalEditing", "reference/settings#external-editing"),
    // Settings ▸ Import
    ("importWindow", "library/import#browse-without-importing"),
    ("rawDefaults", "reference/settings#raw-defaults"),
    ("cameraProfile", "reference/settings#camera-profile"),
    ("importMetadata", "reference/settings#metadata"),
    ("xmpSidecars", "reference/settings#xmp-sidecars"),
    ("folderPolling", "reference/settings#folder-polling-and-drag-and-drop"),
    ("autoImport", "reference/settings#auto-import"),
    ("lensProfiles", "develop/lens#where-profiles-come-from"),
    // Settings ▸ Folders
    ("folders", "reference/settings#folders"),
    // Settings ▸ Performance
    ("rendering", "platforms/performance#rendering"),
    ("exports", "reference/settings#exports"),
    ("thumbnailCache", "reference/settings#thumbnail-cache-and-local-folders"),
    ("backgroundPrep", "reference/settings#background-preparation"),
    ("smartPreviews", "reference/settings#smart-previews"),
    // Settings ▸ Display
    ("display", "reference/settings#display"),
    // Settings ▸ Interface
    ("appearance", "reference/settings#appearance"),
    ("gridAndFilmstrip", "reference/settings#filmstrip-grid-keywords-and-presets"),
    ("detailView", "reference/settings#detail-view-and-eye-focus-strip"),
    ("changeOverlay", "reference/settings#on-screen-overlay"),
    // Settings ▸ Slideshow
    ("slideshow", "reference/settings#slideshow"),
    // Settings ▸ Faces
    ("faces", "ai/faces"),
    ("faceScan", "reference/settings#face-scan"),
    // Settings ▸ Profiles
    ("profiles", "reference/settings#profiles"),
    // Settings ▸ Develop
    ("toolProfiles", "reference/settings#tool-profiles"),
    // Settings ▸ AI Denoise
    ("denoise", "reference/settings#ai-denoise"),
    ("denoiseModels", "ai/denoise#optional-models"),
    // Settings ▸ AI Models
    ("aiModels", "reference/settings#ai-models"),
    ("sam3", "ai/models#installing-sam-3"),
    ("semanticSearch", "reference/settings#semantic-search"),
    // Settings ▸ Generative Fill
    ("genfill", "reference/settings#generative-fill"),
    // Settings ▸ Shortcuts, Controls, Connections, Sync
    ("shortcuts", "reference/settings#shortcuts"),
    ("controls", "reference/settings#controls"),
    ("connections", "sync/immich#connect"),
    ("sync", "sync/devices#setup"),
];

/// The manual page and anchor of `topic`.
pub fn target(topic: &str) -> Option<&'static str> {
    TOPICS.iter().find(|(id, _)| *id == topic).map(|(_, page)| *page)
}

/// The web address of `topic`: the docs site, the page (as a folder: the site's URLs end in `/`),
/// then the heading's anchor. A brand without a docs site (`DOCS` falls back to the repository)
/// opens that address as it is.
pub fn url(topic: &str) -> Option<String> {
    let target = target(topic)?;
    if lcc_brand::DOCS == lcc_brand::REPOSITORY {
        return Some(lcc_brand::DOCS.to_string());
    }
    let (page, anchor) = target.split_once('#').unwrap_or((target, ""));
    let base = lcc_brand::DOCS.trim_end_matches('/');
    let anchor = if anchor.is_empty() { String::new() } else { format!("#{anchor}") };
    Some(format!("{base}/{page}/{anchor}"))
}

/// `app.openHelp {topic}`: opens the topic's page in the browser → `{topic, url}`. Without a topic,
/// the topics and their addresses → `{topics: [{topic, url}]}`.
pub fn run(app: &mut LccApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    if id != "app.openHelp" {
        return None;
    }
    let Some(topic) = p.get("topic").and_then(Value::as_str) else {
        let topics: Vec<Value> = TOPICS.iter().map(|(t, _)| json!({"topic": t, "url": url(t)})).collect();
        return Some(Ok(json!({ "topics": topics })));
    };
    let Some(link) = url(topic) else {
        return Some(Err(format!("app.openHelp: unknown topic `{topic}` (app.openHelp without a topic lists them)")));
    };
    Some(crate::links::open(app, &link).map(|_| json!({"topic": topic, "url": link})))
}

/// Where a click waits for [`poll`].
fn pending_id() -> egui::Id {
    egui::Id::new("help-link-pending")
}

/// Opens the topic a help link was clicked on this frame, if any (once a frame, from the app).
pub fn poll(app: &mut LccApp, ctx: &egui::Context) {
    let Some(topic) = ctx.data_mut(|d| d.remove_temp::<String>(pending_id())) else { return };
    if let Err(e) = app.run("app.openHelp", json!({ "topic": topic })) {
        app.toast_error(ctx, e);
    }
}

/// The icon's side (points).
pub const SIZE: f32 = 15.0;

/// A help link in line with what is before it (a row's controls, a checkbox), addressable as
/// `help:{topic}`.
pub fn icon(ui: &mut Ui, topic: &str) -> egui::Response {
    let (rect, _) = ui.allocate_exact_size(vec2(SIZE, SIZE), Sense::hover());
    draw(ui, rect, topic)
}

/// A help link just after `title` (a heading drawn as a label or painted text), on its line.
pub fn beside(ui: &mut Ui, title: Rect, topic: &str) -> egui::Response {
    let rect = Rect::from_center_size(pos2(title.right() + 6.0 + SIZE / 2.0, title.center().y), vec2(SIZE, SIZE));
    draw(ui, rect, topic)
}

fn draw(ui: &mut Ui, rect: Rect, topic: &str) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    let tip = crate::i18n::tr("Open the manual at this topic").to_string();
    let r = ui.interact(rect, ui.id().with(("help-link", topic)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
    r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Link, true, &tip));
    crate::widgets::register(ui.ctx(), format!("help:{topic}"), rect);
    if ui.is_rect_visible(rect) {
        let hot = r.hovered() || r.has_focus();
        let ink = if hot { t.text } else { t.text_dim };
        let p = ui.painter();
        let c = rect.center();
        let radius = SIZE / 2.0 - 1.0;
        if hot {
            p.circle_filled(c, radius, t.hover);
        }
        p.circle_stroke(c, radius, Stroke::new(1.0, ink));
        p.text(c + vec2(0.0, 0.5), egui::Align2::CENTER_CENTER, "?", t.semibold(10.0), ink);
        if r.has_focus() {
            p.circle_stroke(c, radius + 1.5, Stroke::new(1.0, t.accent.gamma_multiply(0.8)));
        }
    }
    if r.clicked() {
        let topic = topic.to_string();
        ui.ctx().data_mut(|d| d.insert_temp(pending_id(), topic));
    }
    r.on_hover_text(tip)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};

    /// The manual's content folder.
    fn content() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/manual-site/src/content/docs")
    }

    /// A heading's anchor as the site makes it (Astro's `github-slugger`): lower case, letters,
    /// digits, `-` and `_` kept, spaces to `-`, everything else dropped.
    fn slug(text: &str) -> String {
        text.to_lowercase()
            .chars()
            .filter_map(|c| match c {
                ' ' => Some('-'),
                c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c),
                _ => None,
            })
            .collect()
    }

    /// A heading's text as it reads: inline code, emphasis and link targets dropped.
    fn plain(heading: &str) -> String {
        let mut out = String::new();
        let mut rest = heading;
        while let Some(c) = rest.chars().next() {
            if c == ']' && rest.starts_with("](") {
                // a link's target: skip to its `)`
                rest = rest.find(')').and_then(|i| rest.get(i + 1..)).unwrap_or("");
                continue;
            }
            if !matches!(c, '`' | '*' | '[' | ']') {
                out.push(c);
            }
            rest = &rest[c.len_utf8()..];
        }
        out.trim().to_string()
    }

    /// The anchors of a manual page's headings (outside code blocks), repeats numbered `-1`, `-2`
    /// as the site does.
    fn anchors(source: &str) -> BTreeSet<String> {
        let mut seen: BTreeMap<String, usize> = BTreeMap::new();
        let mut out = BTreeSet::new();
        let mut fenced = false;
        let mut front = false;
        for (i, line) in source.lines().enumerate() {
            if i == 0 && line.trim() == "---" {
                front = true;
                continue;
            }
            if front {
                front = line.trim() != "---";
                continue;
            }
            if line.trim_start().starts_with("```") {
                fenced = !fenced;
                continue;
            }
            if fenced {
                continue;
            }
            let hashes = line.chars().take_while(|c| *c == '#').count();
            if !(1..=6).contains(&hashes) || !line[hashes..].starts_with(' ') {
                continue;
            }
            let base = slug(&plain(&line[hashes..]));
            let n = seen.entry(base.clone()).or_insert(0);
            out.insert(if *n == 0 { base.clone() } else { format!("{base}-{n}") });
            *n += 1;
        }
        out
    }

    fn page_source(page: &str) -> Option<String> {
        let dir = content();
        [format!("{page}.md"), format!("{page}.mdx"), format!("{page}/index.md"), format!("{page}/index.mdx")]
            .iter()
            .find_map(|f| std::fs::read_to_string(dir.join(f)).ok())
    }

    #[test]
    fn slugs_follow_the_site() {
        assert_eq!(slug(&plain("Installing SAM 3")), "installing-sam-3");
        assert_eq!(slug(&plain("Folder polling and drag and drop")), "folder-polling-and-drag-and-drop");
        assert_eq!(slug(&plain("Filmstrip, grid, keywords and presets")), "filmstrip-grid-keywords-and-presets");
        assert_eq!(slug(&plain("`view.toolProfile` and *friends*")), "viewtoolprofile-and-friends");
        let a = anchors("---\ntitle: x\n---\n## A\n```sh\n# not a heading\n```\n## A\n### B c\n");
        assert_eq!(a, ["a", "a-1", "b-c"].iter().map(|s| s.to_string()).collect());
    }

    /// Every topic names a manual page that exists and a heading on it: a renamed or removed
    /// heading fails here, not as a dead link in the app.
    #[test]
    fn every_topic_resolves_to_a_manual_heading() {
        let mut ids = BTreeSet::new();
        for (topic, target) in TOPICS {
            assert!(ids.insert(*topic), "topic {topic} is listed twice");
            let (page, anchor) = target.split_once('#').unwrap_or((target, ""));
            let source = page_source(page).unwrap_or_else(|| panic!("{topic}: no manual page {page} under {}", content().display()));
            if !anchor.is_empty() {
                let found = anchors(&source);
                assert!(found.contains(anchor), "{topic}: {page} has no heading with the anchor #{anchor}; it has {found:?}");
            }
        }
    }

    #[test]
    fn urls_are_the_docs_site_page_and_anchor() {
        let u = url("sam3").unwrap();
        assert!(u.starts_with("https://"), "{u}");
        if lcc_brand::DOCS != lcc_brand::REPOSITORY {
            assert!(u.starts_with(lcc_brand::DOCS.trim_end_matches('/')), "{u}");
            assert!(u.ends_with("/ai/models/#installing-sam-3"), "{u}");
        }
        assert!(url("no such topic").is_none());
    }

    #[test]
    fn the_command_opens_a_topic_and_lists_them() {
        let mut app = LccApp::new(lcc_engine::Session::with_demo(), crate::Services::default());
        let opened = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let o = opened.clone();
        app.services.open_url = Some(Box::new(move |u: &str| {
            o.lock().unwrap().push(u.to_string());
            Ok(())
        }));
        let r = app.run("app.openHelp", json!({"topic": "faceScan"})).unwrap();
        assert_eq!(opened.lock().unwrap().as_slice(), [url("faceScan").unwrap()]);
        assert_eq!(r["topic"], "faceScan");
        assert!(app.run("app.openHelp", json!({"topic": "nope"})).is_err());
        let list = app.run("app.openHelp", json!({})).unwrap();
        assert_eq!(list["topics"].as_array().map(Vec::len), Some(TOPICS.len()));
    }
}
