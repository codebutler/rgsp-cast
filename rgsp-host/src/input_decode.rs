//! Decoding the client's input packets into a [`PadState`].
//!
//! Packet layouts and button flags are from moonlight-common-c (`src/Input.h`,
//! `src/Limelight.h`) - the client half every Moonlight build shares, so it is
//! the definition of what arrives here.
//!
//! Only the controller packets are decoded. The handheld has no pointer and no
//! keyboard, so mouse, scroll, pen and text events have nowhere sensible to go
//! and are ignored rather than guessed at.

use crate::input::PadState;

// Packet magics (Input.h). Little-endian on the wire.
const CONTROLLER_MAGIC: u32 = 0x0000_000A;
const MULTI_CONTROLLER_MAGIC: u32 = 0x0000_000D;
const MULTI_CONTROLLER_MAGIC_GEN5: u32 = 0x0000_000C;

// Button flags (Limelight.h).
const UP_FLAG: u16 = 0x0001;
const DOWN_FLAG: u16 = 0x0002;
const LEFT_FLAG: u16 = 0x0004;
const RIGHT_FLAG: u16 = 0x0008;
const PLAY_FLAG: u16 = 0x0010; // Start
const BACK_FLAG: u16 = 0x0020; // Select
const LB_FLAG: u16 = 0x0100;
const RB_FLAG: u16 = 0x0200;
const SPECIAL_FLAG: u16 = 0x0400; // Guide
const A_FLAG: u16 = 0x1000;
const B_FLAG: u16 = 0x2000;
const X_FLAG: u16 = 0x4000;
const Y_FLAG: u16 = 0x8000;

// evdev codes this hardware reports. See `input::KEYS` for the full set and
// where it comes from.
const BTN_SOUTH: u16 = 304;
const BTN_EAST: u16 = 305;
const BTN_NORTH: u16 = 307;
const BTN_WEST: u16 = 308;
const BTN_TL: u16 = 310;
const BTN_TR: u16 = 311;
const BTN_TL2: u16 = 312;
const BTN_SELECT: u16 = 314;
const BTN_START: u16 = 315;
const KEY_GOTO: u16 = 354;

/// Where each Moonlight button lands on this device.
///
/// Two notes on the awkward ones. The hardware exposes `BTN_TL2` (312) but no
/// `BTN_TR2` (313), so an analog right trigger has no matching code; it is
/// reported on `ABS_RZ` instead, which the hardware does have. And there is no
/// `BTN_MODE` (316), so Guide maps to `KEY_GOTO` - the code this device uses
/// for its own menu button.
const BUTTON_MAP: &[(u16, u16)] = &[
    (A_FLAG, BTN_SOUTH),
    (B_FLAG, BTN_EAST),
    (X_FLAG, BTN_NORTH),
    (Y_FLAG, BTN_WEST),
    (LB_FLAG, BTN_TL),
    (RB_FLAG, BTN_TR),
    (BACK_FLAG, BTN_SELECT),
    (PLAY_FLAG, BTN_START),
    (SPECIAL_FLAG, KEY_GOTO),
];

fn u32_le(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn u16_le(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

// Keyboard packets (Input.h). The payload carries Windows virtual-key codes.
const KEY_DOWN_MAGIC: u32 = 0x0000_0003;
const KEY_UP_MAGIC: u32 = 0x0000_0004;

/// Keyboard fallback, so a client with no gamepad can still drive the handheld.
///
/// The layout is the familiar emulator one: arrows for the d-pad, Z/X for A/B,
/// A/S for the shoulders, Enter/Backspace for Start/Select. Virtual-key codes
/// on the left, this device's evdev codes on the right; the four arrows are
/// handled separately because they drive the hat rather than a key.
const KEYBOARD_MAP: &[(u16, u16)] = &[
    (0x5A, BTN_SOUTH),  // Z -> A
    (0x58, BTN_EAST),   // X -> B
    (0x41, BTN_NORTH),  // A -> X
    (0x53, BTN_WEST),   // S -> Y
    (0x51, BTN_TL),     // Q -> L1
    (0x57, BTN_TR),     // W -> R1
    (0x0D, BTN_START),  // Enter
    (0x08, BTN_SELECT), // Backspace
    (0x1B, KEY_GOTO),   // Escape -> menu
];

const VK_LEFT: u16 = 0x25;
const VK_UP: u16 = 0x26;
const VK_RIGHT: u16 = 0x27;
const VK_DOWN: u16 = 0x28;

/// Apply one keyboard event. Unlike the controller packets, these are edges -
/// a key down or a key up for a single code - so the hat is tracked as two
/// independent held flags rather than recomputed from a bitmask.
fn apply_keyboard(payload: &[u8], down: bool, state: &mut PadState) -> bool {
    // magic(4), flags(1), keyCode(2, little-endian), modifiers(1), zero2(2),
    // packed with no padding.
    let Some(raw) = u16_le(payload, 5) else {
        return false;
    };

    // Clients set a 0x80 flag in the high byte: the down arrow arrives as
    // 0x8028, not 0x0028. Win32 virtual-key codes are a single byte, so mask
    // it off - Sunshine does the same (`packet->keyCode & 0x00FF`,
    // src/input.cpp). Without this nothing ever matches and every key is
    // silently ignored.
    let key = raw & 0x00FF;

    for &(vk, code) in KEYBOARD_MAP {
        if vk == key {
            state.set_key(code, down);
            return true;
        }
    }

    match key {
        VK_LEFT => state.hat_x = if down { -1 } else { 0 },
        VK_RIGHT => state.hat_x = if down { 1 } else { 0 },
        VK_UP => state.hat_y = if down { -1 } else { 0 },
        VK_DOWN => state.hat_y = if down { 1 } else { 0 },
        _ => return false,
    }
    true
}

/// Decode one input payload, updating `state` in place.
///
/// Returns `false` for packets that are not controller input, so the caller can
/// tell "nothing to apply" from "the pad changed".
pub fn apply_packet(payload: &[u8], state: &mut PadState) -> bool {
    let Some(magic) = u32_le(payload, 0) else {
        return false;
    };

    // Field offsets differ between the two controller packet shapes: the
    // multi-controller form carries header/controller-number/active-mask
    // fields ahead of the buttons that the original does not.
    match magic {
        KEY_DOWN_MAGIC => return apply_keyboard(payload, true, state),
        KEY_UP_MAGIC => return apply_keyboard(payload, false, state),
        _ => {},
    }

    // 0x0D is ambiguous: it is both MULTI_CONTROLLER_MAGIC and
    // ENABLE_HAPTICS_MAGIC. Only the length separates them - a controller
    // packet is 24 bytes, a haptics packet 6 - so check it before reading
    // fields at controller offsets.
    const MULTI_CONTROLLER_LEN: usize = 24;
    if magic == MULTI_CONTROLLER_MAGIC && payload.len() < MULTI_CONTROLLER_LEN {
        return false; // haptics enable, not controller state
    }

    let buttons_at = match magic {
        MULTI_CONTROLLER_MAGIC | MULTI_CONTROLLER_MAGIC_GEN5 => 12,
        // NV_CONTROLLER_PACKET puts a headerB short before the buttons; the
        // multi-controller form has three more fields there instead.
        CONTROLLER_MAGIC => 6,
        _ => return false,
    };

    let Some(flags) = u16_le(payload, buttons_at) else {
        return false;
    };

    for &(flag, code) in BUTTON_MAP {
        state.set_key(code, flags & flag != 0);
    }

    // The d-pad is a hat on this hardware, not four buttons. Opposing
    // directions held together cancel, which is what a physical hat does.
    state.hat_x = match (flags & LEFT_FLAG != 0, flags & RIGHT_FLAG != 0) {
        (true, false) => -1,
        (false, true) => 1,
        _ => 0,
    };
    state.hat_y = match (flags & UP_FLAG != 0, flags & DOWN_FLAG != 0) {
        (true, false) => -1,
        (false, true) => 1,
        _ => 0,
    };

    // Triggers follow the buttons: two bytes, then the four stick axes.
    let after_buttons = buttons_at + 2;
    if let (Some(&left), Some(&right)) =
        (payload.get(after_buttons), payload.get(after_buttons + 1))
    {
        // Left trigger has a digital code to land on; the right one does not,
        // so it drives ABS_RZ (0..255), the range the hardware advertises.
        state.set_key(BTN_TL2, left > 127);
        state.rz = right as i32;
    }
    if let (Some(x), Some(y)) = (
        u16_le(payload, after_buttons + 2),
        u16_le(payload, after_buttons + 4),
    ) {
        state.rx = x as i16 as i32;
        state.ry = y as i16 as i32;
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a multi-controller packet with the given flags, triggers and
    /// left-stick position, laid out per NV_MULTI_CONTROLLER_PACKET.
    fn packet(flags: u16, left_trigger: u8, right_trigger: u8, x: i16, y: i16) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&MULTI_CONTROLLER_MAGIC.to_le_bytes()); // 0
        p.extend_from_slice(&0x001Au16.to_le_bytes()); // 4  headerB
        p.extend_from_slice(&0u16.to_le_bytes()); // 6  controllerNumber
        p.extend_from_slice(&1u16.to_le_bytes()); // 8  activeGamepadMask
        p.extend_from_slice(&0x0014u16.to_le_bytes()); // 10 midB
        p.extend_from_slice(&flags.to_le_bytes()); // 12 buttonFlags
        p.push(left_trigger); // 14
        p.push(right_trigger); // 15
        p.extend_from_slice(&x.to_le_bytes()); // 16 leftStickX
        p.extend_from_slice(&y.to_le_bytes()); // 18 leftStickY
        p.extend_from_slice(&0i16.to_le_bytes()); // 20 rightStickX
        p.extend_from_slice(&0i16.to_le_bytes()); // 22 rightStickY
        p
    }

    #[test]
    fn a_button_maps_to_btn_south() {
        let mut state = PadState::default();
        assert!(apply_packet(&packet(A_FLAG, 0, 0, 0, 0), &mut state));

        let mut expected = PadState::default();
        expected.set_key(BTN_SOUTH, true);
        assert_eq!(state.keys, expected.keys, "A should press BTN_SOUTH and nothing else");
    }

    #[test]
    fn releasing_clears_the_button() {
        let mut state = PadState::default();
        apply_packet(&packet(A_FLAG, 0, 0, 0, 0), &mut state);
        apply_packet(&packet(0, 0, 0, 0, 0), &mut state);
        assert_eq!(state.keys, 0, "an empty packet must release everything");
    }

    #[test]
    fn dpad_becomes_a_hat_and_opposites_cancel() {
        let mut state = PadState::default();

        apply_packet(&packet(LEFT_FLAG, 0, 0, 0, 0), &mut state);
        assert_eq!((state.hat_x, state.hat_y), (-1, 0));

        apply_packet(&packet(DOWN_FLAG, 0, 0, 0, 0), &mut state);
        assert_eq!((state.hat_x, state.hat_y), (0, 1), "left released, down held");

        apply_packet(&packet(LEFT_FLAG | RIGHT_FLAG, 0, 0, 0, 0), &mut state);
        assert_eq!(state.hat_x, 0, "opposing directions cancel, as on a physical hat");
    }

    #[test]
    fn triggers_and_sticks_are_read_from_the_right_offsets() {
        let mut state = PadState::default();
        apply_packet(&packet(0, 255, 200, -20000, 15000), &mut state);

        let mut with_l2 = PadState::default();
        with_l2.set_key(BTN_TL2, true);
        assert_eq!(state.keys, with_l2.keys, "a full left trigger presses BTN_TL2");
        assert_eq!(state.rz, 200, "the right trigger drives ABS_RZ");
        assert_eq!((state.rx, state.ry), (-20000, 15000), "sticks keep their sign");
    }

    /// magic(4), flags(1), keyCode(2), modifiers(1), zero2(2), packed.
    fn key_packet(magic: u32, vk: u16) -> Vec<u8> {
        let mut p = magic.to_le_bytes().to_vec();
        p.push(0); // flags
        p.extend_from_slice(&vk.to_le_bytes());
        p.push(0); // modifiers
        p.extend_from_slice(&0u16.to_le_bytes());
        p
    }

    #[test]
    fn keyboard_z_presses_a_and_releases_it() {
        let mut state = PadState::default();
        assert!(apply_packet(&key_packet(KEY_DOWN_MAGIC, 0x5A), &mut state));
        let mut expected = PadState::default();
        expected.set_key(BTN_SOUTH, true);
        assert_eq!(state.keys, expected.keys, "Z should press BTN_SOUTH");

        assert!(apply_packet(&key_packet(KEY_UP_MAGIC, 0x5A), &mut state));
        assert_eq!(state.keys, 0, "releasing Z must release BTN_SOUTH");
    }

    #[test]
    fn arrow_keys_drive_the_hat() {
        let mut state = PadState::default();

        apply_packet(&key_packet(KEY_DOWN_MAGIC, VK_LEFT), &mut state);
        assert_eq!(state.hat_x, -1, "left arrow moves the hat left");

        apply_packet(&key_packet(KEY_UP_MAGIC, VK_LEFT), &mut state);
        assert_eq!(state.hat_x, 0, "releasing it centres the hat");

        apply_packet(&key_packet(KEY_DOWN_MAGIC, VK_DOWN), &mut state);
        assert_eq!((state.hat_x, state.hat_y), (0, 1));
    }

    #[test]
    fn an_unmapped_key_changes_nothing() {
        let mut state = PadState::default();
        state.set_key(BTN_SOUTH, true);
        let before = state;
        assert!(!apply_packet(&key_packet(KEY_DOWN_MAGIC, 0x70), &mut state), "F1 is unmapped");
        assert_eq!(state.keys, before.keys);
    }

    #[test]
    fn legacy_controller_packet_reads_buttons_after_headerb() {
        // NV_CONTROLLER_PACKET: magic(4), headerB(2), buttonFlags(2), ...
        let mut p = CONTROLLER_MAGIC.to_le_bytes().to_vec();
        p.extend_from_slice(&0x1400u16.to_le_bytes());
        p.extend_from_slice(&A_FLAG.to_le_bytes());
        p.extend_from_slice(&[0; 12]);

        let mut state = PadState::default();
        assert!(apply_packet(&p, &mut state));
        let mut expected = PadState::default();
        expected.set_key(BTN_SOUTH, true);
        assert_eq!(state.keys, expected.keys, "buttons sit at offset 6, not 4");
    }

    #[test]
    fn a_haptics_packet_is_not_mistaken_for_controller_state() {
        // ENABLE_HAPTICS_MAGIC shares 0x0D with MULTI_CONTROLLER_MAGIC and is
        // 6 bytes; misreading it would inject whatever lies at offset 12.
        let mut haptics = MULTI_CONTROLLER_MAGIC.to_le_bytes().to_vec();
        haptics.extend_from_slice(&1u16.to_le_bytes());
        assert_eq!(haptics.len(), 6);

        let mut state = PadState::default();
        state.set_key(BTN_SOUTH, true);
        let before = state;
        assert!(!apply_packet(&haptics, &mut state));
        assert_eq!(state.keys, before.keys, "haptics must not disturb the pad");
    }

    #[test]
    fn the_high_flag_byte_is_masked_off() {
        // Real capture from moonlight-qt: down arrow arrives as 0x8028.
        let raw: [u8; 10] = [0x03, 0, 0, 0, 0, 0x28, 0x80, 0, 0, 0];
        let mut state = PadState::default();
        assert!(apply_packet(&raw, &mut state), "0x8028 must decode as VK_DOWN");
        assert_eq!(state.hat_y, 1, "down arrow presses the hat down");

        // And 'A' as 0x8041.
        let raw_a: [u8; 10] = [0x03, 0, 0, 0, 0, 0x41, 0x80, 0, 0, 0];
        let mut state = PadState::default();
        assert!(apply_packet(&raw_a, &mut state));
        let mut expected = PadState::default();
        expected.set_key(BTN_NORTH, true);
        assert_eq!(state.keys, expected.keys, "'A' maps to BTN_NORTH");
    }

    #[test]
    fn non_controller_packets_are_ignored() {
        let mut state = PadState::default();
        state.set_key(BTN_SOUTH, true);
        let before = state;

        // A mouse-move packet (MOUSE_MOVE_REL_MAGIC) must not touch the pad.
        let mut mouse = 0x0000_0006u32.to_le_bytes().to_vec();
        mouse.extend_from_slice(&[0; 8]);
        assert!(!apply_packet(&mouse, &mut state));
        assert_eq!(state.keys, before.keys);
    }

    #[test]
    fn a_truncated_packet_is_rejected_rather_than_read_past() {
        let mut state = PadState::default();
        assert!(!apply_packet(&[], &mut state));
        assert!(!apply_packet(&MULTI_CONTROLLER_MAGIC.to_le_bytes(), &mut state));
    }
}
