//! Base rendering profiles, backed by `craft_develop::profiles`.

use std::borrow::Cow;

use lightcraft_develop::DevelopSettings;

pub use craft_develop::profiles::LOOK_IDS;

/// The settings the pipeline actually renders: user settings plus the profile's look.
pub fn effective(s: &DevelopSettings) -> Cow<'_, DevelopSettings> {
    let craft = crate::bridge::to_craft_settings(s);
    match craft_develop::profiles::effective(&craft) {
        Cow::Borrowed(_) => Cow::Borrowed(s),
        Cow::Owned(modified) => {
            let mut out = s.clone();
            crate::bridge::apply_craft_profile_fields(&mut out, &modified);
            Cow::Owned(out)
        }
    }
}
