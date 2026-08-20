//! PIN entry screen: four digits, one cursor.
//!
//! [`Pin::update`] is pure — no [`Ui`], no FFI — so it unit-tests off-device;
//! only [`Pin::draw`] touches `Ui`, mirroring [`crate::screens::home`].

use crate::ui::{Buttons, Ui};

/// What the caller should do in response to this frame's input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinAction {
    /// Nothing to act on this frame.
    None,
    /// `A`: submit the four digits, concatenated in cursor order.
    Submit(String),
    /// `B`: abandon pairing, back to the previous screen.
    Back,
}

/// Four-digit PIN entry for pairing `client_id`.
pub struct Pin {
    client_id: String,
    client_name: Option<String>,
    client_address: Option<String>,
    digits: [u8; 4],
    cursor: usize,
    error: Option<String>,
}

impl Pin {
    pub fn new(client_id: String, client_name: Option<String>, client_address: Option<String>) -> Pin {
        Pin { client_id, client_name, client_address, digits: [0; 4], cursor: 0, error: None }
    }

    /// The client id this screen is pairing, for the caller to pass back to
    /// [`crate::rpc::Control::submit_pin`] on [`PinAction::Submit`].
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// The label to show for the client being paired: its name if the
    /// daemon reported one, else its address, else the same truncated id
    /// fallback as the home screen's pending list (see
    /// [`crate::screens::client_label`]).
    pub fn label(&self) -> String {
        crate::screens::client_label(self.client_name.as_deref(), self.client_address.as_deref(), &self.client_id)
    }

    /// Advance the cursor and/or the digit under it.
    ///
    /// Left/right move the cursor but do **not** wrap: overshooting past the
    /// first or last digit would silently land back on a different digit,
    /// which is easy to lose track of on a 4-digit entry with no scrollback.
    /// Up/down wrap the digit itself — 9 up is 0, 0 down is 9 — since there
    /// is no "past the end" for a single decimal digit the way there is for
    /// the cursor.
    pub fn update(&mut self, buttons: &Buttons) -> PinAction {
        if buttons.b {
            return PinAction::Back;
        }

        if buttons.left && self.cursor > 0 {
            self.cursor -= 1;
            self.error = None;
        } else if buttons.right && self.cursor + 1 < self.digits.len() {
            self.cursor += 1;
            self.error = None;
        }

        if buttons.up {
            self.digits[self.cursor] = (self.digits[self.cursor] + 1) % 10;
            self.error = None;
        } else if buttons.down {
            self.digits[self.cursor] = (self.digits[self.cursor] + 9) % 10;
            self.error = None;
        }

        if buttons.a {
            let pin: String = self.digits.iter().map(u8::to_string).collect();
            return PinAction::Submit(pin);
        }

        PinAction::None
    }

    pub fn digits(&self) -> [u8; 4] {
        self.digits
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Records a message to show under the digit row, e.g. after
    /// `Control::submit_pin` reports [`crate::rpc::PinOutcome::Rejected`] or
    /// [`crate::rpc::PinOutcome::NotReady`]. Cleared automatically the next
    /// time the user edits a digit or the cursor (see [`Pin::update`]), so a
    /// stale rejection message doesn't linger over a PIN the user has since
    /// changed.
    pub fn set_error(&mut self, msg: &str) {
        self.error = Some(msg.to_string());
    }

    pub fn draw(&self, ui: &mut Ui) {
        let chrome_w = ui.hardware_group();
        ui.header(&format!("Pair with {}", self.label()), chrome_w);
        ui.pin(&self.digits, self.cursor);
        // Below the cursor underline, not `row()`'s left-aligned list-row
        // slot at the top of the screen -- the error is feedback about the
        // digits, so it belongs near them, centred to match the digit
        // group's own alignment.
        if let Some(err) = &self.error {
            ui.pin_error(err);
        }
        ui.hints(&[("A", "Submit"), ("B", "Back")]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn up() -> Buttons {
        Buttons { up: true, ..Default::default() }
    }

    fn down() -> Buttons {
        Buttons { down: true, ..Default::default() }
    }

    fn left() -> Buttons {
        Buttons { left: true, ..Default::default() }
    }

    fn right() -> Buttons {
        Buttons { right: true, ..Default::default() }
    }

    fn a() -> Buttons {
        Buttons { a: true, ..Default::default() }
    }

    fn b() -> Buttons {
        Buttons { b: true, ..Default::default() }
    }

    #[test]
    fn left_and_right_move_the_cursor_without_wrapping() {
        let mut p = Pin::new("AA".into(), None, None);
        assert_eq!(p.cursor(), 0);
        p.update(&left());
        assert_eq!(p.cursor(), 0, "cursor stops at the first digit");
        p.update(&right());
        assert_eq!(p.cursor(), 1);
    }

    #[test]
    fn up_and_down_wrap_the_digit() {
        let mut p = Pin::new("AA".into(), None, None);
        p.update(&down());
        assert_eq!(p.digits()[0], 9, "0 down wraps to 9");
        p.update(&up());
        assert_eq!(p.digits()[0], 0);
    }

    #[test]
    fn a_submits_all_four_digits_in_order() {
        let mut p = Pin::new("AA".into(), None, None);
        p.update(&up()); // 1
        p.update(&right());
        p.update(&up());
        p.update(&up()); // 2
        p.update(&right());
        p.update(&right());
        assert_eq!(p.update(&a()), PinAction::Submit("1200".into()));
    }

    #[test]
    fn b_goes_back() {
        let mut p = Pin::new("AA".into(), None, None);
        assert_eq!(p.update(&b()), PinAction::Back);
    }

    #[test]
    fn cursor_does_not_overshoot_the_last_digit() {
        let mut p = Pin::new("AA".into(), None, None);
        for _ in 0..10 {
            p.update(&right());
        }
        assert_eq!(p.cursor(), 3);
    }

    #[test]
    fn editing_clears_a_previously_set_error() {
        let mut p = Pin::new("AA".into(), None, None);
        p.set_error("PIN rejected");
        p.update(&up());
        assert!(p.error.is_none());
    }

    #[test]
    fn client_id_is_reported_back_for_submit_pin() {
        let p = Pin::new("client-123".into(), Some("phone".into()), None);
        assert_eq!(p.client_id(), "client-123");
    }

    #[test]
    fn label_falls_back_to_a_truncated_id_without_a_name() {
        let p = Pin::new("A1B2C3D4E5F60718".into(), None, None);
        assert_eq!(p.label(), "A1B2C3D4");
    }

    #[test]
    fn label_prefers_the_name_when_present() {
        let p = Pin::new("A1B2C3D4E5F60718".into(), Some("phone".into()), None);
        assert_eq!(p.label(), "phone");
    }

    #[test]
    fn label_falls_back_to_the_address_without_a_name() {
        let p = Pin::new("A1B2C3D4E5F60718".into(), None, Some("192.168.180.44".into()));
        assert_eq!(p.label(), "192.168.180.44");
    }
}
