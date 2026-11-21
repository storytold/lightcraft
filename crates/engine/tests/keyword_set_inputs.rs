//! Invalid keyword-set slots must not erase a set or shift its keyboard shortcuts.

use lightcraft_engine::Session;
use serde_json::{Value, json};

fn sets(s: &mut Session) -> Value {
    s.execute("keyword.sets", &json!({})).unwrap()
}

fn named(s: &mut Session, name: &str) -> Value {
    sets(s)["sets"].as_array().unwrap().iter().find(|set| set["name"] == name).unwrap()["keywords"].clone()
}

#[test]
fn malformed_keyword_slots_leave_saved_sets_and_current_set_unchanged() {
    let mut s = Session::new();
    s.execute("keyword.saveSet", &json!({"name": "Saved", "keywords": ["first", "", "third"]})).unwrap();
    s.execute("keyword.saveSet", &json!({"name": "Other", "keywords": ["other"]})).unwrap();
    let before = sets(&mut s);
    for keywords in [
        json!(true),
        json!(7),
        json!(1.5),
        json!({"name": "replacement"}),
        json!(["first", null, "third"]),
        json!(["first", false, "third"]),
        json!(["first", 7, "third"]),
        json!(["first", {}, "third"]),
        json!(["first", [], "third"]),
    ] {
        for mut params in [json!({"name": "Saved"}), json!({"name": "Renamed", "replace": "Saved"}), json!({"name": "New", "new": true})] {
            params["keywords"] = keywords.clone();
            let error = s.execute("keyword.saveSet", &params).unwrap_err().to_string();
            assert!(error.contains("`keywords`"), "{params}: {error}");
            assert_eq!(sets(&mut s), before, "{params}: saved or current sets changed");
        }
    }
}

#[test]
fn valid_keyword_slot_forms_keep_their_existing_meaning() {
    let mut s = Session::new();
    s.execute("keyword.saveSet", &json!({"name": "Text", "keywords": " Parent | Child "})).unwrap();
    assert_eq!(named(&mut s, "Text"), json!(["Parent|Child"]), "text stays a single slot");
    s.execute("keyword.saveSet", &json!({"name": "Slots", "keywords": [" Alpha ", "", "Été", "été", "Last", ""]})).unwrap();
    assert_eq!(named(&mut s, "Slots"), json!(["Alpha", "", "Été", "", "Last"]));

    s.execute("keyword.saveSet", &json!({"name": "Inherited"})).unwrap();
    assert_eq!(named(&mut s, "Inherited"), named(&mut s, "Slots"), "omission inherits the current slots");
    s.execute("keyword.saveSet", &json!({"name": "Renamed", "replace": "Text"})).unwrap();
    assert_eq!(named(&mut s, "Renamed"), json!(["Parent|Child"]), "rename without keywords keeps the renamed set's slots");

    s.execute("keyword.saveSet", &json!({"name": "Slots", "keywords": null})).unwrap();
    assert_eq!(named(&mut s, "Slots"), json!([]), "explicit null still clears");
    s.execute("keyword.saveSet", &json!({"name": "Renamed", "keywords": []})).unwrap();
    assert_eq!(named(&mut s, "Renamed"), json!([]), "an empty list still clears");

    let many: Vec<String> = (1..=10).map(|n| n.to_string()).collect();
    s.execute("keyword.saveSet", &json!({"name": "Nine", "keywords": many})).unwrap();
    assert_eq!(named(&mut s, "Nine"), json!(many[..9]), "the nine-slot limit is unchanged");
}
