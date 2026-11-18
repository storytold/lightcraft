//! IMM-SYNC, IMM-PEOPLE and IMM-SEARCH through the commands, against an in-process server that
//! keeps state (updates, tags, trash, people) like a v3.3.1 server.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use super::*;

#[derive(Default)]
pub(crate) struct Server {
    /// Asset id → its bytes (`/assets/{id}/original` and `/thumbnail`).
    pub(crate) bytes: std::collections::HashMap<String, Vec<u8>>,
    /// Uploads keep their bytes and the SHA-1 of them as checksum (as a real server does).
    pub(crate) real_uploads: bool,
    pub(crate) assets: Vec<Value>,
    faces: Vec<(String, Value)>,
    people: Vec<Value>,
    renamed: Vec<Value>,
    merged: Vec<String>,
    clock: u32,
    /// (id, name, asset ids)
    pub(crate) albums: Vec<(String, String, Vec<String>)>,
    stacks: Vec<Vec<String>>,
    pub(crate) uploads: u32,
    /// Uploads whose multipart body carried a `sidecarData` part (IMM-SIDECARS).
    sidecar_uploads: u32,
    /// Asset id → the XMP sidecar the server holds for it.
    sidecars: std::collections::HashMap<String, String>,
    /// Assets removed with `DELETE /assets` (the temporary sidecar-copy assets).
    deleted: Vec<String>,
    /// Failure injection: assets whose `PUT /assets/{id}` answers 503.
    fail_update: Vec<String>,
    /// Failure injection: `PUT /albums/{id}/assets` answers 503.
    album_add_down: bool,
    /// Failure injection: assets an album add refuses (200, `success: false`), as a real server
    /// does for an asset the key may not share.
    album_add_refused: Vec<String>,
    /// `POST /assets/bulk-upload-check` answers like a real server: a checksum it has is rejected
    /// as a duplicate, with the asset's id. (Off by default: the publish tests upload files the
    /// server "has" and count on every upload being accepted.)
    pub(crate) check_checksums: bool,
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

pub(crate) type Shared = Arc<Mutex<Server>>;

pub(crate) fn serve_state(state: Shared, down: Arc<AtomicBool>, hits: Arc<AtomicU32>) -> String {
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

/// The `sidecarData` part of a multipart upload, as the server would store it.
fn sidecar_part(body: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(body);
    let i = text.find("name=\"sidecarData\"")?;
    let after = &text[i..];
    let start = after.find("\r\n\r\n")? + 4;
    let rest = &after[start..];
    let end = rest.find("\r\n--")?;
    Some(rest[..end].to_string())
}

/// The bytes of the `assetData` part of a multipart upload.
fn file_part(body: &[u8]) -> Option<Vec<u8>> {
    let marker = b"name=\"assetData\"";
    let i = body.windows(marker.len()).position(|w| w == marker)?;
    let after = body.get(i..)?;
    let start = after.windows(4).position(|w| w == b"\r\n\r\n")? + 4;
    let rest = after.get(start..)?;
    let end = rest.windows(4).position(|w| w == b"\r\n--")?;
    rest.get(..end).map(<[u8]>::to_vec)
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
            let album: Option<Vec<String>> =
                b["albumIds"][0].as_str().map(|a| st.albums.iter().find(|x| x.0 == a).map(|x| x.2.clone()).unwrap_or_default());
            let items: Vec<Value> = if let Some(ids) = album {
                ids.iter().map(|i| st.assets.iter().find(|a| a["id"] == i.as_str()).cloned().unwrap_or_else(|| json!({"id": i}))).collect()
            } else if b["page"].as_u64().unwrap_or(1) == 1 {
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
        ("POST", "/api/assets") => {
            st.uploads += 1;
            let id = format!("up-{}", st.uploads);
            let now = st.tick();
            // the checksum of the uploaded part is not parsed: the test server keys duplicates by name
            let text = String::from_utf8_lossy(body);
            let name = text
                .split("name=\"assetData\"")
                .nth(1)
                .and_then(|x| x.split("filename=\"").nth(1))
                .and_then(|x| x.split('"').next())
                .unwrap_or("x")
                .to_string();
            if text.contains("name=\"sidecarData\"") {
                st.sidecar_uploads += 1;
            }
            if let Some(xml) = sidecar_part(body) {
                st.sidecars.insert(id.clone(), xml);
            }
            // like the real server, the asset remembers the uploading device (`deviceId` form field)
            let device = text.split("name=\"deviceId\"").nth(1).and_then(|x| x.trim_start().lines().next()).unwrap_or("").trim().to_string();
            let mut checksum = format!("name:{name}");
            if st.real_uploads
                && let Some(data) = file_part(body)
            {
                checksum = lcc_hash::sha1_bytes(&data).to_base64();
                st.bytes.insert(id.clone(), data);
            }
            st.assets.push(json!({"id": id, "type": "IMAGE", "deviceId": device, "originalFileName": name, "updatedAt": now, "checksum": checksum, "exifInfo": {}, "tags": []}));
            resp("201 Created", "application/json", json!({"id": id, "status": "created"}).to_string().as_bytes())
        }
        ("POST", "/api/assets/bulk-upload-check") => {
            let hex = |c: &str| lcc_hash::Sha1Digest::parse(c).map(|d| d.to_hex());
            let results: Vec<Value> = b["assets"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|x| {
                    let want = x["checksum"].as_str().and_then(hex);
                    let have = st.assets.iter().find(|a| st.check_checksums && want.is_some() && a["checksum"].as_str().and_then(hex) == want);
                    match have {
                        Some(a) => json!({"id": x["id"], "action": "reject", "reason": "duplicate", "assetId": a["id"]}),
                        None => json!({"id": x["id"], "action": "accept"}),
                    }
                })
                .collect();
            json(json!({"results": results}))
        }
        ("GET", "/api/albums") => {
            let with = query.strip_prefix("assetId=");
            json(json!(st.albums.iter().filter(|(_, _, a)| with.is_none_or(|w| a.iter().any(|x| x == w))).map(|(i, n, a)| json!({"id": i, "albumName": n, "assetCount": a.len()})).collect::<Vec<_>>()))
        }
        ("POST", "/api/albums") => {
            let id = format!("album-{}", st.albums.len() + 1);
            st.albums.push((id.clone(), b["albumName"].as_str().unwrap_or("").to_string(), vec![]));
            json(json!({"id": id, "albumName": b["albumName"]}))
        }
        (_, p) if p.starts_with("/api/albums/") => {
            let rest = p.trim_start_matches("/api/albums/");
            let (id, tail) = rest.split_once('/').unwrap_or((rest, ""));
            let ids: Vec<String> = b["ids"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
            if (m, tail) == ("DELETE", "") {
                st.albums.retain(|a| a.0 != id);
                return json(json!({}));
            }
            if (m, tail) == ("PUT", "assets") && st.album_add_down {
                return resp("503 Service Unavailable", "application/json", b"{}");
            }
            let refused = st.album_add_refused.clone();
            let Some(al) = st.albums.iter_mut().find(|a| a.0 == id) else { return not_found() };
            match (m, tail) {
                ("PATCH", "") => al.1 = b["albumName"].as_str().unwrap_or_default().to_string(),
                ("PUT", "assets") => {
                    // v3: one `{id, success, error?}` per asset, with status 200 either way
                    let mut out = Vec::new();
                    for i in ids {
                        if refused.contains(&i) {
                            out.push(json!({"id": i, "success": false, "error": "no_permission"}));
                        } else if al.2.contains(&i) {
                            out.push(json!({"id": i, "success": false, "error": "duplicate"}));
                        } else {
                            out.push(json!({"id": i, "success": true}));
                            al.2.push(i);
                        }
                    }
                    return json(json!(out));
                }
                ("DELETE", "assets") => al.2.retain(|x| !ids.contains(x)),
                _ => {}
            }
            json(json!({"id": al.0, "albumName": al.1, "assets": al.2.iter().map(|i| json!({"id": i})).collect::<Vec<_>>()}))
        }
        ("POST", "/api/stacks") => {
            // v3.3.1: the first is the primary; stacks whose primary is listed are merged in
            let mut ids: Vec<String> = b["assetIds"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
            for s in st.stacks.iter_mut() {
                if s.first().is_some_and(|p| ids.contains(p)) {
                    for m in s.drain(..) {
                        if !ids.contains(&m) {
                            ids.push(m);
                        }
                    }
                } else {
                    s.retain(|m| !ids.contains(m));
                    if s.len() < 2 {
                        s.clear();
                    }
                }
            }
            st.stacks.push(ids.clone());
            json(json!({"id": format!("stack-{}", st.stacks.len() - 1), "primaryAssetId": ids[0], "assets": ids.iter().map(|i| json!({"id": i})).collect::<Vec<_>>()}))
        }
        ("GET", "/api/stacks") => json(json!(st
            .stacks
            .iter()
            .enumerate()
            .filter(|(_, s)| s.len() >= 2)
            .map(|(i, s)| json!({"id": format!("stack-{i}"), "primaryAssetId": s[0], "assets": s.iter().map(|a| json!({"id": a})).collect::<Vec<_>>()}))
            .collect::<Vec<_>>())),
        ("DELETE", "/api/stacks") => {
            for id in b["ids"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                if let Some(s) = id.strip_prefix("stack-").and_then(|i| i.parse::<usize>().ok()).and_then(|i| st.stacks.get_mut(i)) {
                    s.clear();
                }
            }
            resp("204 No Content", "application/json", b"")
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
                st.deleted.push(id.to_string());
            }
            resp("204 No Content", "application/json", b"")
        }
        ("PUT", "/api/assets/copy") => {
            let (src, tgt) = (b["sourceId"].as_str().unwrap_or(""), b["targetId"].as_str().unwrap_or(""));
            if b["sidecar"] == true && let Some(xml) = st.sidecars.get(src).cloned() {
                st.sidecars.insert(tgt.to_string(), xml);
            }
            resp("204 No Content", "application/json", b"")
        }
        ("POST", "/api/assets/jobs") => resp("204 No Content", "application/json", b""),
        ("GET", "/api/asset-files") => {
            let asset = query.split('&').find_map(|kv| kv.strip_prefix("assetId=")).unwrap_or("");
            let files: Vec<Value> = match st.sidecars.get(asset) {
                Some(_) => vec![json!({"id": format!("sidecar-{asset}"), "type": "sidecar", "path": format!("/x/{asset}.xmp")})],
                None => Vec::new(),
            };
            json(Value::Array(files))
        }
        ("GET", p) if p.starts_with("/api/asset-files/") && p.ends_with("/download") => {
            let asset = p.trim_start_matches("/api/asset-files/").trim_end_matches("/download").trim_start_matches("sidecar-");
            match st.sidecars.get(asset) {
                Some(xml) => resp("200 OK", "application/xml", xml.as_bytes()),
                None => not_found(),
            }
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
        ("GET", p) if p.starts_with("/api/assets/") && (p.ends_with("/original") || p.ends_with("/thumbnail")) => {
            let id = p.trim_start_matches("/api/assets/").split('/').next().unwrap_or_default();
            match st.bytes.get(id) {
                Some(b) => resp("200 OK", "image/png", b),
                None => not_found(),
            }
        }
        (_, p) if p.starts_with("/api/assets/") => {
            let id = p.trim_start_matches("/api/assets/").to_string();
            if m == "PUT" && st.fail_update.contains(&id) {
                return resp("503 Service Unavailable", "application/json", b"{}");
            }
            let now = st.tick();
            let stack = st.stacks.iter().enumerate().find(|(_, s)| s.len() >= 2 && s.contains(&id)).map(|(i, s)| json!({"id": format!("stack-{i}"), "primaryAssetId": s[0], "assetCount": s.len()}));
            let Some(a) = st.asset_mut(&id) else { return not_found() };
            a["stack"] = stack.unwrap_or(Value::Null);
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
    s.open_library(dir.join("lib"), false).unwrap();
    s.execute("library.import", &json!({"paths": [dir.join("one.png").to_string_lossy(), dir.join("two.png").to_string_lossy()]})).unwrap();
    let one = s.catalog.photos().find(|p| p.file_name == "one.png").unwrap().id;
    let two = s.catalog.photos().find(|p| p.file_name == "two.png").unwrap().id;
    assert_eq!(s.execute("immich.connect", &json!({"url": url, "apiKey": KEY})).unwrap()["ok"], true);
    s.execute("immich.link", &json!({})).unwrap();
    pump_until(&mut s, |s, _| lcc_immich::link::link_state(&s.catalog, one) == "linked" && lcc_immich::link::link_state(&s.catalog, two) == "linked");
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
            info: Some(lcc_catalog::keywords::KeywordInfo { include_on_export: false, ..Default::default() }),
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
    assert_eq!(lcc_immich::link::link_state(&s.catalog, one), "linked");
    assert!(s.catalog.remote_of(one).any(|r| r.sync_state == lcc_catalog::SyncState::Conflict));
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
    // an explicit empty list means none, never "all queued"
    let r = s.execute("immich.resolveDeletion", &json!({"action": "apply", "assets": []})).unwrap();
    assert_eq!(r["resolved"], 0, "{r}");
    assert!(!s.catalog.photo(two).unwrap().deleted, "empty list must not delete anything");
    assert_eq!(remote(&state, "aaaa-1")["isTrashed"], Value::Null, "empty list must not trash anything");
    assert_eq!(s.execute("immich.syncStatus", &json!({})).unwrap()["deletions"].as_array().map(Vec::len), Some(2));
    // a non-list `assets` is refused, not read as "all"
    assert!(s.execute("immich.resolveDeletion", &json!({"action": "apply", "assets": "bbbb-2"})).is_err());
    assert!(s.execute("immich.resolveDeletion", &json!({"action": "apply", "assets": {}})).is_err());
    assert!(!s.catalog.photo(two).unwrap().deleted);
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
    assert!(std::fs::read_dir(dir.join("lib/Immich/sync")).unwrap().count() == 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn sync_touches_a_locked_folder_asset_and_runs_in_the_background_and_on_a_schedule() {
    let Setup { mut s, state, hits, one, dir, .. } = setup("sync-locked");
    // Immich's locked folders: the asset carries visibility `locked`. Since the app can unlock
    // its server session (`immich.unlockSession`), such an asset takes part like any other; a
    // locked session would simply not list it.
    {
        let mut st = state.lock().unwrap();
        st.asset_mut("aaaa-1").unwrap()["visibility"] = json!("locked");
    }
    s.execute("photo.rate", &json!({"ids": [one.0], "rating": 2})).unwrap();
    let r = s.execute("immich.sync", &json!({})).unwrap();
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["pushed"], 1, "{r}");
    assert_eq!(remote(&state, "aaaa-1")["exifInfo"]["rating"], 2, "the locked asset is updated");

    // a background sync runs off the session thread
    s.execute("photo.rate", &json!({"ids": [one.0], "rating": 3})).unwrap();
    assert_eq!(s.execute("immich.sync", &json!({"background": true})).unwrap()["started"], true);
    // a second start while one runs is refused, not queued twice
    let again = s.execute("immich.sync", &json!({"background": true})).unwrap();
    assert!(again["started"] == false || again["ok"].is_boolean(), "{again}");
    pump_until(&mut s, |s, _| s.remote.sync.values().all(|a| !a.running));
    assert_eq!(remote(&state, "aaaa-1")["exifInfo"]["rating"], 3);
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
fn publish_to_immich_albums_renders_and_stacks() {
    let Setup { mut s, state, one, two, dir, .. } = setup("publish");
    let account = s.immich_accounts().unwrap().immich[0].id.clone();
    // rendered: the collection becomes an album of renders
    let svc = s
        .execute(
            "publish.createService",
            &json!({"kind": "immich", "name": "Immich", "settings": {"account": account}, "export": {"format": "jpeg", "longEdge": 20}}),
        )
        .unwrap();
    assert!(s.execute("publish.createService", &json!({"kind": "immich", "name": "Bad", "settings": {}})).is_err(), "an account is required");
    s.execute("library.select", &json!({"ids": [one.0]})).unwrap();
    let st = s.execute("publish.createCollection", &json!({"service": svc["id"], "name": "Web", "addSelected": true})).unwrap();
    let coll = st["collection"].as_u64().unwrap();
    let r = s.execute("publish.run", &json!({"collection": coll})).unwrap();
    assert_eq!(r["runs"][0]["published"].as_array().unwrap().len(), 1, "{r}");
    let first = r["runs"][0]["published"][0]["remoteId"].as_str().unwrap().to_string();
    assert!(first.starts_with("r:up-"), "{first}");
    {
        let st = state.lock().unwrap();
        assert_eq!(st.albums.len(), 1);
        assert_eq!(st.albums[0].1, "Web");
        assert_eq!(st.albums[0].2, vec![first.trim_start_matches("r:").to_string()]);
    }
    // an edit, then re-publish: the new render replaces the old one, which goes to the trash
    s.execute("photo.rate", &json!({"ids": [one.0], "rating": 4})).unwrap();
    let r = s.execute("publish.run", &json!({"collection": coll})).unwrap();
    let second = r["runs"][0]["published"][0]["remoteId"].as_str().unwrap().to_string();
    assert_ne!(first, second);
    assert_eq!(remote(&state, first.trim_start_matches("r:"))["isTrashed"], true);
    assert_eq!(state.lock().unwrap().albums[0].2, vec![second.trim_start_matches("r:").to_string()]);
    // taken out of the collection: out of the album, not deleted (deleteRemoved is off)
    s.execute("album.removePhotos", &json!({"id": coll, "ids": [one.0]})).unwrap();
    let r = s.execute("publish.run", &json!({"collection": coll})).unwrap();
    assert_eq!(r["runs"][0]["removed"], json!([one.0]), "{r}");
    assert!(state.lock().unwrap().albums[0].2.is_empty());
    assert_eq!(remote(&state, second.trim_start_matches("r:"))["isTrashed"], Value::Null);

    // both: render on top of the original; a linked original is never uploaded again
    let svc2 = s
        .execute(
            "publish.createService",
            &json!({"kind": "immich", "name": "Immich both", "settings": {"account": account, "send": "both"}, "export": {"format": "jpeg", "longEdge": 20}}),
        )
        .unwrap();
    s.execute("library.select", &json!({"ids": [two.0]})).unwrap();
    let st = s.execute("publish.createCollection", &json!({"service": svc2["id"], "name": "Stacked", "addSelected": true})).unwrap();
    let uploads = state.lock().unwrap().uploads;
    let r = s.execute("publish.run", &json!({"collection": st["collection"]})).unwrap();
    let id = r["runs"][0]["published"][0]["remoteId"].as_str().unwrap().to_string();
    assert!(id.ends_with("+o:bbbb-2"), "the linked asset is the original: {id}");
    let st = state.lock().unwrap();
    assert_eq!(st.uploads, uploads + 1, "only the render was uploaded");
    assert_eq!(st.stacks.last().unwrap()[1], "bbbb-2");
    drop(st);

    // an original that is not linked yet: it is uploaded with the photo's whole XMP sidecar
    // (IMM-SIDECARS — this is how a managed asset's develop settings reach Immich)
    let three_bytes = png(42);
    std::fs::write(dir.join("three.png"), &three_bytes).unwrap();
    s.execute("library.import", &json!({"paths": [dir.join("three.png").to_string_lossy()]})).unwrap();
    let three = s.catalog.photos().find(|p| p.file_name == "three.png").unwrap().id;
    s.execute("photo.rate", &json!({"ids": [three.0], "rating": 5})).unwrap();
    let svc3 = s
        .execute(
            "publish.createService",
            &json!({"kind": "immich", "name": "Immich originals", "settings": {"account": account, "send": "original"}, "export": {"format": "jpeg", "longEdge": 20}}),
        )
        .unwrap();
    s.execute("library.select", &json!({"ids": [three.0]})).unwrap();
    let coll3 = s.execute("publish.createCollection", &json!({"service": svc3["id"], "name": "Originals", "addSelected": true})).unwrap();
    let before = state.lock().unwrap().sidecar_uploads;
    let r = s.execute("publish.run", &json!({"collection": coll3["collection"]})).unwrap();
    assert_eq!(r["runs"][0]["published"].as_array().map(Vec::len), Some(1), "{r}");
    assert_eq!(state.lock().unwrap().sidecar_uploads, before + 1, "the original carried its sidecar");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn smart_search_maps_results_to_photos() {
    let Setup { mut s, two, dir, .. } = setup("albums");
    // smart search maps Immich's results to photos, and shows or saves them
    let r = s.execute("immich.smartSearch", &json!({"query": "from immich", "show": true, "saveAs": "Immich: from immich"})).unwrap();
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["photos"], json!([two.0]));
    assert_eq!(r["unmatched"], 1);
    assert_eq!(s.visible(), vec![two], "the temporary collection");
    let album = lcc_catalog::AlbumId(r["album"].as_u64().unwrap());
    assert_eq!(s.catalog.album(album).unwrap().photos, vec![two]);
    // nothing found: an empty grid, not everything
    s.execute("immich.smartSearch", &json!({"query": "zebra", "show": true})).unwrap();
    assert!(s.visible().is_empty());
    assert!(s.execute("immich.smartSearch", &json!({"query": "  "})).is_err());
    assert!(s.execute("immich.smartSearch", &json!({"query": "x", "mode": "nope"})).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// RT.C1/C2: an Edit In version of a linked photo goes to Immich stacked on the original as the
/// primary; saving it again asks (or follows the remembered choice): Keep both adds the new upload
/// on top, Replace also trashes the previous upload after copying its albums.
#[test]
fn edit_versions_upload_and_stack_on_immich() {
    let Setup { mut s, state, one, dir, .. } = setup("versions");
    let r = s.execute("photo.editIn", &json!({"id": one.0})).unwrap();
    let ed = PhotoId(r["id"].as_u64().unwrap());
    let r = s.execute("immich.uploadVersion", &json!({"id": ed.0, "background": false})).unwrap();
    assert_eq!(r["stacked"], true, "{r}");
    let first = r["asset"].as_str().unwrap().to_string();
    assert_eq!(state.lock().unwrap().stacks.iter().find(|x| x.len() >= 2).unwrap(), &vec![first.clone(), "aaaa-1".to_string()]);
    assert_eq!(s.catalog.remote_of(ed).next().unwrap().remote_id, first, "the version is linked to its upload");
    // saved again: the setting is `ask`
    let r = s.execute("immich.uploadVersion", &json!({"id": ed.0, "background": false})).unwrap();
    assert_eq!(r["ask"], true, "{r}");
    let r = s.execute("immich.uploadVersion", &json!({"id": ed.0, "resave": "keepBoth", "background": false})).unwrap();
    let second = r["asset"].as_str().unwrap().to_string();
    {
        let st = state.lock().unwrap();
        let stack = st.stacks.iter().find(|x| x.len() >= 2).unwrap();
        assert_eq!(stack[0], second, "the new save is the primary");
        assert!(stack.contains(&first) && stack.contains(&"aaaa-1".to_string()), "{stack:?}");
    }
    // remembered Replace: the previous upload's album carries over, then it goes to the trash
    s.execute("immich.versionSettings", &json!({"resave": "replace"})).unwrap();
    state.lock().unwrap().albums.push(("album-x".into(), "Trip".into(), vec![second.clone()]));
    let r = s.execute("immich.uploadVersion", &json!({"id": ed.0, "background": false})).unwrap();
    let third = r["asset"].as_str().unwrap().to_string();
    assert_eq!(r["trashed"], json!(second), "{r}");
    assert_eq!(remote(&state, &second)["isTrashed"], true);
    assert!(state.lock().unwrap().albums[0].2.contains(&third), "album membership carried over");
    assert_eq!(s.execute("immich.versionSettings", &json!({})).unwrap()["resave"], "replace");
    // a photo with nothing on Immich is skipped
    std::fs::write(dir.join("loose.png"), png(77)).unwrap();
    let r = s.execute("library.import", &json!({"paths": [dir.join("loose.png").to_string_lossy()]})).unwrap();
    let loose = r["imported"][0].as_u64().unwrap();
    assert!(s.execute("immich.uploadVersion", &json!({"id": loose})).unwrap()["skipped"].is_string());
}

/// RT.C3: catalog stacks of linked photos mirror to Immich (top = primary) and back.
#[test]
fn stacks_mirror_both_ways() {
    let Setup { mut s, state, one, two, .. } = setup("stacks");
    s.execute("stack.group", &json!({"ids": [two.0, one.0], "top": two.0})).unwrap();
    let r = s.execute("immich.sync", &json!({})).unwrap();
    assert_eq!(r["stacks"]["toImmich"], 1, "{r}");
    assert_eq!(state.lock().unwrap().stacks.iter().find(|x| x.len() >= 2).unwrap(), &vec!["bbbb-2".to_string(), "aaaa-1".to_string()]);
    // nothing changed: nothing to do
    let r = s.execute("immich.sync", &json!({})).unwrap();
    assert_eq!((r["stacks"]["toImmich"].as_u64(), r["stacks"]["toCatalog"].as_u64()), (Some(0), Some(0)), "{r}");
    // Immich makes the other asset primary: the catalog follows
    {
        let mut st = state.lock().unwrap();
        for x in st.stacks.iter_mut() {
            x.clear();
        }
        st.stacks.push(vec!["aaaa-1".into(), "bbbb-2".into()]);
    }
    let r = s.execute("immich.sync", &json!({})).unwrap();
    assert_eq!(r["stacks"]["toCatalog"].as_u64(), Some(2), "{r}");
    assert_eq!(s.catalog.stack_of(one).unwrap().top(), one);
    // the catalog dissolves it: Immich follows
    s.execute("stack.ungroup", &json!({"ids": [one.0]})).unwrap();
    s.execute("immich.sync", &json!({})).unwrap();
    assert!(state.lock().unwrap().stacks.iter().all(|x| x.len() < 2));
}

// IMM-SIDECARS full push: Immich's sidecar is the base, the catalog's metadata and develop
// settings are merged into it, and the merged XMP is copied back through a temporary asset
// (Immich has no endpoint that writes an existing asset's sidecar).
#[test]
fn full_sidecar_push_merges_immichs_sidecar_and_copies_it_back() {
    let Setup { mut s, state, one, .. } = setup("full-sidecar");
    let existing = r#"<x:xmpmeta xmlns:x='adobe:ns:meta/'><rdf:RDF xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#'>
<rdf:Description rdf:about='' xmlns:dc='http://purl.org/dc/elements/1.1/' xmlns:xmp='http://ns.adobe.com/xap/1.0/'
 xmlns:crs='http://ns.adobe.com/camera-raw-settings/1.0/' xmlns:other='http://example.invalid/ns/'>
<dc:description><rdf:Alt><rdf:li xml:lang='x-default'>Immich copy</rdf:li></rdf:Alt></dc:description>
<xmp:Rating>1</xmp:Rating><crs:Exposure2012='+0.60' other:Secret='keep me'/></rdf:Description></rdf:RDF></x:xmpmeta>"#;
    state.lock().unwrap().sidecars.insert("aaaa-1".into(), existing.into());
    s.execute("photo.rate", &json!({"ids": [one.0], "rating": 5})).unwrap();
    s.execute("photo.setMeta", &json!({"ids": [one.0], "caption": "Catalog caption", "keywords": ["Trip/Paris"]})).unwrap();
    let r = s.execute("immich.pushSidecars", &json!({"ids": [one.0], "full": true})).unwrap();
    assert_eq!(r["pushed"], 1, "{r}");
    let st = state.lock().unwrap();
    let xml = st.sidecars.get("aaaa-1").expect("the target keeps a sidecar");
    let d = lcc_meta::parse_xmp(xml).unwrap();
    assert_eq!(d.metadata.rating, Some(5), "the catalog's rating replaces Immich's:\n{xml}");
    assert_eq!(d.metadata.caption.as_deref(), Some("Catalog caption"), "{xml}");
    assert!(xml.contains("crs:Exposure2012='+0.60'"), "another app's settings stay:\n{xml}");
    assert!(xml.contains("other:Secret='keep me'"), "unknown namespaces stay:\n{xml}");
    assert!(d.lc_settings.is_some(), "the develop settings were added:\n{xml}");
    assert!(st.deleted.iter().any(|id| id.starts_with("up-")), "the temporary asset was removed: {:?}", st.deleted);
}

/// The order a sync run takes the linked pairs in.
fn pair_order(s: &Session) -> Vec<(PhotoId, String)> {
    s.catalog.remote_links().iter().filter(|r| r.service == lcc_immich::link::SERVICE).map(|r| (r.photo_id, r.remote_id.clone())).collect()
}

fn remote_tags(st: &Shared, id: &str) -> Vec<String> {
    remote(st, id)["tags"].as_array().unwrap().iter().filter_map(|t| t["value"].as_str().map(str::to_string)).collect()
}

/// A run the server cuts short must not record what it never sent as synced: a pair it did not
/// reach, and a pair whose tags were still to send, are sent by the next run. Before, their base
/// moved anyway, so the edit stayed local for good and a later run took the keyword away again.
#[test]
fn a_run_cut_short_sends_the_rest_next_time_and_pulls_nothing_back() {
    let Setup { mut s, state, dir, .. } = setup("sync-cut");
    assert_eq!(s.execute("immich.sync", &json!({})).unwrap()["ok"], true);
    let order = pair_order(&s);
    let ((first, first_asset), (last, last_asset)) = (order.first().unwrap().clone(), order.last().unwrap().clone());
    assert_ne!(first, last);

    // 1. the last pair's update fails: the run stops before the tag step of the first pair
    let mut meta = s.catalog.photo(first).unwrap().meta.clone();
    meta.keywords.push("Trip|Rome".into());
    s.catalog.apply(Op::SetMeta { id: first, meta: Box::new(meta) }).unwrap();
    s.execute("photo.rate", &json!({"ids": [last.0], "rating": 2})).unwrap();
    state.lock().unwrap().fail_update = vec![last_asset.clone()];
    let r = s.execute("immich.sync", &json!({})).unwrap();
    assert_eq!(r["ok"], false, "{r}");
    assert!(!remote_tags(&state, &first_asset).contains(&"Trip/Rome".to_string()), "the tag step did not run");
    state.lock().unwrap().fail_update.clear();
    // a full run looks at every pair, as any later change in Immich would make a run do
    let r = s.execute("immich.sync", &json!({"full": true})).unwrap();
    assert_eq!(r["ok"], true, "{r}");
    assert!(s.catalog.photo(first).unwrap().meta.keywords.contains(&"Trip|Rome".to_string()), "the keyword is still in the catalog");
    assert!(remote_tags(&state, &first_asset).contains(&"Trip/Rome".to_string()), "and reached Immich");
    assert_eq!(remote(&state, &last_asset)["exifInfo"]["rating"], 2);

    // 2. the first pair's update fails: the last pair is never reached
    s.execute("photo.rate", &json!({"ids": [first.0], "rating": 5})).unwrap();
    s.execute("photo.rate", &json!({"ids": [last.0], "rating": 3})).unwrap();
    state.lock().unwrap().fail_update = vec![first_asset.clone()];
    let r = s.execute("immich.sync", &json!({})).unwrap();
    assert_eq!(r["ok"], false, "{r}");
    assert_eq!(remote(&state, &last_asset)["exifInfo"]["rating"], 2, "not reached");
    state.lock().unwrap().fail_update.clear();
    // an ordinary (incremental) run sends both: neither counts as synced
    let r = s.execute("immich.sync", &json!({})).unwrap();
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(remote(&state, &first_asset)["exifInfo"]["rating"], 5);
    assert_eq!(remote(&state, &last_asset)["exifInfo"]["rating"], 3);
    assert_eq!((s.catalog.photo(first).unwrap().rating, s.catalog.photo(last).unwrap().rating), (5, 3), "nothing was pulled back");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A photo edited while a run is out is not written over, and the change Immich made is not
/// recorded as taken: the next run takes it. Before, the base moved to Immich's value, so the
/// next run read the catalog's old text as a new edit and sent it back over Immich's.
#[test]
fn a_photo_edited_during_a_run_takes_immichs_change_on_the_next_run() {
    let Setup { mut s, state, two, dir, .. } = setup("sync-during");
    assert_eq!(s.execute("immich.sync", &json!({})).unwrap()["ok"], true);
    assert_eq!(s.catalog.photo(two).unwrap().meta.caption, "From Immich");
    {
        let mut st = state.lock().unwrap();
        let now = st.tick();
        let b = st.asset_mut("bbbb-2").unwrap();
        b["exifInfo"]["description"] = json!("Newer from Immich");
        b["updatedAt"] = json!(now);
    }
    // the run is out (the server is held) while the user rates the photo
    let guard = state.lock().unwrap();
    assert_eq!(s.execute("immich.sync", &json!({"background": true})).unwrap()["started"], true);
    s.execute("photo.rate", &json!({"ids": [two.0], "rating": 1})).unwrap();
    drop(guard);
    pump_until(&mut s, |s, _| s.remote.sync.values().all(|a| !a.running));
    assert_eq!(s.catalog.photo(two).unwrap().rating, 1, "the newer edit is not written over");
    // the next run: Immich's description comes in, the rating goes out
    let r = s.execute("immich.sync", &json!({})).unwrap();
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(s.catalog.photo(two).unwrap().meta.caption, "Newer from Immich");
    assert_eq!(remote(&state, "bbbb-2")["exifInfo"]["description"], "Newer from Immich", "Immich's edit was not overwritten");
    assert_eq!(remote(&state, "bbbb-2")["exifInfo"]["rating"], 1);
    assert_eq!(s.catalog.photo(two).unwrap().rating, 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The sync state belongs to one catalog. A second catalog opened in the same session starts
/// from its own (empty) state: merged against the first one's bases, its freshly imported,
/// unrated photos read as "the user cleared this" and wiped the values in Immich.
#[test]
fn another_catalog_is_not_merged_against_the_first_ones_sync_state() {
    let Setup { mut s, state, dir, .. } = setup("sync-two-libs");
    assert_eq!(s.execute("immich.sync", &json!({})).unwrap()["ok"], true);
    assert_eq!(s.execute("immich.syncStatus", &json!({})).unwrap()["items"], 2);

    s.open_library(dir.join("lib2"), false).unwrap();
    assert_eq!(s.execute("immich.syncStatus", &json!({})).unwrap()["items"], 0, "a new catalog has no sync state yet");
    s.execute("library.import", &json!({"paths": [dir.join("one.png").to_string_lossy(), dir.join("two.png").to_string_lossy()]})).unwrap();
    let two = s.catalog.photos().find(|p| p.file_name == "two.png").unwrap().id;
    assert_eq!(s.catalog.photo(two).unwrap().rating, 0);
    // an ordinary link pass: the cursor of the first catalog's pass is not this catalog's (it
    // was kept per machine, and this pass then listed nothing and linked nothing)
    assert_eq!(s.execute("immich.status", &json!({})).unwrap()["accounts"][0]["linkedUntil"], Value::Null);
    s.execute("immich.link", &json!({})).unwrap();
    pump_until(&mut s, |s, _| lcc_immich::link::link_state(&s.catalog, two) == "linked");
    assert!(s.execute("immich.status", &json!({})).unwrap()["accounts"][0]["linkedUntil"].is_string());
    assert!(dir.join("lib2/Immich/link").is_dir(), "the cursor is kept with the catalog");
    let r = s.execute("immich.sync", &json!({})).unwrap();
    assert_eq!(r["ok"], true, "{r}");
    let b = remote(&state, "bbbb-2");
    assert_eq!(b["exifInfo"]["rating"], 4, "Immich keeps its rating");
    assert_eq!(b["exifInfo"]["description"], "From Immich");
    let p = s.catalog.photo(two).unwrap();
    assert_eq!((p.rating, p.meta.caption.as_str()), (4, "From Immich"), "and the new catalog takes it");
    assert_eq!(std::fs::read_dir(dir.join("lib2/Immich/sync")).unwrap().count(), 1, "its own state file");

    // back in the first catalog its state is its own again
    s.open_library(dir.join("lib"), false).unwrap();
    let st = s.execute("immich.syncStatus", &json!({})).unwrap();
    assert_eq!(st["items"], 2, "{st}");
    let r = s.execute("immich.sync", &json!({"full": true})).unwrap();
    assert_eq!((r["ok"].as_bool(), r["pulled"].as_u64(), r["pushed"].as_u64()), (Some(true), Some(0), Some(0)), "{r}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A queued deletion that was undone in Immich (the asset restored from the trash) leaves the
/// queue, and "apply all queued" never deletes its photo.
#[test]
fn a_deletion_undone_in_immich_leaves_the_queue_and_is_never_applied() {
    let Setup { mut s, state, two, dir, .. } = setup("sync-restore");
    assert_eq!(s.execute("immich.sync", &json!({})).unwrap()["ok"], true);
    let trash = |on: bool| {
        let mut st = state.lock().unwrap();
        let now = st.tick();
        let b = st.asset_mut("bbbb-2").unwrap();
        b["isTrashed"] = json!(on);
        b["updatedAt"] = json!(now);
    };
    trash(true);
    assert_eq!(s.execute("immich.sync", &json!({})).unwrap()["deletionsQueued"], 1);
    // restored in Immich; no run since. Applying everything queued looks first
    trash(false);
    let r = s.execute("immich.resolveDeletion", &json!({"action": "apply"})).unwrap();
    assert_eq!((r["resolved"].as_u64(), r["restored"].as_u64()), (Some(0), Some(1)), "{r}");
    assert!(!s.catalog.photo(two).unwrap().deleted, "the photo of a restored asset is not deleted");
    assert_eq!(s.execute("immich.syncStatus", &json!({})).unwrap()["deletions"].as_array().map(Vec::len), Some(0));
    // a run prunes the queue by itself
    trash(true);
    assert_eq!(s.execute("immich.sync", &json!({})).unwrap()["deletionsQueued"], 1);
    trash(false);
    let r = s.execute("immich.sync", &json!({})).unwrap();
    assert_eq!(r["deletionsQueued"], 0, "{r}");
    // still trashed: applied as before
    trash(true);
    s.execute("immich.sync", &json!({})).unwrap();
    let r = s.execute("immich.resolveDeletion", &json!({"action": "apply"})).unwrap();
    assert_eq!(r["resolved"], 1, "{r}");
    assert!(s.catalog.photo(two).unwrap().deleted);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Immich has no "no location" update. A location cleared in the catalog stays cleared: it is
/// not pulled back by the next run, and it is not a conflict every run either.
#[test]
fn a_location_cleared_in_the_catalog_is_not_pulled_back() {
    let Setup { mut s, state, one, dir, .. } = setup("sync-gps");
    let set_gps = |s: &mut Session, gps: Option<(f64, f64)>| {
        let mut meta = s.catalog.photo(one).unwrap().meta.clone();
        meta.gps = gps;
        s.catalog.apply(Op::SetMeta { id: one, meta: Box::new(meta) }).unwrap();
    };
    set_gps(&mut s, Some((48.8566, 2.3522)));
    let r = s.execute("immich.sync", &json!({})).unwrap();
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(remote(&state, "aaaa-1")["exifInfo"]["latitude"], 48.8566);
    set_gps(&mut s, None);
    for full in [false, true, false] {
        let r = s.execute("immich.sync", &json!({"full": full})).unwrap();
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(s.catalog.photo(one).unwrap().meta.gps, None, "cleared stays cleared: {r}");
        assert_eq!(r["openConflicts"], 0, "{r}");
    }
    // a new location set in Immich afterwards is a real change on that side: it asks (the rule)
    {
        let mut st = state.lock().unwrap();
        let now = st.tick();
        let a = st.asset_mut("aaaa-1").unwrap();
        a["exifInfo"]["latitude"] = json!(10.0);
        a["exifInfo"]["longitude"] = json!(20.0);
        a["updatedAt"] = json!(now);
    }
    let r = s.execute("immich.sync", &json!({})).unwrap();
    assert_eq!(r["openConflicts"], 1, "{r}");
    assert_eq!(s.catalog.photo(one).unwrap().meta.gps, None);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A photo imported after the link pass has passed its asset is still linked: the server is
/// asked for the new photo's checksum (on a worker). The incremental pass alone never finds it.
/// The link pass holds an activity row and can be cancelled without moving its cursor.
#[test]
fn photos_imported_later_are_linked_by_checksum_and_the_link_pass_shows_in_the_activity_stack() {
    let Setup { mut s, state, dir, .. } = setup("link-late");
    let cursor = |s: &mut Session| s.execute("immich.status", &json!({})).unwrap()["accounts"][0]["linkedUntil"].clone();
    assert!(cursor(&mut s).is_string(), "the first pass is complete");
    // an asset Immich has had all along; its file reaches the catalog only now
    let c = png(43);
    std::fs::write(dir.join("three.png"), &c).unwrap();
    {
        let mut st = state.lock().unwrap();
        let mut a = asset("cccc-3", "three.png", &c, json!({"visibility": "timeline", "tags": []}));
        a["updatedAt"] = json!("2026-10-01T00:00:00.000Z");
        st.assets.push(a);
        st.check_checksums = true;
    }
    s.execute("library.import", &json!({"paths": [dir.join("three.png").to_string_lossy()]})).unwrap();
    let three = s.catalog.photos().find(|p| p.file_name == "three.png").unwrap().id;
    pump_until(&mut s, |s, _| lcc_immich::link::link_state(&s.catalog, three) == "linked");
    assert!(s.catalog.remote_of(three).any(|r| r.remote_id == "cccc-3"));
    pump_until(&mut s, |s, _| !s.remote.link_checking);
    assert!(!s.activity.list().iter().any(|t| t.label == "Looking up new photos in Immich"));

    // the link pass: a row while it runs; cancelled, its cursor stays where it was
    let before = cursor(&mut s);
    state.lock().unwrap().asset_mut("aaaa-1").unwrap()["updatedAt"] = json!("2026-10-20T00:00:00.000Z");
    let guard = state.lock().unwrap();
    assert_eq!(s.execute("immich.link", &json!({})).unwrap()["started"], true);
    let row = s.activity.list().into_iter().find(|t| t.kind == "immich" && t.label == "Linking photos with Immich").expect("the link pass has a row");
    s.activity.cancel(row.id).unwrap();
    drop(guard);
    pump_until(&mut s, |s, _| s.remote.links.values().all(|l| !l.active));
    assert!(!s.activity.list().iter().any(|t| t.label == "Linking photos with Immich"));
    assert_eq!(cursor(&mut s), before, "a cancelled pass did not see everything");
    // run to the end, it moves
    s.execute("immich.link", &json!({})).unwrap();
    pump_until(&mut s, |s, _| s.remote.links.values().all(|l| !l.active));
    assert_eq!(cursor(&mut s), "2026-10-20T00:00:00.000Z");
    let _ = std::fs::remove_dir_all(&dir);
}

/// "Newest wins" compares when the catalog was edited, not when a sync first noticed: an edit
/// made before Immich's change loses to it even though the sync runs after both.
#[test]
fn newest_wins_goes_by_when_the_catalog_was_edited() {
    let Setup { mut s, state, one, two, dir, .. } = setup("sync-newest");
    // the session's clock (when a run "notices") is later than anything the server does
    s.clock = Box::new(|| "2026-10-10T00:30:00+00:00".to_string());
    assert_eq!(s.execute("immich.sync", &json!({})).unwrap()["ok"], true);
    s.clock = Box::new(|| "2026-10-10T09:00:00+00:00".to_string());
    let sidecar_at = |s: &mut Session, id: PhotoId, secs_after_midnight: u64| {
        // the photo's metadata written to its sidecar at that time on 2026-10-10 (UTC)
        s.execute("photo.saveMetadataToFile", &json!({"ids": [id.0]})).unwrap();
        let Source::File { path } = &s.catalog.photo(id).unwrap().source else { panic!() };
        let xmp = [format!("{path}.xmp"), std::path::Path::new(path).with_extension("xmp").to_string_lossy().to_string()]
            .into_iter()
            .find(|p| std::path::Path::new(p).exists())
            .expect("a sidecar was written");
        let midnight = std::time::UNIX_EPOCH + Duration::from_secs(1_791_590_400);
        std::fs::File::options().write(true).open(&xmp).unwrap().set_modified(midnight + Duration::from_secs(secs_after_midnight)).unwrap();
        s.record_xmp_stamps(&[id], true);
    };
    let immich_rates = |asset: &str, rating: u8, at: &str| {
        let mut st = state.lock().unwrap();
        let a = st.asset_mut(asset).unwrap();
        a["exifInfo"]["rating"] = json!(rating);
        a["updatedAt"] = json!(at);
    };
    // photo one: edited here at 02:00, in Immich at 03:00 → Immich's is the newer
    s.execute("photo.rate", &json!({"ids": [one.0], "rating": 5})).unwrap();
    sidecar_at(&mut s, one, 2 * 3600);
    immich_rates("aaaa-1", 1, "2026-10-10T03:00:00.000Z");
    // photo two: edited in Immich at 03:00, here at 04:00 → the catalog's is the newer
    s.execute("photo.rate", &json!({"ids": [two.0], "rating": 2})).unwrap();
    sidecar_at(&mut s, two, 4 * 3600);
    immich_rates("bbbb-2", 3, "2026-10-10T03:00:00.000Z");
    let r = s.execute("immich.sync", &json!({})).unwrap();
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["openConflicts"], 0, "{r}");
    assert_eq!(s.catalog.photo(one).unwrap().rating, 1, "Immich's later change wins over the earlier catalog edit");
    assert_eq!(remote(&state, "aaaa-1")["exifInfo"]["rating"], 1);
    assert_eq!(s.catalog.photo(two).unwrap().rating, 2, "the later catalog edit wins");
    assert_eq!(remote(&state, "bbbb-2")["exifInfo"]["rating"], 2);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The calls the Connections screen makes while the window is up run on workers with
/// `background: true`: an activity row while they are out, the result through `immich.answer`.
/// A dry run in the background leaves its list of changes in `immich.syncStatus` → `preview`.
#[test]
fn background_calls_hold_an_activity_row_and_answer_through_the_pump() {
    let Setup { mut s, state, one, dir, .. } = setup("background");
    let answer = |s: &mut Session, key: &str| {
        let t = Instant::now();
        loop {
            let v = s.execute("immich.answer", &json!({"key": key})).unwrap();
            if v["done"] == true {
                return v["result"].clone();
            }
            assert_eq!(v["running"], true, "{v}");
            assert!(t.elapsed() < Duration::from_secs(10), "timed out: {v}");
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    let rows = |s: &Session| s.activity.list().into_iter().map(|t| t.label).collect::<Vec<_>>();
    {
        let mut st = state.lock().unwrap();
        st.people = vec![json!({"id": "p-ann", "name": "Ann"})];
        st.faces = vec![(
            "aaaa-1".into(),
            json!({"id": "f1", "boundingBoxX1": 4, "boundingBoxY1": 3, "boundingBoxX2": 20, "boundingBoxY2": 15, "imageWidth": 40, "imageHeight": 30, "person": {"id": "p-ann", "name": "Ann"}}),
        )];
    }
    // Import People: the server is read on a worker, the catalog written when it is taken in
    let guard = state.lock().unwrap();
    let r = s.execute("immich.importPeople", &json!({"background": true})).unwrap();
    assert_eq!(r["started"], true, "{r}");
    let key = r["key"].as_str().unwrap().to_string();
    assert!(rows(&s).iter().any(|l| l == "Importing people from Immich"), "{:?}", rows(&s));
    assert_eq!(s.execute("immich.importPeople", &json!({"background": true})).unwrap()["started"], false, "one at a time");
    assert!(s.catalog.photo(one).unwrap().people().is_empty(), "nothing yet: the call returned at once");
    drop(guard);
    let r = answer(&mut s, &key);
    assert_eq!((r["ok"].as_bool(), r["added"].as_u64()), (Some(true), Some(1)), "{r}");
    assert_eq!(s.catalog.photo(one).unwrap().people(), vec!["Ann"]);
    assert!(!rows(&s).iter().any(|l| l == "Importing people from Immich"));
    assert_eq!(s.execute("immich.answer", &json!({"key": key})).unwrap()["done"], false, "taken once");

    // Send Names, and a server that refuses: the failure is the answer, not a silent nothing
    let mut meta = s.catalog.photo(one).unwrap().meta.clone();
    meta.regions[0].name = Some("Ann Lee".into());
    s.catalog.apply(Op::SetMeta { id: one, meta: Box::new(meta) }).unwrap();
    let r = s.execute("immich.pushPeople", &json!({"background": true})).unwrap();
    let r = answer(&mut s, r["key"].as_str().unwrap());
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(state.lock().unwrap().renamed[0]["name"], "Ann Lee");
    let r = s.execute("immich.libraries", &json!({"background": true})).unwrap();
    assert_eq!(r["started"], true, "{r}");
    let r = answer(&mut s, r["key"].as_str().unwrap());
    assert_eq!(r["ok"], false, "this server has no libraries endpoint: {r}");
    assert!(r["error"]["message"].is_string());

    // Preview Changes in the background: its changes are kept apart from the last real run
    s.execute("photo.rate", &json!({"ids": [one.0], "rating": 3})).unwrap();
    assert_eq!(s.execute("immich.sync", &json!({"dryRun": true, "background": true})).unwrap()["started"], true);
    pump_until(&mut s, |s, _| s.remote.sync.values().all(|a| !a.running));
    let st = s.execute("immich.syncStatus", &json!({})).unwrap();
    assert_eq!(st["preview"]["dryRun"], true, "{st}");
    assert!(st["preview"]["changes"].as_array().unwrap().iter().any(|c| c["toImmich"]["rating"] == 3), "{st}");
    assert_eq!(remote(&state, "aaaa-1")["exifInfo"]["rating"], Value::Null, "a preview changes nothing");
    let _ = std::fs::remove_dir_all(&dir);
}
