//! The UI's windows as `windows.json` lists them (`cargo xtask windows` keeps the list true to the
//! source), read by the tests: the survey test (`tests_dialogs.rs`) has a row for every modal, and
//! after each headless frame no modal with an id shows while [`crate::keyboard::modal_open`] is false.

use std::sync::LazyLock;

/// One window of the list.
pub(crate) struct Window {
    pub name: String,
    pub modal: bool,
    /// Its row in the survey test.
    pub survey: String,
    /// Its egui id, for a modal that is one at every time it shows.
    pub checked: Option<egui::Id>,
}

pub(crate) static WINDOWS: LazyLock<Vec<Window>> = LazyLock::new(|| {
    let v: serde_json::Value = serde_json::from_str(include_str!("../windows.json")).expect("windows.json is JSON");
    let s = |w: &serde_json::Value, k: &str| w.get(k).and_then(serde_json::Value::as_str).map(str::to_string);
    v["windows"]
        .as_array()
        .expect("a \"windows\" array")
        .iter()
        .map(|w| {
            let name = s(w, "name").expect("a name");
            let modal = s(w, "kind").as_deref() == Some("modal");
            let parts: Vec<&str> =
                w.get("id").and_then(serde_json::Value::as_array).into_iter().flatten().filter_map(serde_json::Value::as_str).collect();
            let id = parts.split_first().map(|(first, rest)| rest.iter().fold(egui::Id::new(*first), |id, part| id.with(*part)));
            Window {
                survey: s(w, "survey").unwrap_or_else(|| name.clone()),
                checked: id.filter(|_| modal && w.get("notModalWhen").is_none()),
                name,
                modal,
            }
        })
        .collect()
});

/// After a frame that began and ended with [`crate::keyboard::modal_open`] false: no modal of the
/// list was on screen in it (one counted nowhere lets keys and clicks through to the photos).
pub(crate) fn assert_no_uncounted_modal(ctx: &egui::Context) {
    let shown = ctx.memory(|m| m.areas().visible_layer_ids());
    for w in WINDOWS.iter() {
        if let Some(id) = w.checked
            && shown.iter().any(|layer| layer.id == id)
        {
            panic!(
                "{} is on screen while `keyboard::modal_open` is false: windows.json lists it as a modal, so count it there \
                 (or make it an exception with its reason)",
                w.name
            );
        }
    }
}
