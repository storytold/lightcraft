//! The plug-in marketplace's commands: the index is fetched and listed by tier, platform and
//! search; an entry installs and updates through `plugin.install`'s path only when the file is
//! the one listed; the automatic pass installs only what asks for nothing new.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::Session;
use crate::cmd::marketplace;
use crate::cmd::plugins::tests::turn;
use crate::tests_plugin_installs::{Served, listed, signed, stamped_file};

/// The marketplace as the tests see it: a manager in memory whose fetcher serves from a map, an
/// empty store, and an index the test rewrites. Everything goes back as it was on drop.
struct Market {
    served: Arc<Served>,
    entries: Vec<Value>,
    was: Option<lcc_plugins::Manager>,
    ids: Vec<String>,
}

impl Market {
    fn new() -> Market {
        let served = Arc::new(Served::default());
        let mut m = lcc_plugins::Manager::in_memory();
        m.set_fetcher(served.clone());
        let was = crate::cmd::plugins::manager().replace(m);
        marketplace::reset_for_test();
        Market { served, entries: Vec::new(), was, ids: Vec::new() }
    }

    /// Lists `bytes` as entry `id` at `version` (replacing an earlier entry of that id), served
    /// from a release link; `more` overrides fields.
    fn list(&mut self, id: &str, version: &str, kind: &str, tier: &str, bytes: &[u8], more: Value) -> String {
        let url = format!("https://releases.test/{id}/{version}/module.wasm");
        self.served.0.lock().unwrap().insert(url.clone(), bytes.to_vec());
        let mut e = json!({"id": id, "name": format!("Name of {id}"), "version": version, "kind": kind, "tier": tier, "url": url,
            "sha256": lcc_plugins::marketplace::sha256_hex(bytes), "size": bytes.len(), "summary": "Does a thing", "author": "Ann",
            "platforms": ["macos", "windows", "linux", "android"]});
        for (k, v) in more.as_object().into_iter().flatten() {
            e[k] = v.clone();
        }
        self.entries.retain(|o| o["id"] != id);
        self.entries.push(e);
        if !self.ids.iter().any(|i| i == id) {
            self.ids.push(id.to_string());
        }
        self.publish();
        url
    }

    fn publish(&self) {
        let index = json!({"format": 1, "generated": "2026-11-23T04:17:09Z", "source": "https://market.test", "plugins": self.entries});
        self.served.0.lock().unwrap().insert(marketplace::index_url().unwrap(), index.to_string().into_bytes());
    }
}

impl Drop for Market {
    fn drop(&mut self) {
        for id in &self.ids {
            crate::filter_plugins::registry().remove(id);
        }
        *crate::cmd::plugins::manager() = self.was.take();
        marketplace::reset_for_test();
    }
}

/// A stamped plug-in module `id` at `version` asking for `permissions`.
fn plugin(id: &str, version: &str, permissions: Value) -> Vec<u8> {
    let m = json!({"id": id, "name": format!("Name of {id}"), "version": version, "permissions": permissions});
    std::fs::read(stamped_file(&format!("market-{id}-{version}"), Some(&m), &m).path()).unwrap()
}

fn filter(id: &str, version: &str) -> Vec<u8> {
    let manifest = format!(r#"{{"id":"{id}","name":"Filter {id}","version":"{version}","kind":"filter","params":{{}}}}"#);
    crate::tests_plugin_filters::module(&manifest, None)
}

fn entry<'a>(l: &'a Value, id: &str) -> Option<&'a Value> {
    l["entries"].as_array().unwrap().iter().find(|e| e["id"] == id)
}

fn ids(l: &Value) -> Vec<&str> {
    l["entries"].as_array().unwrap().iter().map(|e| e["id"].as_str().unwrap()).collect()
}

#[test]
fn the_index_is_listed_by_tier_search_and_platform() {
    let _turn = turn();
    let mut s = Session::with_demo();
    let mut m = Market::new();
    assert_eq!(s.execute("marketplace.list", &json!({})).unwrap()["fetchedAt"], Value::Null, "nothing is fetched until asked");
    m.list(
        "test.market.vetted",
        "1.0.0",
        "plugin",
        "vetted",
        &plugin("test.market.vetted", "1.0.0", json!({"catalog": true})),
        json!({"permissions": ["catalog"], "review": {"by": "bob", "date": "2026-11-01"}, "tags": ["gps"]}),
    );
    m.list("test.market.look", "0.3", "filter", "community", &filter("test.market.look", "0.3"), json!({"summary": "A film look"}));
    m.list(
        "test.market.elsewhere",
        "1.0.0",
        "plugin",
        "community",
        &plugin("test.market.elsewhere", "1.0.0", json!({})),
        json!({"platforms": ["web"]}),
    );
    m.list(
        "test.market.future",
        "1.0.0",
        "plugin",
        "community",
        &plugin("test.market.future", "1.0.0", json!({})),
        json!({"requires": ["teleport"]}),
    );

    let l = s.execute("marketplace.refresh", &json!({})).unwrap();
    assert_eq!(ids(&l), ["test.market.vetted"], "community entries are hidden until asked for: {l}");
    assert_eq!(
        (&l["hiddenCommunity"], &l["total"], &l["community"], &l["source"]),
        (&json!(3), &json!(4), &json!(false), &json!("https://market.test"))
    );
    let v = entry(&l, "test.market.vetted").unwrap();
    assert_eq!((&v["tier"], &v["reviewedBy"], &v["installed"], &v["installable"]), (&json!("vetted"), &json!("bob"), &Value::Null, &json!(true)));

    assert_eq!(s.execute("marketplace.list", &json!({"community": true})).unwrap()["entries"].as_array().unwrap().len(), 4);
    s.execute("marketplace.setCommunity", &json!({"on": true})).unwrap();
    let all = s.execute("marketplace.list", &json!({})).unwrap();
    assert_eq!((all["entries"].as_array().unwrap().len(), &all["hiddenCommunity"]), (4, &json!(0)));
    assert_eq!(ids(&s.execute("marketplace.list", &json!({"query": "FILM look"})).unwrap()), ["test.market.look"]);
    assert_eq!(ids(&s.execute("marketplace.list", &json!({"query": "gps"})).unwrap()), ["test.market.vetted"]);
    assert!(ids(&s.execute("marketplace.list", &json!({"query": "nothing-has-this"})).unwrap()).is_empty());

    // where it can't be installed, it says so and installs nothing
    let elsewhere = entry(&all, "test.market.elsewhere").unwrap();
    assert_eq!((&elsewhere["supported"], &elsewhere["installable"]), (&json!(false), &json!(false)));
    let e = s.execute("marketplace.install", &json!({"id": "test.market.elsewhere"})).unwrap_err().to_string();
    assert!(e.contains("isn't available on this system"), "{e}");
    let future = entry(&all, "test.market.future").unwrap();
    assert_eq!((&future["missing"], &future["installable"]), (&json!(["teleport"]), &json!(false)));
    assert!(s.execute("marketplace.install", &json!({"id": "test.market.future"})).unwrap_err().to_string().contains("teleport"));
    assert!(s.execute("marketplace.install", &json!({"id": "test.market.unlisted"})).unwrap_err().to_string().contains("doesn't list"));

    // a fetch that fails keeps the last good list and says why
    m.served.0.lock().unwrap().insert(marketplace::index_url().unwrap(), b"{ not json".to_vec());
    assert!(s.execute("marketplace.refresh", &json!({})).is_err());
    let kept = s.execute("marketplace.list", &json!({})).unwrap();
    assert!(kept["error"].as_str().unwrap().contains("isn't JSON") && kept["entries"].as_array().unwrap().len() == 4, "{kept}");
}

#[test]
fn an_entry_installs_and_updates_only_as_the_file_the_index_names() {
    let _turn = turn();
    let mut s = Session::with_demo();
    let mut m = Market::new();
    let (id, look) = ("test.market.tools", "test.market.film");
    m.list(id, "1.0.0", "plugin", "vetted", &plugin(id, "1.0.0", json!({"catalog": true, "network": ["api.example.org"]})), json!({}));
    m.list(look, "1.9", "filter", "vetted", &filter(look, "1.9"), json!({}));
    s.execute("marketplace.refresh", &json!({})).unwrap();

    let r = s.execute("marketplace.install", &json!({"id": id})).unwrap();
    assert_eq!((&r["installed"], &r["plugin"]["version"], &r["from"]), (&json!(true), &json!("1.0.0"), &Value::Null), "{r}");
    let p = listed(&mut s, id).unwrap();
    assert_eq!(p["granted"], json!(lcc_plugins::Permissions::default()), "installing grants nothing: {p}");
    s.execute("marketplace.install", &json!({"id": look})).unwrap();
    let l = s.execute("marketplace.list", &json!({})).unwrap();
    assert_eq!((&entry(&l, id).unwrap()["installed"], &entry(&l, id).unwrap()["update"], &l["updates"]), (&json!("1.0.0"), &json!(false), &json!(0)));
    assert_eq!(ids(&s.execute("marketplace.list", &json!({"installed": true})).unwrap()).len(), 2);

    // the index moves on: nothing is pinned, the next fetch offers what is newest
    m.list(id, "1.1.0", "plugin", "vetted", &plugin(id, "1.1.0", json!({"catalog": true})), json!({}));
    m.list(look, "1.10", "filter", "vetted", &filter(look, "1.10"), json!({}));
    let l = s.execute("marketplace.refresh", &json!({"updates": true})).unwrap();
    assert_eq!((ids(&l).len(), &l["updates"]), (2, &json!(2)), "{l}");
    let done = s.execute("marketplace.updateAll", &json!({})).unwrap();
    assert_eq!((done["updated"].as_array().unwrap().len(), &done["waiting"]), (2, &json!([])), "{done}");
    assert_eq!(listed(&mut s, id).unwrap()["version"], "1.1.0");
    assert_eq!(listed(&mut s, look).unwrap()["version"], "1.10", "1.10 is above 1.9");
    assert_eq!(s.execute("marketplace.updateAll", &json!({})).unwrap()["updated"], json!([]), "nothing more to install");

    // a file that isn't the one listed is never installed
    let url = m.list(id, "2.0.0", "plugin", "vetted", &plugin(id, "2.0.0", json!({})), json!({}));
    s.execute("marketplace.refresh", &json!({})).unwrap();
    m.served.0.lock().unwrap().insert(url.clone(), plugin(id, "2.0.0", json!({"metadataWrite": true})));
    let e = s.execute("marketplace.install", &json!({"id": id})).unwrap_err().to_string();
    assert!(e.contains("isn't the one the marketplace lists"), "{e}");
    // the hash matches, the file is another version, another plug-in, another kind, or no module
    for (bytes, kind, why) in [
        (plugin(id, "2.0.1", json!({})), "plugin", "not test.market.tools 2.0.0"),
        (plugin("test.market.other", "2.0.0", json!({})), "plugin", "not test.market.tools 2.0.0"),
        (filter(id, "2.0.0"), "plugin", "listed as a plug-in but the file is a pixel filter"),
        (plugin(id, "2.0.0", json!({})), "filter", "listed as a pixel filter but the file is a plug-in"),
        (b"\0asm\x01\0\0\0 not a module".to_vec(), "plugin", ""),
        (Vec::new(), "plugin", ""),
    ] {
        m.list(id, "2.0.0", kind, "vetted", &bytes, json!({}));
        s.execute("marketplace.refresh", &json!({})).unwrap();
        let e = s.execute("marketplace.install", &json!({"id": id})).unwrap_err().to_string();
        assert!(e.contains(why), "{why:?}: {e}");
        assert_eq!(listed(&mut s, id).unwrap()["version"], "1.1.0", "nothing was installed");
    }
    assert!(listed(&mut s, "test.market.other").is_none());
}

#[test]
fn another_signer_over_an_installed_plug_in_asks_first() {
    let _turn = turn();
    let mut s = Session::with_demo();
    let mut m = Market::new();
    let id = "test.market.signed";
    let (v1, fp1) = signed(&plugin(id, "1.0.0", json!({})), 7);
    let (v2, fp2) = signed(&plugin(id, "2.0.0", json!({})), 9);
    m.list(id, "1.0.0", "plugin", "vetted", &v1, json!({"signer": fp1}));
    s.execute("marketplace.refresh", &json!({})).unwrap();
    s.execute("marketplace.install", &json!({"id": id})).unwrap();
    assert_eq!(listed(&mut s, id).unwrap()["signer"], fp1, "the first signed install pins its key");

    m.list(id, "2.0.0", "plugin", "vetted", &v2, json!({"signer": fp2}));
    s.execute("marketplace.refresh", &json!({})).unwrap();
    let asked = s.execute("marketplace.install", &json!({"id": id})).unwrap();
    assert_eq!(
        (&asked["installed"], &asked["needs"], &asked["signer"], &asked["pinned"]),
        (&json!(false), &json!("signer"), &json!(fp2), &json!(fp1)),
        "{asked}"
    );
    assert_eq!(listed(&mut s, id).unwrap()["version"], "1.0.0");
    let waiting = s.execute("marketplace.updateAll", &json!({})).unwrap();
    assert!(waiting["waiting"][0]["why"].as_str().unwrap().contains(&fp2) && waiting["updated"] == json!([]), "{waiting}");
    let done = s.execute("marketplace.install", &json!({"id": id, "acceptSigner": fp2})).unwrap();
    assert_eq!((&done["installed"], &done["from"]), (&json!(true), &json!("1.0.0")), "{done}");
    assert_eq!(listed(&mut s, id).unwrap()["signer"], fp2);

    // a signature that doesn't hold up is never "unsigned"
    let mut broken = signed(&plugin(id, "3.0.0", json!({})), 9).0;
    let last = broken.len() - 1;
    broken[last] ^= 1;
    m.list(id, "3.0.0", "plugin", "vetted", &broken, json!({}));
    s.execute("marketplace.refresh", &json!({})).unwrap();
    assert!(s.execute("marketplace.install", &json!({"id": id, "acceptUnsigned": true})).is_err());
    assert_eq!(listed(&mut s, id).unwrap()["version"], "2.0.0");
}

#[test]
fn the_automatic_pass_installs_only_what_asks_for_nothing_new() {
    let _turn = turn();
    let mut s = Session::with_demo();
    let mut m = Market::new();
    let (id, look, manual) = ("test.market.auto", "test.market.autolook", "test.market.manual");
    let (v1, fp) = signed(&plugin(id, "1.0.0", json!({"catalog": true})), 7);
    m.list(id, "1.0.0", "plugin", "community", &v1, json!({"signer": fp}));
    m.list(look, "1.0", "filter", "community", &filter(look, "1.0"), json!({}));
    m.list(manual, "1.0.0", "plugin", "vetted", &plugin(manual, "1.0.0", json!({})), json!({}));
    s.execute("marketplace.refresh", &json!({})).unwrap();
    for i in [id, look, manual] {
        s.execute("marketplace.install", &json!({"id": i})).unwrap();
    }
    assert!(!marketplace::auto_update_wanted(), "off by default: the app fetches nothing by itself");
    s.execute("marketplace.setAutoUpdate", &json!({"on": true})).unwrap();
    s.execute("marketplace.setAutoUpdate", &json!({"on": false, "id": manual})).unwrap();
    assert!(marketplace::auto_update_wanted());

    // same permissions, same signer: installed unasked; the one switched off is left alone
    m.list(id, "1.1.0", "plugin", "community", &signed(&plugin(id, "1.1.0", json!({"catalog": true})), 7).0, json!({"signer": fp}));
    m.list(look, "1.1", "filter", "community", &filter(look, "1.1"), json!({}));
    m.list(manual, "1.1.0", "plugin", "vetted", &plugin(manual, "1.1.0", json!({})), json!({}));
    let r = s.execute("marketplace.autoUpdate", &json!({})).unwrap();
    let mut updated: Vec<&str> = r["updated"].as_array().unwrap().iter().map(|u| u["id"].as_str().unwrap()).collect();
    updated.sort_unstable();
    assert_eq!((updated, &r["waiting"]), (vec![id, look], &json!([])), "{r}");
    assert_eq!(listed(&mut s, manual).unwrap()["version"], "1.0.0");
    let l = s.execute("marketplace.list", &json!({"community": true})).unwrap();
    assert_eq!(
        (&entry(&l, id).unwrap()["autoUpdate"], &entry(&l, manual).unwrap()["autoUpdate"], &entry(&l, manual).unwrap()["update"]),
        (&json!(true), &json!(false), &json!(true))
    );

    // a version that asks for more waits for the user, once
    m.list(id, "1.2.0", "plugin", "community", &signed(&plugin(id, "1.2.0", json!({"catalog": true, "network": ["evil.example"]})), 7).0, json!({}));
    let r = s.execute("marketplace.autoUpdate", &json!({})).unwrap();
    assert_eq!(r["updated"], json!([]), "{r}");
    assert!(r["waiting"][0]["why"].as_str().unwrap().contains("network evil.example"), "{r}");
    assert_eq!(listed(&mut s, id).unwrap()["version"], "1.1.0");
    let l = s.execute("marketplace.list", &json!({"community": true})).unwrap();
    assert_eq!(entry(&l, id).unwrap()["waiting"]["version"], "1.2.0", "{l}");
    assert_eq!(s.execute("marketplace.autoUpdate", &json!({})).unwrap(), json!({"updated": [], "waiting": [], "refreshed": true}), "not tried again");
    // another signer waits too
    m.list(id, "1.3.0", "plugin", "community", &signed(&plugin(id, "1.3.0", json!({"catalog": true})), 9).0, json!({}));
    let r = s.execute("marketplace.autoUpdate", &json!({})).unwrap();
    assert!(r["waiting"][0]["why"].as_str().unwrap().contains("signed by"), "{r}");
    assert_eq!(listed(&mut s, id).unwrap()["version"], "1.1.0");
    // the user's own install goes ahead with their answer, and nothing waits any more
    let fp9 = signed(b"", 9).1;
    s.execute("marketplace.install", &json!({"id": id, "acceptSigner": fp9})).unwrap();
    let l = s.execute("marketplace.list", &json!({"community": true})).unwrap();
    assert_eq!((&entry(&l, id).unwrap()["installed"], &entry(&l, id).unwrap()["waiting"]), (&json!("1.3.0"), &Value::Null));

    // turning it off for all forgets the per-plug-in choices
    s.execute("marketplace.setAutoUpdate", &json!({"on": false})).unwrap();
    assert!(!marketplace::auto_update_wanted());
    assert!(s.execute("marketplace.setAutoUpdate", &json!({})).is_err() && s.execute("marketplace.setCommunity", &json!({"on": "yes"})).is_err());
}
