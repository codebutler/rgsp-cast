//! Full-screen "Pairing…" state: the daemon has been asked to verify a PIN
//! and hasn't answered yet. Owns the screen until it resolves or the user
//! cancels, matching NextUI's own full-screen state convention
//! (`ledcontrol.c:262-283`) — no header, no hardware-status chrome, the
//! message is the whole screen.
//!
//! Stateless on purpose: `main.rs` holds the [`crate::screens::pin::Pin`]
//! this state was entered from (to return to it, digits intact, on a
//! `NotReady` answer) and the in-flight request itself
//! ([`crate::rpc::Control`]); there is nothing else to remember here, so
//! free functions rather than a struct with an empty body.

use crate::ui::{Buttons, Ui};

/// What the caller should do in response to this frame's input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingAction {
    /// Nothing to act on this frame — still waiting on the daemon.
    None,
    /// `B`: stop waiting. This does **not** un-submit the PIN — the daemon
    /// may still complete the pairing after this; it only means the frame
    /// loop no longer cares about the answer. See
    /// [`crate::rpc::Control::cancel_submit_pin`].
    Cancel,
}

pub fn update(buttons: &Buttons) -> PairingAction {
    if buttons.b { PairingAction::Cancel } else { PairingAction::None }
}

pub fn draw(ui: &mut Ui) {
    ui.full_screen_message("Pairing...");
    ui.hints(&[("B", "Cancel")]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b() -> Buttons {
        Buttons { b: true, ..Default::default() }
    }

    #[test]
    fn b_cancels() {
        assert_eq!(update(&b()), PairingAction::Cancel);
    }

    #[test]
    fn anything_else_is_a_no_op() {
        assert_eq!(update(&Buttons::default()), PairingAction::None);
    }
}
