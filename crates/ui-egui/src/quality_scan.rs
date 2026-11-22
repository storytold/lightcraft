//! The Image Quality Scanner's frame work (issue #1313): `photo.qualityScan` runs in the
//! background, the scores are written as one undo step when it ends (`photo.qualityScanPump`), and
//! the frame it ends reports how many photos landed on each side of the two cutoffs.
#![allow(clippy::indexing_slicing, reason = "not yet converted to checked indexing: tracked in docs/gaps.md (indexing lint)")]

use serde_json::Value;
use serde_json::json;

use crate::LccApp;

/// Every frame: takes a finished scan in, and keeps the frames coming while one runs.
pub(crate) fn pump(app: &mut LccApp, ctx: &egui::Context) {
    let Ok(v) = app.session.execute("photo.qualityScanPump", &json!({})) else { return };
    if v["done"] == true {
        if let Some(text) = summary_text(&v) {
            let now = app.tasks.repaint.as_ref().map(|ctx| ctx.input(|i| i.time)).unwrap_or(app.last_time);
            app.ui.show_toast((text, now + crate::TOAST_SECONDS, None));
        }
        return;
    }
    if v["active"] == true {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

/// What a finished run says in a toast: nothing at all when it measured nothing (a run cancelled
/// before its first photo is quiet, as cancelling says it was wanted).
fn summary_text(v: &Value) -> Option<String> {
    let n = |k: &str| v[k].as_u64().unwrap_or(0);
    let (measured, cancelled) = (n("measured"), v["cancelled"] == true);
    if measured == 0 && !cancelled && n("failed") == 0 {
        return None;
    }
    let mut text = if cancelled {
        crate::i18n::tr_format!("Image Quality Scan stopped: {n} photos scanned", n = measured).to_string()
    } else {
        crate::i18n::tr_format!("Image Quality Scan: {n} photos scanned", n = measured).to_string()
    };
    if measured > 0 {
        text.push_str(&crate::i18n::tr_format!(
            ": {accepted} accepted, {pending} to review, {rejected} rejected",
            accepted = n("accepted"),
            pending = n("pending"),
            rejected = n("rejected")
        ));
    }
    if n("failed") > 0 {
        text.push_str(&crate::i18n::tr_format!(" ({n} could not be read)", n = n("failed")));
    }
    Some(text)
}
