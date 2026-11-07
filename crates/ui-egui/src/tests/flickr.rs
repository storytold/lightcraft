//! Flickr in the desktop app against a local stand-in for Flickr (never the internet): the
//! Settings → Accounts sign-in in the background, File → Share → Flickr… and its activity row.

#![cfg(not(target_arch = "wasm32"))]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(30);

/// A stand-in for Flickr; `delay` holds each answer back (to see an upload in progress).
fn fake(delay: Duration) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    std::thread::spawn(move || {
        let mut n = 0u64;
        for s in l.incoming() {
            let Ok(mut s) = s else { break };
            let mut got = Vec::new();
            let mut buf = [0u8; 65536];
            let head_end = loop {
                let Ok(k) = s.read(&mut buf) else { break 0 };
                if k == 0 {
                    break 0;
                }
                got.extend_from_slice(&buf[..k]);
                if let Some(i) = got.windows(4).position(|w| w == b"\r\n\r\n") {
                    break i + 4;
                }
            };
            if head_end == 0 {
                continue;
            }
            let head = String::from_utf8_lossy(&got[..head_end]).to_string();
            let len: usize = head.lines().find_map(|l| l.strip_prefix("Content-Length: ")).map_or(0, |v| v.trim().parse().unwrap());
            while got.len() < head_end + len {
                let Ok(k) = s.read(&mut buf) else { break };
                if k == 0 {
                    break;
                }
                got.extend_from_slice(&buf[..k]);
            }
            std::thread::sleep(delay);
            n += 1;
            let body = if head.contains("/request_token") {
                "oauth_callback_confirmed=true&oauth_token=req&oauth_token_secret=reqsecret".to_string()
            } else if head.contains("/services/rest/") && String::from_utf8_lossy(&got).contains("photosets.create") {
                r#"{"photoset":{"id":"601","url":"x"},"stat":"ok"}"#.to_string()
            } else if head.contains("/services/rest/") {
                r#"{"photosets":{"page":1,"pages":1,"photoset":[{"id":"501","title":{"_content":"Portfolio"},"photos":"9"},{"id":"502","title":{"_content":"Travel"},"photos":4}]},"stat":"ok"}"#.to_string()
            } else if head.contains("/access_token") {
                "fullname=Ansel%20A&oauth_token=acc&oauth_token_secret=accsecret&user_nsid=7%40N01&username=ansel".to_string()
            } else {
                format!("<rsp stat=\"ok\"><photoid>{n}</photoid></rsp>")
            };
            let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        }
    });
    base
}

/// The demo library with Flickr pointed at `base`; opened URLs are collected.
fn app(base: &str) -> (Headless, Arc<Mutex<Vec<String>>>) {
    let opened = Arc::new(Mutex::new(Vec::new()));
    let log = opened.clone();
    let services = Services {
        png: None,
        open_url: Some(Box::new(move |u: &str| {
            log.lock().unwrap().push(u.to_string());
            Ok(())
        })),
        ..Default::default()
    };
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    app.session.flickr.endpoints = lightcraft_engine::flickr::Endpoints::all(base);
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.settle(T);
    (h, opened)
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn settings(h: &mut Headless) {
    h.app.ui.dialog = Some(Dialog::Settings { tab: "accounts".into() });
    h.step();
    h.step();
}

#[test]
fn settings_lead_from_the_api_key_through_the_sign_in() {
    let base = fake(Duration::ZERO);
    let (mut h, opened) = app(&base);
    settings(&mut h);
    // step 1 and 2: get a key, paste it; Continue waits for both halves
    assert!(has(&h, "button:flickrGetKey") && has(&h, "field:flickrKey") && has(&h, "field:flickrSecret"), "the key steps");
    assert!(!has(&h, "button:flickrSignIn"), "no sign-in button before there is a key");
    h.request("ui.clickWidget", json!({"id": "button:flickrGetKey"}), T);
    h.step();
    assert_eq!(opened.lock().unwrap().last().map(String::as_str), Some(lightcraft_engine::flickr::API_KEY_PAGE));
    h.app.flickr.key = "mykey".into();
    h.app.flickr.secret = "mysecret".into();
    h.step();
    h.step();
    // Continue saves the key and goes straight on to Flickr's approval page
    assert_eq!(h.request("ui.clickWidget", json!({"id": "button:flickrContinue"}), T)["ok"], true);
    assert!(h.step_until(T, |h| h.app.session.flickr_status()["pending"] == true && h.app.flickr.busy.is_none()), "step 1 of the sign-in finishes");
    assert_eq!(h.app.session.flickr_status()["ownKey"], true);
    assert!(h.app.flickr.secret.is_empty(), "the secret field empties once saved");
    assert_eq!(
        opened.lock().unwrap().last().map(String::as_str),
        Some(format!("{base}/services/oauth/authorize?oauth_token=req&perms=write").as_str())
    );
    settings(&mut h);
    assert!(has(&h, "field:flickrCode") && has(&h, "button:flickrFinishSignIn"), "waiting for the code");
    assert!(h.app.run("flickr.completeSignIn", json!({"code": " "})).is_err(), "an empty code");

    // a whole pasted code is sent by itself
    h.app.flickr.code = "123-456-789".into();
    h.step();
    assert!(h.step_until(T, |h| h.app.session.flickr_status()["signedIn"] == true), "signed in without another click");
    assert_eq!(h.app.session.flickr_status()["username"], "ansel");
    settings(&mut h);
    assert!(has(&h, "button:flickrSignOut") && has(&h, "button:flickrPhotostream"), "the signed-in tab");
    h.request("ui.clickWidget", json!({"id": "button:flickrSignOut"}), T);
    assert!(h.step_until(T, |h| h.app.session.flickr_status()["signedIn"] == false));
    // with a key already there, the tab offers the sign-in at once
    settings(&mut h);
    assert!(has(&h, "button:flickrSignIn") && !has(&h, "field:flickrKey"), "the key is folded away");
}

#[test]
fn a_refused_code_is_not_sent_again_until_it_changes() {
    let (mut h, _) = app("http://127.0.0.1:9");
    h.app.session.flickr_set_api_key("k", "s").unwrap();
    h.app.session.flickr_begin_sign_in(lightcraft_engine::flickr::RequestToken { token: "t".into(), secret: "s".into() });
    h.app.flickr.code = "111-222-333".into();
    settings(&mut h);
    assert!(h.step_until(T, |h| h.app.flickr.busy.is_none() && h.app.flickr.error.is_some()), "the refused code says why");
    assert_eq!(h.app.flickr.submitted_code, "111-222-333");
    h.app.flickr.error = None;
    for _ in 0..5 {
        h.step();
    }
    assert!(h.app.flickr.busy.is_none() && h.app.flickr.error.is_none(), "not sent again by itself");
}

fn signed_in(h: &mut Headless) {
    h.app.session.flickr_set_api_key("k", "s").unwrap();
    h.app
        .session
        .flickr_finish_sign_in(lightcraft_engine::flickr::Token {
            token: "acc".into(),
            secret: "accsecret".into(),
            user_nsid: "7@N01".into(),
            username: "ansel".into(),
            fullname: String::new(),
        })
        .unwrap();
}

#[test]
fn the_upload_dialog_needs_a_sign_in() {
    let (mut h, _) = app("http://127.0.0.1:9");
    assert!(h.app.run("dialog.flickrUpload", json!({})).is_err());
    assert!(matches!(&h.app.ui.dialog, Some(Dialog::Settings { tab }) if tab == "accounts"), "Settings → Accounts opens instead");
    signed_in(&mut h);
    h.app.run("dialog.flickrUpload", json!({})).unwrap();
    assert!(matches!(&h.app.ui.dialog, Some(Dialog::FlickrUpload { .. })));
    h.step();
    h.step();
    assert!(has(&h, "combo:flickrPrivacy") && has(&h, "check:flickrRemoveLocation"));
}

#[test]
fn uploads_in_the_background_with_a_row_and_remembers_the_choices() {
    let base = fake(Duration::from_millis(300));
    let (mut h, _) = app(&base);
    signed_in(&mut h);
    let ids: Vec<u64> = h.app.session.catalog.photos().take(3).map(|p| p.id.0).collect();
    h.app.run("library.select", json!({"ids": ids, "active": ids[0]})).unwrap();
    let opts = crate::flickr::UploadDialog { long_edge: 256, privacy: "private".into(), ..Default::default() };
    h.app.ui.dialog = Some(Dialog::FlickrUpload { opts: opts.clone() });
    let dialog = h.app.ui.dialog.take().unwrap();
    crate::panels::dialogs::confirm_dialog(&mut h.app, &dialog).unwrap();
    let rows = h.app.session.activity.list();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!((rows[0].kind, rows[0].label.as_str(), rows[0].total), ("upload", "Uploading to Flickr", 3));
    assert!(rows[0].cancellable);
    assert!(!crate::menus::ui_enabled(&h.app, "dialog.flickrUpload"), "one upload at a time");
    assert!(h.step_until(T, |h| h.app.flickr.last_result.is_some()), "the upload finishes");
    let r = h.app.flickr.last_result.clone().unwrap();
    assert_eq!((r["uploaded"].as_u64(), r["failed"].as_u64()), (Some(3), Some(0)), "{r}");
    assert!(h.app.session.activity.list().is_empty(), "the row goes with the upload");
    assert_eq!(h.app.flickr.last_dialog, Some(opts), "the next dialog starts from these choices");
    assert!(h.app.ui.toast.as_ref().is_some_and(|(t, _, _)| t.contains("3 of 3")), "{:?}", h.app.ui.toast);
    let inspect = h.request("ui.inspect", json!({}), T);
    assert_eq!(inspect["result"]["flickr"]["last"]["uploaded"], 3);
}

#[test]
fn the_rows_cross_stops_an_upload() {
    let base = fake(Duration::from_millis(400));
    let (mut h, _) = app(&base);
    signed_in(&mut h);
    let ids: Vec<u64> = h.app.session.catalog.photos().take(6).map(|p| p.id.0).collect();
    h.app.run("flickr.upload", json!({"ids": ids, "longEdge": 128})).unwrap();
    let row = h.app.session.activity.list()[0].id;
    h.app.session.activity.cancel(row).unwrap();
    assert!(h.step_until(T, |h| h.app.flickr.last_result.is_some()));
    let r = h.app.flickr.last_result.clone().unwrap();
    assert_eq!(r["cancelled"], true, "{r}");
    assert!(r["uploaded"].as_u64().unwrap() < 6);
    assert!(h.app.session.activity.list().is_empty());
}

#[test]
fn the_upload_dialog_lists_albums_and_starts_with_none() {
    let base = fake(Duration::ZERO);
    let (mut h, _) = app(&base);
    signed_in(&mut h);
    // the last upload went into an album: the next dialog still starts with none
    h.app.flickr.last_dialog = Some(crate::flickr::UploadDialog { album: "502".into(), privacy: "friends".into(), ..Default::default() });
    h.app.run("dialog.flickrUpload", json!({})).unwrap();
    let Some(Dialog::FlickrUpload { opts }) = &h.app.ui.dialog else { panic!("no dialog") };
    assert_eq!((opts.album.as_str(), opts.privacy.as_str()), ("", "friends"), "no album by default; the other choices remembered");
    assert!(h.step_until(T, |h| h.app.flickr.albums.is_some()), "the albums load in the background");
    assert_eq!(h.app.flickr.albums.clone().unwrap(), [("501".to_string(), "Portfolio".to_string(), 9), ("502".to_string(), "Travel".to_string(), 4)]);
    h.step();
    h.step();
    assert!(has(&h, "combo:flickrAlbum"));
    assert_eq!(crate::flickr::UploadDialog::default().params()["album"], serde_json::Value::Null);

    // choose an album and upload: each photo joins it
    let ids: Vec<u64> = h.app.session.catalog.photos().take(2).map(|p| p.id.0).collect();
    h.app.run("library.select", json!({"ids": ids, "active": ids[0]})).unwrap();
    let dialog = Dialog::FlickrUpload { opts: crate::flickr::UploadDialog { album: "501".into(), long_edge: 256, ..Default::default() } };
    h.app.ui.dialog = None;
    crate::panels::dialogs::confirm_dialog(&mut h.app, &dialog).unwrap();
    assert!(h.step_until(T, |h| h.app.flickr.last_result.is_some()));
    let r = h.app.flickr.last_result.clone().unwrap();
    assert_eq!((r["uploaded"].as_u64(), r["albumFailed"].as_u64(), r["album"].as_str()), (Some(2), Some(0), Some("501")), "{r}");

    // an album that disappeared from the list falls back to none
    h.app.flickr.albums = Some(vec![("501".into(), "Portfolio".into(), 9)]);
    h.app.ui.dialog = Some(Dialog::FlickrUpload { opts: crate::flickr::UploadDialog { album: "gone".into(), ..Default::default() } });
    h.step();
    h.step();
    let Some(Dialog::FlickrUpload { opts }) = &h.app.ui.dialog else { panic!("no dialog") };
    assert_eq!(opts.album, "");
}

#[test]
fn makes_a_new_album_from_the_dialog() {
    let base = fake(Duration::ZERO);
    let (mut h, _) = app(&base);
    signed_in(&mut h);
    h.app.flickr.last_dialog = Some(crate::flickr::UploadDialog { new_album: true, new_album_title: "Last time".into(), ..Default::default() });
    h.app.run("dialog.flickrUpload", json!({})).unwrap();
    let Some(Dialog::FlickrUpload { opts }) = &h.app.ui.dialog else { panic!("no dialog") };
    assert!(!opts.new_album && opts.new_album_title.is_empty(), "a new dialog doesn't make an album unless asked");
    h.step_until(T, |h| h.app.flickr.albums.is_some());

    // the params a new album sends, and an empty name stays in the dialog with a reason
    let named = crate::flickr::UploadDialog { new_album: true, new_album_title: "Road Trip".into(), album: "501".into(), ..Default::default() };
    assert_eq!((named.params()["newAlbum"].as_str(), named.params()["album"].is_null()), (Some("Road Trip"), true));
    let unnamed = Dialog::FlickrUpload { opts: crate::flickr::UploadDialog { new_album: true, ..Default::default() } };
    let e = crate::panels::dialogs::confirm_dialog(&mut h.app, &unnamed).unwrap_err();
    assert!(e.contains("name"), "{e}");

    let ids: Vec<u64> = h.app.session.catalog.photos().take(2).map(|p| p.id.0).collect();
    h.app.run("library.select", json!({"ids": ids, "active": ids[0]})).unwrap();
    h.app.ui.dialog = None;
    crate::panels::dialogs::confirm_dialog(&mut h.app, &Dialog::FlickrUpload { opts: crate::flickr::UploadDialog { long_edge: 256, ..named } })
        .unwrap();
    assert!(h.step_until(T, |h| h.app.flickr.last_result.is_some()));
    let r = h.app.flickr.last_result.clone().unwrap();
    assert_eq!((r["uploaded"].as_u64(), r["album"].as_str(), r["albumFailed"].as_u64()), (Some(2), Some("601"), Some(0)), "{r}");
    assert!(h.app.ui.toast.as_ref().is_some_and(|(t, _, _)| t.contains("Road Trip")), "{:?}", h.app.ui.toast);
    assert_eq!(h.app.flickr.albums.as_ref().unwrap()[0], ("601".to_string(), "Road Trip".to_string(), 2), "listed at once");
}

#[test]
fn share_lives_in_the_file_menu_and_the_top_bar() {
    let entry = crate::menus::UI_COMMANDS.iter().find(|c| c.0 == "dialog.flickrUpload").expect("a menu entry");
    assert_eq!((entry.1, entry.3), ("Flickr…", "File>Share"));
    let (mut h, _) = app("http://127.0.0.1:9");
    signed_in(&mut h);
    h.step();
    assert_eq!(h.request("ui.clickWidget", json!({"id": "icon:share"}), T)["ok"], true);
    for _ in 0..3 {
        h.step();
    }
    assert!(has(&h, "menu:shareExport") && has(&h, "menu:shareFlickr"), "the share icon opens Export… and Share to Flickr…");
    assert_eq!(h.request("ui.clickWidget", json!({"id": "menu:shareFlickr"}), T)["ok"], true);
    for _ in 0..3 {
        h.step();
    }
    assert!(matches!(&h.app.ui.dialog, Some(Dialog::FlickrUpload { .. })), "{:?}", h.app.ui.dialog.is_some());
}

/// Sends on drop, so the stand-in's held answer is released on every way out of a test (a failed
/// assertion included) and its thread ends.
struct Release(std::sync::mpsc::Sender<()>);

impl Drop for Release {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

#[test]
fn a_refused_upload_does_not_sign_out_an_account_connected_meanwhile() {
    // Flickr refuses the upload's (old) sign-in, once the test lets it answer
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let release = Release(release_tx);
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { break };
            let mut buf = [0u8; 65536];
            let mut got = Vec::new();
            let head_end = loop {
                let Ok(n) = s.read(&mut buf) else { break 0 };
                if n == 0 {
                    break 0;
                }
                got.extend_from_slice(&buf[..n]);
                if let Some(i) = got.windows(4).position(|w| w == b"\r\n\r\n") {
                    break i + 4;
                }
            };
            // read the whole upload: closing with part of it unread resets the connection (os
            // error 10054 on Windows), and the client would see that instead of the answer below
            let head = String::from_utf8_lossy(&got[..head_end]).to_string();
            let len: usize = head.lines().find_map(|l| l.strip_prefix("Content-Length: ")).map_or(0, |v| v.trim().parse().unwrap());
            while got.len() < head_end + len {
                let Ok(n) = s.read(&mut buf) else { break };
                if n == 0 {
                    break;
                }
                got.extend_from_slice(&buf[..n]);
            }
            // held until the test releases it (or gives up after T)
            let _ = release_rx.recv_timeout(T);
            let body = "<rsp stat=\"fail\"><err code=\"98\" msg=\"Invalid auth token\" /></rsp>";
            let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        }
    });
    let (mut h, _) = app(&base);
    signed_in(&mut h);
    let id = h.app.session.catalog.photos().next().unwrap().id.0;
    h.app.run("flickr.upload", json!({"ids": [id], "longEdge": 128})).unwrap();
    // while it runs (its answer is held), Settings won't sign out or in
    settings(&mut h);
    assert!(h.app.flickr.uploading() && has(&h, "button:flickrSignOut"), "Sign Out is shown");
    h.request("ui.clickWidget", json!({"id": "button:flickrSignOut"}), T);
    h.step();
    assert!(h.app.flickr.uploading(), "the upload is still held");
    assert_eq!(h.app.session.flickr_status()["signedIn"], true, "a click on the held Sign Out does nothing");
    // nor does the command, from an agent
    assert!(h.app.run("flickr.signOut", json!({})).is_err());
    assert!(h.request("engine.execute", json!({"command": "flickr.signOut"}), T)["error"].is_string());
    assert!(h.app.run("flickr.setApiKey", json!({"key": "other", "secret": "key"})).is_err(), "a new key would sign out too");
    assert_eq!(h.app.session.flickr_status()["signedIn"], true);
    // another account signs in anyway (through the engine, as an agent could)
    h.app
        .session
        .flickr_finish_sign_in(lightcraft_engine::flickr::Token {
            token: "account-b".into(),
            secret: "b-secret".into(),
            user_nsid: "8@N01".into(),
            username: "dorothea".into(),
            fullname: String::new(),
        })
        .unwrap();
    drop(release);
    assert!(h.step_until(T, |h| h.app.flickr.last_result.is_some()), "the upload ends");
    assert_eq!(h.app.flickr.last_result.as_ref().unwrap()["signInNeeded"], true, "{}", h.app.flickr.last_result.as_ref().unwrap());
    assert_eq!(h.app.session.flickr_status()["username"], "dorothea", "the account connected meanwhile stays signed in");
}

#[test]
fn albums_through_the_control_channel_load_in_the_background() {
    let base = fake(Duration::from_millis(300));
    let (mut h, _) = app(&base);
    assert!(h.app.run("flickr.albums", json!({})).is_err(), "signed out: an error, not an empty list");
    signed_in(&mut h);
    // it returns at once (the stand-in answers 300 ms later): the window keeps drawing
    let r = h.request("engine.execute", json!({"command": "flickr.albums"}), T);
    assert_eq!(r["result"]["started"], true, "{r}");
    assert!(h.app.flickr.albums_loading && h.app.flickr.albums.is_none());
    assert!(h.step_until(T, |h| h.app.flickr.albums.is_some()), "the albums arrive");
    let inspect = h.request("ui.inspect", json!({}), T);
    let flickr = &inspect["result"]["flickr"];
    assert_eq!(flickr["albumsLoading"], false);
    assert_eq!(flickr["albums"], json!([{"id": "501", "title": "Portfolio", "photos": 9}, {"id": "502", "title": "Travel", "photos": 4}]));
}

#[test]
fn the_album_list_belongs_to_the_signed_in_account() {
    let base = fake(Duration::from_millis(300));
    let (mut h, _) = app(&base);
    signed_in(&mut h);
    h.app.run("flickr.albums", json!({})).unwrap();
    assert!(h.step_until(T, |h| h.app.flickr.albums.is_some()));
    // another account signs in: the first one's list goes
    h.app
        .session
        .flickr_finish_sign_in(lightcraft_engine::flickr::Token {
            token: "account-b".into(),
            secret: "b-secret".into(),
            user_nsid: "8@N01".into(),
            username: "dorothea".into(),
            fullname: String::new(),
        })
        .unwrap();
    h.step();
    assert!(h.app.flickr.albums.is_none(), "account A's albums aren't offered for account B");
    // a read started for B, then a switch back to A before it answers: B's list is ignored
    h.app.run("flickr.albums", json!({})).unwrap();
    signed_in(&mut h);
    for _ in 0..40 {
        h.step();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(h.app.flickr.albums_account.as_deref(), Some("acc"));
    assert!(h.app.flickr.albums.is_none(), "the late answer for account B is dropped");
}

#[test]
fn the_dialog_says_why_there_are_no_albums() {
    let (mut h, _) = app("http://127.0.0.1:9");
    // signed in, but with no API key: the albums can't even be asked for
    h.app
        .session
        .flickr_finish_sign_in(lightcraft_engine::flickr::Token { token: "acc".into(), secret: "accsecret".into(), ..Default::default() })
        .unwrap();
    if lightcraft_engine::flickr::albums_request(&h.app.session).is_ok() {
        return; // a build with its own key can always ask
    }
    h.app.run("dialog.flickrUpload", json!({})).unwrap();
    assert!(h.app.flickr.albums_error.is_some(), "the dialog shows a reason, not an empty list");
}

#[test]
fn a_cancelled_sign_in_does_not_come_back() {
    let base = fake(Duration::from_millis(400));
    let (mut h, opened) = app(&base);
    h.app.session.flickr_set_api_key("k", "s").unwrap();
    // cancelled while Flickr is still giving the request token
    h.app.run("flickr.signIn", json!({})).unwrap();
    h.app.run("flickr.cancelSignIn", json!({})).unwrap();
    assert!(h.step_until(T, |h| h.app.flickr.busy.is_none()), "the step ends");
    assert_eq!(h.app.session.flickr_status()["pending"], false, "nothing resumed");
    assert!(opened.lock().unwrap().is_empty(), "no approval page opened");
    // cancelled while the code is being exchanged: the token is dropped
    h.app.run("flickr.signIn", json!({})).unwrap();
    assert!(h.step_until(T, |h| h.app.session.flickr_status()["pending"] == true));
    h.app.run("flickr.completeSignIn", json!({"code": "123-456-789"})).unwrap();
    h.app.run("flickr.cancelSignIn", json!({})).unwrap();
    assert!(h.step_until(T, |h| h.app.flickr.busy.is_none()));
    assert_eq!(h.app.session.flickr_status()["signedIn"], false, "a cancelled sign-in never becomes the account");
}

#[test]
fn the_key_field_follows_a_key_changed_elsewhere() {
    let (mut h, _) = app("http://127.0.0.1:9");
    h.app.session.flickr_set_api_key("first", "s1").unwrap();
    settings(&mut h);
    assert_eq!(h.app.flickr.key, "first");
    // an agent sets another key
    h.app.run("flickr.setApiKey", json!({"key": "second", "secret": "s2"})).unwrap();
    h.step();
    h.step();
    assert_eq!(h.app.flickr.key, "second", "Save Key would otherwise send the old key");
}

#[test]
fn signing_out_drops_a_sign_in_still_on_its_way() {
    let base = fake(Duration::from_millis(400));
    let (mut h, opened) = app(&base);
    h.app.session.flickr_set_api_key("k", "s").unwrap();
    h.app.run("flickr.signIn", json!({})).unwrap();
    // an agent signs out (or sets another key) while Flickr is still answering
    h.app.run("flickr.signOut", json!({})).unwrap();
    assert!(h.step_until(T, |h| h.app.flickr.busy.is_none()), "the step ends");
    assert_eq!(h.app.session.flickr_status()["pending"], false, "the old sign-in is not resumed");
    assert!(opened.lock().unwrap().is_empty(), "no approval page opened");
}

#[test]
fn a_new_sign_in_can_start_right_after_a_cancel() {
    let base = fake(Duration::from_millis(1500));
    let (mut h, opened) = app(&base);
    h.app.session.flickr_set_api_key("k", "s").unwrap();
    h.app.run("flickr.signIn", json!({})).unwrap();
    assert!(h.app.flickr.busy.is_some());
    let first = h.app.flickr.busy_cancel.clone().expect("the step can be stopped");
    h.app.run("flickr.cancelSignIn", json!({})).unwrap();
    assert!(h.app.flickr.busy.is_none(), "Cancel frees the sign-in at once");
    assert!(first.load(std::sync::atomic::Ordering::Relaxed), "the stopped step's worker is told to give up");
    // a new sign-in starts without waiting for the old request; it is the one that goes on
    h.app.run("flickr.signIn", json!({})).unwrap();
    assert!(h.step_until(T, |h| h.app.session.flickr_status()["pending"] == true), "the new sign-in reaches the approval step");
    assert!(h.app.flickr.busy.is_none());
    assert_eq!(opened.lock().unwrap().len(), 1, "one approval page, for the new sign-in");
}

#[test]
fn a_new_key_stops_a_sign_in_under_way_and_the_same_key_does_not() {
    let base = fake(Duration::from_millis(400));
    let (mut h, opened) = app(&base);
    h.app.session.flickr_set_api_key("k", "s").unwrap();
    // the same key again changes nothing: the sign-in goes on
    h.app.run("flickr.signIn", json!({})).unwrap();
    h.app.run("flickr.setApiKey", json!({"key": "k", "secret": "s"})).unwrap();
    assert!(h.app.flickr.busy.is_some(), "still signing in");
    assert!(h.step_until(T, |h| h.app.session.flickr_status()["pending"] == true));
    assert_eq!(opened.lock().unwrap().len(), 1);
    // another key while a sign-in is on its way: it stops, and its answer (for the old key) is dropped
    h.app.run("flickr.cancelSignIn", json!({})).unwrap();
    h.app.run("flickr.signIn", json!({})).unwrap();
    h.app.run("flickr.setApiKey", json!({"key": "other", "secret": "key"})).unwrap();
    assert!(h.app.flickr.busy.is_none(), "the old key's sign-in stops at once");
    for _ in 0..30 {
        h.step();
        std::thread::sleep(Duration::from_millis(30));
    }
    assert_eq!(h.app.session.flickr_status()["pending"], false, "not resumed for the new key");
    assert_eq!(opened.lock().unwrap().len(), 1, "no approval page for the old key");
}

#[test]
fn switching_accounts_stops_the_album_read() {
    let base = fake(Duration::from_millis(1500));
    let (mut h, _) = app(&base);
    signed_in(&mut h);
    h.app.run("flickr.albums", json!({})).unwrap();
    let read = h.app.flickr.albums_cancel.clone().expect("the read can be stopped");
    h.app.run("flickr.signOut", json!({})).unwrap();
    h.step();
    assert!(read.load(std::sync::atomic::Ordering::Relaxed), "the old account's read is told to give up");
    assert!(!h.app.flickr.albums_loading && h.app.flickr.albums.is_none());
}

#[test]
fn a_new_album_made_before_the_list_was_read_brings_the_whole_list() {
    let base = fake(Duration::ZERO);
    let (mut h, _) = app(&base);
    signed_in(&mut h);
    assert!(h.app.flickr.albums.is_none(), "the album list hasn't been read");
    let ids: Vec<u64> = h.app.session.catalog.photos().take(1).map(|p| p.id.0).collect();
    h.app.run("flickr.upload", json!({"ids": ids, "longEdge": 128, "newAlbum": "Road Trip"})).unwrap();
    assert!(h.step_until(T, |h| h.app.flickr.last_result.is_some()));
    assert!(h.app.flickr.albums_loading, "the list is read, not left holding only the new album");
    assert!(h.step_until(T, |h| !h.app.flickr.albums_loading));
    let ids: Vec<String> = h.app.flickr.albums.clone().unwrap().into_iter().map(|(id, _, _)| id).collect();
    assert_eq!(ids, ["501", "502"], "the list as Flickr gives it");
}

#[test]
fn a_new_album_already_in_the_list_is_not_added_twice() {
    let base = fake(Duration::ZERO);
    let (mut h, _) = app(&base);
    signed_in(&mut h);
    h.app.run("flickr.albums", json!({})).unwrap();
    assert!(h.step_until(T, |h| h.app.flickr.albums.is_some()));
    // a read that finished before the upload's answer already lists the album it made
    h.app.flickr.albums.as_mut().unwrap().insert(0, ("601".into(), "Road Trip".into(), 1));
    let ids: Vec<u64> = h.app.session.catalog.photos().take(1).map(|p| p.id.0).collect();
    h.app.run("flickr.upload", json!({"ids": ids, "longEdge": 128, "newAlbum": "Road Trip"})).unwrap();
    assert!(h.step_until(T, |h| h.app.flickr.last_result.is_some()));
    let n = h.app.flickr.albums.as_ref().unwrap().iter().filter(|(id, _, _)| id == "601").count();
    assert_eq!(n, 1, "listed once");
}

fn widget_rect(h: &Headless, id: &str) -> egui::Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"))
}

fn click_widget(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
}

#[test]
fn the_photo_right_click_menu_in_develop_shares_to_flickr() {
    let (mut h, _) = app("http://127.0.0.1:9");
    signed_in(&mut h);
    let r = h.request("ui.set", json!({"view": "detail", "right": "edit"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(T);
    // right-click the photo being developed: Share ▸ Share to Flickr…
    let c = widget_rect(&h, "canvas:image").center();
    let r = h.request("ui.click", json!({"x": c.x, "y": c.y, "button": "right"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(has(&h, "menu:contextShare"), "the menu has Share");
    let share = widget_rect(&h, "menu:contextShare").center();
    h.app.synthetic.push(egui::Event::PointerMoved(share));
    for _ in 0..6 {
        h.step();
    }
    click_widget(&mut h, "menu:contextShareFlickr");
    assert!(matches!(h.app.ui.dialog, Some(Dialog::FlickrUpload { .. })), "the Flickr dialog opens: {:?}", h.app.ui.dialog);
}

#[test]
fn the_dialog_offers_the_photos_title_and_description() {
    let (mut h, _) = app("http://127.0.0.1:9");
    signed_in(&mut h);
    let photos: Vec<_> = h.app.session.catalog.photos().take(2).map(|p| (p.id, p.file_name.clone())).collect();
    // no title of its own: the dialog offers its file name
    h.app.session.execute("library.select", &json!({"ids": [photos[0].0.0]})).unwrap();
    h.app.session.execute("photo.setMeta", &json!({"title": ""})).unwrap();
    // one photo: its title (none yet: its file name) and caption, ready to edit
    h.app.session.execute("library.select", &json!({"ids": [photos[0].0.0]})).unwrap();
    h.app.session.execute("photo.setMeta", &json!({"caption": "Dawn over the lake"})).unwrap();
    h.app.run("dialog.flickrUpload", json!({})).unwrap();
    let Some(Dialog::FlickrUpload { opts }) = &h.app.ui.dialog else { panic!("no dialog") };
    let stem = std::path::Path::new(&photos[0].1).file_stem().unwrap().to_string_lossy().to_string();
    assert_eq!(opts.title, stem);
    assert_eq!(opts.description_text, "Dawn over the lake");
    assert_eq!(opts.params()["title"], stem.as_str());
    h.app.ui.dialog = None;
    // two photos: empty, so each keeps its own (sent as null)
    h.app.session.execute("library.select", &json!({"ids": [photos[0].0.0, photos[1].0.0]})).unwrap();
    h.app.run("dialog.flickrUpload", json!({})).unwrap();
    let Some(Dialog::FlickrUpload { opts }) = &h.app.ui.dialog else { panic!("no dialog") };
    assert!(opts.title.is_empty() && opts.description_text.is_empty());
    assert!(opts.params()["title"].is_null() && opts.params()["descriptionText"].is_null());
    h.step();
    assert!(has(&h, "field:flickrTitle") && has(&h, "field:flickrDescriptionText"), "both fields are shown");
}
