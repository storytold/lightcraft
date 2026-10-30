//! `activity.list` / `activity.cancel` / `activity.skip`: the background tasks in flight (see [`crate::activity`]).

use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            query "activity.list",
            "Background Tasks",
            [],
            None,
            "{} → {tasks: [{id, kind, label, done, total, unit: count|bytes|percent, detail, cancellable, cancelling, ageMs, stalled}]} — the long-running tasks in flight (imports, exports, preview builds, downloads, the face scan…), oldest first; total 0 = not known yet; stalled is null, or {what, secs} while the task waits on something that does not answer (an import's file on a drive that stopped responding: what is its name, secs how long): activity.skip gives it up",
            always,
            |s, _| Ok(json!({"tasks": s.activity.list()}))
        ),
        cmd!(
            "activity.cancel",
            "Cancel Background Task",
            [],
            None,
            "{id} | {all: true} → {cancelled: n} — ask a task (or every cancellable task) to stop; it stops at its next file or photo (a preview build also stops between a photo's own previews). An unknown id (it may have just finished) or a task that can't be cancelled is an error",
            always,
            |s, p| {
                const C: &str = "activity.cancel";
                if p.get("all").and_then(Value::as_bool) == Some(true) {
                    return Ok(json!({"cancelled": s.activity.cancel_all()}));
                }
                let id = p.get("id").and_then(Value::as_u64).ok_or_else(|| bad(C, "id or all"))?;
                s.activity.cancel(id).map_err(|e| bad(C, e))?;
                Ok(json!({"cancelled": 1}))
            }
        ),
        cmd!(
            "activity.skip",
            "Skip Stalled File",
            [],
            None,
            "{id} → {skipped: 1} — give up the one file a task is stuck waiting on, the one that has gone longest without progress (its row in activity.list has stalled: {what, secs}); the next stalled file is named afterwards. An import records the file as failed (\"skipped: no answer after N s\"), the review and Local-view scans list it as unreadable with that reason, and the work goes on. The read itself stays blocked until the drive answers, and its late result is ignored. The fourth skipped file ends the import or scan with an error that the drive is not answering. So does a run started while 32 or more given-up reads (of any drive) are still blocked in the system. Listing folders is not covered: ✕ waits for it. An unknown id (it may have just finished) or a task that waits on nothing is an error",
            always,
            |s, p| {
                const C: &str = "activity.skip";
                let id = p.get("id").and_then(Value::as_u64).ok_or_else(|| bad(C, "id"))?;
                s.activity.skip(id).map_err(|e| bad(C, e))?;
                Ok(json!({"skipped": 1}))
            }
        ),
    ]
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::Session;
    use crate::activity::Cancel;

    #[test]
    fn list_and_cancel_through_execute() {
        let mut s = Session::new();
        let g = s.activity.start("export", "Exporting", Cancel::Yes);
        let r = s.execute("activity.list", &json!({})).unwrap();
        assert_eq!(r["tasks"][0]["kind"], "export");
        assert_eq!(r["tasks"][0]["cancellable"], true);
        let r = s.execute("activity.cancel", &json!({"id": g.id()})).unwrap();
        assert_eq!(r["cancelled"], 1);
        assert!(g.is_cancelled());
    }

    #[test]
    fn cancel_all_and_errors() {
        let mut s = Session::new();
        let (_a, _b) = (s.activity.start("export", "E", Cancel::Yes), s.activity.start("faces", "F", Cancel::No));
        assert_eq!(s.execute("activity.cancel", &json!({"all": true})).unwrap()["cancelled"], 1);
        assert!(s.execute("activity.cancel", &json!({"id": 999})).is_err());
        assert!(s.execute("activity.cancel", &json!({})).is_err());
    }

    /// `activity.skip` gives up what a stalled task waits on; an unknown id, a task that waits on nothing, and a
    /// request without an id are errors; `activity.list` shows what is waited for.
    #[test]
    fn skip_through_execute() {
        let mut s = Session::new();
        let g = s.activity.start("import", "Importing", Cancel::Yes);
        assert!(s.execute("activity.skip", &json!({"id": 999})).is_err());
        assert!(s.execute("activity.skip", &json!({})).is_err());
        assert!(s.execute("activity.skip", &json!({"id": g.id()})).is_err(), "nothing to skip yet");
        assert_eq!(s.execute("activity.list", &json!({})).unwrap()["tasks"][0]["stalled"], json!(null));
        g.waiting_on("IMG_0457.jpg", std::time::Duration::from_secs(14));
        let stalled = s.execute("activity.list", &json!({})).unwrap()["tasks"][0]["stalled"].clone();
        assert_eq!(stalled["what"], "IMG_0457.jpg");
        assert!(stalled["secs"].as_u64().is_some_and(|n| (14..16).contains(&n)), "{stalled}");
        assert_eq!(s.execute("activity.skip", &json!({"id": g.id()})).unwrap()["skipped"], 1);
        assert!(g.take_skip());
        assert!(!g.take_skip(), "taken once");
        // a skip for something that answered meanwhile is moot
        s.execute("activity.skip", &json!({"id": g.id()})).unwrap();
        g.clear_waiting();
        assert_eq!(s.execute("activity.list", &json!({})).unwrap()["tasks"][0]["stalled"], json!(null));
        assert!(!g.take_skip());
    }
}
