//! Full-screen "are you sure" state: `A` on a paired row lands here before
//! anything is actually unpaired. Matches NextUI's own full-screen state
//! convention (`ledcontrol.c:262-283`), same as [`crate::screens::message`]
//! and [`crate::screens::pairing`] — no header, no hardware-status chrome,
//! the question is the whole screen — but with two buttons instead of one,
//! since a plain `B Back` has nothing to confirm.
//!
//! Unpairing is destructive (the device has to pair again to reconnect) and
//! easy to hit by accident on a D-pad, so it does not fire straight off the
//! home row's `A` press the way pairing does.

use crate::ui::{Buttons, Ui};

/// What the caller should do in response to this frame's input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmAction {
    /// Nothing to act on this frame.
    None,
    /// `A`: go through with it.
    Confirm,
    /// `B`: back out, nothing changes.
    Cancel,
}

/// Confirms unpairing the client certificate fingerprint `fingerprint`,
/// labeled `label` for the question's wording.
pub struct Confirm {
    fingerprint: String,
    label: String,
}

impl Confirm {
    pub fn new(fingerprint: String, label: String) -> Confirm {
        Confirm { fingerprint, label }
    }

    /// The fingerprint this screen is confirming, for the caller to pass
    /// back to [`crate::rpc::Control::start_unpair`] on
    /// [`ConfirmAction::Confirm`].
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// The label this screen was built with, for the caller to reuse when
    /// naming the outcome (`main.rs`'s `Unpairing` screen) instead of
    /// re-deriving it from `state`, which may have already lost this entry
    /// by the time confirmation lands.
    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn update(&self, buttons: &Buttons) -> ConfirmAction {
        if buttons.a {
            ConfirmAction::Confirm
        } else if buttons.b {
            ConfirmAction::Cancel
        } else {
            ConfirmAction::None
        }
    }

    pub fn draw(&self, ui: &mut Ui) {
        ui.full_screen_message(&format!("Unpair {}?", self.label));
        ui.hints(&[("A", "Unpair"), ("B", "Cancel")]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a() -> Buttons {
        Buttons { a: true, ..Default::default() }
    }

    fn b() -> Buttons {
        Buttons { b: true, ..Default::default() }
    }

    #[test]
    fn a_confirms() {
        let confirm = Confirm::new("ff00".into(), "phone".into());
        assert_eq!(confirm.update(&a()), ConfirmAction::Confirm);
    }

    #[test]
    fn b_cancels() {
        let confirm = Confirm::new("ff00".into(), "phone".into());
        assert_eq!(confirm.update(&b()), ConfirmAction::Cancel);
    }

    #[test]
    fn anything_else_is_a_no_op() {
        let confirm = Confirm::new("ff00".into(), "phone".into());
        assert_eq!(confirm.update(&Buttons::default()), ConfirmAction::None);
    }

    #[test]
    fn the_fingerprint_is_reported_back_for_unpair() {
        let confirm = Confirm::new("ff00".into(), "phone".into());
        assert_eq!(confirm.fingerprint(), "ff00");
    }
}
