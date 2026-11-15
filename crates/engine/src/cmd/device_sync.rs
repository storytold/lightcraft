//! `sync.*`: pairing with the app on other devices, and syncing the open catalogue with them
//! (Settings ▸ Sync; [`crate::device_sync`]).

use lcc_sync::model::{Grant, Mode, Take};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, str_param};
use crate::device_sync as sync;

fn bool_req(p: &Value, key: &str, cmd: &str) -> crate::Result<bool> {
    p.get(key).and_then(Value::as_bool).ok_or_else(|| bad(cmd, format!("missing boolean `{key}`")))
}

fn device(p: &Value, cmd: &str) -> crate::Result<String> {
    str_param(p, "device").filter(|d| !d.is_empty()).map(str::to_string).ok_or_else(|| bad(cmd, "missing `device` (a device id from sync.status)"))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(query "sync.status", "Sync Status", [], None, "{} → {running, available, deviceId, name, finding, found: [{deviceId, name}], paired: [{deviceId, name, online, pairedAt}], pairing: {deviceId, name, code, stage, message?} | null, offers: [{deviceId, name, catalogue, mode, keeper, needsEmptyCatalogue}], links: {<deviceId>: {running, last, error, sent, received, files}}, catalogue: {name, photos, unkeyed, group: {id, mode, keeper, devices: [{deviceId, name, me, online, paired, left}], invited} | null, collections: [{id, album, name, kind, owner, ownerName, mine, grant, take, most, takes, photos, letters, pill}], undecided: [{album, name}]} | null}", always, |s, _p| sync::status(s)),
        cmd!(query "sync.find", "Find Devices", [], None, "{on: bool}: look for devices to pair with (while Settings ▸ Sync is open) → sync.status", always, |s, p| {
            let on = bool_req(p, "on", "sync.find")?;
            sync::find(s, on)
        }),
        cmd!(query "sync.start", "Start Sync", [], None, "{}: start listening for paired devices, if there are any → {running}", always, |s, _p| sync::start(s)),
        cmd!(query "sync.pair", "Pair Device", [], None, "{device}: pair with a device that is looking for one (both screens then show a code) → sync.status", always, |s, p| {
            let d = device(p, "sync.pair")?;
            sync::pair(s, &d)
        }),
        cmd!(query "sync.confirm", "Confirm Pairing", [], None, "{accept: bool}: whether both screens show the same code → sync.status", always, |s, p| {
            let accept = bool_req(p, "accept", "sync.confirm")?;
            sync::confirm(s, accept)
        }),
        cmd!(query "sync.clearPairing", "Dismiss Pairing", [], None, "{}: forget a finished or failed pairing → sync.status", always, |s, _p| sync::clear_pairing(s)),
        cmd!(query "sync.cancelPairing", "Cancel Pairing", [], None, "{}: end the pairing in progress at any stage; the other device is told → sync.status", always, |s, _p| sync::cancel_pairing(s)),
        cmd!(query "sync.unpair", "Remove Paired Device", [], None, "{device}: it is no longer paired, and leaves the open catalogue's sync → sync.status", always, |s, p| {
            let d = device(p, "sync.unpair")?;
            sync::unpair(s, &d)
        }),
        cmd!(
            "sync.tick",
            "Sync With Paired Devices",
            [],
            None,
            "{}: tell the sync what changed in the catalogue, answer running exchanges and start the due ones (the app calls it every second while sync runs) → {running, jobs, started}",
            always,
            |s, _p| sync::tick(s)
        ),
        cmd!(query "sync.setup", "Sync With Device", [], None, "{device, mode: catalogue|collections, keep?: this|other}: start syncing the open catalogue with a paired device (it is asked to join), or ask one more device into the sync it already has. `catalogue` shares one catalogue: `keep` says whose it starts from (default `this`; with `other` the open catalogue must be empty). `collections` keeps each device's catalogue and shares only the collections allowed → sync.status", always, |s, p| {
            const C: &str = "sync.setup";
            let d = device(p, C)?;
            let mode = match str_param(p, "mode").unwrap_or("collections") {
                "catalogue" | "catalog" => Mode::Catalogue,
                "collections" => Mode::Collections,
                other => return Err(bad(C, format!("unknown mode `{other}` (catalogue|collections)"))),
            };
            let keep_mine = match str_param(p, "keep").unwrap_or("this") {
                "this" => true,
                "other" => false,
                other => return Err(bad(C, format!("unknown keep `{other}` (this|other)"))),
            };
            sync::setup(s, &d, mode, keep_mine)
        }),
        cmd!(query "sync.answer", "Answer Sync Invitation", [], None, "{device, accept: bool}: join the sync a paired device asked this catalogue into, or turn it down → sync.status", always, |s, p| {
            let d = device(p, "sync.answer")?;
            let accept = bool_req(p, "accept", "sync.answer")?;
            sync::answer(s, &d, accept)
        }),
        cmd!(query "sync.leave", "Stop Syncing This Catalog", [], None, "{}: this device leaves the open catalogue's sync; nothing is deleted → sync.status", always, |s, _p| sync::leave(s)),
        cmd!(query "sync.allow", "Set What a Collection Allows", [], None, "{album | collection | unfiled: true, allow: none|preview|serve|sync}: what the other devices may have of a collection this device owns. none: not shared; preview: previews only, others can't add; serve: originals too, others can't add; sync: any device adds → sync.status", always, |s, p| {
            const C: &str = "sync.allow";
            let grant = str_param(p, "allow").and_then(Grant::parse).ok_or_else(|| bad(C, "missing `allow` (none|preview|serve|sync)"))?;
            sync::set_grant(s, p, grant)
        }),
        cmd!(query "sync.take", "Set What This Device Takes", [], None, "{album | collection, take: none|previews|fullQuality|fullSync}: what this device takes of a collection another device owns (never more than it allows). none: nothing kept here; previews: previews, edits go back; fullQuality: originals, edits go back; fullSync: also photos added here go to the others → sync.status", always, |s, p| {
            const C: &str = "sync.take";
            let take = str_param(p, "take").and_then(Take::parse).ok_or_else(|| bad(C, "missing `take` (none|previews|fullQuality|fullSync)"))?;
            sync::set_take(s, p, take)
        }),
        cmd!(query "sync.skip", "Keep a Collection on This Device", [], None, "{album}: a new collection isn't shared (the question isn't asked again) → sync.status", always, |s, p| {
            let album = p.get("album").and_then(Value::as_u64).ok_or_else(|| bad("sync.skip", "missing `album`"))?;
            sync::skip(s, album)
        }),
        cmd!(query "sync.now", "Sync Now", [], None, "{}: exchange with every device now → sync.status", always, |s, _p| {
            sync::sync_now(s);
            sync::status(s)
        }),
        cmd!(query "sync.pills", "Sync Pills", [], None, "{ids}: the sync pill of each photo → {<id>: {text, pill: {location, depth, status, syncing}} | null}", always, |s, p| {
            let ids: Vec<u64> = p.get("ids").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).collect()).unwrap_or_default();
            Ok(json!(sync::pills(s, &ids)))
        }),
        cmd!(query "sync.rename", "Rename This Device", [], None, "{name}: what other devices call this one (unique among the devices it syncs with) → sync.status", always, |s, p| {
            let name = str_param(p, "name").filter(|n| !n.trim().is_empty()).ok_or_else(|| bad("sync.rename", "missing `name`"))?;
            sync::rename(s, name)
        }),
    ]
}
