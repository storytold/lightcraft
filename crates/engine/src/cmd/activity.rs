//! `activity.list` / `activity.cancel`: the background tasks in flight and those waiting in line; `activity.finished`
//! / `activity.dismiss` / `activity.retry`: the ones that ended and left a row (see [`crate::activity`]).

use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            query "activity.list",
            "Background Tasks",
            [],
            None,
            "{} → {tasks: [{id, kind, label, done, total, unit: count|bytes|percent, detail, cancellable, cancelling, ageMs, waiting, place, lane}]} — the long-running tasks in flight (imports, exports, preview builds, downloads, the face scan…), oldest first, then those waiting in line for their lane (waiting: true, place 1 = next in its lane, lane: previews; they start by themselves when the tasks ahead end, keeping their id), in the order they were asked for; total 0 = not known yet",
            always,
            |s, _| Ok(json!({"tasks": s.activity.list()}))
        ),
        cmd!(
            "activity.cancel",
            "Cancel Background Task",
            [],
            None,
            "{id} | {all: true} → {cancelled: n} — ask a task (or every cancellable task) to stop; it stops at its next file or photo. A task waiting in line is taken out of it and never runs; all also empties the line. An unknown id (it may have just finished) or a task that can't be cancelled is an error",
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
            query "activity.finished",
            "Finished Background Tasks",
            [],
            None,
            "{} → {finished: [{id, kind, label, ending: done|problems|failed|stopped|died, summary, detail, failures: [{name, reason}], failed, path, ranMs, ageMs, retryable}]} — the tasks that ended and left a row, newest first: those that ran 10 s or more, and every one that did not end well (problems = not for every file, stopped = cancelled, died = its worker crashed). `failed` counts the files that failed; `failures` holds the first 500 with their reasons (none for a job that only counts them); `path` is one of the files it made; `retryable`: activity.retry asks for it again. `summary` and `detail` are as the app shows them, in its language, and `summary` is empty for a job that says nothing of itself. At most 50 clean endings and 200 rows in all are kept. They stay until dismissed, for this session, and the ones that did not end well ask for attention until then",
            always,
            |s, _| Ok(json!({"finished": s.activity.finished()}))
        ),
        cmd!(
            "activity.dismiss",
            "Dismiss Finished Task",
            [],
            None,
            "{id} | {all: true} → {dismissed: n} — let go of a finished task's row (or of all of them). An unknown id (it may have been dismissed already) is an error",
            always,
            |s, p| {
                const C: &str = "activity.dismiss";
                if p.get("all").and_then(Value::as_bool) == Some(true) {
                    return Ok(json!({"dismissed": s.activity.dismiss_all()}));
                }
                let id = p.get("id").and_then(Value::as_u64).ok_or_else(|| bad(C, "id or all"))?;
                s.activity.dismiss(id).map_err(|e| bad(C, e))?;
                Ok(json!({"dismissed": 1}))
            }
        ),
        cmd!(
            "activity.retry",
            "Retry Background Task",
            [],
            None,
            "{id} → {queued: id, place} — ask again for a task that did not end well (a row of activity.finished with retryable), as it was first asked (the same photos): its row goes, and it waits at the end of its lane's line, starting at once when the lane is free. Rows of another library can't be retried; nor can clean endings, nor tasks that don't wait in line. An unknown id is an error",
            always,
            |s, p| {
                const C: &str = "activity.retry";
                let id = p.get("id").and_then(Value::as_u64).ok_or_else(|| bad(C, "id"))?;
                let (queued, place) = s.activity.retry(id).map_err(|e| bad(C, e))?;
                Ok(json!({"queued": queued, "place": place}))
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
    fn finished_and_dismiss_through_execute() {
        use crate::activity::{Ending, Failure, Outcome};
        let mut s = Session::new();
        let g = s.activity.start("export", "Exporting", Cancel::Yes);
        let id = g.id();
        g.finish(Outcome {
            ending: Ending::Problems,
            summary: "Exported 1 of 2 photos".into(),
            failures: vec![Failure { name: "IMG_2.jpg".into(), reason: "The disk is full".into() }],
            ..Default::default()
        });
        let r = s.execute("activity.finished", &json!({})).unwrap();
        assert_eq!(r["finished"][0]["id"], id);
        assert_eq!(r["finished"][0]["ending"], "problems");
        assert_eq!(r["finished"][0]["failures"][0], json!({"name": "IMG_2.jpg", "reason": "The disk is full"}));
        assert_eq!(r["finished"][0]["failed"], 1);
        assert!(s.execute("activity.dismiss", &json!({})).is_err());
        assert!(s.execute("activity.dismiss", &json!({"id": 999})).is_err());
        assert_eq!(s.execute("activity.dismiss", &json!({"id": id})).unwrap()["dismissed"], 1);
        assert_eq!(s.execute("activity.dismiss", &json!({"all": true})).unwrap()["dismissed"], 0);
        assert_eq!(s.execute("activity.finished", &json!({})).unwrap()["finished"], json!([]));
    }

    #[test]
    fn cancel_all_and_errors() {
        let mut s = Session::new();
        let (_a, _b) = (s.activity.start("export", "E", Cancel::Yes), s.activity.start("faces", "F", Cancel::No));
        assert_eq!(s.execute("activity.cancel", &json!({"all": true})).unwrap()["cancelled"], 1);
        assert!(s.execute("activity.cancel", &json!({"id": 999})).is_err());
        assert!(s.execute("activity.cancel", &json!({})).is_err());
    }
}
