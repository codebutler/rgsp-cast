//! Screens: pure input/state logic (unit-testable off-device) plus a thin
//! `draw` method that is the only part touching [`crate::ui::Ui`].

pub mod confirm;
pub mod home;
pub mod message;
pub mod pairing;
pub mod pin;
pub mod unpairing;

/// Label a client for the pairing UI: a real name if one ever arrives,
/// otherwise its IP address, otherwise the first 8 characters of its id.
///
/// Every real Moonlight client hardcodes `devicename=roth`, which the daemon
/// normalizes to `None` (see `normalize_devicename` in `rgsp-host`), so the
/// name is essentially never present in practice. Many clients also hardcode
/// `uniqueid=0123456789ABCDEF` unless a build opts into a true unique id, so
/// the id fallback can be identical across every client pairing at once. The
/// peer's IP address doesn't have either problem -- on a home LAN it is
/// genuinely identifying -- so it sits ahead of the id in the fallback
/// order. Ids are 16 hex characters; 8 is still enough to tell two same-id
/// clients apart when even the address is unknown. Shared by [`home`] and
/// [`pin`] so the two screens can't drift apart.
///
/// Truncates on a character boundary (`chars().take(8)`, not `&id[..8]`):
/// ids are ASCII hex in practice, but a byte-index slice would panic on
/// anything that isn't. Ids shorter than 8 characters are returned intact.
pub fn client_label(name: Option<&str>, address: Option<&str>, id: &str) -> String {
    match name {
        Some(name) => name.to_string(),
        None => match address {
            Some(address) => address.to_string(),
            None => id.chars().take(8).collect(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_id_truncates_to_eight_characters() {
        assert_eq!(client_label(None, None, "A1B2C3D4E5F60718"), "A1B2C3D4");
    }

    #[test]
    fn short_id_is_returned_intact() {
        assert_eq!(client_label(None, None, "AB"), "AB");
    }

    #[test]
    fn a_present_name_always_wins_over_the_address_and_id() {
        assert_eq!(
            client_label(Some("eric-mbp"), Some("192.168.180.44"), "A1B2C3D4E5F60718"),
            "eric-mbp"
        );
    }

    #[test]
    fn no_name_falls_back_to_the_address() {
        assert_eq!(
            client_label(None, Some("192.168.180.44"), "A1B2C3D4E5F60718"),
            "192.168.180.44"
        );
    }

    #[test]
    fn no_name_or_address_falls_back_to_the_truncated_id() {
        assert_eq!(client_label(None, None, "A1B2C3D4E5F60718"), "A1B2C3D4");
    }
}
