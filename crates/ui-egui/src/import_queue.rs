//! "Queue without review": a folder added while the library lane is busy can go straight into the waiting line, with
//! the way the last review was confirmed, instead of waiting through its own review scan first (on a network share a
//! scan of a few thousand files takes minutes). This is the pure part: what a queued import is asked, and how its row
//! says so. The app side (the scan row's link, the waiting row's Review…) is in [`crate::import`].

use crate::i18n::{tr, tr_format};
use serde_json::{Value, json};

/// How the last confirmed review brought its files in: what a folder queued without a review is asked to do.
/// Only the way the files are brought in is kept; an album, keywords and presets belong to one batch and would
/// be wrong for another folder (the engine's import defaults still apply to every import when it runs).
/// Never Move: a review that moved its originals is remembered as a copy, so a folder nobody looked at is never
/// emptied of its originals.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Remembered {
    /// Copy into the library (else add in place).
    copy: bool,
    destination: Option<String>,
    organize: Option<String>,
    rename: Option<String>,
    dng: bool,
}

/// A queued import's request, and how its row names the way it brings files in.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    /// The `library.import` params (without `paths`).
    pub params: Value,
    pub transfer: Transfer,
}

/// How a queued import brings its files in, as its row says it.
#[derive(Clone, Debug, PartialEq)]
pub enum Transfer {
    InPlace,
    /// Into the chosen folder, or the library's Originals when `None`.
    Copy(Option<String>),
}

impl Remembered {
    /// What `params` (a confirmed review's `library.import` params) bring in and where.
    pub fn of(params: &Value) -> Self {
        let text = |k: &str| params[k].as_str().map(str::to_string);
        let copy = matches!(params["mode"].as_str(), Some("copy" | "move"));
        if !copy {
            return Remembered::default();
        }
        Remembered {
            copy,
            destination: text("destination"),
            organize: text("organize"),
            rename: text("rename"),
            // (a Move never converts: the dialog turns it off then)
            dng: params["mode"] == "copy" && params["dng"].as_bool().unwrap_or(false),
        }
    }
}

/// The request of a folder queued without a review: as the last review was confirmed (`last`); with none, as the
/// review opens (add in place), or for a card or camera (`card`) copied into the library, as its review opens.
pub fn plan(last: Option<&Remembered>, card: bool) -> Plan {
    let fallback;
    let how = match last {
        Some(last) => last,
        None => {
            fallback = Remembered { copy: card, ..Default::default() };
            &fallback
        }
    };
    if !how.copy {
        return Plan { params: json!({"mode": "add"}), transfer: Transfer::InPlace };
    }
    let mut params = json!({"mode": "copy"});
    if let Some(d) = &how.destination {
        params["destination"] = json!(d);
    }
    if let Some(o) = &how.organize {
        params["organize"] = json!(o);
    }
    if let Some(r) = &how.rename {
        params["rename"] = json!(r);
        params["renameStart"] = json!(1);
    }
    if how.dng {
        params["dng"] = json!(true);
    }
    Plan { params, transfer: Transfer::Copy(how.destination.clone()) }
}

/// What a queued import's row says of how it brings its files in, so that nothing is added, copied or moved
/// without it being plain.
pub fn describe(transfer: &Transfer) -> String {
    match transfer {
        Transfer::InPlace => tr("Without review: add in place").to_string(),
        Transfer::Copy(None) => tr("Without review: copy into the library").to_string(),
        Transfer::Copy(Some(destination)) => tr_format!("Without review: copy to {destination}", destination = destination),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_no_review_before_a_folder_is_added_in_place() {
        let p = plan(None, false);
        assert_eq!(p.params, json!({"mode": "add"}));
        assert_eq!(p.transfer, Transfer::InPlace);
    }

    #[test]
    fn with_no_review_before_a_card_is_copied_into_the_library() {
        let p = plan(None, true);
        assert_eq!(p.params, json!({"mode": "copy"}));
        assert_eq!(p.transfer, Transfer::Copy(None));
    }

    #[test]
    fn a_folder_follows_the_last_review_that_added_in_place() {
        let last = Remembered::of(&json!({"mode": "add", "keywords": ["x"], "album": 3}));
        assert_eq!(plan(Some(&last), false).params, json!({"mode": "add"}));
        // even a card: the person last chose to leave files where they are
        assert_eq!(plan(Some(&last), true).transfer, Transfer::InPlace);
    }

    #[test]
    fn a_folder_follows_the_last_review_that_copied() {
        let last = Remembered::of(&json!({
            "mode": "copy", "destination": "/Volumes/Photos", "organize": "month", "rename": "{date:%Y%m%d}_{seq}", "renameStart": 1,
            "dng": true, "keywords": ["holiday"], "album": 9, "preset": "p1", "metadataPreset": "m1", "onDeleted": "restore"
        }));
        let p = plan(Some(&last), false);
        assert_eq!(
            p.params,
            json!({
                "mode": "copy", "destination": "/Volumes/Photos", "organize": "month", "rename": "{date:%Y%m%d}_{seq}", "renameStart": 1,
                "dng": true
            }),
            "the way files come in, never the batch's album, keywords or presets"
        );
        assert_eq!(p.transfer, Transfer::Copy(Some("/Volumes/Photos".into())));
    }

    #[test]
    fn a_review_that_moved_is_remembered_as_a_copy() {
        let last = Remembered::of(&json!({"mode": "move", "destination": "/Volumes/Photos", "organize": "date"}));
        let p = plan(Some(&last), false);
        assert_eq!(p.params, json!({"mode": "copy", "destination": "/Volumes/Photos", "organize": "date"}));
        assert!(p.params.get("dng").is_none());
        assert_eq!(p.transfer, Transfer::Copy(Some("/Volumes/Photos".into())));
    }

    #[test]
    fn a_row_says_how_its_files_come_in() {
        assert_eq!(describe(&Transfer::InPlace), "Without review: add in place");
        assert_eq!(describe(&Transfer::Copy(None)), "Without review: copy into the library");
        assert_eq!(describe(&Transfer::Copy(Some("/Volumes/Photos".into()))), "Without review: copy to /Volumes/Photos");
    }
}
