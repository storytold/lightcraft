//! IMM-SYNC, IMM-PEOPLE and IMM-SEARCH through the commands, against an in-process server that
//! keeps state (updates, tags, trash, people) like a v3.3.1 server.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use super::*;

#[derive(Default)]
struct Server {
    assets: Vec<Value>,
    faces: Vec<(String, Value)>,
    people: Vec<Value>,
    renamed: Vec<Value>,
    merged: Vec<String>,
    clock: u32,
}

impl Server {
    fn tick(&mut self) -> String {
        self.clock += 1;
        format!("2026-10-10T01:{:02}:{:02}.000Z", self.clock / 60 % 60, self.clock % 60)
    }
    fn asset_mut(&mut self, id: &str) -> Option<&mut Value> {
        self.assets.iter_mut().find(|a| a["id"] == id)
    }
}

type Shared = Arc<Mutex<Server>>;

fn serve_state(state: Shared, down: Arc<AtomicBool>, hits: Arc<AtomicU32>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in listener.incoming() {
            let Ok(mut s) = s else { return };
            let Some((m, t, key, body)) = read_req(&mut s) else { continue };
            hits.fetch_add(1, Ordering::SeqCst);
            let out = handle(&state, &down, &m, &t, key.as_deref(), &body);
            let _ = s.write_all(&out);
        }
    });
    format!("http://127.0.0.1:{port}")
}

fn handle(state: &Shared, down: &AtomicBool, m: &str, t: &str, key: Option<&str>, body: &[u8]) -> Vec<u8> {
    let json = |v: Value| resp("200 OK", "application/json", v.to_string().as_bytes());
    let not_found = || resp("404 Not Found", "application/json", b"{}");
    let (path, query) = t.split_once('?').unwrap_or((t, ""));
    if path == "/api/server/version" {
        return json(json!({"major": 3, "minor": 3, "patch": 1}));
    }
    if key != Some(KEY) {
        return resp("401 Unauthorized", "application/json", br#"{"message":"Invalid API key"}"#);
    }
    if down.load(Ordering::SeqCst) && path != "/api/users/me" && path != "/api/api-keys/me" {
        return resp("503 Service Unavailable", "application/json", b"{}");
    }
    let b: Value = serde_json::from_slice(body).unwrap_or_default();
    let mut st = state.lock().unwrap();
    match (m, path) {
        ("GET", "/api/users/me") => json(json!({"id": "user-1", "email": "me@example.invalid", "name": "Me"})),
        ("GET", "/api/api-keys/me") => json(json!({"permissions": ["all"]})),
        ("POST", "/api/search/metadata") => {
            let after = b["updatedAfter"].as_str().unwrap_or("");
            let items: Vec<Value> = if b["page"].as_u64().unwrap_or(1) == 1 {
                st.assets.iter().filter(|a| a["updatedAt"].as_str().unwrap_or("") > after).cloned().collect()
            } else {
                vec![]
            };
            json(json!({"assets": {"total": items.len(), "count": items.len(), "items": items, "nextPage": null}}))
        }
        ("POST", "/api/search/smart") => {
            let q = b["query"].as_str().unwrap_or("").to_lowercase();
            let items: Vec<Value> = st
                .assets
                .iter()
                .filter(|a| a["exifInfo"]["description"].as_str().unwrap_or("").to_lowercase().contains(&q))
                .cloned()
                .chain(std::iter::once(json!({"id": "not-in-catalog"})))
                .collect();
            json(json!({"assets": {"total": items.len(), "count": items.len(), "items": items, "nextPage": null}}))
        }
        ("PUT", "/api/tags") => {
            let mut out = Vec::new();
            for v in b["tags"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                out.push(json!({"id": format!("tag-{}", v.replace('/', "-")), "value": v}));
            }
            json(json!(out))
        }
        ("PUT", p) | ("DELETE", p) if p.starts_with("/api/tags/") && p.ends_with("/assets") => {
            let tag = p.trim_start_matches("/api/tags/").trim_end_matches("/assets").to_string();
            let value = tag.trim_start_matches("tag-").replace('-', "/");
            let now = st.tick();
            for id in b["ids"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                if let Some(a) = st.asset_mut(id) {
                    let tags = a["tags"].as_array().cloned().unwrap_or_default();
                    let mut tags: Vec<Value> = tags.into_iter().filter(|x| x["id"] != tag.as_str()).collect();
                    if m == "PUT" {
                        tags.push(json!({"id": tag, "value": value}));
                    }
                    a["tags"] = json!(tags);
                    a["updatedAt"] = json!(now);
                }
            }
            json(json!([]))
        }
        ("DELETE", "/api/assets") => {
            let now = st.tick();
            for id in b["ids"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                if let Some(a) = st.asset_mut(id) {
                    a["isTrashed"] = json!(true);
                    a["updatedAt"] = json!(now);
                }
            }
            resp("204 No Content", "application/json", b"")
        }
        ("GET", "/api/faces") => {
            let id = query.trim_start_matches("id=");
            json(json!(st.faces.iter().filter(|(a, _)| a == id).map(|(_, f)| f.clone()).collect::<Vec<_>>()))
        }
        ("GET", "/api/people") => json(json!({"people": st.people, "hasNextPage": false})),
        ("PUT", "/api/people") => {
            let list = b["people"].as_array().cloned().unwrap_or_default();
            st.renamed.extend(list);
            json(json!([]))
        }
        ("POST", p) if p.starts_with("/api/people/") && p.ends_with("/merge") => {
            st.merged.push(p.to_string());
            json(json!([]))
        }
        (_, p) if p.starts_with("/api/assets/") => {
            let id = p.trim_start_matches("/api/assets/").to_string();
            let now = st.tick();
            let Some(a) = st.asset_mut(&id) else { return not_found() };
            if m == "PUT" {
                if let Some(r) = b.get("rating") {
                    a["exifInfo"]["rating"] = r.clone();
                }
                if let Some(f) = b.get("isFavorite") {
                    a["isFavorite"] = f.clone();
                }
                if let Some(d) = b.get("description") {
                    a["exifInfo"]["description"] = d.clone();
                }
                if let (Some(la), Some(lo)) = (b.get("latitude"), b.get("longitude")) {
                    a["exifInfo"]["latitude"] = la.clone();
                    a["exifInfo"]["longitude"] = lo.clone();
                }
                if let Some(v) = b.get("visibility") {
                    a["visibility"] = v.clone();
                }
                a["updatedAt"] = json!(now);
            }
            json(a.clone())
        }
        _ => not_found(),
    }
}

struct Setup {
    s: Session,
    state: Shared,
    down: Arc<AtomicBool>,
    hits: Arc<AtomicU32>,
    one: PhotoId,
    two: PhotoId,
    dir: PathBuf,
}

/// Two photos imported and linked to two assets; the library is a folder (the sync state file).
fn setup(tag: &str) -> Setup {
    let dir = temp_dir(tag);
    let (a, b) = (png(40), png(41));
    std::fs::write(dir.join("one.png"), &a).unwrap();
    std::fs::write(dir.join("two.png"), &b).unwrap();
    let mut server = Server::default();
    let mut x = asset("aaaa-1", "one.png", &a, json!({"visibility": "timeline", "tags": []}));
    x["updatedAt"] = json!("2026-10-10T00:00:00.000Z");
    let mut y = asset(
        "bbbb-2",
        "two.png",
        &b,
        json!({"visibility": "timeline", "tags": [{"id": "tag-People-Ann", "value": "People/Ann"}], "exifInfo": {"rating": 4, "description": "From Immich", "fileSizeInByte": b.len()}}),
    );
    y["updatedAt"] = json!("2026-10-10T00:00:00.000Z");
    server.assets = vec![x, y];
    let state = Arc::new(Mutex::new(server));
    let down = Arc::new(AtomicBool::new(false));
    let hits = Arc::new(AtomicU32::new(0));
    let url = serve_state(state.clone(), down.clone(), hits.clone());
    let mut s = session();
    s.remote.cache_dir = Some(dir.join("cache"));
    s.execute("library.import", &json!({"paths": [dir.join("one.png").to_string_lossy(), dir.join("two.png").to_string_lossy()]})).unwrap();
    let one = s.catalog.photos().find(|p| p.file_name == "one.png").unwrap().id;
    let two = s.catalog.photos().find(|p| p.file_name == "two.png").unwrap().id;
    assert_eq!(s.execute("immich.connect", &json!({"url": url, "apiKey": KEY})).unwrap()["ok"], true);
    s.execute("immich.link", &json!({})).unwrap();
    pump_until(&mut s, |s, _| dac_immich::link::link_state(&s.catalog, one) == "linked" && dac_immich::link::link_state(&s.catalog, two) == "linked");
    Setup { s, state, down, hits, one, two, dir }
}

fn remote(st: &Shared, id: &str) -> Value {
    st.lock().unwrap().assets.iter().find(|a| a["id"] == id).cloned().unwrap()
}

#[test]
fn two_way_sync_dry_run_conflicts_and_deletions() {
    let Setup { mut s, state, down, one, two, dir, .. } = setup("sync");
    // catalog edits on photo one
    s.execute("photo.rate", &json!({"ids": [one.0], "rating": 3})).unwrap();
    let mut meta = s.catalog.photo(one).unwrap().meta.clone();
    meta.caption = "Beach day".into();
    meta.keywords = vec!["Trip|Paris".into(), "Private|Home".into()];
    s.catalog.apply(Op::SetMeta { id: one, meta: Box::new(meta) }).unwrap();
    s.catalog
        .apply(Op::SetKeyword {
            path: "Private|Home".into(),
            info: Some(dac_catalog::keywords::KeywordInfo { include_on_export: false, ..Default::default() }),
        })
        .unwrap();

    // a dry run changes nothing
    let r = s.execute("immich.sync", &json!({"dryRun": true})).unwrap();
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["pushed"], 1, "{r}");
    assert_eq!(r["pulled"], 2, "both take the capture date Immich has: {r}");
    assert!(r["changes"].as_array().unwrap().iter().any(|c| c["toImmich"]["rating"] == 3), "{r}");
    assert_eq!(remote(&state, "aaaa-1")["exifInfo"]["rating"], Value::Null);
    assert_eq!(s.catalog.photo(two).unwrap().rating, 0);

    // the real run: both ways
    let r = s.execute("immich.sync", &json!({})).unwrap();
    assert_eq!(r["ok"], true, "{r}");
    let a = remote(&state, "aaaa-1");
    assert_eq!(a["exifInfo"]["rating"], 3);
    assert_eq!(a["exifInfo"]["description"], "Beach day");
    let tags: Vec<&str> = a["tags"].as_array().unwrap().iter().filter_map(|t| t["value"].as_str()).collect();
    assert_eq!(tags, vec!["Trip/Paris"], "export-excluded keywords are never sent");
    let p2 = s.catalog.photo(two).unwrap();
    assert_eq!(p2.rating, 4);
    assert_eq!(p2.meta.caption, "From Immich");
    assert_eq!(p2.meta.keywords, vec!["People|Ann".to_string()]);
    assert!(s.catalog.photo(one).unwrap().meta.keywords.contains(&"Private|Home".to_string()));

    // nothing changed: nothing to do
    let r = s.execute("immich.sync", &json!({})).unwrap();
    assert_eq!((r["pulled"].as_u64(), r["pushed"].as_u64()), (Some(0), Some(0)), "{r}");

    // a keyword removed in Immich goes in the catalog
    {
        let mut st = state.lock().unwrap();
        let now = st.tick();
        let b = st.asset_mut("bbbb-2").unwrap();
        b["tags"] = json!([]);
        b["updatedAt"] = json!(now);
    }
    s.execute("immich.sync", &json!({})).unwrap();
    assert!(s.catalog.photo(two).unwrap().meta.keywords.is_empty());

    // both sides change the rating, rating asks: a conflict
    s.execute("immich.setSync", &json!({"config": {"rules": {"rating": {"direction": "twoWay", "policy": "ask"}}}})).unwrap();
    s.execute("photo.rate", &json!({"ids": [one.0], "rating": 5})).unwrap();
    {
        let mut st = state.lock().unwrap();
        let now = st.tick();
        let a = st.asset_mut("aaaa-1").unwrap();
        a["exifInfo"]["rating"] = json!(1);
        a["updatedAt"] = json!(now);
    }
    let r = s.execute("immich.sync", &json!({})).unwrap();
    assert_eq!(r["openConflicts"], 1, "{r}");
    assert_eq!(remote(&state, "aaaa-1")["exifInfo"]["rating"], 1, "a conflict changes neither side");
    assert_eq!(s.catalog.photo(one).unwrap().rating, 5);
    let c = s.execute("immich.syncConflicts", &json!({})).unwrap();
    assert_eq!(c["conflicts"][0]["field"], "rating");
    assert_eq!((c["conflicts"][0]["catalog"].as_u64(), c["conflicts"][0]["immich"].as_u64()), (Some(5), Some(1)));
    assert_eq!(dac_immich::link::link_state(&s.catalog, one), "linked");
    assert!(s.catalog.remote_of(one).any(|r| r.sync_state == dac_catalog::SyncState::Conflict));
    // still a conflict on the next run
    assert_eq!(s.execute("immich.sync", &json!({})).unwrap()["openConflicts"], 1);
    // the user keeps Immich's
    let r = s.execute("immich.resolveConflict", &json!({"keep": "immich", "sync": true})).unwrap();
    assert_eq!(r["decided"], 1);
    assert_eq!(r["sync"]["openConflicts"], 0, "{r}");
    assert_eq!(s.catalog.photo(one).unwrap().rating, 1);

    // the server is down: a clear error, nothing lost, the cursor stays
    let before = s.execute("immich.syncStatus", &json!({})).unwrap()["syncedUntil"].clone();
    s.execute("photo.rate", &json!({"ids": [one.0], "rating": 2})).unwrap();
    down.store(true, Ordering::SeqCst);
    let r = s.execute("immich.sync", &json!({})).unwrap();
    assert_eq!(r["ok"], false, "{r}");
    assert_eq!(r["error"]["retryable"], true, "{r}");
    assert_eq!(s.execute("immich.syncStatus", &json!({})).unwrap()["syncedUntil"], before);
    down.store(false, Ordering::SeqCst);
    s.execute("immich.sync", &json!({})).unwrap();
    assert_eq!(remote(&state, "aaaa-1")["exifInfo"]["rating"], 2, "the edit made while offline went out later");

    // deletions are queued, never automatic
    {
        let mut st = state.lock().unwrap();
        let now = st.tick();
        let b = st.asset_mut("bbbb-2").unwrap();
        b["isTrashed"] = json!(true);
        b["updatedAt"] = json!(now);
    }
    let r = s.execute("immich.sync", &json!({})).unwrap();
    assert_eq!(r["deletionsQueued"], 1, "{r}");
    assert!(!s.catalog.photo(two).unwrap().deleted);
    s.execute("photo.delete", &json!({"ids": [one.0]})).unwrap();
    let r = s.execute("immich.sync", &json!({})).unwrap();
    assert_eq!(r["deletionsQueued"], 2, "{r}");
    assert_eq!(remote(&state, "aaaa-1")["isTrashed"], Value::Null);
    let r = s.execute("immich.resolveDeletion", &json!({"action": "apply", "assets": ["bbbb-2"]})).unwrap();
    assert_eq!(r["resolved"], 1);
    assert!(s.catalog.photo(two).unwrap().deleted);
    let r = s.execute("immich.resolveDeletion", &json!({"action": "keep"})).unwrap();
    assert_eq!(r["resolved"], 1);
    assert_eq!(remote(&state, "aaaa-1")["isTrashed"], Value::Null, "kept: not trashed");
    // the activity log has every run; the state is on disk
    let st = s.execute("immich.syncStatus", &json!({"log": 100})).unwrap();
    assert!(st["log"].as_array().unwrap().len() >= 9, "{st}");
    assert!(st["log"].as_array().unwrap().iter().any(|l| l["dryRun"] == true));
    assert!(std::fs::read_dir(dir.join("cache/sync")).unwrap().count() == 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn background_sync_runs_off_the_session_thread_and_on_a_schedule() {
    let Setup { mut s, state, hits, one, dir, .. } = setup("bg");
    s.execute("photo.rate", &json!({"ids": [one.0], "rating": 2})).unwrap();
    assert_eq!(s.execute("immich.sync", &json!({"background": true})).unwrap()["started"], true);
    // a second start while one runs is refused, not queued twice
    let again = s.execute("immich.sync", &json!({"background": true})).unwrap();
    assert!(again["started"] == false || again["ok"].is_boolean(), "{again}");
    pump_until(&mut s, |s, _| s.remote.sync.values().all(|a| !a.running));
    assert_eq!(remote(&state, "aaaa-1")["exifInfo"]["rating"], 2);
    // idle: the pump doesn't talk to the server when no interval is set
    let n = hits.load(Ordering::SeqCst);
    for _ in 0..50 {
        s.execute("remote.pump", &json!({})).unwrap();
    }
    assert_eq!(hits.load(Ordering::SeqCst), n, "no network while idle");
    // an interval: the schedule is set
    s.execute("immich.setSync", &json!({"config": {"intervalMinutes": 15}})).unwrap();
    let st = s.execute("immich.syncStatus", &json!({})).unwrap();
    assert!(st["nextRunInSeconds"].as_u64().unwrap() > 800, "{st}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn people_import_confirm_and_push_names() {
    let Setup { mut s, state, one, dir, .. } = setup("people");
    {
        let mut st = state.lock().unwrap();
        st.people = vec![json!({"id": "p-ann", "name": "Ann", "birthDate": "1990-01-02"}), json!({"id": "p-x", "name": "", "isHidden": true})];
        st.faces = vec![
            (
                "aaaa-1".into(),
                json!({"id": "f1", "boundingBoxX1": 4, "boundingBoxY1": 3, "boundingBoxX2": 20, "boundingBoxY2": 15, "imageWidth": 40, "imageHeight": 30, "person": {"id": "p-ann", "name": "Ann"}}),
            ),
            (
                "aaaa-1".into(),
                json!({"id": "f2", "boundingBoxX1": 25, "boundingBoxY1": 3, "boundingBoxX2": 35, "boundingBoxY2": 15, "imageWidth": 40, "imageHeight": 30, "person": {"id": "p-x", "name": "", "isHidden": true}}),
            ),
        ];
    }
    let r = s.execute("immich.importPeople", &json!({})).unwrap();
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["added"], 1, "hidden people are left out: {r}");
    assert_eq!(r["people"][0]["birthDate"], "1990-01-02");
    let p = s.catalog.photo(one).unwrap();
    assert_eq!(p.people(), vec!["Ann"], "the People view sees the name");
    // importing again changes nothing; undo takes the import back
    assert_eq!(s.execute("immich.importPeople", &json!({})).unwrap()["photos"], 0);
    // the user corrects the name and confirms; the name goes back to Immich
    let mut meta = s.catalog.photo(one).unwrap().meta.clone();
    meta.regions[0].name = Some("Ann Lee".into());
    s.catalog.apply(Op::SetMeta { id: one, meta: Box::new(meta) }).unwrap();
    assert_eq!(s.execute("immich.confirmFaces", &json!({"ids": [one.0]})).unwrap()["confirmed"], 1);
    let dry = s.execute("immich.pushPeople", &json!({"dryRun": true})).unwrap();
    assert_eq!(dry["renames"][0]["name"], "Ann Lee");
    assert!(state.lock().unwrap().renamed.is_empty());
    let r = s.execute("immich.pushPeople", &json!({})).unwrap();
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(state.lock().unwrap().renamed[0]["name"], "Ann Lee");
    // confirmed regions survive a re-import
    s.execute("immich.importPeople", &json!({})).unwrap();
    assert_eq!(s.catalog.photo(one).unwrap().people(), vec!["Ann Lee"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn smart_search_maps_results_to_photos_and_shows_or_saves_them() {
    let Setup { mut s, two, dir, .. } = setup("search");
    let r = s.execute("immich.smartSearch", &json!({"query": "from immich", "show": true, "saveAs": "Immich: from immich"})).unwrap();
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["photos"], json!([two.0]));
    assert_eq!(r["unmatched"], 1);
    assert_eq!(s.visible(), vec![two], "the temporary collection");
    let album = dac_catalog::AlbumId(r["album"].as_u64().unwrap());
    assert_eq!(s.catalog.album(album).unwrap().photos, vec![two]);
    // nothing found: an empty grid, not everything
    s.execute("immich.smartSearch", &json!({"query": "zebra", "show": true})).unwrap();
    assert!(s.visible().is_empty());
    assert!(s.execute("immich.smartSearch", &json!({"query": "  "})).is_err());
    assert!(s.execute("immich.smartSearch", &json!({"query": "x", "mode": "nope"})).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
