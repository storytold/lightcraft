//! Menu rows AppKit adds to the menu bar by itself, switched off through the app's user defaults
//! (macOS).
//!
//! AppKit puts "Send <app> Feedback" at the top of the Help menu for every app (Feedback
//! Assistant's entry), beside LightCraft's own Help ▸ Send Feedback…. Setting `NSShowFeedbackMenu`
//! to `false` removes it (read on macOS 27 from the app's menu bar). AppKit reads the default when
//! it builds the menus, so it is set before the menu bar exists.
//!
//! The value is written to the app's preferences (the safe `setBool:forKey:`; registering it only
//! for the process takes `unsafe`, which this crate forbids) and written again at every start,
//! except in a `--memory` session, which writes nothing (issue #164); a run with
//! `LIGHTCRAFT_NO_PREFS` still writes it.

use objc2_foundation::{NSString, NSUserDefaults};

/// The user defaults that stop AppKit adding a row, each set to `false`.
const OFF: &[&str] = &["NSShowFeedbackMenu"];

/// Writes [`OFF`] into `defaults`.
fn switch_off(defaults: &NSUserDefaults) {
    for key in OFF {
        defaults.setBool_forKey(false, &NSString::from_str(key));
    }
}

/// Switches off the rows in [`OFF`] for this app. Call before the menu bar is installed.
pub fn install() {
    switch_off(&NSUserDefaults::standardUserDefaults());
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2::AllocAnyThread;

    /// Removes the throwaway domain, also when an assertion fails.
    struct Domain(objc2::rc::Retained<NSString>, objc2::rc::Retained<NSUserDefaults>);
    impl Drop for Domain {
        fn drop(&mut self) {
            self.1.removePersistentDomainForName(&self.0);
        }
    }

    #[test]
    fn the_system_feedback_row_is_switched_off() {
        // a throwaway domain: the test never touches the real preferences of the app
        let suite = NSString::from_str(&format!("com.storytold.lightcraft.test.appkit-defaults.{}", std::process::id()));
        let defaults = NSUserDefaults::initWithSuiteName(NSUserDefaults::alloc(), Some(&suite)).expect("a test domain");
        let _domain = Domain(suite, defaults.clone());
        let key = NSString::from_str("NSShowFeedbackMenu");
        // the row shows while the default is on (AppKit registers `true`, measured); set it so the
        // test doesn't depend on the developer's own defaults
        defaults.setBool_forKey(true, &key);
        assert!(defaults.boolForKey(&key), "the row shows until the default says otherwise");
        switch_off(&defaults);
        assert!(!defaults.boolForKey(&key), "AppKit's Send Feedback row is off");
    }
}
