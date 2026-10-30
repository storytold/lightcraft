//! System notifications for jobs that end while LightCraft is in the background (macOS).
//!
//! The UI decides when (`Services::notify`: a job the person started has ended while the window is not in front, and
//! Settings ▸ General ▸ Notifications is on). This shows it through `UNUserNotificationCenter`. The system asks the
//! person for permission the first time a notification is requested and remembers the answer; asking again later
//! returns that answer without a prompt, so every notification asks first and shows itself only when allowed.
//!
//! `UNUserNotificationCenter` needs an app bundle: in a process without a bundle identifier (a binary started from
//! a build folder, a test) it raises an Objective-C exception. Such a process shows nothing and says so once in the
//! log. All calls are safe API: the completion blocks get the system's error as a raw pointer, which is never read.

use std::sync::atomic::{AtomicU64, Ordering};

use block2::RcBlock;
use objc2::runtime::Bool;
use objc2_foundation::{NSBundle, NSError, NSString};
use objc2_user_notifications::{UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationRequest, UNUserNotificationCenter};

use lightcraft_ui_egui::Notice;

/// Whether notifications can be shown from a process with this bundle identifier: only an app bundle has one.
fn can_notify(bundle_identifier: Option<&str>) -> bool {
    bundle_identifier.is_some_and(|id| !id.is_empty())
}

/// Ask the system to show `notice`, once the person has allowed notifications from LightCraft.
fn post(notice: &Notice) {
    let (title, body) = (notice.title.clone(), notice.body.clone());
    // the system calls this on a queue of its own, with the person's answer
    let answered = RcBlock::new(move |allowed: Bool, _error: *mut NSError| {
        if allowed.as_bool() {
            add(&title, &body);
        } else {
            log::info!("notifications from LightCraft are not allowed (System Settings ▸ Notifications)");
        }
    });
    UNUserNotificationCenter::currentNotificationCenter().requestAuthorizationWithOptions_completionHandler(UNAuthorizationOptions::Alert, &answered);
}

/// Put the notification in the system's list: it is shown at once because it has no trigger.
fn add(title: &str, body: &str) {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(title));
    content.setBody(&NSString::from_str(body));
    let id = NSString::from_str(&format!("ai.storyteller.lightcraft.job-{}", NEXT.fetch_add(1, Ordering::Relaxed)));
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(&id, &content, None);
    UNUserNotificationCenter::currentNotificationCenter().addNotificationRequest_withCompletionHandler(&request, None);
}

/// The host service for `Services::notify`.
pub fn service() -> lightcraft_ui_egui::NotifyFn {
    // asked of the bundle once; None until the first notification
    let mut available: Option<bool> = None;
    Box::new(move |notice| {
        let available = *available.get_or_insert_with(|| {
            let id = NSBundle::mainBundle().bundleIdentifier().map(|id| id.to_string());
            let ok = can_notify(id.as_deref());
            if !ok {
                log::info!("no notifications: this process is not an app bundle");
            }
            ok
        });
        if available {
            post(notice);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_process_with_a_bundle_identifier_can_notify() {
        assert!(can_notify(Some("ai.storyteller.lightcraft")));
        assert!(!can_notify(Some("")));
        assert!(!can_notify(None));
    }

    #[test]
    fn a_process_that_is_not_an_app_bundle_shows_nothing_and_does_not_crash() {
        // the test binary has no bundle identifier: asking the notification center would raise an exception
        assert_eq!(NSBundle::mainBundle().bundleIdentifier(), None);
        let mut notify = service();
        for _ in 0..2 {
            notify(&Notice { title: "LightCraft".into(), body: "Exporting finished".into() });
        }
    }
}
