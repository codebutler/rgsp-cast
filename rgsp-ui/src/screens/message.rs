//! Full-screen terminal message: a pairing attempt ended (rejected, or the
//! connection to the daemon was lost) and there is nothing left to do but
//! tell the user and send them back to the home screen. Matches NextUI's
//! own full-screen state convention (`ledcontrol.c:262-283`): no header, no
//! hardware-status chrome, the message is the whole screen.

use crate::ui::{Buttons, Ui};

/// A short, app-authored message shown full-screen with a single `B Back`
/// button. Only ever constructed from fixed strings in `main.rs` — never
/// from anything caller-supplied — which is what makes drawing it via
/// `GFX_blitMessage` (see [`crate::ui::Ui::full_screen_message`]'s doc
/// comment) safe rather than a hazard.
pub struct Message {
    text: String,
}

impl Message {
    pub fn new(text: impl Into<String>) -> Message {
        Message { text: text.into() }
    }

    /// `true` on `B` — the caller should return to the home screen. There
    /// is only one way out of this state, unlike [`crate::screens::pin`]'s
    /// [`crate::screens::pin::PinAction`], so this doesn't need its own
    /// enum.
    pub fn update(&self, buttons: &Buttons) -> bool {
        buttons.b
    }

    pub fn draw(&self, ui: &mut Ui) {
        ui.full_screen_message(&self.text);
        ui.hints(&[("B", "Back")]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b() -> Buttons {
        Buttons { b: true, ..Default::default() }
    }

    #[test]
    fn b_returns_home() {
        let message = Message::new("Wrong PIN");
        assert!(message.update(&b()));
    }

    #[test]
    fn anything_else_stays() {
        let message = Message::new("Wrong PIN");
        assert!(!message.update(&Buttons::default()));
    }
}
