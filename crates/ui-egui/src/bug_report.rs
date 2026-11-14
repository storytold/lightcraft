//! Help ▸ Report a Bug… (issue #1015): a bug report as a text file to attach and a pre-filled GitHub
//! issue. It holds what happened, the System Info rows and the logs. LightCraft uploads nothing itself:
//! the file is written next to the logs and shown in the file manager, and the issue opens in the
//! browser with its text in the link (so that text reaches GitHub's page), for the person to check
//! and submit there themselves.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::LightcraftApp;

/// The longest pre-filled issue link: GitHub and browsers cut longer ones.
pub const MAX_URL: usize = 8000;
/// Lines of this run's log quoted in the issue itself (the report file has the whole log).
pub const EXCERPT_LINES: usize = 40;
/// The most of one log a report file takes (its end): logs are capped at 16 MB.
const MAX_LOG_BYTES: u64 = 4 << 20;

/// The choices in the Report a Bug dialog.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ReportOptions {
    /// What happened, in the reporter's words.
    pub what: String,
    /// Also attach the logs of the two runs before this one.
    pub previous_logs: bool,
    /// Replace the home folder with `~` and photo file names with `<photo>`.
    pub private: bool,
}

impl Default for ReportOptions {
    fn default() -> Self {
        ReportOptions { what: String::new(), previous_logs: false, private: true }
    }
}

/// One log the report can take: its file name, size and last lines (read when the dialog opens).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LogInfo {
    pub name: String,
    pub path: String,
    pub bytes: u64,
    /// The last [`EXCERPT_LINES`] lines.
    pub tail: String,
    /// This run's log (the others are the two runs before it).
    #[serde(default)]
    pub current: bool,
    /// What the report file takes from it (its last [`MAX_LOG_BYTES`]), read when the dialog opened, so the
    /// file holds what the preview described.
    #[serde(skip)]
    pub text: String,
}

/// This run's log (`log_file`) and the two before it (`lightcraft.1.log`, `lightcraft.2.log`), those
/// that exist. None without a log file (the web, `--memory`).
pub fn logs(log_file: Option<&str>) -> Vec<LogInfo> {
    let Some(current) = log_file.map(std::path::Path::new) else { return Vec::new() };
    let dir = current.parent().unwrap_or(std::path::Path::new("."));
    let paths = [current.to_path_buf(), dir.join("lightcraft.1.log"), dir.join("lightcraft.2.log")];
    paths
        .iter()
        .filter_map(|path| {
            let text = read_end(path, MAX_LOG_BYTES)?;
            let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(text.len() as u64);
            let name = path.file_name()?.to_string_lossy().to_string();
            let tail = last_lines(&text, EXCERPT_LINES).to_string();
            Some(LogInfo { name, path: path.to_string_lossy().to_string(), bytes, tail, current: path.as_path() == current, text })
        })
        .collect()
}

/// The last `max` bytes of a file as text (from the first whole line in them); None when it can't be read.
fn read_end(path: &std::path::Path, max: u64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(max);
    // one byte before the cut tells whether it falls inside a line
    let from = start.saturating_sub(1);
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut bytes = Vec::new();
    file.take(max + (start - from)).read_to_end(&mut bytes).ok()?;
    let at_line_start = start == 0 || bytes.first() == Some(&b'\n');
    let text = String::from_utf8_lossy(bytes.get((start - from) as usize..).unwrap_or(&[])).into_owned();
    if at_line_start {
        return Some(text);
    }
    // (cut in the middle of a line: start at the next one)
    Some(text.split_once('\n').map_or(text.clone(), |(_, rest)| rest.to_string()))
}

/// The last `n` lines of `text`.
fn last_lines(text: &str, n: usize) -> &str {
    let text = text.trim_end_matches('\n');
    let mut start = text.len();
    for _ in 0..n {
        match text.get(..start).and_then(|t| t.rfind('\n')) {
            Some(i) => start = i,
            None => return text,
        }
    }
    text.get(start + 1..).unwrap_or(text)
}

/// The home folder, for [`redact`].
pub fn home_dir() -> Option<String> {
    ["HOME", "USERPROFILE"].iter().filter_map(|v| std::env::var(v).ok()).find(|h| h.trim_end_matches(['/', '\\']).len() > 1)
}

/// `text` with the home folder replaced by `~` and the name of every photo file (or its sidecar) by
/// `<photo>`, its extension kept: `/Users/ann/Pictures/Ann birthday.CR2` → `~/Pictures/<photo>.CR2`.
pub fn redact(text: &str, home: Option<&str>) -> String {
    let mut out = text.to_string();
    if let Some(home) = home.map(|h| h.trim_end_matches(['/', '\\'])).filter(|h| h.len() > 1) {
        out = out.replace(home, "~");
        let slashed = home.replace('\\', "/");
        if slashed != home {
            out = out.replace(&slashed, "~");
            // a Windows path as `{:?}` writes it: `C:\\Users\\ann`
            out = out.replace(&home.replace('\\', "\\\\"), "~");
        }
    }
    hide_photo_names(&out)
}

/// Characters that end a file name on the left besides a path separator and a space.
fn separates(c: char) -> bool {
    matches!(c, '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | ',' | ';' | '|' | '=' | '\n' | '\t')
}

fn is_photo_extension(ext: &str) -> bool {
    let ext = ext.to_ascii_lowercase();
    ext == "xmp" || lightcraft_engine::import::EXTENSIONS.contains(&ext.as_str())
}

/// Every `name.ext` with a photo (or sidecar) extension becomes `<photo>.ext`. After a path separator
/// the name runs back to it, spaces and all (`Pictures/Ann birthday.CR2`); inside quotes it runs back
/// to the opening quote (`"Ann birthday.CR2"`); elsewhere it starts after the last space
/// (`sidecar IMG_0001.xmp`).
fn hide_photo_names(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    // where the name before a '.' would start: after the last path separator while no other break
    // came after it, else after the last space or break; never before the end of the last name hidden
    let (mut after_path, mut after_space, mut hidden_end, mut copied) = (None::<usize>, 0usize, 0usize, 0usize);
    // after an opening `"` or `` ` `` while it is open: a quoted name without a folder is hidden whole
    let mut after_quote = None::<usize>;
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    for (i, &(at, c)) in chars.iter().enumerate() {
        let next = at + c.len_utf8();
        // punctuation inside a path's last name is part of the name (`Client (1).CR2`, `[final].jpg`,
        // `O'Brien.CR2`) until a name has been hidden there; after that it ends a word again
        let in_basename = after_path.is_some_and(|p| hidden_end <= p);
        // an apostrophe between letters is part of a word anywhere (`O'Brien.CR2`)
        let in_word = c == '\''
            && i > 0
            && chars.get(i - 1).is_some_and(|(_, l)| l.is_alphanumeric())
            && chars.get(i + 1).is_some_and(|(_, r)| r.is_alphanumeric());
        if c == '/' || c == '\\' {
            after_path = Some(next);
            after_space = next;
        } else if separates(c)
            && !in_word
            && !(in_basename && matches!(c, '(' | ')' | '[' | ']' | '{' | '}' | '\'' | ',' | ';' | '=' | '"' | '`' | '<' | '>' | '|'))
        {
            if c == '"' || c == '`' {
                after_quote = if after_quote.is_some() { None } else { Some(next) };
            }
            after_path = None;
            after_space = next;
        } else if c.is_whitespace() {
            after_space = next;
        } else if c == '.' {
            let ext_len = chars.get(i + 1..).unwrap_or(&[]).iter().take_while(|(_, e)| e.is_ascii_alphanumeric()).count();
            let end = chars.get(i + 1 + ext_len).map_or(text.len(), |(b, _)| *b);
            let ext = text.get(next..end).unwrap_or("");
            // a path's last name runs back to its separator; once a name there was hidden, a later
            // name starts after the last space like any word
            let base = after_path.filter(|p| hidden_end <= *p).or(after_quote).unwrap_or(after_space);
            let start = base.max(hidden_end).max(copied);
            let name = text.get(start..at).unwrap_or("");
            if !ext.is_empty() && is_photo_extension(ext) && !name.trim().is_empty() {
                out.push_str(text.get(copied..start).unwrap_or(""));
                out.push_str("<photo>");
                copied = at;
                hidden_end = end;
            }
        }
    }
    out.push_str(text.get(copied..).unwrap_or(""));
    out
}

/// `text` percent-encoded for a URL query value (RFC 3986 unreserved characters kept).
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The `key: value` lines of the system rows.
fn system_text(rows: &[(String, String)]) -> String {
    rows.iter().map(|(k, v)| format!("{k}: {v}\n")).collect()
}

/// The issue's title: "Bug: " and the first line of what happened, shortened.
fn title(what: &str) -> String {
    let first = what.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    if first.is_empty() {
        return "Bug report".into();
    }
    let short: String = first.chars().take(80).collect();
    format!("Bug: {short}{}", if first.chars().count() > 80 { "…" } else { "" })
}

/// A pre-filled GitHub issue: its title, its text (what happened, the system rows and the end of
/// this run's log) and the link that opens it, at most [`MAX_URL`] long: the log loses lines from
/// its start, then what happened is shortened, until it fits. `attached`: a report file goes with it.
#[derive(Clone, Debug, PartialEq)]
pub struct Issue {
    pub title: String,
    pub body: String,
    pub url: String,
}

pub fn issue(rows: &[(String, String)], opts: &ReportOptions, log: Option<&LogInfo>, attached: bool) -> Issue {
    let home = home_dir();
    let private = |t: &str| if opts.private { redact(t, home.as_deref()) } else { t.to_string() };
    issue_with(rows, opts, log, attached, &private)
}

fn issue_with(rows: &[(String, String)], opts: &ReportOptions, log: Option<&LogInfo>, attached: bool, private: &dyn Fn(&str) -> String) -> Issue {
    let system = private(&system_text(rows));
    let log_lines: Vec<String> = log.map(|l| private(&l.tail).lines().map(str::to_string).collect()).unwrap_or_default();
    let what = private(opts.what.trim());
    let title = title(&what);
    let body_of = |what: &str, lines: &[String]| {
        let mut b =
            format!("### What happened\n\n{}\n\n### System\n\n```\n{system}```\n", if what.is_empty() { "(describe what happened)" } else { what });
        if let (Some(log), false) = (log, lines.is_empty()) {
            b.push_str(&format!("\n### End of {}\n\n```\n{}\n```\n", log.name, lines.join("\n")));
        }
        if attached {
            b.push_str("\nThe full logs are in the attached `lightcraft-report-….txt`.\n");
        }
        b
    };
    let url_of = |title: &str, body: &str| format!("{}?title={}&body={}", crate::links::FEEDBACK, encode(title), encode(body));
    let mut lines = log_lines.as_slice();
    let mut what = what;
    let mut shortened = false;
    let note = if attached { " … (shortened to fit the link: the full text is in the report file)" } else { " … (shortened to fit the link)" };
    loop {
        let shown = if shortened { format!("{what}{note}") } else { what.clone() };
        let body = body_of(&shown, lines);
        let url = url_of(&title, &body);
        if url.len() <= MAX_URL {
            return Issue { title, body, url };
        }
        if let Some(rest) = lines.get(1..) {
            lines = rest;
        } else if !what.is_empty() {
            // a very long description: keep its start, and say so (the report file, when there is one, has
            // all of it)
            let keep = what.chars().count() * 9 / 10;
            what = what.chars().take(keep).collect();
            shortened = true;
        } else {
            let url = url_of(&title, "");
            return Issue { title, url, body: String::new() };
        }
    }
}

/// The report file's text: what happened, the system rows and each log (up to its last 4 MB, which its name
/// says when it is longer).
pub fn file_text(rows: &[(String, String)], opts: &ReportOptions, logs: &[(String, String)], when: &str) -> String {
    let home = home_dir();
    let private = |t: &str| if opts.private { redact(t, home.as_deref()) } else { t.to_string() };
    let mut out = format!(
        "LightCraft bug report, {when}\n\n== What happened ==\n{}\n\n== System ==\n{}",
        private(opts.what.trim()),
        private(&system_text(rows))
    );
    for (name, text) in logs {
        out.push_str(&format!("\n== {name} ==\n{}", private(text)));
        if !out.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

/// The logs a report takes: this run's, and the two before it when asked for.
pub fn chosen(logs: &[LogInfo], opts: &ReportOptions) -> Vec<LogInfo> {
    logs.iter().filter(|l| l.current || opts.previous_logs).cloned().collect()
}

/// The System Info rows (Help ▸ System Info…), also the system part of a bug report.
pub fn system_rows(app: &mut LightcraftApp) -> Vec<(String, String)> {
    let info = app.session.execute("library.info", &json!({})).unwrap_or_default();
    let gpu = (lightcraft_engine::gpu::ready() && lightcraft_engine::gpu::available()).then(lightcraft_engine::gpu::adapter_name).flatten();
    let mb = |b: u64| format!("{:.0} MB", b as f64 / (1u64 << 20) as f64);
    let mut rows = vec![
        ("Version".to_string(), env!("CARGO_PKG_VERSION").to_string()),
        ("System".to_string(), format!("{} ({})", std::env::consts::OS, std::env::consts::ARCH)),
        ("CPU threads".to_string(), std::thread::available_parallelism().map(|n| n.get().to_string()).unwrap_or_else(|_| "?".into())),
        (
            "GPU".to_string(),
            gpu.unwrap_or_else(|| match lightcraft_engine::gpu::unavailable_reason() {
                Some(why) => crate::i18n::tr_format!("none (CPU rendering): {why}", why = why),
                None => "none (CPU rendering)".into(),
            }),
        ),
        ("GPU rendering".to_string(), if app.ui.settings.gpu { "on".into() } else { "off".into() }),
        ("Memory budget".to_string(), mb(lightcraft_engine::memory::default_budget() as u64)),
        (
            "Preview size".to_string(),
            match app.ui.settings.preview_limit {
                0 => "automatic".to_string(),
                n => format!("{n} px"),
            },
        ),
        ("Photos".to_string(), info["photos"].to_string()),
        ("Albums".to_string(), info["albums"].to_string()),
    ];
    if let Some(dir) = info["dir"].as_str().or(info["path"].as_str()) {
        rows.push(("Library".into(), dir.to_string()));
    }
    rows.push(("Frame time".into(), format!("{:.1} ms ({:.0} fps)", app.perf.frame_ms, app.perf.fps)));
    rows.push((
        "Frame update".into(),
        format!("{:.1} ms (logic {:.1} ms; slowest {:.0} ms)", app.perf.update_ms, app.perf.logic_ms, app.perf.max_update_ms),
    ));
    rows.push(("Last loupe render".into(), format!("{:.0} ms", app.renderer.last_main_ms)));
    if let Some(f) = lightcraft_engine::gpu::last_fallback() {
        rows.push(("Last GPU fallback".into(), f));
    }
    rows
}

/// `app.reportBug`: open the dialog, or with `create: true` (agents, tests) write the report and open
/// the issue without it. Params: the [`ReportOptions`] (`what`, `previousLogs`, `private`).
pub fn command(app: &mut LightcraftApp, p: &Value) -> Result<Value, String> {
    let opts: ReportOptions = if p.is_object() { serde_json::from_value(p.clone()).map_err(|e| e.to_string())? } else { ReportOptions::default() };
    let rows = system_rows(app);
    let logs = logs(app.services.log_file.as_deref());
    if p.get("create").and_then(Value::as_bool).unwrap_or(false) {
        return create(app, &opts, &rows, &logs);
    }
    let r = json!({"logs": logs.iter().map(|l| json!({"name": l.name, "bytes": l.bytes})).collect::<Vec<_>>()});
    app.ui.dialog = Some(crate::state::Dialog::ReportBug { opts, rows, logs });
    Ok(r)
}

/// Write the report file next to the logs (when there are logs), show it in the file manager and
/// open the pre-filled issue in the browser. Returns the file's path, the link and the logs taken.
pub fn create(app: &mut LightcraftApp, opts: &ReportOptions, rows: &[(String, String)], logs: &[LogInfo]) -> Result<Value, String> {
    let logs = chosen(logs, opts);
    let when = (app.session.clock)();
    // a report file only when there are logs to put in it: what the dialog's preview says
    let stamp: String = when.chars().filter(|c| c.is_ascii_digit()).take(14).collect();
    let file = app.services.log_file.as_deref().filter(|_| !logs.is_empty()).map(std::path::Path::new).and_then(|log| log.parent());
    let mut path = None;
    if let Some(file) = &file {
        // the text read when the dialog opened (what its preview described); read now only for a report made
        // without the dialog. A log longer than the limit is named with the part it gives.
        let full: Vec<(String, String)> = logs
            .iter()
            .map(|l| {
                let text =
                    if l.text.is_empty() { read_end(std::path::Path::new(&l.path), MAX_LOG_BYTES).unwrap_or_default() } else { l.text.clone() };
                let name = if l.bytes > MAX_LOG_BYTES {
                    format!("{} (its last {} MB of {:.0} MB)", l.name, MAX_LOG_BYTES >> 20, l.bytes as f64 / f64::from(1u32 << 20))
                } else {
                    l.name.clone()
                };
                (name, text)
            })
            .collect();
        let text = file_text(rows, opts, &full, &when);
        let p = write_new_report(file, &stamp, text.as_bytes())?;
        if let Some(reveal) = app.services.reveal.as_mut() {
            reveal(&p)?;
        }
        path = Some(p);
    }
    let issue = issue(rows, opts, logs.first(), path.is_some());
    if let Some(open) = app.services.open_url.as_mut() {
        open(&issue.url)?;
    }
    Ok(json!({"path": path, "url": issue.url, "title": issue.title, "logs": logs.iter().map(|l| &l.name).collect::<Vec<_>>()}))
}

/// Write a report into `dir` under a name no file has: `lightcraft-report-<stamp>.txt`, else with
/// `-2`, `-3`… (two reports in one second). Never replaces a file, even one that appears meanwhile.
fn write_new_report(dir: &std::path::Path, stamp: &str, bytes: &[u8]) -> Result<String, String> {
    for n in 1..=99 {
        let name = if n == 1 { format!("lightcraft-report-{stamp}.txt") } else { format!("lightcraft-report-{stamp}-{n}.txt") };
        let path = dir.join(name);
        let p = path.to_string_lossy().to_string();
        match lightcraft_engine::export::write_file_new(&p, bytes) {
            Ok(()) => return Ok(p),
            Err(_) if path.exists() => continue,
            Err(e) => return Err(e),
        }
    }
    Err(format!("{}: no free name for the report file", dir.display()))
}

/// The Report a Bug dialog's body: what happened, the two choices, and a preview of the issue and the
/// report file.
pub fn dialog_body(ui: &mut egui::Ui, t: &crate::theme::Tokens, opts: &mut ReportOptions, rows: &[(String, String)], logs: &[LogInfo]) {
    use crate::i18n::tr;
    ui.set_min_width(520.0);
    ui.label(
        egui::RichText::new(tr(
            "LightCraft uploads nothing itself. Create Report saves a report file to attach, and opens a new GitHub issue in your browser filled in with what is shown below: check it there before you submit it.",
        ))
        .color(t.text_dim),
    );
    crate::text_field::TextField::multiline("field:reportBugWhat", &mut opts.what)
        .hint(tr("What did you do, what did you expect, and what happened instead?"))
        .rows(4)
        .width(ui.available_width())
        .show(ui);
    let r = ui.checkbox(&mut opts.private, tr("Hide my home folder and photo file names"));
    crate::widgets::register(ui, "check:reportBugPrivate", r.rect);
    if logs.len() > 1 {
        let r = ui.checkbox(&mut opts.previous_logs, tr("Also attach the logs of the two previous runs"));
        crate::widgets::register(ui, "check:reportBugPrevious", r.rect);
    }
    ui.label(
        egui::RichText::new(tr("A raw file? Name the camera model, and share the file through a link (Google Drive, Dropbox…).")).color(t.text_dim),
    );
    let chosen = chosen(logs, opts);
    let issue = issue(rows, opts, chosen.first(), !chosen.is_empty());
    ui.label(egui::RichText::new(tr("The new issue")).color(t.text_label));
    egui::ScrollArea::vertical().id_salt("reportBugPreview").max_height(200.0).show(ui, |ui| {
        ui.add(egui::Label::new(egui::RichText::new(issue.body.as_str()).monospace().size(11.0).color(t.text_label)).wrap());
    });
    let files = if chosen.is_empty() {
        tr("This session keeps no log, so there is no report file: the issue holds the system info only.").to_string()
    } else {
        let names: Vec<String> = chosen.iter().map(|l| format!("{} ({:.0} KB)", l.name, l.bytes as f64 / 1024.0)).collect();
        format!("{} {}", tr("The report file holds the same, and these logs (up to the last 4 MB of each):"), names.join(", "))
    };
    ui.label(egui::RichText::new(files).color(t.text_dim));
    let r = ui.button(tr("Copy to Clipboard"));
    crate::widgets::register(ui, "button:reportBugCopy", r.rect);
    if r.clicked() {
        ui.ctx().copy_text(issue.body);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<(String, String)> {
        vec![("Version".into(), "0.6.0".into()), ("Library".into(), "/Users/ann/Pictures/LightCraft Library".into())]
    }

    fn log(tail: &str) -> LogInfo {
        LogInfo {
            name: "lightcraft.log".into(),
            path: String::new(),
            bytes: tail.len() as u64,
            tail: tail.into(),
            current: true,
            text: String::new(),
        }
    }

    #[test]
    fn redact_hides_the_home_folder_and_photo_names() {
        let home = Some("/Users/ann");
        let line = "2026-10-11 WARN import: /Users/ann/Pictures/Ann birthday.CR2: no preview; sidecar IMG_0001.xmp kept";
        assert_eq!(redact(line, home), "2026-10-11 WARN import: ~/Pictures/<photo>.CR2: no preview; sidecar <photo>.xmp kept");
        // quoted, in brackets, other cases and Windows paths
        assert_eq!(redact(r#"opened "C:\Users\ann\a b.HEIC" (DSC_1.nef)"#, Some(r"C:\Users\ann")), r#"opened "~\<photo>.HEIC" (<photo>.nef)"#);
        // other files and plain words keep their names
        assert_eq!(redact("wrote /tmp/lightcraft.log and settings.json at 1.5 s", home), "wrote /tmp/lightcraft.log and settings.json at 1.5 s");
        assert_eq!(redact("no home given: /Users/ann/x.jpg", None), "no home given: /Users/ann/<photo>.jpg");
        // never cuts inside a character
        assert_eq!(redact("写真/桜.jpg ✓", None), "写真/<photo>.jpg ✓");
        // punctuation inside a photo's name is part of it (#1128 review): the whole name is hidden
        assert_eq!(redact("import /Users/ann/Pictures/Private Client (1).CR2: no preview", home), "import ~/Pictures/<photo>.CR2: no preview");
        assert_eq!(redact("/Users/ann/Wedding/[final].jpg", home), "~/Wedding/<photo>.jpg");
        assert_eq!(redact("/Users/ann/O'Brien.CR2 and O'Brien.CR2", home), "~/<photo>.CR2 and <photo>.CR2");
        assert_eq!(redact(r"C:\Users\ann\Smith, J. {2}.NEF", Some(r"C:\Users\ann")), r"~\<photo>.NEF");
        // after a hidden name, the same marks end words again
        assert_eq!(redact("/Users/ann/x.jpg (DSC_1.nef)", home), "~/<photo>.jpg (<photo>.nef)");
    }

    /// CodeRabbit on #3763: a cut that falls exactly on a line start keeps that line; one inside a line drops
    /// the partial line.
    #[test]
    fn read_end_keeps_a_whole_first_line() {
        let dir = std::env::temp_dir().join(format!("lc-read-end-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("log.txt");
        std::fs::write(&p, "aaaa\nbbbb\ncccc\n").unwrap();
        assert_eq!(read_end(&p, 10).unwrap(), "bbbb\ncccc\n", "the cut is at the start of bbbb");
        assert_eq!(read_end(&p, 8).unwrap(), "cccc\n", "the cut is inside bbbb");
        assert_eq!(read_end(&p, 100).unwrap(), "aaaa\nbbbb\ncccc\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn redact_hides_quoted_names_and_debug_formatted_homes() {
        // a debug-formatted Windows path doubles its backslashes
        assert_eq!(redact(r#"path: "C:\\Users\\ann\\x.jpg""#, Some(r"C:\Users\ann")), r#"path: "~\\<photo>.jpg""#);
        // a quoted name with no folder before it is hidden whole
        assert_eq!(redact(r#"could not read "Ann birthday.CR2": truncated"#, None), r#"could not read "<photo>.CR2": truncated"#);
        // quotes, pipes and angle brackets inside a path's last name are part of it
        assert_eq!(redact("/Users/ann/a|b <c>.jpg", Some("/Users/ann")), "~/<photo>.jpg");
    }

    #[test]
    fn this_runs_log_is_chosen_by_name_not_position() {
        let older =
            LogInfo { name: "lightcraft.1.log".into(), path: String::new(), bytes: 1, tail: String::new(), current: false, text: String::new() };
        let current = LogInfo { current: true, name: "lightcraft.log".into(), ..older.clone() };
        let both_logs = [older.clone(), current.clone()];
        let opts = ReportOptions::default();
        assert!(chosen(std::slice::from_ref(&older), &opts).is_empty(), "only an older log: none is this run's");
        assert_eq!(chosen(&both_logs, &opts), std::slice::from_ref(&current));
        let both = ReportOptions { previous_logs: true, ..Default::default() };
        assert_eq!(chosen(&both_logs, &both).len(), 2);
    }

    #[test]
    fn a_shortened_description_says_so() {
        let opts = ReportOptions { what: "x".repeat(20_000), ..Default::default() };
        let issue = issue_with(&rows(), &opts, None, false, &|t: &str| t.to_string());
        assert!(issue.url.len() <= MAX_URL);
        assert!(issue.body.contains("(shortened to fit the link)"), "{}", &issue.body[issue.body.len().saturating_sub(200)..]);
    }

    #[test]
    fn the_issue_link_stays_short_with_a_long_log() {
        let tail = (0..EXCERPT_LINES).map(|i| format!("{i:03} {}", "x".repeat(400))).collect::<Vec<_>>().join("\n");
        let opts = ReportOptions { what: "Export hangs".into(), ..Default::default() };
        let issue = issue_with(&rows(), &opts, Some(&log(&tail)), true, &|t: &str| t.to_string());
        assert!(issue.url.len() <= MAX_URL, "{}", issue.url.len());
        assert!(issue.url.starts_with("https://github.com/storytold/lightcraft/issues/new?title=Bug%3A%20Export%20hangs&body="), "{}", issue.url);
        // the newest lines are kept, the oldest dropped
        assert!(issue.body.contains(&format!("{:03} ", EXCERPT_LINES - 1)) && !issue.body.contains("000 "), "{}", issue.body);
        assert!(issue.body.contains("lightcraft-report-"));
        // a description too long for any link is shortened too
        let opts = ReportOptions { what: "y".repeat(20_000), ..Default::default() };
        let issue = issue_with(&rows(), &opts, Some(&log(&tail)), false, &|t: &str| t.to_string());
        assert!(issue.url.len() <= MAX_URL && issue.body.contains("yyyy"), "{}", issue.url.len());
    }

    #[test]
    fn the_issue_holds_what_happened_the_system_and_the_log() {
        let opts = ReportOptions { what: "Crop resets\nafter undo".into(), ..Default::default() };
        let issue = issue_with(&rows(), &opts, Some(&log("line one\nline two")), false, &|t: &str| t.to_string());
        assert_eq!(issue.title, "Bug: Crop resets");
        assert!(issue.body.contains("Crop resets\nafter undo") && issue.body.contains("Version: 0.6.0\n") && issue.body.contains("line two"));
        // no log (the web, --memory): system info only
        let issue = issue_with(&rows(), &ReportOptions::default(), None, false, &|t: &str| t.to_string());
        assert_eq!(issue.title, "Bug report");
        assert!(issue.body.contains("Version: 0.6.0") && !issue.body.contains("### End of"));
    }

    #[test]
    fn last_lines_and_the_end_of_a_file() {
        assert_eq!(last_lines("a\nb\nc\n", 2), "b\nc");
        assert_eq!(last_lines("a\nb", 5), "a\nb");
        assert_eq!(last_lines("", 3), "");
        let dir = std::env::temp_dir().join(format!("lc-bug-report-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("x.log");
        std::fs::write(&p, "first line\nsecond line\nthird\n").unwrap();
        // from the first whole line inside the last bytes
        assert_eq!(read_end(&p, 10).unwrap(), "third\n");
        assert_eq!(read_end(&p, 1000).unwrap(), "first line\nsecond line\nthird\n");
        assert!(read_end(&dir.join("missing.log"), 10).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn encode_keeps_only_unreserved_characters() {
        assert_eq!(encode("a b/ü&=#"), "a%20b%2F%C3%BC%26%3D%23");
    }
}
