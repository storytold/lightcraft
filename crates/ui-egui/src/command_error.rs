//! A refused or failed command as the UI handles it (#1046): the message for people, and what kind of
//! error it is for code that decides on it. Decide on [`CommandError::kind`], never on the message:
//! messages get reworded and will be translated.

use lightcraft_engine::{EngineError, ErrorKind};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandError {
    pub message: String,
    pub kind: ErrorKind,
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// An error the UI raised itself (its own commands, dialogs): [`ErrorKind::Other`].
impl From<String> for CommandError {
    fn from(message: String) -> Self {
        Self { message, kind: ErrorKind::Other }
    }
}

impl From<&str> for CommandError {
    fn from(message: &str) -> Self {
        message.to_string().into()
    }
}

impl From<EngineError> for CommandError {
    fn from(e: EngineError) -> Self {
        Self { kind: e.kind(), message: e.to_string() }
    }
}

impl From<CommandError> for String {
    fn from(e: CommandError) -> Self {
        e.message
    }
}
