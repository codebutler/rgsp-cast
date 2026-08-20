//! Screens: pure input/state logic (unit-testable off-device) plus a thin
//! `draw` method that is the only part touching [`crate::ui::Ui`].

pub mod home;
pub mod pin;

/// Label a client by its name, falling back to the first 8 characters of its
/// id when it has none.
///
/// Every real Moonlight client hardcodes `devicename=roth`, which the daemon
/// normalizes to `None` (see `normalize_devicename` in `rgsp-host`), so the
/// id fallback is not an edge case -- it is what every pairing shows. Ids are
/// 16 hex characters; 8 is still enough to tell two clients pairing at once
/// apart, which is the only case the label has to disambiguate anything.
/// Shared by [`home`] and [`pin`] so the two screens can't drift apart.
///
/// Truncates on a character boundary (`chars().take(8)`, not `&id[..8]`):
/// ids are ASCII hex in practice, but a byte-index slice would panic on
/// anything that isn't. Ids shorter than 8 characters are returned intact.
pub fn client_label(name: Option<&str>, id: &str) -> String {
    match name {
        Some(name) => name.to_string(),
        None => id.chars().take(8).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_id_truncates_to_eight_characters() {
        assert_eq!(client_label(None, "A1B2C3D4E5F60718"), "A1B2C3D4");
    }

    #[test]
    fn short_id_is_returned_intact() {
        assert_eq!(client_label(None, "AB"), "AB");
    }

    #[test]
    fn a_present_name_always_wins_over_the_id() {
        assert_eq!(client_label(Some("eric-mbp"), "A1B2C3D4E5F60718"), "eric-mbp");
    }
}
