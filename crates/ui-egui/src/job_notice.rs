//! Telling a person who is in another app that a job they started has ended.
//!
//! [`JobWatch`] is the pure part: given the jobs running now it notices which ones have ended without being asked to
//! stop, waits a moment for the toast that says how it went (the screen's own account of the end, success or
//! failure), and hands over what to say. The host shows it (`Services::notify`); the window decides whether anybody
//! is looking (`LightcraftApp::sync_job_notices`).

use lightcraft_engine::activity::TaskInfo;

/// How long an ended job waits for the toast that reports it before it is announced with what it was called.
pub const GRACE_SECS: f64 = 0.5;
/// A toast that appeared this long before the announcement still belongs to the job that ended.
const TOAST_WINDOW_SECS: f64 = 3.0;

/// Jobs that ended, as they are announced.
#[derive(Debug, PartialEq, Eq)]
pub struct Ended {
    /// What they were called (English, as the activity stack has it; the window translates it).
    pub labels: Vec<String>,
    /// What the window said about the end meanwhile ("Exported 24 photos"), when it said anything.
    pub said: Option<String>,
    /// The window was in the background at some moment between the first job's end (or the end of the follow-on work the
    /// announcement waited for) and now: somebody to tell.
    pub away: bool,
}

/// What to say about jobs that ended: the window's own account of it when one job ended and it gave one, else what
/// they were called (or how many ended: one toast does not speak for several jobs).
pub fn compose(ended: &Ended) -> crate::Notice {
    let body = match (&ended.said, ended.labels.as_slice()) {
        (Some(said), [_]) => said.clone(),
        (_, [one]) => crate::i18n::tr_format!("{label} finished", label = crate::i18n::tr(one)),
        (_, many) => crate::i18n::tr_format!("{n} jobs finished", n = many.len()),
    };
    crate::Notice { title: "LightCraft".to_owned(), body }
}

/// Remembers the running jobs between looks.
#[derive(Default)]
pub struct JobWatch {
    /// (id, label, asked to stop) of the jobs seen running at the last look.
    running: Vec<(u64, String, bool)>,
    /// The toast last seen (its text and the moment it goes away identify it), and when it first was.
    toast: Option<(String, u64, f64)>,
    /// The toast an announcement has used or passed over: it speaks for no later job.
    claimed: Option<(String, u64)>,
    /// Jobs that ended and wait for their announcement, and when the last of them was missed.
    pending: Option<(f64, Vec<String>)>,
    /// Jobs named like one waiting in `pending` that started meanwhile, still running: a second phase of the same work (a folder read
    /// that goes on to add its photos), so nothing is announced until they have ended.
    follow_on: Vec<u64>,
    /// The window was in the background while jobs waited in `pending`.
    away: bool,
}

impl JobWatch {
    /// Look at the jobs the person started that run at `now` (seconds, any steady clock; `jobs` as
    /// `Activity::person_jobs` has them), the toast on screen (its text and the moment it goes away) and whether the
    /// window is in the background. Returns what to announce once the jobs that ended have waited out [`GRACE_SECS`].
    pub fn update(&mut self, now: f64, jobs: &[TaskInfo], toast: Option<(&str, f64)>, in_background: bool) -> Option<Ended> {
        if let Some((text, until)) = toast {
            let key = (text.to_owned(), until.to_bits());
            let seen = self.toast.as_ref().is_some_and(|(t, u, _)| (t, u) == (&key.0, &key.1));
            if !seen && self.claimed.as_ref() != Some(&key) {
                self.toast = Some((key.0, key.1, now));
            }
        }
        let started: Vec<(u64, &str)> =
            jobs.iter().filter(|j| !self.running.iter().any(|(id, ..)| *id == j.id)).map(|j| (j.id, j.label.as_str())).collect();
        let follow_on_stopped =
            self.follow_on.iter().any(|f| !jobs.iter().any(|j| j.id == *f) && self.running.iter().any(|(id, _, stopping)| id == f && *stopping));
        // a job that vanished without having been asked to stop has ended on its own (done, or failed)
        let gone: Vec<(u64, String)> = self
            .running
            .iter()
            .filter(|(id, _, stopping)| !stopping && !jobs.iter().any(|j| j.id == *id))
            .map(|(id, label, _)| (*id, label.clone()))
            .collect();
        self.running = jobs.iter().map(|j| (j.id, j.label.clone(), j.cancelling)).collect();
        if !gone.is_empty() {
            if self.pending.is_none() || gone.iter().any(|(id, _)| self.follow_on.contains(id)) {
                // a new batch, or the end of the work the batch waited for: an end after the window came forward is
                // one it saw (an unrelated job joining a batch keeps what the first end found)
                self.away = false;
            }
            let (since, labels) = self.pending.get_or_insert_with(|| (now, Vec::new()));
            *since = now;
            for (_, label) in gone {
                if !labels.contains(&label) {
                    labels.push(label);
                }
            }
        }
        if follow_on_stopped {
            // the person stopped the second phase: the work was not left to finish
            self.pending = None;
            self.follow_on.clear();
            self.away = false;
        } else {
            // only a job with the name of one that ended carries its work on (a folder read's second phase)
            if let Some((_, labels)) = &self.pending {
                self.follow_on.extend(started.iter().filter(|(_, label)| labels.iter().any(|l| l == label)).map(|(id, _)| *id));
            }
            self.follow_on.retain(|f| jobs.iter().any(|j| j.id == *f));
        }
        // (the window comes forward again while the toast is awaited: the job did end while nobody looked)
        self.away |= in_background && self.pending.is_some();
        let (since, _) = self.pending.as_ref()?;
        if now - since < GRACE_SECS || !self.follow_on.is_empty() {
            return None;
        }
        let since = *since;
        let (_, labels) = self.pending.take()?;
        // one toast belongs to one announcement: whether or not it was this one's, it speaks for no later job
        let toast = self.toast.take();
        if let Some((text, until, _)) = &toast {
            self.claimed = Some((text.clone(), *until));
        }
        let said = toast.filter(|(_, _, at)| *at >= since - TOAST_WINDOW_SECS).map(|(text, ..)| text);
        Some(Ended { labels, said, away: std::mem::take(&mut self.away) })
    }

    /// Whether something ended and is waiting to be announced: the window must look again soon.
    pub fn waiting(&self) -> bool {
        self.pending.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_engine::activity::{Activity, Cancel};

    fn running(a: &Activity) -> Vec<TaskInfo> {
        a.list()
    }

    #[test]
    fn nothing_running_says_nothing() {
        let mut w = JobWatch::default();
        assert_eq!(w.update(0.0, &[], None, true), None);
        assert!(!w.waiting());
    }

    #[test]
    fn a_job_that_ends_is_announced_after_the_grace() {
        let a = Activity::default();
        let mut w = JobWatch::default();
        let g = a.start("export", "Exporting", Cancel::Yes);
        assert_eq!(w.update(0.0, &running(&a), None, true), None);
        drop(g);
        assert_eq!(w.update(1.0, &running(&a), None, true), None, "not before it has waited for the toast");
        assert!(w.waiting());
        assert_eq!(w.update(1.0 + GRACE_SECS, &running(&a), None, true), Some(Ended { labels: vec!["Exporting".into()], said: None, away: true }));
        assert!(!w.waiting());
        assert_eq!(w.update(5.0, &[], None, true), None, "only once");
    }

    #[test]
    fn a_job_asked_to_stop_is_not_announced() {
        let a = Activity::default();
        let mut w = JobWatch::default();
        let g = a.start("export", "Exporting", Cancel::Yes);
        w.update(0.0, &running(&a), None, true);
        a.cancel(g.id()).unwrap();
        w.update(1.0, &running(&a), None, true);
        drop(g);
        w.update(2.0, &running(&a), None, true);
        assert_eq!(w.update(3.0, &[], None, true), None);
        assert!(!w.waiting());
    }

    #[test]
    fn what_the_window_said_about_the_end_goes_with_it() {
        let a = Activity::default();
        let mut w = JobWatch::default();
        let g = a.start("export", "Exporting", Cancel::Yes);
        w.update(0.0, &running(&a), None, true);
        drop(g);
        w.update(1.0, &[], None, true);
        w.update(1.2, &[], Some(("Exported 24 photos", 15.0)), true);
        let ended = w.update(1.0 + GRACE_SECS, &[], Some(("Exported 24 photos", 15.0)), true);
        assert_eq!(ended, Some(Ended { labels: vec!["Exporting".into()], said: Some("Exported 24 photos".into()), away: true }));
    }

    #[test]
    fn a_toast_from_long_before_is_not_the_jobs() {
        let a = Activity::default();
        let mut w = JobWatch::default();
        // an old toast still on screen when the job ends
        w.update(0.0, &[], Some(("Auto Sync: On", 15.0)), true);
        let g = a.start("export", "Exporting", Cancel::Yes);
        w.update(10.0, &running(&a), Some(("Auto Sync: On", 15.0)), true);
        drop(g);
        w.update(20.0, &[], Some(("Auto Sync: On", 15.0)), true);
        let ended = w.update(20.0 + GRACE_SECS, &[], Some(("Auto Sync: On", 15.0)), true);
        assert_eq!(ended, Some(Ended { labels: vec!["Exporting".into()], said: None, away: true }));
    }

    #[test]
    fn jobs_that_end_together_are_announced_together() {
        let a = Activity::default();
        let mut w = JobWatch::default();
        let x = a.start("export", "Exporting", Cancel::Yes);
        let i = a.start("import", "Importing", Cancel::Yes);
        w.update(0.0, &running(&a), None, true);
        drop(x);
        w.update(1.0, &running(&a), None, true);
        drop(i);
        w.update(1.2, &running(&a), None, true);
        let ended = w.update(1.2 + GRACE_SECS, &[], None, true).expect("announced");
        assert_eq!(ended.labels, ["Exporting", "Importing"]);
    }

    #[test]
    fn a_job_that_ends_with_the_window_in_front_is_not_for_anybody_away() {
        let a = Activity::default();
        let mut w = JobWatch::default();
        let g = a.start("export", "Exporting", Cancel::Yes);
        w.update(0.0, &running(&a), None, false);
        drop(g);
        w.update(1.0, &[], None, false);
        let ended = w.update(1.0 + GRACE_SECS, &[], None, false).expect("announced");
        assert!(!ended.away);
        // the window went to the background while the end waited for its toast: somebody away missed it
        let g = a.start("export", "Exporting", Cancel::Yes);
        w.update(5.0, &running(&a), None, false);
        drop(g);
        w.update(6.0, &[], None, false);
        w.update(6.2, &[], None, true);
        assert!(w.update(6.0 + GRACE_SECS, &[], None, false).expect("announced").away);
    }

    #[test]
    fn a_toast_speaks_for_one_announcement_only() {
        let a = Activity::default();
        let mut w = JobWatch::default();
        let toast = Some(("Exported 24 photos", 15.0));
        let g = a.start("export", "Exporting", Cancel::Yes);
        w.update(0.0, &running(&a), None, true);
        drop(g);
        w.update(1.0, &[], toast, true);
        let first = w.update(1.0 + GRACE_SECS, &[], toast, true).expect("announced");
        assert_eq!(first.said.as_deref(), Some("Exported 24 photos"));
        // a later job that says nothing is not given the old toast, which is still on screen
        let g = a.start("import", "Importing", Cancel::Yes);
        w.update(2.0, &running(&a), toast, true);
        drop(g);
        w.update(2.2, &[], toast, true);
        let second = w.update(2.2 + GRACE_SECS, &[], toast, true).expect("announced");
        assert_eq!(second.said, None);
        // nor is a third, shortly after, by the toast an announcement without one has not forgotten
        let g = a.start("import", "Importing", Cancel::Yes);
        w.update(2.8, &running(&a), toast, true);
        drop(g);
        w.update(3.0, &[], toast, true);
        let third = w.update(3.0 + GRACE_SECS, &[], toast, true).expect("announced");
        assert_eq!(third.said, None);
        // the same words in a new toast (another moment to go away) are that job's own
        let g = a.start("export", "Exporting", Cancel::Yes);
        w.update(6.0, &running(&a), None, true);
        drop(g);
        let again = Some(("Exported 24 photos", 19.0));
        w.update(6.2, &[], again, true);
        let fourth = w.update(6.2 + GRACE_SECS, &[], again, true).expect("announced");
        assert_eq!(fourth.said.as_deref(), Some("Exported 24 photos"));
    }

    #[test]
    fn one_toast_does_not_speak_for_several_jobs() {
        let ended = Ended { labels: vec!["Exporting".into(), "Importing".into()], said: Some("Imported 3 photos".into()), away: true };
        assert_eq!(compose(&ended).body, crate::i18n::tr_format!("{n} jobs finished", n = 2));
        let one = Ended { labels: vec!["Importing".into()], ..ended };
        assert_eq!(compose(&one).body, "Imported 3 photos");
    }

    #[test]
    fn a_job_that_follows_on_from_the_one_that_ended_is_announced_once_when_nothing_runs() {
        let a = Activity::default();
        let mut w = JobWatch::default();
        // a folder read: the scan ends, then the import that adds its photos starts under the same name
        let scan = a.start("scan", "Reading folder", Cancel::Yes);
        w.update(0.0, &running(&a), None, true);
        drop(scan);
        let add = a.start("import", "Reading folder", Cancel::Yes);
        w.update(1.0, &running(&a), None, true);
        assert_eq!(w.update(1.0 + GRACE_SECS, &running(&a), None, true), None, "the work goes on");
        assert_eq!(w.update(30.0, &running(&a), None, true), None);
        drop(add);
        w.update(31.0, &[], None, true);
        let ended = w.update(31.0 + GRACE_SECS, &[], None, true).expect("announced");
        assert_eq!(ended.labels, ["Reading folder"]);
        assert_eq!(w.update(40.0, &[], None, true), None, "once");
    }

    #[test]
    fn a_follow_on_job_holds_the_announcement_but_an_unrelated_long_job_does_not() {
        let a = Activity::default();
        let mut w = JobWatch::default();
        let long = a.start("previews", "Building previews", Cancel::Yes);
        let scan = a.start("scan", "Reading folder", Cancel::Yes);
        w.update(0.0, &running(&a), None, true);
        drop(scan);
        let add = a.start("import", "Reading folder", Cancel::Yes);
        w.update(1.0, &running(&a), None, true);
        assert_eq!(w.update(5.0, &running(&a), None, true), None, "the folder read goes on");
        drop(add);
        w.update(6.0, &running(&a), None, true);
        let ended = w.update(6.0 + GRACE_SECS, &running(&a), None, true).expect("not held up by the long build");
        assert_eq!(ended.labels, ["Reading folder"]);
        drop(long);
    }

    #[test]
    fn a_follow_on_the_person_stops_announces_nothing_and_a_return_to_the_window_is_seen() {
        let a = Activity::default();
        let mut w = JobWatch::default();
        let scan = a.start("scan", "Reading folder", Cancel::Yes);
        w.update(0.0, &running(&a), None, true);
        drop(scan);
        let add = a.start("import", "Reading folder", Cancel::Yes);
        w.update(1.0, &running(&a), None, true);
        a.cancel(add.id()).unwrap();
        w.update(2.0, &running(&a), None, false);
        drop(add);
        assert_eq!(w.update(3.0, &[], None, false), None);
        assert!(!w.waiting());
        // the window was in front when the follow-on ended: nobody is away
        let scan = a.start("scan", "Reading folder", Cancel::Yes);
        w.update(10.0, &running(&a), None, true);
        drop(scan);
        let add = a.start("import", "Reading folder", Cancel::Yes);
        w.update(11.0, &running(&a), None, true);
        drop(add);
        w.update(20.0, &[], None, false);
        let ended = w.update(20.0 + GRACE_SECS, &[], None, false).expect("announced");
        assert!(!ended.away);
    }

    #[test]
    fn a_job_with_another_name_that_starts_while_one_waits_does_not_hold_the_announcement() {
        let a = Activity::default();
        let mut w = JobWatch::default();
        let export = a.start("export", "Exporting", Cancel::Yes);
        w.update(0.0, &running(&a), None, true);
        drop(export);
        // an unrelated job starts during the grace (an Auto Import, say)
        let other = a.start("import", "Importing", Cancel::Yes);
        w.update(0.5, &running(&a), None, true);
        let ended = w.update(1.0, &running(&a), None, true).expect("not held by an unrelated job");
        assert_eq!(ended.labels, ["Exporting"]);
        drop(other);
    }

    #[test]
    fn a_second_end_joining_a_batch_keeps_that_the_window_was_away_at_the_first() {
        let a = Activity::default();
        let mut w = JobWatch::default();
        let x = a.start("export", "Exporting", Cancel::Yes);
        let i = a.start("import", "Importing", Cancel::Yes);
        w.update(0.0, &running(&a), None, true);
        drop(x);
        w.update(1.0, &running(&a), None, true);
        // the window comes forward, then the second job ends within the same wait
        drop(i);
        w.update(1.2, &[], None, false);
        let ended = w.update(1.2 + GRACE_SECS, &[], None, false).expect("announced");
        assert_eq!(ended.labels, ["Exporting", "Importing"]);
        assert!(ended.away, "the first end happened while nobody looked");
    }
}
