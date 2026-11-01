//! Flickr in the desktop app: the sign-in steps and uploads run on worker threads, so the window
//! keeps answering while Flickr is contacted. The engine's `flickr.*` commands are run here in the
//! background instead (same ids and params): sign-in steps as [`crate::tasks`] with a row in the
//! activity stack, an upload as an [`UploadTask`] with progress and ✕ (issue #345).
//!
//! Settings → Accounts (`panels::flickr`) holds the API key and the sign-in; File ▸ Upload to
//! Flickr… opens [`crate::state::Dialog::FlickrUpload`] for the selected photos.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::LightcraftApp;

/// The Share to Flickr dialog's choices (`flickr.upload` params).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UploadDialog {
    /// Long edge in pixels; 0: full size.
    pub long_edge: u32,
    pub quality: u8,
    /// public | friends | family | friendsAndFamily | private
    pub privacy: String,
    pub hidden: bool,
    pub tags: bool,
    pub description: bool,
    pub remove_location: bool,
    /// The Flickr album to put the photos in (its id; empty: none). Starts empty every time.
    pub album: String,
    /// Make a new album for the photos instead, named `new_album_title`.
    pub new_album: bool,
    pub new_album_title: String,
}

impl Default for UploadDialog {
    fn default() -> Self {
        UploadDialog {
            long_edge: 2048,
            quality: 90,
            privacy: "public".into(),
            hidden: false,
            tags: true,
            description: true,
            remove_location: true,
            album: String::new(),
            new_album: false,
            new_album_title: String::new(),
        }
    }
}

impl UploadDialog {
    pub fn params(&self) -> Value {
        json!({
            "longEdge": self.long_edge,
            "quality": self.quality,
            "privacy": self.privacy,
            "hidden": self.hidden,
            "tags": self.tags,
            "description": self.description,
            "removeLocation": self.remove_location,
            "album": if self.album.is_empty() || self.new_album { Value::Null } else { json!(self.album) },
            "newAlbum": if self.new_album { json!(self.new_album_title) } else { Value::Null },
        })
    }
}

/// Sizes offered in the dialog (long edge, px; 0 = full size).
pub const SIZES: &[(u32, &str)] = &[(1024, "1024 px"), (2048, "2048 px"), (4096, "4096 px"), (0, "Full size")];
/// Privacy choices (value, label).
pub const PRIVACY: &[(&str, &str)] =
    &[("public", "Public"), ("friends", "Friends"), ("family", "Family"), ("friendsAndFamily", "Friends & family"), ("private", "Only you")];

/// What the Flickr settings and dialog remember between frames.
#[derive(Default)]
pub struct FlickrUi {
    /// The API key fields of Settings → Accounts (filled from the saved account when first shown).
    pub key: String,
    pub secret: String,
    /// The code Flickr shows after the user approves LightCraft.
    pub code: String,
    /// The key fields were filled from the account.
    pub loaded: bool,
    /// A sign-in step is running (what it is doing).
    pub busy: Option<&'static str>,
    /// Why the last step failed.
    pub error: Option<String>,
    /// The page where the user approves LightCraft (sign-in in progress).
    pub authorize_url: Option<String>,
    /// The code field takes the focus (the approval page just opened).
    pub focus_code: bool,
    /// The code last sent (a refused one isn't sent again until it changes).
    pub submitted_code: String,
    /// The last Upload dialog's choices (the next one starts from them, except the album).
    pub last_dialog: Option<UploadDialog>,
    /// The account's albums as last read (id, title, photo count), for the Upload dialog.
    pub albums: Option<Vec<(String, String, u64)>>,
    /// The albums are being read.
    pub albums_loading: bool,
    /// Why the albums could not be read.
    pub albums_error: Option<String>,
    /// The sign-in the album list (and a read in progress) belongs to: its token's public half.
    /// When the signed-in account changes, the list is dropped and a late answer is ignored.
    pub albums_account: Option<String>,
    /// Counts sign-in attempts: Cancel moves it on, so a request-token answer that arrives after
    /// it neither resumes the sign-in nor opens the browser.
    pub sign_in_attempt: u64,
    /// The stored API key the Key field was last filled from (a key changed elsewhere refills it).
    pub loaded_key: String,
    /// An upload in progress.
    #[cfg(not(target_arch = "wasm32"))]
    pub upload: Option<UploadTask>,
    /// The result of the last finished upload (`ui.inspect` → `flickr.last`).
    pub last_result: Option<Value>,
}

impl FlickrUi {
    pub fn uploading(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.upload.is_some()
        }
        #[cfg(target_arch = "wasm32")]
        {
            false
        }
    }

    /// `ui.inspect` → `flickr`.
    pub fn status(&self) -> Value {
        #[cfg(not(target_arch = "wasm32"))]
        let running = self.upload.as_ref().map(UploadTask::status);
        #[cfg(target_arch = "wasm32")]
        let running: Option<Value> = None;
        let albums =
            self.albums.as_ref().map(|l| l.iter().map(|(id, title, photos)| json!({"id": id, "title": title, "photos": photos})).collect::<Vec<_>>());
        json!({
            "busy": self.busy,
            "error": self.error,
            "authorizeUrl": self.authorize_url,
            "upload": running,
            "last": self.last_result,
            "albums": albums,
            "albumsLoading": self.albums_loading,
            "albumsError": self.albums_error,
        })
    }
}

/// The `flickr.*` and `dialog.flickrUpload` commands, run the desktop app's way.
pub fn command(app: &mut LightcraftApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    if !matches!(
        id,
        "dialog.flickrUpload"
            | "flickr.upload"
            | "flickr.albums"
            | "flickr.signIn"
            | "flickr.completeSignIn"
            | "flickr.cancelSignIn"
            | "flickr.signOut"
            | "flickr.setApiKey"
    ) {
        return None;
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (app, p);
        Some(Err("Flickr uploads need the desktop app".into()))
    }
    #[cfg(not(target_arch = "wasm32"))]
    Some(native::command(app, id, p))
}

/// Per frame: the upload's activity row, and its end.
pub fn poll(app: &mut LightcraftApp, ctx: &egui::Context) {
    #[cfg(not(target_arch = "wasm32"))]
    native::poll(app, ctx);
    #[cfg(target_arch = "wasm32")]
    let _ = (app, ctx);
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::UploadTask;

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{Receiver, TryRecvError, channel};
    use std::sync::{Arc, Mutex};

    use lightcraft_engine::activity::{Cancel, TaskGuard};
    use serde_json::{Value, json};

    use crate::LightcraftApp;
    use crate::state::Dialog;

    /// An upload running on a worker thread.
    pub struct UploadTask {
        pub total: usize,
        /// Photos started so far and the one in progress.
        pub progress: Arc<Mutex<(usize, String)>>,
        pub cancel: Arc<AtomicBool>,
        rx: Receiver<Value>,
        /// Its row in the activity stack (`activity.cancel` sets `cancel`).
        guard: TaskGuard,
        /// The sign-in it uses: a refusal signs out only that one.
        account: String,
    }

    impl UploadTask {
        pub fn status(&self) -> Value {
            let (done, current) = self.progress.lock().map(|g| g.clone()).unwrap_or_default();
            json!({"total": self.total, "done": done, "current": current})
        }
    }

    pub(super) fn command(app: &mut LightcraftApp, id: &str, p: &Value) -> Result<Value, String> {
        scope_albums(app);
        match id {
            "dialog.flickrUpload" => {
                if app.session.flickr_token().is_none() {
                    app.ui.dialog = Some(Dialog::Settings { tab: "accounts".into() });
                    return Err(crate::i18n::tr("Sign in to Flickr first").to_string());
                }
                // no album unless one is chosen this time
                let opts = super::UploadDialog {
                    album: String::new(),
                    new_album: false,
                    new_album_title: String::new(),
                    ..app.flickr.last_dialog.clone().unwrap_or_default()
                };
                app.ui.dialog = Some(Dialog::FlickrUpload { opts });
                // the dialog says why when the list can't even be asked for
                if let Err(e) = load_albums(app) {
                    app.flickr.albums_error = Some(e);
                }
                Ok(Value::Null)
            }
            // read on a worker thread like the dialog's list (it can take many pages): the result
            // arrives in `ui.inspect` → `flickr.albums`
            "flickr.albums" => {
                load_albums(app)?;
                Ok(json!({"started": true}))
            }
            "flickr.upload" => start_upload(app, p),
            "flickr.signIn" => sign_in(app),
            "flickr.completeSignIn" => {
                let code = p.get("code").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| app.flickr.code.clone());
                complete_sign_in(app, code)
            }
            // an upload keeps the sign-in it started with, as in Settings: no signing out, and no
            // new key (which signs out), until it ends
            "flickr.signOut" | "flickr.setApiKey" => {
                if app.flickr.uploading() {
                    return Err(crate::i18n::tr("Wait for the upload to Flickr to finish (or stop it) first").to_string());
                }
                app.session.execute(id, p).map_err(|e| e.to_string())
            }
            "flickr.cancelSignIn" => {
                // a request-token answer still on its way is ignored when it comes
                app.flickr.sign_in_attempt = app.flickr.sign_in_attempt.wrapping_add(1);
                app.session.flickr_cancel_sign_in();
                app.flickr.authorize_url = None;
                app.flickr.code.clear();
                app.flickr.submitted_code.clear();
                app.flickr.error = None;
                Ok(app.session.flickr_status())
            }
            _ => Err(format!("unknown command {id}")),
        }
    }

    /// Read the account's albums in the background (the Upload dialog shows the last list
    /// meanwhile). Quiet: no activity row, nothing on screen but the dialog's list.
    fn load_albums(app: &mut LightcraftApp) -> Result<(), String> {
        if app.flickr.albums_loading {
            return Ok(());
        }
        let (client, token) = lightcraft_engine::flickr::albums_request(&app.session)?;
        let account = token.token.clone();
        app.flickr.albums_account = Some(account.clone());
        app.flickr.albums_loading = true;
        let r = crate::tasks::spawn(
            app,
            "Reading Flickr albums",
            None,
            move || lightcraft_engine::flickr::list_albums(&client, &token, &AtomicBool::new(false)),
            move |app, _, r| {
                // another account signed in meanwhile: this list isn't its list
                if current_account(app).as_deref() != Some(account.as_str()) {
                    return;
                }
                app.flickr.albums_loading = false;
                match r {
                    Ok(list) => {
                        app.flickr.albums = Some(list.into_iter().map(|a| (a.id, a.title, a.photos)).collect());
                        app.flickr.albums_error = None;
                    }
                    Err(e) => app.flickr.albums_error = Some(e.to_string()),
                }
            },
        );
        if let Err(e) = r {
            app.flickr.albums_loading = false;
            return Err(e);
        }
        Ok(())
    }

    /// The signed-in account (its token's public half).
    fn current_account(app: &LightcraftApp) -> Option<String> {
        app.session.flickr_token().map(|t| t.token.clone())
    }

    /// Keep the album list to the signed-in account: after a sign-out or another sign-in it is
    /// dropped (with any read in progress, whose answer is then ignored).
    fn scope_albums(app: &mut LightcraftApp) {
        let current = current_account(app);
        if app.flickr.albums_account != current {
            app.flickr.albums = None;
            app.flickr.albums_error = None;
            app.flickr.albums_loading = false;
            app.flickr.albums_account = current;
        }
    }

    /// Step 1 on a worker thread; then the browser opens Flickr's approval page.
    fn sign_in(app: &mut LightcraftApp) -> Result<Value, String> {
        if app.flickr.busy.is_some() {
            return Err("Flickr is already being contacted".into());
        }
        let client = app.session.flickr_client()?;
        app.flickr.busy = Some("Contacting Flickr…");
        app.flickr.error = None;
        app.flickr.sign_in_attempt = app.flickr.sign_in_attempt.wrapping_add(1);
        let attempt = app.flickr.sign_in_attempt;
        let r = crate::tasks::spawn(
            app,
            "Signing in to Flickr",
            Some("flickr"),
            move || client.request_token(&AtomicBool::new(false)).map(|req| (client.authorize_url(&req), req)),
            move |app, ctx, r| {
                app.flickr.busy = None;
                // cancelled while Flickr was answering: nothing to resume, no browser
                if app.flickr.sign_in_attempt != attempt {
                    return;
                }
                match r {
                    Ok((url, req)) => {
                        app.session.flickr_begin_sign_in(req);
                        app.flickr.code.clear();
                        app.flickr.submitted_code.clear();
                        app.flickr.focus_code = true;
                        app.flickr.authorize_url = Some(url.clone());
                        if let Err(e) = crate::links::open(app, &url) {
                            app.flickr.error = Some(e);
                        }
                    }
                    Err(e) => {
                        app.flickr.error = Some(e.to_string());
                        app.toast(ctx, e.to_string());
                    }
                }
            },
        );
        if let Err(e) = r {
            app.flickr.busy = None;
            return Err(e);
        }
        Ok(json!({"started": true}))
    }

    /// Step 3 on a worker thread; the account is saved when it succeeds.
    fn complete_sign_in(app: &mut LightcraftApp, code: String) -> Result<Value, String> {
        if app.flickr.busy.is_some() {
            return Err("Flickr is already being contacted".into());
        }
        if code.trim().is_empty() {
            return Err(crate::i18n::tr("Enter the code Flickr shows").to_string());
        }
        let request = app.session.flickr_pending().cloned().ok_or("start the sign-in first")?;
        let client = app.session.flickr_client()?;
        app.flickr.busy = Some("Contacting Flickr…");
        app.flickr.error = None;
        let asked = request.clone();
        let r = crate::tasks::spawn(
            app,
            "Signing in to Flickr",
            Some("flickr"),
            move || client.access_token(&asked, &code, &AtomicBool::new(false)),
            move |app, ctx, r| {
                app.flickr.busy = None;
                // cancelled (or restarted) while the code was being exchanged: the token is dropped,
                // so a cancelled sign-in never becomes the account
                if app.session.flickr_pending() != Some(&request) {
                    return;
                }
                let r = r.map_err(|e| e.to_string()).and_then(|token| {
                    let name = if token.fullname.trim().is_empty() { token.username.clone() } else { token.fullname.clone() };
                    app.session.flickr_finish_sign_in(token).map(|_| name)
                });
                match r {
                    Ok(name) => {
                        app.flickr.authorize_url = None;
                        app.flickr.code.clear();
                        app.toast(ctx, crate::i18n::tr_format!("Signed in to Flickr as {name}", name = name));
                    }
                    Err(e) => {
                        app.flickr.error = Some(e.clone());
                        app.toast(ctx, e);
                    }
                }
            },
        );
        if let Err(e) = r {
            app.flickr.busy = None;
            return Err(e);
        }
        Ok(json!({"started": true}))
    }

    /// Render and upload in the background (photos are set up here, on the UI thread).
    fn start_upload(app: &mut LightcraftApp, p: &Value) -> Result<Value, String> {
        if app.flickr.upload.is_some() {
            return Err("an upload to Flickr is already running".into());
        }
        let job = lightcraft_engine::flickr::prepare_upload(&mut app.session, p)?;
        let (total, account) = (job.len(), job.account());
        let progress = Arc::new(Mutex::new((0, String::new())));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        let (pr, c) = (progress.clone(), cancel.clone());
        let repaint = app.tasks.repaint.clone();
        let work = move || {
            let r = job.run(
                &mut |done, name| {
                    if let Ok(mut g) = pr.lock() {
                        *g = (done, name.to_string());
                    }
                    if let Some(ctx) = &repaint {
                        ctx.request_repaint();
                    }
                    !c.load(Ordering::Relaxed)
                },
                &c,
            );
            let _ = tx.send(r);
            if let Some(ctx) = &repaint {
                ctx.request_repaint();
            }
        };
        std::thread::Builder::new().name("flickr-upload".into()).spawn(work).map_err(|e| format!("could not start the upload: {e}"))?;
        let guard = app.session.activity.start("upload", "Uploading to Flickr", Cancel::Flag(cancel.clone()));
        guard.progress(0, total as u64);
        app.flickr.upload = Some(UploadTask { total, progress, cancel, rx, guard, account });
        Ok(json!({"background": true, "total": total}))
    }

    pub(super) fn poll(app: &mut LightcraftApp, ctx: &egui::Context) {
        scope_albums(app);
        let Some(task) = &app.flickr.upload else { return };
        match task.rx.try_recv() {
            Ok(r) => {
                let account = app.flickr.upload.take().map(|t| t.account).unwrap_or_default();
                app.session.flickr_record_upload(&r, &account);
                let (ok, total, failed) = (r["uploaded"].as_u64().unwrap_or(0), r["total"].as_u64().unwrap_or(0), r["failed"].as_u64().unwrap_or(0));
                let mut m =
                    crate::i18n::tr_format!("Uploaded {ok} of {total} photo{} to Flickr", if total == 1 { "" } else { "s" }, ok = ok, total = total);
                if r["signInNeeded"] == true {
                    m = crate::i18n::tr("Flickr no longer accepts the sign-in: sign in again in Settings → Accounts").to_string();
                } else if failed > 0 {
                    let first = r["photos"].as_array().and_then(|a| a.iter().find_map(|p| p["error"].as_str())).unwrap_or("error");
                    m += &crate::i18n::tr_format!(" · {} failed: {}", failed, first);
                }
                let album_failed = r["albumFailed"].as_u64().unwrap_or(0);
                if let Some(e) = r["albumCreateError"].as_str() {
                    m += &crate::i18n::tr_format!(" · the album couldn't be made: {}", e);
                } else if album_failed > 0 && r["signInNeeded"] != true {
                    m += &crate::i18n::tr_format!(" · {} not added to the album", album_failed);
                }
                if let (Some(id), Some(title)) = (r["createdAlbum"]["id"].as_str(), r["createdAlbum"]["title"].as_str()) {
                    m += &crate::i18n::tr_format!(" · new album “{}”", title);
                    // the next dialog lists it even before the albums are read again (when the list
                    // is still this account's)
                    let photos = r["uploaded"].as_u64().unwrap_or(0).saturating_sub(album_failed);
                    if app.flickr.albums_account.as_deref() == Some(account.as_str()) {
                        app.flickr.albums.get_or_insert_with(Vec::new).insert(0, (id.to_string(), title.to_string(), photos));
                    }
                }
                if r["cancelled"] == true {
                    m += crate::i18n::tr(" · cancelled");
                }
                app.flickr.last_result = Some(r);
                app.toast(ctx, m);
            }
            Err(TryRecvError::Empty) => {
                let (done, current) = task.progress.lock().map(|g| g.clone()).unwrap_or_default();
                task.guard.progress(done as u64, task.total as u64);
                task.guard.detail(&current);
            }
            Err(TryRecvError::Disconnected) => {
                app.flickr.upload = None;
                app.toast(ctx, crate::i18n::tr("The upload to Flickr stopped unexpectedly"));
            }
        }
    }
}
