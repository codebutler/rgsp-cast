//! Safe drawing and input wrapper over NextUI's C toolkit ([`crate::sys`]).
//!
//! `Ui::new()` performs the same init sequence Task 3's `main.rs` proved out.
//! `Drop for Ui` *is* the release mechanism, not a convenience wrapper around
//! an explicit teardown call: it runs on every exit path, including a panic
//! unwinding through this crate. That matters because a process that returns
//! while something still holds `/dev/fb0` corrupts the console's launcher
//! until reboot — this feature exists because of a bug in exactly that class.

use std::ffi::CString;
use std::os::raw::c_char;

use sdl2_sys::{SDL_Color, SDL_Rect, SDL_UpperBlit};

use crate::sys;

const COLOR_WHITE: SDL_Color = SDL_Color { r: 0xff, g: 0xff, b: 0xff, a: 0 };
const COLOR_BLACK: SDL_Color = SDL_Color { r: 0x00, g: 0x00, b: 0x00, a: 0 };

/// NextUI's `PADDING` (defines.h), unscaled. Not bound from C: h700's
/// platform.h redefines it as `(hdmi_active||is_cube)?5:10`, a runtime
/// expression like `FIXED_WIDTH`/`FIXED_HEIGHT` (see task-3-context.md), so
/// bindgen silently drops it rather than emit a wrong constant.
///
/// `10` is provably the only value it can take here: `Ui::new()` (below)
/// already rejects any surface that is not 720x480, and per
/// `h700/platform.h:162-163`, `FIXED_WIDTH`/`FIXED_HEIGHT` land on 720x480
/// only when `hdmi_active == false && is_cube == false` — HDMI forces
/// 1280x720, cube forces 720x720, neither of which is 720x480. That rules
/// out both disjuncts of `PADDING`'s condition, so `PADDING == 10` for every
/// `Ui` that exists.
const PADDING: i32 = 10;

/// NextUI clock.c's digit-atlas cell size, unscaled (`clock.c:31-32`). Ours:
/// the PIN entry is not a stock NextUI widget, but the owner asked for the
/// same number-entry UI as clock's date/time fields, and this is the cell
/// every digit there is centred and monospaced within.
const DIGIT_WIDTH: i32 = 10;
const DIGIT_HEIGHT: i32 = 16;

/// Vertical gap from the digit row down to its cursor underline, unscaled
/// (`clock.c:303`: `y += SCALE1(19);`, applied after the digit row's own y).
const CURSOR_GAP: i32 = 19;

/// Which of the six navigation/action buttons fired *this frame*: `up`,
/// `down`, `left`, `right` from `PAD_justRepeated`, `a`/`b` from
/// `PAD_justPressed` — see [`Ui::poll`] for why they differ. Neither is
/// `PAD_isPressed`: a mask built from `isPressed` would fire every single
/// frame a key is held, not once per press (or once per repeat interval).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Buttons {
    pub up: bool,
    pub down: bool,
    pub left: bool,
    pub right: bool,
    pub a: bool,
    pub b: bool,
}

impl Buttons {
    fn from_mask(mask: i32) -> Self {
        use crate::sys::*;
        Self {
            up: mask & BTN_DPAD_UP as i32 != 0,
            down: mask & BTN_DPAD_DOWN as i32 != 0,
            left: mask & BTN_DPAD_LEFT as i32 != 0,
            right: mask & BTN_DPAD_RIGHT as i32 != 0,
            a: mask & BTN_A as i32 != 0,
            b: mask & BTN_B as i32 != 0,
        }
    }
}

/// Vertical position of row `index`, in scaled pixels. A free function, not
/// a method, so it stays unit-testable without an open display.
///
/// Offset by `scale1(PADDING)` below the header band: `header()` draws its
/// title at `y = scale1(PADDING)`, and `Ui::hardware_group()` (the
/// battery/wifi/bt status pill every screen now draws) occupies the same
/// `scale1(PADDING)..scale1(PADDING) + scale1(PILL_SIZE)` band. Without the
/// offset, row 0 started at `scale1(PILL_SIZE)` — 20px inside that band —
/// and its pill visibly overlapped both on hardware.
///
/// `nextui.c`'s own file browser positions its rows the same way,
/// `SCALE1(row_index * PILL_SIZE + PADDING)`, but that screen has no
/// separate header band to clear — its row 0 sits at `scale1(PADDING)`
/// (20px), level with its own hardware group, because that list is the
/// only content on the screen. Ours is not: `header()`'s title text
/// occupies the same band `hardware_group()` does, so row 0 has to clear
/// a full `scale1(PADDING) + scale1(PILL_SIZE)` (80px) before it, one
/// `scale1(PADDING)` further down than that reference.
fn row_y(index: i32) -> i32 {
    sys::scale1(PADDING) + sys::scale1(sys::PILL_SIZE as i32) * (index + 1)
}

/// The open NextUI display, and everything acquired to open it.
///
/// Every field the constructor can fail after setting is tracked, so `Drop`
/// releases exactly what was acquired — including from a panic unwinding out
/// of a partially built `Ui`, and including the case where `GFX_init` itself
/// fails and there is no display to release.
pub struct Ui {
    screen: *mut sdl2_sys::SDL_Surface,
    w: i32,
    h: i32,
    input_inited: bool,
    pwr_inited: bool,
    settings_inited: bool,
}

impl Ui {
    /// Open the display, matching Task 3's proven init order
    /// (`workspace/all/clock/clock.c`): `InitSettings`, `GFX_init`,
    /// `PLAT_initInput`, `PWR_init`.
    ///
    /// Fails loud rather than silently drawing at the wrong size. On h700,
    /// panel geometry is a runtime expression keyed off the `DEVICE` and
    /// `RGXX_MODEL` environment variables (see `platform.c`'s `is_rgsp`),
    /// and running without them produces a plausible-looking 640x480
    /// surface on this device's 720x480 panel.
    pub fn new() -> anyhow::Result<Self> {
        // SAFETY: single-threaded startup. `ui` records each step that
        // succeeds as it goes, so an early return via `anyhow::ensure!`
        // drops a `Ui` that only tears down what it actually acquired.
        unsafe {
            sys::InitSettings();
        }
        let mut ui = Ui {
            screen: std::ptr::null_mut(),
            w: 0,
            h: 0,
            input_inited: false,
            pwr_inited: false,
            settings_inited: true,
        };

        // SAFETY: MODE_MENU is a real GFX_init mode constant; the result is
        // null-checked before any other use.
        let screen = unsafe { sys::GFX_init(sys::MODE_MENU as i32) };
        anyhow::ensure!(!screen.is_null(), "GFX_init returned null");
        ui.screen = screen;

        // SAFETY: screen is non-null, owned by C until GFX_quit (which Drop
        // calls last), and its w/h fields are populated by GFX_init itself.
        let (w, h) = unsafe { ((*screen).w, (*screen).h) };
        anyhow::ensure!(
            (w, h) == (720, 480),
            "panel reported {w}x{h}, expected 720x480 for the RGSP — set \
             DEVICE=rgsp and RGXX_MODEL=RGSP (and PLATFORM=h700) so \
             platform.c's is_rgsp picks the RGSP geometry instead of the \
             generic 640x480 default"
        );
        ui.w = w;
        ui.h = h;

        // SAFETY: screen is open; this is the next step in NextUI's init
        // order.
        unsafe {
            sys::PLAT_initInput();
        }
        ui.input_inited = true;

        // SAFETY: input is initialized; this is the last step in NextUI's
        // init order.
        unsafe {
            sys::PWR_init();
        }
        ui.pwr_inited = true;

        Ok(ui)
    }

    /// The panel size, in real (post-`FIXED_SCALE`) pixels. Always
    /// `(720, 480)` — `new()` refuses to return a `Ui` otherwise.
    pub fn size(&self) -> (i32, i32) {
        (self.w, self.h)
    }

    /// Buttons pressed since the last call.
    ///
    /// Direction and action read differently on purpose, matching clock.c's
    /// own number-entry loop (`clock.c:150,177,205,210` vs `:203,209`): the
    /// directions use `PAD_justRepeated` so holding one auto-repeats — the
    /// feel a user already knows from clock's date/time fields — while A/B
    /// stay on `PAD_justPressed`, since they are one-shot actions where
    /// repeating on a held button would be wrong (e.g. re-submitting a PIN
    /// every few frames while A is held).
    pub fn poll(&mut self) -> Buttons {
        // SAFETY: input was initialized in `new` and stays valid until Drop.
        unsafe {
            sys::PLAT_pollInput();
        }
        let mut mask = 0i32;
        for btn in [sys::BTN_DPAD_UP, sys::BTN_DPAD_DOWN, sys::BTN_DPAD_LEFT, sys::BTN_DPAD_RIGHT] {
            // SAFETY: PLAT_pollInput was just called; btn is a real BTN_*
            // bit, which is what PAD_justRepeated expects.
            if unsafe { sys::PAD_justRepeated(btn as i32) } != 0 {
                mask |= btn as i32;
            }
        }
        for btn in [sys::BTN_A, sys::BTN_B] {
            // SAFETY: PLAT_pollInput was just called; btn is a real BTN_*
            // bit, which is what PAD_justPressed expects.
            if unsafe { sys::PAD_justPressed(btn as i32) } != 0 {
                mask |= btn as i32;
            }
        }
        Buttons::from_mask(mask)
    }

    /// Clear the frame. Call once per frame before any drawing.
    pub fn begin(&mut self) {
        // SAFETY: screen is valid for the lifetime of `self`.
        unsafe {
            sys::PLAT_clearVideo(self.screen);
        }
    }

    /// Present the frame drawn since `begin`.
    pub fn end(&mut self) {
        // SAFETY: screen is valid for the lifetime of `self`.
        unsafe {
            sys::GFX_flip(self.screen);
        }
    }

    /// Draw the battery/WiFi/Bluetooth status pill every NextUI screen
    /// carries in its top-right corner (`GFX_blitHardwareGroup`,
    /// `api.h:402`). Call once per frame, right after `begin()` and before
    /// any other drawing — the same position `clock.c:254` uses.
    /// `show_setting = 0` is the idle case: `GFX_blitHardwareGroup`
    /// (`api.c:2300`) only switches to a brightness/volume/colortemp
    /// indicator when `show_setting` names one; `0` isn't a valid
    /// `IndicatorType`, so it falls through to the battery/wifi/bt/clock
    /// pill every screen shows outside of actively adjusting a hardware
    /// setting — which neither of ours ever does.
    pub fn hardware_group(&mut self) {
        // SAFETY: screen is valid for the lifetime of `self`.
        unsafe {
            sys::GFX_blitHardwareGroup(self.screen, 0);
        }
    }

    /// Draw a page title at the top of the screen.
    pub fn header(&mut self, title: &str) {
        let pad = sys::scale1(PADDING);
        self.blit_text(title, pad, pad, COLOR_WHITE);
    }

    /// The font's line height, in pixels. Used to centre text against a
    /// fixed band the way NextUI centres its own elements —
    /// `(container - element) / 2`, e.g. `ay = oy + (SCALE1(PILL_SIZE) -
    /// asset_rect.h) / 2` (`api.c:2253`) — except the "element" here is
    /// `TTF_FontHeight`, not a rendered surface's height: it is constant per
    /// font, so every row's baseline lines up whether or not a particular
    /// string happens to contain descenders.
    fn text_height(&self) -> i32 {
        // SAFETY: font.large is populated by GFX_init before any Ui exists.
        // TTF_FontHeight reads the font's metrics; it does not touch any
        // rendered surface.
        unsafe { sys::TTF_FontHeight(sys::font.large) }
    }

    /// Draw one menu row: a label, an optional right-aligned value, at
    /// vertical slot `index`, highlighted if `selected`.
    pub fn row(&mut self, label: &str, value: Option<&str>, index: i32, selected: bool) {
        let y = row_y(index);
        let pill_h = sys::scale1(sys::PILL_SIZE as i32);
        let pad = sys::scale1(PADDING);

        if selected {
            let mut rect = SDL_Rect { x: pad, y, w: self.w - 2 * pad, h: pill_h };
            // SAFETY: screen is valid; rect lies within the panel because
            // `new()` already rejected any surface that is not 720x480.
            unsafe {
                sys::GFX_blitPill(sys::ASSET_WHITE_PILL as i32, self.screen, &mut rect);
            }
        }

        let color = if selected { COLOR_BLACK } else { COLOR_WHITE };
        let text_y = y + (pill_h - self.text_height()) / 2;

        // Compute the value's placement (if any) before the label, so the
        // label can be truncated to the space actually left for it — a row
        // label is caller-supplied text (a device name, a peer address)
        // with no length guarantee, and NextUI's own row drawing never
        // lets a label run into whatever sits to its right.
        let value_x = value.map(|v| self.w - pad * 2 - self.text_width(v));
        let label_max_width = match value_x {
            Some(x) => x - pad - pad * 2,
            None => self.w - pad * 2 - pad * 2,
        };
        let label = self.truncate(label, label_max_width);
        self.blit_text(&label, pad * 2, text_y, color);

        if let (Some(value), Some(x)) = (value, value_x) {
            self.blit_text(value, x, text_y, color);
        }
    }

    /// Draw a 4-digit PIN entry, matching NextUI's clock.c number-entry UI:
    /// monospaced digit cells with no boxes behind them, and the selected
    /// digit marked by an underline beneath it (`ASSET_UNDERLINE`), not a
    /// filled pill behind it.
    pub fn pin(&mut self, digits: &[u8; 4], cursor: usize) {
        let cell_w = sys::scale1(DIGIT_WIDTH);

        // clock.c:265's `y = SCALE1(((FIXED_HEIGHT/FIXED_SCALE - PILL_SIZE -
        // DIGIT_HEIGHT)/2))` can't be ported literally: FIXED_HEIGHT is a
        // runtime expression on h700 (see PADDING above) that bindgen
        // cannot emit. `new()` already proved self.h == FIXED_HEIGHT for
        // every `Ui` that exists, so `scale1(FIXED_HEIGHT/FIXED_SCALE) ==
        // self.h` exactly, and scale1 is linear — distributing it over the
        // subtraction gives the identical y without ever naming
        // FIXED_HEIGHT.
        let y = (self.h - sys::scale1(sys::PILL_SIZE as i32) - sys::scale1(DIGIT_HEIGHT)) / 2;
        let x0 = (self.w - digits.len() as i32 * cell_w) / 2;

        for (i, &digit) in digits.iter().enumerate() {
            if digit > 9 {
                continue; // not yet entered; leave the cell blank
            }
            let cell_x = x0 + i as i32 * cell_w;
            let text = digit.to_string();
            // Digits stay white and unhighlighted regardless of the
            // cursor — the underline drawn below is the only cursor
            // affordance, matching clock.c.
            let width = self.text_width(&text);
            let tx = cell_x + (cell_w - width) / 2;
            let ty = y + (sys::scale1(DIGIT_HEIGHT) - self.text_height()) / 2;
            self.blit_text(&text, tx, ty, COLOR_WHITE);
        }

        let bar_x = x0 + cursor as i32 * cell_w;
        let bar_y = y + sys::scale1(CURSOR_GAP);
        let mut rect = SDL_Rect { x: bar_x, y: bar_y, w: cell_w, h: 0 };
        // SAFETY: screen is valid; rect is within the panel because x0/y
        // are derived from self.w/self.h and cursor < digits.len()
        // (Pin::update never advances it past the last digit). `h: 0`
        // tells GFX_blitPillColor to fall back to ASSET_UNDERLINE's own
        // natural height (api.c:1868), same as clock.c's blitBar, which
        // never sets one either.
        unsafe {
            sys::GFX_blitPill(sys::ASSET_UNDERLINE as i32, self.screen, &mut rect);
        }
    }

    /// Draw the button-hint bar, e.g. `[("A", "OK"), ("B", "BACK")]`.
    ///
    /// `GFX_blitButtonGroup` only ever looks at the first two pairs
    /// (`workspace/common/api.c`'s `hints[2]`), so extra pairs are silently
    /// ignored rather than overflowing anything.
    pub fn hints(&mut self, hints: &[(&str, &str)]) {
        let mut owned = Vec::with_capacity(hints.len() * 2);
        for (button, label) in hints {
            let (Ok(button), Ok(label)) = (CString::new(*button), CString::new(*label)) else {
                continue; // embedded NUL; nothing sane to draw for this pair
            };
            owned.push(button);
            owned.push(label);
        }
        let mut ptrs: Vec<*mut c_char> =
            owned.iter().map(|s| s.as_ptr().cast_mut()).collect();
        ptrs.push(std::ptr::null_mut());

        // SAFETY: ptrs is NUL-terminated and every non-null entry points
        // into `owned`, which outlives this call. GFX_blitButtonGroup takes
        // `char **` but only reads through it.
        unsafe {
            sys::GFX_blitButtonGroup(ptrs.as_mut_ptr(), 0, self.screen, 0);
        }
    }

    /// Render `text` and blit it at `(x, y)`, freeing the rendered surface
    /// before returning — every rendered surface must be freed in the same
    /// frame it was created, or the app leaks per frame.
    fn blit_text(&mut self, text: &str, x: i32, y: i32, color: SDL_Color) {
        let Ok(cstr) = CString::new(text) else {
            return; // embedded NUL; nothing sane to draw
        };
        // SAFETY: font.large is populated by GFX_init before any Ui exists;
        // cstr is a valid NUL-terminated C string for the call's duration.
        let rendered = unsafe { sys::TTF_RenderUTF8_Blended(sys::font.large, cstr.as_ptr(), color) };
        if rendered.is_null() {
            return;
        }
        let mut dst_rect = SDL_Rect { x, y, w: 0, h: 0 };
        // SAFETY: rendered was just checked non-null; self.screen is valid.
        // SDL_UpperBlit is the real symbol behind the SDL_BlitSurface macro
        // (see task-3-context.md's macro-alias table).
        unsafe {
            SDL_UpperBlit(rendered, std::ptr::null(), self.screen, &mut dst_rect);
            sdl2_sys::SDL_FreeSurface(rendered);
        }
    }

    /// The width `text` would render at, in pixels — NextUI's own
    /// `GFX_getTextWidth` (`api.c:979`), which measures with `TTF_SizeUTF8`
    /// rather than rendering a throwaway surface the way this used to.
    fn text_width(&self, text: &str) -> i32 {
        let Ok(cstr) = CString::new(text) else {
            return 0;
        };
        let mut out = Self::scratch_buffer(&cstr);
        // SAFETY: font.large is populated by GFX_init before any Ui exists;
        // cstr is a valid NUL-terminated C string for the call's duration;
        // `out` is sized by `scratch_buffer` to hold GFX_getTextWidth's own
        // `strcpy(out_name, in_name)` (api.c:982), which runs unconditionally
        // before it measures — `max_width` is accepted but never read by the
        // function body, so passing `i32::MAX` here is inert, not a real
        // limit.
        unsafe { sys::GFX_getTextWidth(sys::font.large, cstr.as_ptr(), out.as_mut_ptr().cast(), i32::MAX, 0) }
    }

    /// `text` shortened to fit within `max_width` pixels, trailing with
    /// "..." if it does not — NextUI's own `GFX_truncateText` (`api.c:953`).
    fn truncate(&self, text: &str, max_width: i32) -> String {
        let Ok(cstr) = CString::new(text) else {
            return text.to_string();
        };
        let mut out = Self::scratch_buffer(&cstr);
        // SAFETY: font.large is populated by GFX_init before any Ui exists;
        // cstr is a valid NUL-terminated C string for the call's duration;
        // `out` is sized by `scratch_buffer` to hold the initial full-string
        // strcpy GFX_truncateText performs before it starts shortening
        // (api.c:956). Each shortening pass afterward only ever splices
        // "...\0" starting 4 bytes before the current end (api.c:963), which
        // stays within a buffer sized for the (longer) original string.
        unsafe {
            sys::GFX_truncateText(sys::font.large, cstr.as_ptr(), out.as_mut_ptr().cast(), max_width, 0);
        }
        let end = out.iter().position(|&b| b == 0).unwrap_or(out.len());
        String::from_utf8_lossy(&out[..end]).into_owned()
    }

    /// A zeroed buffer sized for NextUI's `out_name` contract: several of
    /// its text helpers (`GFX_getTextWidth`, `GFX_truncateText`) start with
    /// an unconditional `strcpy(out_name, in_name)`, so the caller's buffer
    /// must hold the *entire* input up front — sizing it to whatever the
    /// call's own result turns out to be would let that first copy overflow
    /// it.
    fn scratch_buffer(cstr: &CString) -> Vec<u8> {
        vec![0u8; cstr.as_bytes().len() + 1]
    }
}

impl Drop for Ui {
    fn drop(&mut self) {
        // SAFETY: each guarded call mirrors an acquire that happened in
        // `new`, in the reverse order NextUI's own teardown uses. This runs
        // on every exit path, including a panic unwinding through this
        // crate — that is what keeps a bug here from leaving /dev/fb0 held
        // open and the launcher corrupted until reboot.
        unsafe {
            if self.pwr_inited {
                sys::PWR_quit();
            }
            if self.input_inited {
                sys::PLAT_quitInput();
            }
            if !self.screen.is_null() {
                sys::GFX_quit();
            }
            if self.settings_inited {
                sys::QuitSettings();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_decode_the_bitmask() {
        let b = Buttons::from_mask(crate::sys::BTN_A as i32 | crate::sys::BTN_DPAD_UP as i32);
        assert!(b.a);
        assert!(b.up);
        assert!(!b.b);
        assert!(!b.down);
    }

    #[test]
    fn row_y_starts_below_the_header_band() {
        // The header band -- header()'s title and hardware_group()'s
        // status pill -- occupies scale1(PADDING)..scale1(PADDING +
        // PILL_SIZE). Row 0 must start at its bottom edge, not overlap it.
        let header_band_bottom = crate::sys::scale1(PADDING) + crate::sys::scale1(crate::sys::PILL_SIZE as i32);
        assert_eq!(row_y(0), header_band_bottom);
    }

    #[test]
    fn row_y_positions_step_by_a_scaled_pill() {
        assert_eq!(
            row_y(1) - row_y(0),
            crate::sys::scale1(crate::sys::PILL_SIZE as i32)
        );
    }
}
