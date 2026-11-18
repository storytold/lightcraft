//! The graphics cards AI models run on. AI Denoise (`nn.rs`, `bm3d.rs`) and the face models (`faces.rs`) each keep a
//! device of their own on each card they use ([`Card`]): the second graphics card (Settings ▸ Performance), which
//! doesn't draw the window, and the card for editing. Each card's device is made on first use and given up on alone
//! after a device error or loss; the other card and the processor carry on.

/// A graphics card AI models run on: each has a device of its own for each model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Card {
    /// The second graphics card (Settings ▸ Performance), when there is one: it doesn't draw the window.
    Second,
    /// The card for editing.
    Editing,
}

impl Card {
    /// `second` or `editing`.
    pub fn id(self) -> &'static str {
        match self {
            Card::Second => "second",
            Card::Editing => "editing",
        }
    }

    /// Its place in a pair of per-card slots ([`CARDS`] order).
    pub(crate) fn index(self) -> usize {
        match self {
            Card::Second => 0,
            Card::Editing => 1,
        }
    }

    /// The other card of the two.
    pub fn other(self) -> Card {
        match self {
            Card::Second => Card::Editing,
            Card::Editing => Card::Second,
        }
    }
}

/// The cards in the order background AI work takes them: the second graphics card first, so the card for editing stays
/// with the photo being edited.
pub const CARDS: [Card; 2] = [Card::Second, Card::Editing];

/// Why there is no AI device on [`Card::Second`].
pub const NO_SECOND_CARD: &str = "there is no second graphics card";
