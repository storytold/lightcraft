//! Menu rows AppKit adds to the menu bar by itself, switched off through the app's user defaults
//! (macOS).
//!
//! AppKit inserts its own "Enter Full Screen" row into the View menu (measured: not when LightCraft
//! runs in German, so the menu bar had two rows in English and one in German). LightCraft's row
//! (the menu model's `view.enterFullScreen`) is the one row in every language: it reads "Exit Full
//! Screen" while the window is full screen and takes the system key ⌃⌘F. So
//! `NSFullScreenMenuItemEverywhere` is set to `false` and AppKit adds none. AppKit reads it when it
//! builds the menus, so it is set before the menu bar exists.
//!
//! The value is written to the app's preferences (the safe `setBool:forKey:`; registering it only
//! for the process takes `unsafe`, which this crate forbids) and written again at every start,
//! except in a `--memory` session, which writes nothing (issue #164); a run with
//! `LIGHTCRAFT_NO_PREFS` still writes it.

use objc2_foundation::{NSString, NSUserDefaults};

/// The user defaults that stop AppKit adding a row, each set to `false`.
const OFF: &[&str] = &["NSFullScreenMenuItemEverywhere"];

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
    fn appkits_own_full_screen_row_is_switched_off() {
        // a throwaway domain: the test never touches the real preferences of the app
        let suite = NSString::from_str(&format!("com.storytold.lightcraft.test.appkit-defaults.{}", std::process::id()));
        let defaults = NSUserDefaults::initWithSuiteName(NSUserDefaults::alloc(), Some(&suite)).expect("a test domain");
        let _domain = Domain(suite, defaults.clone());
        let key = NSString::from_str("NSFullScreenMenuItemEverywhere");
        assert!(defaults.objectForKey(&key).is_none(), "nothing sets it until the call");
        switch_off(&defaults);
        assert!(defaults.objectForKey(&key).is_some(), "the key is set, not just absent");
        assert!(!defaults.boolForKey(&key), "the default AppKit reads is off");
    }
}
