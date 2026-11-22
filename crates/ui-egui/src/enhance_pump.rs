//! Keeps Photo ▸ Enhance going (`enhance.pump`): the engine makes Raw Details / Super Resolution DNGs on a worker
//! thread, and this imports the finished ones. Its progress is its row in the activity stack.

use crate::json_ext::JsonExt as _;
use serde_json::json;

use crate::LccApp;

/// How often a running Enhance is asked for finished photos.
const BUSY_SECS: f64 = 0.25;

/// Called every frame; idle, it costs one cheap command.
pub fn pump(app: &mut LccApp, ctx: &egui::Context) {
    crate::enhance_ui::poll(app, ctx);
    let Ok(v) = app.session.execute("enhance.pump", &json!({})) else { return };
    if v.field("active") == true {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(BUSY_SECS));
    }
}
