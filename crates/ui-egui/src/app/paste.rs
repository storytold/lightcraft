//! Shared feedback for every paste entry point, including commands invoked without a UI context.

use super::{CommandError, LightcraftApp, TOAST_SECONDS, Value, i18n};

impl LightcraftApp {
    pub(super) fn paste_feedback(&mut self, result: &Result<Value, CommandError>) {
        let (text, seconds) = match result {
            Err(e) => (e.message.clone(), 6.0),
            // Queuing is not completion: background progress belongs only in Activity.
            Ok(v) if v.get("pending").and_then(Value::as_bool) == Some(true) => return,
            Ok(v) if v.get("changed").and_then(Value::as_u64).is_some_and(|n| n > 0) => (i18n::tr("Settings pasted").to_string(), TOAST_SECONDS),
            Ok(_) => return,
        };
        self.ui.toast = Some((text, self.tick_now() + seconds, None));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn app() -> LightcraftApp {
        LightcraftApp::new(crate::Session::new(), crate::Services::default())
    }

    #[test]
    fn pending_batches_never_announce_completion() {
        for changed in [0, 2] {
            let mut app = app();
            app.paste_feedback(&Ok(json!({"pending": true, "changed": changed, "targets": 2})));
            assert!(app.ui.toast.is_none(), "progress belongs in Activity, not a success toast");
        }
    }

    #[test]
    fn empty_pastes_do_not_announce_changes() {
        let mut app = app();
        app.paste_feedback(&Ok(json!({"changed": 0})));
        assert!(app.ui.toast.is_none());
    }

    #[test]
    fn completed_batches_announce_success() {
        let mut app = app();
        app.paste_feedback(&Ok(json!({"changed": 2})));
        assert_eq!(app.ui.toast.as_ref().map(|t| t.0.as_str()), Some("Settings pasted"));
    }

    #[test]
    fn refusals_show_the_actionable_error() {
        let mut app = app();
        app.paste_feedback(&Err(CommandError::from("Finish or cancel the edit and try again.")));
        let (text, until, _) = app.ui.toast.as_ref().unwrap();
        assert_eq!(text, "Finish or cancel the edit and try again.");
        assert!(*until > app.tick_now() + 5.0, "errors stay readable for six seconds");
    }
}
