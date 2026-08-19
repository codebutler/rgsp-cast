//! A virtual gamepad, injected through `/dev/uinput`.
//!
//! Input from the client arrives on the control stream and has to reach the
//! emulator somehow. The emulator reads evdev, so the host creates a second
//! input device that looks like the handheld's own and feeds events into it.
//!
//! # Why it mirrors `ANBERNIC-keys` exactly
//!
//! The handheld's controls are `gpio-keys-polled` under the name
//! `ANBERNIC-keys` (`event1`, also `js0`). Its capabilities, decoded from
//! `/proc/bus/input/devices`:
//!
//! ```text
//! KEY: 1 (ESC), 114/115 (volume), 304-312, 314, 315, 354 (GOTO)
//! ABS: 3,4,5 (RX/RY/RZ), 16,17 (HAT0X/HAT0Y)
//! ```
//!
//! This device advertises the same set, so anything that maps the real pad by
//! key code treats this one identically and needs no per-device configuration.
//! Note what is *absent* on this hardware: no `BTN_TR2` (313) to pair with
//! `BTN_TL2` (312), and no `BTN_MODE` (316).
//!
//! # Lifetime
//!
//! Created once when the daemon starts, not per session. SDL enumerates input
//! devices when it initialises, so a device that appears after a game has
//! launched may never be noticed by that game. Existing for the daemon's whole
//! life is what makes it visible to whatever launches next.

use anyhow::{Context, Result};
use std::fs::{File, OpenOptions};
use std::os::unix::io::AsRawFd;

// linux/input.h
const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_ABS: u16 = 0x03;
const SYN_REPORT: u16 = 0;

// linux/uinput.h
const UINPUT_MAX_NAME_SIZE: usize = 80;
const UI_SET_EVBIT: libc::c_ulong = 0x4004_5564;
const UI_SET_KEYBIT: libc::c_ulong = 0x4004_5565;
const UI_SET_ABSBIT: libc::c_ulong = 0x4004_5567;
const UI_DEV_CREATE: libc::c_ulong = 0x5501;
const UI_DEV_DESTROY: libc::c_ulong = 0x5502;

/// Every key code the real pad reports, in its order.
const KEYS: &[u16] = &[
    1,   // KEY_ESC
    114, // KEY_VOLUMEDOWN
    115, // KEY_VOLUMEUP
    304, // BTN_SOUTH  (A)
    305, // BTN_EAST   (B)
    306, // BTN_C
    307, // BTN_NORTH  (X)
    308, // BTN_WEST   (Y)
    309, // BTN_Z
    310, // BTN_TL     (L1)
    311, // BTN_TR     (R1)
    312, // BTN_TL2    (L2)
    314, // BTN_SELECT
    315, // BTN_START
    354, // KEY_GOTO
];

/// Absolute axes: the d-pad hat, and the analog axes the hardware exposes.
/// `(code, min, max)` — the hat is a three-state -1/0/1 like the real one.
const AXES: &[(u16, i32, i32)] = &[
    (3, -32768, 32767), // ABS_RX
    (4, -32768, 32767), // ABS_RY
    (5, 0, 255),        // ABS_RZ
    (16, -1, 1),        // ABS_HAT0X
    (17, -1, 1),        // ABS_HAT0Y
];

#[repr(C)]
#[derive(Clone, Copy)]
struct InputId {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

#[repr(C)]
struct UinputUserDev {
    name: [u8; UINPUT_MAX_NAME_SIZE],
    id: InputId,
    ff_effects_max: u32,
    absmax: [i32; 64],
    absmin: [i32; 64],
    absfuzz: [i32; 64],
    absflat: [i32; 64],
}

#[repr(C)]
struct InputEvent {
    time: libc::timeval,
    type_: u16,
    code: u16,
    value: i32,
}

/// The buttons and axes a client can be holding at one instant.
///
/// Sent whole rather than as deltas: the protocol reports absolute state on
/// every packet, and diffing against the previous state is what turns that
/// into the press/release edges evdev wants.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub struct PadState {
    /// Key codes currently held, as a bitmask over `KEYS` by index.
    pub keys: u32,
    pub hat_x: i32,
    pub hat_y: i32,
    pub rx: i32,
    pub ry: i32,
    pub rz: i32,
}

impl PadState {
    pub fn set_key(&mut self, code: u16, down: bool) {
        if let Some(i) = KEYS.iter().position(|&k| k == code) {
            if down {
                self.keys |= 1 << i;
            } else {
                self.keys &= !(1 << i);
            }
        }
    }

    fn key_down(&self, index: usize) -> bool {
        self.keys >> index & 1 == 1
    }
}

/// A uinput device that stays alive as long as this value does.
pub struct VirtualPad {
    file: File,
    last: PadState,
}

impl VirtualPad {
    pub fn open() -> Result<VirtualPad> {
        let file = OpenOptions::new()
            .write(true)
            .open("/dev/uinput")
            .context("opening /dev/uinput (needs root and CONFIG_INPUT_UINPUT)")?;
        let fd = file.as_raw_fd();

        // Declare which event types and codes this device can report. The
        // kernel rejects any event whose bit was not set here, silently.
        unsafe {
            ioctl_or_err(fd, UI_SET_EVBIT, EV_KEY as libc::c_ulong, "UI_SET_EVBIT(EV_KEY)")?;
            ioctl_or_err(fd, UI_SET_EVBIT, EV_ABS as libc::c_ulong, "UI_SET_EVBIT(EV_ABS)")?;
            ioctl_or_err(fd, UI_SET_EVBIT, EV_SYN as libc::c_ulong, "UI_SET_EVBIT(EV_SYN)")?;
            for &key in KEYS {
                ioctl_or_err(fd, UI_SET_KEYBIT, key as libc::c_ulong, "UI_SET_KEYBIT")?;
            }
            for &(axis, _, _) in AXES {
                ioctl_or_err(fd, UI_SET_ABSBIT, axis as libc::c_ulong, "UI_SET_ABSBIT")?;
            }
        }

        let mut dev: UinputUserDev = unsafe { std::mem::zeroed() };
        let name = b"rgsp-cast remote pad";
        dev.name[..name.len()].copy_from_slice(name);
        dev.id = InputId {
            bustype: 0x03, // BUS_USB - what SDL expects of a gamepad
            vendor: 0x1209,
            product: 0x5350,
            version: 1,
        };
        for &(axis, min, max) in AXES {
            dev.absmin[axis as usize] = min;
            dev.absmax[axis as usize] = max;
        }

        // The device description is written, not ioctl'd, on this kernel's
        // uinput ABI; UI_DEV_CREATE then instantiates it.
        let bytes = unsafe {
            std::slice::from_raw_parts(
                &dev as *const UinputUserDev as *const u8,
                std::mem::size_of::<UinputUserDev>(),
            )
        };
        write_all(fd, bytes).context("writing uinput device description")?;
        unsafe { ioctl_or_err(fd, UI_DEV_CREATE, 0, "UI_DEV_CREATE")? };

        tracing::info!("virtual gamepad created");
        Ok(VirtualPad { file, last: PadState::default() })
    }

    /// Emit only what changed since the last call, then a SYN_REPORT.
    ///
    /// evdev consumers treat a report as one atomic update, so every changed
    /// code must land before the SYN, and an unchanged code must not be
    /// repeated - a repeated press is indistinguishable from a new one to
    /// anything counting edges.
    pub fn apply(&mut self, state: PadState) -> Result<()> {
        if state == self.last {
            return Ok(());
        }
        let mut events: Vec<InputEvent> = Vec::new();
        for (i, &code) in KEYS.iter().enumerate() {
            let now = state.key_down(i);
            if now != self.last.key_down(i) {
                events.push(event(EV_KEY, code, now as i32));
            }
        }
        for (code, now, before) in [
            (16u16, state.hat_x, self.last.hat_x),
            (17, state.hat_y, self.last.hat_y),
            (3, state.rx, self.last.rx),
            (4, state.ry, self.last.ry),
            (5, state.rz, self.last.rz),
        ] {
            if now != before {
                events.push(event(EV_ABS, code, now));
            }
        }
        events.push(event(EV_SYN, SYN_REPORT, 0));

        let bytes = unsafe {
            std::slice::from_raw_parts(
                events.as_ptr() as *const u8,
                std::mem::size_of_val(&events[..]),
            )
        };
        write_all(self.file.as_raw_fd(), bytes).context("writing input events")?;
        self.last = state;
        Ok(())
    }

    /// Release everything. Called when a session ends so a client that
    /// disconnects mid-press cannot leave a button stuck down forever.
    pub fn release_all(&mut self) -> Result<()> {
        self.apply(PadState::default())
    }
}

impl Drop for VirtualPad {
    fn drop(&mut self) {
        let _ = self.release_all();
        unsafe {
            libc::ioctl(self.file.as_raw_fd(), UI_DEV_DESTROY as _);
        }
    }
}

fn event(type_: u16, code: u16, value: i32) -> InputEvent {
    InputEvent {
        time: libc::timeval { tv_sec: 0, tv_usec: 0 },
        type_,
        code,
        value,
    }
}

unsafe fn ioctl_or_err(
    fd: libc::c_int,
    request: libc::c_ulong,
    arg: libc::c_ulong,
    what: &str,
) -> Result<()> {
    if unsafe { libc::ioctl(fd, request as _, arg) } < 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| what.to_string());
    }
    Ok(())
}

fn write_all(fd: libc::c_int, mut buf: &[u8]) -> std::io::Result<()> {
    while !buf.is_empty() {
        let n = unsafe { libc::write(fd, buf.as_ptr() as *const libc::c_void, buf.len()) };
        if n < 0 {
            return Err(std::io::Error::last_os_error());
        }
        buf = &buf[n as usize..];
    }
    Ok(())
}
