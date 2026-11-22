//! Things the user must be told once, as data: the UI words them in the user's language
//! (`panels::notices` in the egui front end) and the CLI prints [`Notice::english`] on stderr.
//! Every notice is also logged in English as it is queued ([`crate::Session::notify`]): that
//! log line is all the MCP server shows, since it never takes the queue (bounded at 64).

/// One thing to tell the user ([`crate::Session::take_notices`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    /// A library settings file (`file`) was damaged: it was moved aside as `kept` and the
    /// defaults of a new library stand in.
    SettingsSetAside { file: String, kept: String },
    /// A library settings file (`file`) exists but couldn't be read: the defaults stand in for
    /// now and the file is not overwritten.
    SettingsUnreadable { file: String, reason: String },
    /// Changes from the device `device` are stamped about `minutes` minutes in the future: they
    /// are held back (not lost) until this device's clock catches up, so its clock is wrong.
    SyncHeldFuture { device: String, minutes: u64 },
}

impl Notice {
    /// The notice in English, for the log and the command line.
    pub fn english(&self) -> String {
        match self {
            Notice::SettingsSetAside { file, kept } => {
                format!("{file} was damaged. It was moved aside as {kept}, and the settings of a new library are used.")
            }
            Notice::SettingsUnreadable { file, reason } => {
                format!("{file} couldn't be read ({reason}). The settings of a new library are used for now, and the file is not overwritten.")
            }
            Notice::SyncHeldFuture { device, minutes } => format!(
                "Changes from {device} are dated {minutes} minutes in the future, so they are held back until then. Set the clock on {device} to the right time."
            ),
        }
    }
}

impl crate::Session {
    /// Notices not handed out yet (each is handed out once).
    pub fn take_notices(&mut self) -> Vec<Notice> {
        std::mem::take(&mut self.notices)
    }

    /// Queues a notice for the user (an equal one still waiting is not repeated).
    pub(crate) fn notify(&mut self, n: Notice) {
        log::warn!("{}", n.english());
        if !self.notices.contains(&n) && self.notices.len() < 64 {
            self.notices.push(n);
        }
    }
}
