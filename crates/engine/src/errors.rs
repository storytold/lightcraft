//! What kind of error a command returned, for code that decides on it (#1046).
//!
//! A frontend that reacts to an error (offers a model download, says "cancelled" in its own words
//! instead of showing a failure) matches [`EngineError::kind`], never the message: messages are
//! for people, get reworded, and will be translated.

use crate::EngineError;

/// A model a feature needs and LightCraft downloads only when the person agrees.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AiModel {
    /// YuNet, which finds faces (`faces.detect`; installed in Settings ▸ Faces).
    FaceDetector,
    /// SAM 3, behind Object and Describe masks (`segment.model.download`).
    Sam3,
}

/// The kind of an [`EngineError`] (or of an error a frontend raised itself: [`ErrorKind::Other`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    UnknownCommand,
    Disabled,
    BadParams,
    Catalog,
    NotSaved,
    LibraryInUse,
    LightroomCatalog,
    /// The work was cancelled: not a failure.
    Cancelled,
    /// The feature needs this model, which is not installed (or still downloading).
    ModelMissing(AiModel),
    Other,
}

impl EngineError {
    /// What kind of error this is: decide on this, not on the message.
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::UnknownCommand(_) => ErrorKind::UnknownCommand,
            Self::Disabled(..) => ErrorKind::Disabled,
            Self::BadParams { .. } => ErrorKind::BadParams,
            Self::Catalog(_) => ErrorKind::Catalog,
            Self::NotSaved(_) => ErrorKind::NotSaved,
            Self::LibraryInUse(_) => ErrorKind::LibraryInUse,
            Self::LightroomCatalog { .. } => ErrorKind::LightroomCatalog,
            Self::Cancelled(_) => ErrorKind::Cancelled,
            Self::ModelMissing { model, .. } => ErrorKind::ModelMissing(*model),
            Self::Other(_) => ErrorKind::Other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_kind_does_not_depend_on_the_message() {
        let cancelled = EngineError::Cancelled("Lightroom-Import abgebrochen".into());
        assert_eq!(cancelled.kind(), ErrorKind::Cancelled);
        assert_eq!(cancelled.to_string(), "Lightroom-Import abgebrochen", "the message is shown as is");
        let missing = EngineError::ModelMissing { model: AiModel::Sam3, msg: "any wording".into() };
        assert_eq!(missing.kind(), ErrorKind::ModelMissing(AiModel::Sam3));
        assert_eq!(missing.to_string(), "any wording");
        // an `Other` whose text reads like a cancellation or a missing model is still `Other`
        assert_eq!(EngineError::Other("The SAM 3 model is not installed; cancelled".into()).kind(), ErrorKind::Other);
    }
}
