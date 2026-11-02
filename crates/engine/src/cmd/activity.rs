//! `activity.list` / `activity.cancel`: the background tasks in flight (see [`crate::activity`]).

use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            query "activity.list",
            "Background Tasks",
            [],
            None,
            "{} → {tasks: [{id, kind, label, done, total, unit: count|bytes|percent, detail, cancellable, cancelling, ageMs, parent?, batch, failed, etaMs?}], finished: [{id, seq, kind, label, done, failed, total, cancelled, reasons, seconds}]} — the long-running tasks in flight (imports, exports, preview builds, downloads, the face scan…), oldest first; total 0 = not known yet. A batch (AI Denoise, AI masks, export, Remove Dust on many photos) is one row with `batch: true`, the photos done and `failed`, and `etaMs` (time left, from the time per photo so far); its per-photo rows name it as `parent`. `finished`: what the last batches did when they ended (newest last)",
            always,
            |s, _| Ok(json!({"tasks": s.activity.list(), "finished": s.activity.finished()}))
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
}
