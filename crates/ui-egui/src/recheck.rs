//! Preview-only raws rechecked when a library opens (issue #662): a raw imported while its format
//! couldn't be decoded is probed again when the library opens in another LightCraft version (see
//! `lightcraft_engine::cmd::recheck`). The probe runs on a worker thread, so opening never waits
//! for it; the result is applied back here, unless another library was opened meanwhile.

use lightcraft_engine::cmd::recheck;

use crate::LightcraftApp;

/// The activity stack's label while the files are read.
pub const LABEL: &str = "Checking raw files";

/// Once per opened library (called every frame): start the recheck when it has stale photos.
pub fn tick(app: &mut LightcraftApp) {
    let library = app.session.library_generation();
    if app.ui.rechecked_library == Some(library) {
        return;
    }
    app.ui.rechecked_library = Some(library);
    // a demo or browser session has no files to read
    let Some(probe) = recheck::file_probe(&app.session) else { return };
    let stale = recheck::stale(&app.session.catalog, false);
    if stale.is_empty() {
        return;
    }
    let work = move || recheck::probe(&probe, stale);
    let done = move |app: &mut LightcraftApp, ctx: &egui::Context, results| {
        if app.session.library_generation() != library {
            return;
        }
        match recheck::apply(&mut app.session, results) {
            Ok(r) => {
                let n = r.cleared.len();
                if n == 1 {
                    app.toast(ctx, crate::i18n::tr("1 raw can now be developed from its raw data"));
                } else if n > 1 {
                    app.toast(ctx, crate::i18n::tr_format!("{n} raws can now be developed from their raw data", n = n));
                }
            }
            Err(e) => log::warn!("recheck of preview-only raws: {e}"),
        }
    };
    if let Err(e) = crate::tasks::spawn(app, LABEL, Some("recheck"), work, done) {
        log::warn!("{e}");
    }
}
