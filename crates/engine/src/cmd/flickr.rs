//! Flickr commands (see [`crate::flickr`]). The network steps (`flickr.signIn`,
//! `flickr.completeSignIn`, `flickr.upload`) run synchronously here, for the CLI and MCP; the
//! desktop app runs the same ids on a worker thread with a row in the activity stack.

use std::sync::atomic::AtomicBool;

use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, has_selection};
use crate::{Result, Session};

fn set_api_key(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "flickr.setApiKey";
    let get = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    s.flickr_set_api_key(&get("key"), &get("secret")).map_err(|e| bad(ID, e))?;
    Ok(s.flickr_status())
}

fn sign_in(s: &mut Session, _: &Value) -> Result<Value> {
    const ID: &str = "flickr.signIn";
    let client = s.flickr_client().map_err(|e| bad(ID, e))?;
    let request = client.request_token(&AtomicBool::new(false)).map_err(|e| bad(ID, e.to_string()))?;
    let url = client.authorize_url(&request);
    s.flickr_begin_sign_in(request);
    Ok(
        json!({"authorizeUrl": url, "next": "open authorizeUrl, approve LightCraft, then run flickr.completeSignIn {code} with the code Flickr shows"}),
    )
}

fn complete_sign_in(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "flickr.completeSignIn";
    let code = p.get("code").and_then(Value::as_str).ok_or_else(|| bad(ID, "missing `code` (the code Flickr shows after you approve LightCraft)"))?;
    let request = s.flickr_pending().cloned().ok_or_else(|| bad(ID, "no sign-in in progress: run flickr.signIn first"))?;
    let client = s.flickr_client().map_err(|e| bad(ID, e))?;
    let token = client.access_token(&request, code, &AtomicBool::new(false)).map_err(|e| bad(ID, e.to_string()))?;
    s.flickr_finish_sign_in(token).map_err(|e| bad(ID, e))?;
    Ok(s.flickr_status())
}

fn albums(s: &mut Session, _: &Value) -> Result<Value> {
    const ID: &str = "flickr.albums";
    let (client, token) = crate::flickr::albums_request(s).map_err(|e| bad(ID, e))?;
    let albums = crate::flickr::list_albums(&client, &token, &AtomicBool::new(false)).map_err(|e| bad(ID, e.to_string()))?;
    Ok(json!({"albums": albums}))
}

fn upload(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "flickr.upload";
    let job = crate::flickr::prepare_upload(s, p).map_err(|e| bad(ID, e))?;
    let account = job.account();
    let result = job.run(&mut |_, _| true, &AtomicBool::new(false));
    s.flickr_record_upload(&result, &account);
    Ok(result)
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "flickr.status", "Flickr Status", [], None,
            "{} → {hasKey, ownKey, builtinKey, apiKey, signedIn, username, fullname, userNsid, pending: a sign-in waits for its code, saved: the account is kept on disk, problem?, apiKeyPage, lastUpload?}",
            always, |s, _| Ok(s.flickr_status())),
        cmd!(query "flickr.setApiKey", "Set Flickr API Key", [], None,
            "{key, secret} — the Flickr API key and secret to use (both empty: the build's own key, if any); a different key signs out → status",
            always, set_api_key),
        cmd!(query "flickr.signIn", "Sign In to Flickr", [], None,
            "{} — starts signing in (needs an API key): → {authorizeUrl}; open it, approve LightCraft, then flickr.completeSignIn {code}",
            always, sign_in),
        cmd!(query "flickr.completeSignIn", "Finish Flickr Sign-In", [], None,
            "{code: the code Flickr shows (123-456-789)} — finishes flickr.signIn; the account is saved for next time → status",
            always, complete_sign_in),
        cmd!(query "flickr.cancelSignIn", "Cancel Flickr Sign-In", [], None,
        "{} — drops a sign-in started with flickr.signIn → status",
        always, |s, _| {
            s.flickr_cancel_sign_in();
            Ok(s.flickr_status())
        }),
        cmd!(query "flickr.signOut", "Sign Out of Flickr", [], None,
        "{} — forgets the signed-in account on this computer (the API key stays) → status",
        always, |s, _| {
            s.flickr_sign_out().map_err(|e| bad("flickr.signOut", e))?;
            Ok(s.flickr_status())
        }),
        cmd!(query "flickr.albums", "Flickr Albums", [], None,
            "{} — the signed-in account's albums, as arranged on Flickr → {albums: [{id, title, photos}]}; pass an id as flickr.upload {album}",
            always, albums),
        CommandSpec {
            explicit_targets: true,
            ..cmd!(query "flickr.upload", "Upload to Flickr", [], None,
                "{ids?: photo ids (default: the selection, at most 500), longEdge?: px (0 = full size; default 2048), quality?: 1..100 (default 90), privacy?: public|friends|family|friendsAndFamily|private (default public), hidden?: keep out of public searches, tags?: keywords as tags (default true), description?: caption as description (default true), removeLocation?: leave GPS out (default true), album?: an album id from flickr.albums (default: none), newAlbum?: a name: make a new album for them (around the first photo that goes up)} — renders each photo with its edits (JPEG, sRGB), uploads it to the signed-in account and puts it in the album → {total, uploaded, failed, albumFailed, cancelled, signInNeeded, album, createdAlbum: {id, title}?, albumCreateError?, photos: [{photo, file, photoId, url, albumError?} | {photo, file, error}]}",
                has_selection, upload)
        },
    ]
}
