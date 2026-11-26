//! The lens database on request: the first launch offers lensfun's database once (its size, Download / Not Now,
//! [`offer`]), Settings ▸ General can download it any time ([`download_default`]), and nothing is fetched without
//! that click (`lightcraft_engine::lensfun_download::fetch_on_start` decides; an activity row while it runs).
//! Whenever the database in use changes
//! (it arrived, a newer pin replaced it) or Enable Lens Corrections is ticked on a photo without a profile, the
//! library's raws that have none are looked up again on a worker thread (`lightcraft_engine::cmd::lens_refresh`);
//! when its files were replaced, so are those whose profile came from the database and those that named no lens.

use lightcraft_engine::cmd::{lens_refresh, recheck};
use lightcraft_engine::{lens_profiles, lensfun_download};

use crate::LightcraftApp;

/// The activity stack's label while files are read for their lens.
pub const LABEL: &str = "Looking up lens profiles";

/// Called every frame.
pub fn tick(app: &mut LightcraftApp) {
    start_download(app);
    refresh(app);
}

/// Once per run, what the start policy says ([`lensfun_download::fetch_on_start`]): offer the database, fetch it
/// (the user said yes before), or nothing. The offer waits for the window to be free (another dialog, the library
/// chooser).
fn start_download(app: &mut LightcraftApp) {
    if app.ui.lens_download_checked {
        return;
    }
    if !app.session.lens_auto_download {
        app.ui.lens_download_checked = true;
        return;
    }
    if app.ui.dialog.is_some() || crate::panels::library_problem::blocks_dialog_input(app) {
        return;
    }
    app.ui.lens_download_checked = true;
    let start = lensfun_download::fetch_on_start(app.session.lens_auto_download, app.ui.settings.lens_database);
    begin(app, start, lensfun_download::mirrors());
}

/// Act on the start policy's answer, fetching from `mirrors`. The offer counts as answered once it is shown: closing
/// it any way but Download (Not Now, Esc) is "not now", and the next start doesn't ask again.
pub(crate) fn begin(app: &mut LightcraftApp, start: lensfun_download::Start, mirrors: Vec<String>) {
    match start {
        lensfun_download::Start::Nothing => {}
        lensfun_download::Start::Ask(dir) => {
            app.ui.settings.lens_database = lensfun_download::Offer::Declined;
            app.ui.dialog = Some(crate::state::Dialog::LensDatabase { dir: dir.to_string_lossy().into_owned(), mirrors });
        }
        lensfun_download::Start::Fetch(dir) => {
            if let Err(e) = download(app, &dir, mirrors) {
                log::warn!("lens database: {e}");
            }
        }
    }
}

/// Download the database into `dir` from `mirrors` (the offer's Download, Settings' button): remembered as the
/// user's yes, shown in the activity stack, and read once it is in place.
pub(crate) fn download(app: &mut LightcraftApp, dir: &std::path::Path, mirrors: Vec<String>) -> Result<(), String> {
    lens_refresh::start_download(&mut app.session, dir, mirrors, lensfun_download::FILES)?;
    // a yes only once it started: a download that couldn't start isn't fetched again at the next start
    app.ui.settings.lens_database = lensfun_download::Offer::Accepted;
    Ok(())
}

/// Settings' Download Lens Database: into the app's own folder, from the usual mirrors.
pub fn download_default(app: &mut LightcraftApp) -> Result<(), String> {
    let dir = lensfun_download::default_dir().ok_or("this computer has no folder for LightCraft's data")?;
    download(app, &dir, lensfun_download::mirrors())
}

/// The offer's window: what the database does, its size, and that Settings has it later.
pub fn offer(ui: &mut egui::Ui) {
    let t = crate::theme::Tokens::get(ui.ctx());
    ui.label(egui::RichText::new(crate::i18n::tr("Download lens profiles?")).strong());
    ui.add_space(4.0);
    ui.add(
        egui::Label::new(crate::i18n::tr_format!(
            "LightCraft can correct distortion and vignetting for many lenses using the open lens database (lensfun, about {size} MB).",
            size = lensfun_download::download_megabytes()
        ))
        .wrap(),
    );
    ui.add_space(4.0);
    ui.add(
        egui::Label::new(
            egui::RichText::new(crate::i18n::tr("Nothing is downloaded until you choose Download. You can download it later from Settings."))
                .color(t.text_dim),
        )
        .wrap(),
    );
}

/// After Enable Lens Corrections was ticked on photo `id`: the photos to look up again, those of `id` and the
/// selection that now have corrections on and no profile. With Auto Sync off only `id` was changed, so the other
/// selected photos are left alone unless their corrections were already on.
pub fn to_look_up(session: &lightcraft_engine::Session, id: lightcraft_catalog::PhotoId) -> Vec<lightcraft_catalog::PhotoId> {
    let mut out: Vec<lightcraft_catalog::PhotoId> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for t in std::iter::once(id).chain(session.selection.ids.iter().copied()) {
        if seen.insert(t) && session.catalog.photo(t).is_some_and(|p| p.embedded_lens.is_none() && p.develop.optics.lens_profile) {
            out.push(t);
        }
    }
    out
}

/// What a lookup's results belong to: the library, the lens database in use and how often its files were replaced.
/// Results for another key are stale.
pub(crate) fn lookup_key(app: &LightcraftApp) -> (u64, u64, u64) {
    (app.session.library_generation(), lens_profiles::generation(), lens_profiles::replacements())
}

/// Whether this lookup also reads the photos that have a database profile or no lens name
/// ([`lens_refresh::missing`]'s `again`): only when the database's files were replaced since the last lookup, not at
/// every start or library switch.
pub(crate) fn look_up_again(last: Option<(u64, u64, u64)>, key: (u64, u64, u64)) -> bool {
    last.map_or(key.2 > 0, |last| last.2 != key.2)
}

/// Look the library's raws up again when the database changed, the library changed, or photos asked for it.
fn refresh(app: &mut LightcraftApp) {
    let key = lookup_key(app);
    let asked = std::mem::take(&mut app.ui.lens_lookup_ids);
    if app.ui.lens_refreshed == Some(key) && asked.is_empty() {
        return;
    }
    // (a lookup already under way finishes first; what was asked meanwhile waits for the next frame)
    if app.tasks.is_running(LABEL) {
        app.ui.lens_lookup_ids.extend(asked);
        return;
    }
    let ids = (app.ui.lens_refreshed == Some(key)).then_some(asked);
    let again = ids.is_none() && look_up_again(app.ui.lens_refreshed, key);
    app.ui.lens_refreshed = Some(key);
    let Some(probe) = recheck::file_probe(&app.session) else { return };
    let Some(db) = lens_profiles::database() else { return };
    let listed = lens_refresh::missing(&app.session.catalog, &db, ids.as_deref(), again);
    if listed.is_empty() {
        return;
    }
    let work = move || lens_refresh::probe(&probe, listed);
    let done = move |app: &mut LightcraftApp, ctx: &egui::Context, results| {
        // another library, or a database reloaded while the files were read (the probe may have used either): the
        // next frame looks them up again with the database in use now (asked for here: an idle window draws none)
        if lookup_key(app) != key {
            ctx.request_repaint();
            return;
        }
        match lens_refresh::apply(&mut app.session, results) {
            Ok(found) if found.len() == 1 => app.toast(ctx, crate::i18n::tr("1 photo got its lens profile")),
            Ok(found) if found.len() > 1 => app.toast(ctx, crate::i18n::tr_format!("{n} photos got their lens profile", n = found.len())),
            Ok(_) => {}
            Err(e) => log::warn!("lens profile lookup: {e}"),
        }
    };
    if let Err(e) = crate::tasks::spawn(app, LABEL, Some("lensLookup"), work, done) {
        log::warn!("{e}");
    }
}

/// The database's name for a lens, remembered per lens and database (the panel asks every frame; a search scores
/// every lens in the database).
fn database_name(camera: &str, lens: &str) -> Option<String> {
    type Key = (String, String, u64);
    thread_local! {
        static LAST: std::cell::RefCell<Option<(Key, Option<String>)>> = const { std::cell::RefCell::new(None) };
    }
    let key = (camera.to_string(), lens.to_string(), lens_profiles::generation());
    LAST.with(|last| {
        if let Some((k, name)) = last.borrow().as_ref()
            && *k == key
        {
            return name.clone();
        }
        let name = lens_profiles::database().and_then(|db| lens_profiles::profile_name(&db, camera, lens));
        *last.borrow_mut() = Some((key, name.clone()));
        name
    })
}

/// What the Optics panel says about photo `p`'s lens profile: which one is loaded and from where, or why there is
/// none.
pub fn readout(app: &LightcraftApp, p: &lightcraft_catalog::Photo) -> String {
    use lightcraft_develop::LensSource;
    match p.embedded_lens.map(|l| l.source) {
        Some(LensSource::File) => crate::i18n::tr("Profile: from the file (the camera's own correction)").to_string(),
        Some(LensSource::Database) => {
            let name = database_name(&p.meta.camera, &p.meta.lens).unwrap_or_else(|| p.meta.lens.trim().to_string());
            crate::i18n::tr_format!("Profile: {} (lens database)", name)
        }
        None if app.session.lensfun.running() => crate::i18n::tr("Downloading the lens database…").to_string(),
        None if matches!(app.session.lensfun.state(), lensfun_download::State::Failed(_)) && lens_profiles::database().is_none() => {
            crate::i18n::tr("The lens database could not be downloaded. LightCraft tries again the next time it starts.").to_string()
        }
        None => match lens_refresh::no_profile_reason(p) {
            Some(r) if p.meta.lens.trim().is_empty() => crate::i18n::tr(&r).to_string(),
            Some(_) => crate::i18n::tr_format!("No lens profile found for {}.", p.meta.lens.trim()),
            None => String::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use crate::{LightcraftApp, Services};
    use lightcraft_develop::{EmbeddedLens, LensSource};

    /// The Optics panel says which profile is loaded and from where, or why there is none.
    #[test]
    fn the_optics_readout_names_the_profile_or_the_lens_without_one() {
        let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
        let id = app.session.active().expect("a photo");
        let mut p = (**app.session.catalog.photo(id).expect("the photo")).clone();
        p.meta.lens = "EF35mm f/1.4L II USM".into();
        p.embedded_lens = None;
        assert_eq!(super::readout(&app, &p), "No lens profile found for EF35mm f/1.4L II USM.");
        p.meta.lens.clear();
        assert_eq!(super::readout(&app, &p), "This file names no lens.");
        p.embedded_lens = Some(EmbeddedLens::default());
        assert_eq!(super::readout(&app, &p), "Profile: from the file (the camera's own correction)");
        p.meta.lens = "EF70-200mm f/2.8L IS II USM".into();
        p.embedded_lens = Some(EmbeddedLens { source: LensSource::Database, ..Default::default() });
        // the database's name for it, or the file's own without a database (LIGHTCRAFT_LENSFUN_DIR may be set)
        let r = super::readout(&app, &p);
        assert!(r.starts_with("Profile: ") && r.contains("70-200mm f/2.8L IS II USM") && r.ends_with(" (lens database)"), "{r}");
    }

    /// A first-run download that failed says so, instead of "no profile" for every lens.
    #[test]
    fn a_failed_download_is_named_in_the_optics_readout() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
        let id = app.session.active().expect("a photo");
        let mut p = (**app.session.catalog.photo(id).expect("the photo")).clone();
        p.meta.lens = "EF35mm f/1.4L II USM".into();
        p.embedded_lens = None;
        let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        let root = std::env::temp_dir().join(format!("lc-lens-db-failed-{}-{unique}", std::process::id()));
        // (the download stages its files beside the database folder: both live in this test's own folder)
        let dir = root.join("lensfun");
        // nothing listens on port 1: the download fails at once, without the network
        let activity = app.session.activity.clone();
        app.session
            .lensfun
            .start(&dir, vec!["http://127.0.0.1:1/db".into()], lightcraft_engine::lensfun_download::FILES, &activity)
            .expect("started");
        let t0 = std::time::Instant::now();
        while app.session.lensfun.running() && t0.elapsed() < std::time::Duration::from_secs(60) {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(matches!(app.session.lensfun.state(), lightcraft_engine::lensfun_download::State::Failed(_)), "{:?}", app.session.lensfun.state());
        if lightcraft_engine::lens_profiles::database().is_none() {
            assert_eq!(super::readout(&app, &p), "The lens database could not be downloaded. LightCraft tries again the next time it starts.");
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
