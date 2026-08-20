//! Full-screen "Unpairing…" state: the daemon has been asked to remove a
//! paired client and hasn't answered yet. Matches NextUI's own full-screen
//! state convention (`ledcontrol.c:262-283`) — no header, no
//! hardware-status chrome, the message is the whole screen — and mirrors
//! [`crate::screens::pairing`] exactly: same reasoning, same shape, just
//! for the other direction of the pairing relationship.
//!
//! Stateless on purpose, same as `pairing`: `main.rs` holds the fingerprint
//! and label this state was entered for (to build the outcome message) and
//! the in-flight request itself ([`crate::rpc::Control`]); there is
//! nothing else to remember here.

use crate::ui::{Buttons, Ui};

/// What the caller should do in response to this frame's input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnpairingAction {
    /// Nothing to act on this frame — still waiting on the daemon.
    None,
    /// `B`: stop waiting. This does **not** undo the request — the daemon
    /// may still complete the unpair after this; it only means the frame
    /// loop no longer cares about the answer. See
    /// [`crate::rpc::Control::cancel_unpair`].
    Cancel,
}

pub fn update(buttons: &Buttons) -> UnpairingAction {
    if buttons.b { UnpairingAction::Cancel } else { UnpairingAction::None }
}

pub fn draw(ui: &mut Ui) {
    ui.full_screen_message("Unpairing...");
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
        assert_eq!(update(&b()), UnpairingAction::Cancel);
    }

    #[test]
    fn anything_else_is_a_no_op() {
        assert_eq!(update(&Buttons::default()), UnpairingAction::None);
    }
}
