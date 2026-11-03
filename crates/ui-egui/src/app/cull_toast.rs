//! The one confirmation for a rating, flag or color label that changed: [`LightcraftApp::run`] calls it
//! for the command that just succeeded, so a key (plain, Shift, an alias, `[` / `]`, the backtick), a menu
//! row, a button and the control channel all say the same thing and no key handler needs a toast of its own.
//!
//! The text names what the photos hold *now* ("Rated ★★★", "Flagged as Pick", "Red Label"), read before
//! Auto Advance or Shift moves on, and says how many photos it acted on when that is more than one.

use lightcraft_catalog::{ColorLabel, Flag, PhotoId};
use serde_json::Value;

use super::LightcraftApp;
use crate::i18n;

/// Whether `id` is a command whose success this module confirms.
fn confirms(id: &str) -> bool {
    matches!(
        id,
        "photo.rate"
            | "photo.increaseRating"
            | "photo.decreaseRating"
            | "photo.flag"
            | "photo.pick"
            | "photo.reject"
            | "photo.unflag"
            | "photo.cycleFlag"
            | "photo.increaseFlag"
            | "photo.decreaseFlag"
            | "photo.label"
            | "photo.clearLabel"
    )
}

/// The photos `id` is about to act on (before it runs: it may move the selection), or `None` for a command
/// this module does not confirm or a target that is not photo ids (the command refuses that itself).
pub(super) fn targets(app: &LightcraftApp, id: &str, params: &Value) -> Option<Vec<PhotoId>> {
    if !confirms(id) {
        return None;
    }
    let mut targets = app.session.targets(params).ok()?;
    // The selection never repeats a photo, but a call's own `ids` may (control channel, agents): a photo named
    // twice is one photo, so the count says so. Only that case pays for the set.
    if params.get("ids").is_some_and(Value::is_array) {
        let mut seen = std::collections::HashSet::with_capacity(targets.len());
        targets.retain(|photo| seen.insert(*photo));
    }
    Some(targets)
}

/// The toast text, and the label color it is tinted with, for `id` having just succeeded on `targets`.
pub(super) fn feedback(app: &LightcraftApp, id: &str, params: &Value, targets: &[PhotoId]) -> Option<(String, Option<ColorLabel>)> {
    // nothing was targeted (`ids: []`): nothing changed, so nothing is confirmed
    if targets.is_empty() {
        return None;
    }
    let (text, color) = match id {
        "photo.label" | "photo.clearLabel" => label_text(app, id, params),
        "photo.rate" | "photo.increaseRating" | "photo.decreaseRating" => (rating_text(app, targets)?, None),
        "photo.flag" | "photo.pick" | "photo.reject" | "photo.unflag" | "photo.cycleFlag" | "photo.increaseFlag" | "photo.decreaseFlag" => {
            (flag_text(app, targets)?, None)
        }
        _ => return None,
    };
    // the photos the key acted on, whether or not each already held the value
    let text = if targets.len() > 1 { i18n::tr_format!("{toast} · {n} photos", toast = text, n = targets.len()) } else { text };
    Some((text, color))
}

/// A color label never reads the photos back: a given label is what was asked for. Clear Color Label takes no
/// color, whatever `label` it is given.
fn label_text(app: &LightcraftApp, id: &str, params: &Value) -> (String, Option<ColorLabel>) {
    let label = params.get("label").and_then(Value::as_str).and_then(ColorLabel::parse).filter(|_| id == "photo.label");
    let text = match label {
        Some(label) => {
            let name = app.session.catalog.custom_label_name(label).map(str::to_owned).unwrap_or_else(|| i18n::tr(&format!("{label:?}")).into());
            i18n::tr_format!("{name} Label", name = name)
        }
        None => i18n::tr("Color label cleared").into(),
    };
    (text, label)
}

/// The value every target now holds, or `None` when they differ (a relative step on a mixed selection) or
/// there is none to read.
fn common<T: PartialEq>(app: &LightcraftApp, targets: &[PhotoId], read: impl Fn(&lightcraft_catalog::Photo) -> T) -> Option<Option<T>> {
    let mut values = targets.iter().filter_map(|id| app.session.catalog.photo(*id)).map(|p| read(p));
    let first = values.next()?;
    Some(values.all(|v| v == first).then_some(first))
}

fn rating_text(app: &LightcraftApp, targets: &[PhotoId]) -> Option<String> {
    Some(match common(app, targets, |p| p.rating)? {
        Some(0) => i18n::tr("Rating cleared").into(),
        Some(stars) => i18n::tr_format!("Rated {}", "★".repeat(usize::from(stars))),
        None => i18n::tr("Rating changed").into(),
    })
}

fn flag_text(app: &LightcraftApp, targets: &[PhotoId]) -> Option<String> {
    Some(
        i18n::tr(match common(app, targets, |p| p.flag)? {
            Some(Flag::Pick) => "Flagged as Pick",
            Some(Flag::Reject) => "Flagged as Reject",
            Some(Flag::None) => "Unflagged",
            None => "Flag changed",
        })
        .into(),
    )
}
