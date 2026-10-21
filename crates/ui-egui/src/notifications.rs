//! What finished while the user was looking elsewhere: an import's outcome, an error that was only a toast. Each
//! toast that matters is also kept here, and the list under the top bar's bell shows the recent ones below the running
//! tasks ([`crate::panels::activity`]). A notification with a [`Report`] has a Details link that opens it as a
//! dialog, so a long list (every photo an import warned about) never has to fit in a toast.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

/// Notifications kept; older ones drop off.
pub const KEEP: usize = 30;

/// A long result, shown by the Details link (as [`crate::state::Dialog::Report`]).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Report {
    /// The dialog's title ("Import Warnings").
    pub title: String,
    /// One line above the groups.
    pub summary: String,
    pub groups: Vec<ReportGroup>,
}

/// One kind of problem and what it concerns ("Not imported: Look" and the photos' paths).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ReportGroup {
    pub heading: String,
    pub items: Vec<String>,
}

impl Report {
    /// The report as plain text (Copy to Clipboard).
    pub fn text(&self) -> String {
        let mut out = format!("{}\n", self.summary);
        for g in &self.groups {
            out.push_str(&format!("\n{} ({})\n", g.heading, g.items.len()));
            for item in &g.items {
                out.push_str(&format!("  {item}\n"));
            }
        }
        out
    }
}

/// Paths grouped by their folder, in the order the folders first come: (folder, the files' names).
pub fn by_folder(paths: &[String]) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for p in paths {
        let path = std::path::Path::new(p);
        let folder = path.parent().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.clone());
        match out.iter_mut().find(|(f, _)| *f == folder) {
            Some((_, names)) => names.push(name),
            None => out.push((folder, vec![name])),
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq)]
pub struct Notification {
    pub id: u64,
    pub text: String,
    /// Something went wrong or wasn't carried over (drawn with the warning colour).
    pub warning: bool,
    pub report: Option<Report>,
    /// When it came (egui time, s).
    pub time: f64,
}

#[derive(Debug, Default)]
pub struct Notifications {
    /// Newest first.
    pub items: VecDeque<Notification>,
    next: u64,
    /// How many arrived since the bell's list was last open.
    pub unread: usize,
    /// Whether any of the unread ones is a warning (the bell's dot turns the warning colour).
    pub unread_warning: bool,
}

impl Notifications {
    pub fn push(&mut self, text: impl Into<String>, warning: bool, report: Option<Report>, time: f64) -> u64 {
        self.next += 1;
        let id = self.next;
        self.items.push_front(Notification { id, text: text.into(), warning, report, time });
        self.items.truncate(KEEP);
        self.unread = (self.unread + 1).min(KEEP);
        self.unread_warning |= warning;
        id
    }

    pub fn get(&self, id: u64) -> Option<&Notification> {
        self.items.iter().find(|n| n.id == id)
    }

    pub fn mark_read(&mut self) {
        self.unread = 0;
        self.unread_warning = false;
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.mark_read();
    }
}

/// "Just now", "5 min", "2 h": how long ago a notification came at `then` (egui time, s).
pub fn age_text(now: f64, then: f64) -> String {
    let s = (now - then).max(0.0);
    if s < 60.0 {
        crate::i18n::tr("Just now").to_string()
    } else if s < 3600.0 {
        format!("{} min", (s / 60.0).floor() as u64)
    } else {
        format!("{} h", (s / 3600.0).floor() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_newest_and_counts_unread() {
        let mut n = Notifications::default();
        for i in 0..KEEP + 5 {
            n.push(format!("n{i}"), i == 3, None, 0.0);
        }
        assert_eq!(n.items.len(), KEEP);
        assert_eq!(n.items[0].text, format!("n{}", KEEP + 4), "newest first");
        assert_eq!(n.unread, KEEP);
        assert!(n.unread_warning);
        n.mark_read();
        assert_eq!((n.unread, n.unread_warning), (0, false));
        let id = n.push("x", false, None, 0.0);
        assert_eq!(n.get(id).map(|x| x.text.as_str()), Some("x"));
        n.clear();
        assert!(n.items.is_empty() && n.unread == 0);
    }

    #[test]
    fn report_text_lists_every_item_under_its_heading() {
        let r = Report {
            title: "T".into(),
            summary: "2 photos".into(),
            groups: vec![ReportGroup { heading: "Not imported: Look".into(), items: vec!["/a/1.ARW".into(), "/a/2.ARW".into()] }],
        };
        assert_eq!(r.text(), "2 photos\n\nNot imported: Look (2)\n  /a/1.ARW\n  /a/2.ARW\n");
    }

    #[test]
    fn paths_group_by_folder() {
        let paths: Vec<String> = ["/a/1.ARW", "/b/2.ARW", "/a/3.ARW", "4.ARW"].iter().map(|s| s.to_string()).collect();
        assert_eq!(
            by_folder(&paths),
            [
                ("/a".to_string(), vec!["1.ARW".to_string(), "3.ARW".to_string()]),
                ("/b".to_string(), vec!["2.ARW".to_string()]),
                (String::new(), vec!["4.ARW".to_string()]),
            ]
        );
    }

    #[test]
    fn ages() {
        assert_eq!(age_text(100.0, 90.0), "Just now");
        assert_eq!(age_text(400.0, 100.0), "5 min");
        assert_eq!(age_text(8000.0, 0.0), "2 h");
    }
}
