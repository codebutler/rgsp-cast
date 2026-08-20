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

/// NextUI's `MAIN_ROW_COUNT` (`defines.h:64`: `FIXED_HEIGHT / (PILL_SIZE *
/// FIXED_SCALE) - 2`, i.e. how many `PILL_SIZE`-tall rows fit once the top
/// status band and the bottom hint band are reserved). Not bound from C for
/// the same reason `PADDING` above is not: `h700/platform.h:175` redefines
/// it as `(hdmi_active||is_cube)?10:6`, a runtime expression.
///
/// `6` is provably the only value it can take here, by the same argument as
/// `PADDING`'s: `Ui::new()` already rejects any surface that is not
/// 720x480, which per `h700/platform.h:162-163` rules out both `hdmi_active`
/// and `is_cube`, so `MAIN_ROW_COUNT == 6` for every `Ui` that exists.
///
/// A screen's own header/title row (drawn via [`Ui::header`]) is not one of
/// these — this bounds rows drawn through [`Ui::row`] at [`row_y`]
/// positions.
pub const MAIN_ROW_COUNT: i32 = 6;

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

/// The reserved top band every screen's content must clear, in scaled
/// pixels: `header()`'s title and `Ui::hardware_group()`'s status pill
/// occupy `scale1(PADDING)..scale1(PADDING + PILL_SIZE)`, and NextUI's own
/// convention leaves `BUTTON_MARGIN` of breathing room below that before any
/// content — `nextui.c:2712-2714`'s own comment names it: `"top pill area:
/// PADDING + PILL_SIZE + BUTTON_MARGIN"`, used at `nextui.c:2728` as `oy =
/// SCALE1(PADDING + PILL_SIZE + BUTTON_MARGIN) + ...`. Composed from the
/// named constants, not hardcoded, so it tracks if any of them change.
fn header_band_height() -> i32 {
    sys::scale1(PADDING + sys::PILL_SIZE as i32 + sys::BUTTON_MARGIN as i32)
}

/// The width available for [`Ui::header`]'s title before it must clear
/// `hardware_group()`'s status pill, in scaled pixels. `screen_w` and
/// `chrome_width` (the pill's own width, from `hardware_group()`'s return
/// value) are parameters rather than reading `self` directly, so this stays
/// unit-testable without an open display — same split as [`row_y`].
///
/// `chrome_x` mirrors `GFX_blitHardwareGroup`'s own `ox = dst->w -
/// SCALE1(PADDING) - ow` (`api.c:2367`); the title then gets one more
/// `scale1(PADDING)` of breathing room before that edge, minus the
/// `scale1(PADDING)` inset `header()` itself draws the title at.
fn header_max_width(screen_w: i32, chrome_width: i32) -> i32 {
    let pad = sys::scale1(PADDING);
    let chrome_x = screen_w - pad - chrome_width;
    chrome_x - pad - pad
}

/// Vertical position of row `index`, in scaled pixels. A free function, not
/// a method, so it stays unit-testable without an open display.
///
/// Row 0 starts at [`header_band_height`], clearing the header/status band;
/// each following row steps by `scale1(PILL_SIZE)`, contiguous with no
/// extra gap, the same as `nextui.c`'s own file-browser list
/// (`SCALE1(row_index * PILL_SIZE + PADDING)` — that screen has no header
/// band to clear, so its row 0 starts right at `scale1(PADDING)` instead of
/// [`header_band_height`], but the per-row step is identical).
///
/// Before `header_band_height` existed here, row 0 was `scale1(PILL_SIZE)`
/// — 20px inside the header/status band — and its pill visibly overlapped
/// both `header()`'s title and `hardware_group()`'s status pill on
/// hardware.
fn row_y(index: i32) -> i32 {
    header_band_height() + sys::scale1(sys::PILL_SIZE as i32) * index
}

/// The top of the reserved bottom band every screen's hint bar
/// ([`Ui::hints`]) occupies, in scaled pixels — nothing should be drawn
/// below this. NextUI's own comment names it the mirror image of
/// [`header_band_height`]'s top band: `nextui.c:2712-2714`'s `"bottom pill
/// area: BUTTON_MARGIN + PILL_SIZE + PADDING"`, reserved from the bottom
/// edge. Composed from the same named constants, not hardcoded.
fn bottom_band_top(screen_h: i32) -> i32 {
    screen_h - sys::scale1(sys::BUTTON_MARGIN as i32 + sys::PILL_SIZE as i32 + PADDING)
}

/// Vertical origin of the PIN digit row, in scaled pixels. See
/// [`Ui::pin`]'s doc comment for the derivation from `clock.c`'s formula. A
/// free function, not a method, so [`pin_error_y`] can share it and both
/// stay unit-testable without an open display.
fn pin_digit_y(screen_h: i32) -> i32 {
    (screen_h - sys::scale1(sys::PILL_SIZE as i32) - sys::scale1(DIGIT_HEIGHT)) / 2
}

/// The vertical band reserved for the PIN screen's error line, as
/// `(top, height)`: from below the cursor underline — with a deliberate
/// `scale1(PADDING)` gap under it, not jammed against it — down to
/// [`bottom_band_top`]. A band, not a single `y`, because
/// [`Ui::pin_error`] hands this straight to `GFX_blitMessage`, which
/// centres *within* whatever rect it's given (`api.c:2130`: `y +=
/// (dst_rect->h - rendered_height) / 2`) — there is no single "the" y to
/// pick here, so this hands over the whole band and lets NextUI's own
/// primitive do the centring. `underline_h` is `ASSET_UNDERLINE`'s real
/// height ([`Ui::underline_height`]), not guessed: the bar's asset could be
/// a different height than the digit text, and the band's top must clear
/// whichever it actually is, the same reasoning [`header_max_width`]'s
/// `chrome_width` parameter uses for the status pill.
fn pin_error_band(screen_h: i32, underline_h: i32) -> (i32, i32) {
    let underline_y = pin_digit_y(screen_h) + sys::scale1(CURSOR_GAP);
    let top = underline_y + underline_h + sys::scale1(PADDING);
    let bottom = bottom_band_top(screen_h);
    debug_assert!(bottom > top, "PIN error band collapsed: nothing left to draw into");
    (top, bottom - top)
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
    ///
    /// Returns the pill's width in pixels, straight from the C function's
    /// own return value — pass it to [`Ui::header`] so a long title
    /// truncates before running into the pill. This width is **not**
    /// something we can compute from constants: in the `show_setting = 0`
    /// branch (`api.c:2308` on), it depends on live state (`BT_isConnected`,
    /// `PLAT_connectionStrength`, `CFG_getShowClock`, the audio sink) and on
    /// `asset_rects[...]` — PNG dimensions loaded at runtime, never exposed
    /// to us. `GFX_blitHardwareGroup` already computes the real value to
    /// draw the pill; reusing its return is exact, not a guess.
    pub fn hardware_group(&mut self) -> i32 {
        // SAFETY: screen is valid for the lifetime of `self`.
        unsafe { sys::GFX_blitHardwareGroup(self.screen, 0) }
    }

    /// Draw a page title at the top of the screen, truncated so it does not
    /// run into the status pill `hardware_group()` draws in the same band.
    /// `chrome_width` is that pill's width — its caller-visible return
    /// value — so the boundary here always matches what was actually drawn,
    /// not a recomputed (and possibly stale) guess at it.
    pub fn header(&mut self, title: &str, chrome_width: i32) {
        let pad = sys::scale1(PADDING);
        let title = self.truncate(title, header_max_width(self.w, chrome_width));
        self.blit_text(&title, pad, pad, COLOR_WHITE);
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
    ///
    /// [`pin_digit_y`] (this call's `y`) mirrors clock.c:265's `y =
    /// SCALE1(((FIXED_HEIGHT/FIXED_SCALE - PILL_SIZE - DIGIT_HEIGHT)/2))`,
    /// which can't be ported literally: `FIXED_HEIGHT` is a runtime
    /// expression on h700 (see `PADDING` above) that bindgen cannot emit.
    /// `new()` already proved `self.h == FIXED_HEIGHT` for every `Ui` that
    /// exists, so `scale1(FIXED_HEIGHT/FIXED_SCALE) == self.h` exactly, and
    /// `scale1` is linear — distributing it over the subtraction gives the
    /// identical `y` without ever naming `FIXED_HEIGHT`.
    pub fn pin(&mut self, digits: &[u8; 4], cursor: usize) {
        let cell_w = sys::scale1(DIGIT_WIDTH);
        let y = pin_digit_y(self.h);
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

    /// `ASSET_UNDERLINE`'s real height in pixels, read from the asset
    /// itself rather than guessed (`GFX_assetRect`, `api.h:439`) — the same
    /// reasoning as `hardware_group()`'s return value: a hand-picked number
    /// here could silently go stale if the asset ever changes, where
    /// reading it cannot.
    fn underline_height(&self) -> i32 {
        let mut rect = SDL_Rect { x: 0, y: 0, w: 0, h: 0 };
        // SAFETY: GFX_assetRect takes no surface — it only reads the
        // `asset_rects` table `GFX_init` already populated before any `Ui`
        // exists. ASSET_UNDERLINE is a real `ASSET_*` constant.
        unsafe {
            sys::GFX_assetRect(sys::ASSET_UNDERLINE as i32, &mut rect);
        }
        rect.h
    }

    /// Draw the PIN screen's error line with NextUI's own `GFX_blitMessage`
    /// (`api.h:400`) — the primitive NextUI itself uses for exactly this: a
    /// short status message centred in a reserved region, e.g.
    /// `ledcontrol.c:262-283`'s `GFX_blitMessage(font.large, "This device
    /// has no RGB lights.", screen, &(SDL_Rect){0, 0, screen->w,
    /// screen->h})`. It centres both horizontally and vertically within the
    /// rect it's given ([`pin_error_band`] computes ours) and *wraps*
    /// text too wide for it rather than truncating — unlike `row()`'s or
    /// `header()`'s truncation, this relies on the caller only ever passing
    /// short, app-authored text. That is true here: PIN rejection messages
    /// (`"PIN rejected"`, `"Not ready yet, try again"`, `"Connection
    /// lost"`, `"Not connected"`) are fixed constants in `pin.rs`, never a
    /// caller-supplied string like a device name or address — the same
    /// distinction that ruled `GFX_blitText`/`GFX_sizeText` out for
    /// `header()`/`row()`'s unbounded labels but does not apply to this
    /// call site.
    pub fn pin_error(&mut self, text: &str) {
        let (y, h) = pin_error_band(self.h, self.underline_height());
        let mut rect = SDL_Rect { x: 0, y, w: self.w, h };
        let Ok(cstr) = CString::new(text) else {
            return; // embedded NUL; nothing sane to draw
        };
        // SAFETY: font.large is populated by GFX_init before any Ui
        // exists; rect is derived from self.w/self.h so it lies within the
        // panel. GFX_blitMessage's `char*` parameter is not const-correct
        // in the C header, but its implementation (api.c:2106) only ever
        // reads through it (strchr, strncpy *from* it) — it never writes,
        // so casting away const here does not create real mutation.
        unsafe {
            sys::GFX_blitMessage(sys::font.large, cstr.as_ptr().cast_mut(), self.screen, &mut rect);
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
        // The reserved top band -- header()'s title, hardware_group()'s
        // status pill, and NextUI's own BUTTON_MARGIN breathing room below
        // both -- is PADDING + PILL_SIZE + BUTTON_MARGIN (nextui.c:2712-2714).
        // Row 0 must start at its bottom edge, not overlap it.
        assert_eq!(row_y(0), header_band_height());
    }

    #[test]
    fn row_y_positions_step_by_a_scaled_pill() {
        assert_eq!(
            row_y(1) - row_y(0),
            crate::sys::scale1(crate::sys::PILL_SIZE as i32)
        );
    }

    #[test]
    fn header_max_width_reserves_a_pad_of_room_before_the_status_pill() {
        let pad = crate::sys::scale1(PADDING);
        let screen_w = 720;
        let chrome_w = 260; // a plausible hardware_group() width, not a real one
        // header_max_width's boundary (chrome_x) must land exactly one
        // `pad` short of where hardware_group() actually starts its pill --
        // any less and a title truncated to fit still touches the chrome.
        let chrome_x = screen_w - pad - chrome_w;
        assert_eq!(header_max_width(screen_w, chrome_w), chrome_x - pad - pad);
    }

    #[test]
    fn header_max_width_shrinks_exactly_as_the_status_pill_grows() {
        // A wider status pill (more of WiFi/BT/clock showing) must eat
        // directly into the title's budget, pixel for pixel -- otherwise a
        // title that fit a moment ago could start overlapping the chrome
        // the next frame the pill grows, with nothing here to catch it.
        let screen_w = 720;
        let narrow = header_max_width(screen_w, 60);
        let wide = header_max_width(screen_w, 260);
        assert_eq!(narrow - wide, 260 - 60);
    }

    #[test]
    fn pin_error_band_sits_between_the_underline_and_the_bottom_band() {
        // A plausible ASSET_UNDERLINE height -- a thin bar, not a real
        // measurement (this test runs without a display). What matters is
        // that the band clears whatever the real one turns out to be, and
        // stays clear of the hint bar's reserved band -- pinning both
        // collisions the owner actually hit on hardware at once.
        let screen_h = 480;
        let underline_h = 4;
        let underline_bottom = pin_digit_y(screen_h) + crate::sys::scale1(CURSOR_GAP) + underline_h;

        let (top, height) = pin_error_band(screen_h, underline_h);
        assert!(
            top > underline_bottom,
            "band top ({top}) must sit below the underline's bottom edge ({underline_bottom})"
        );
        assert_eq!(
            top + height,
            bottom_band_top(screen_h),
            "band bottom must land exactly at the reserved bottom band, not short of or past it"
        );
    }

    #[test]
    fn pin_error_band_shrinks_exactly_as_the_underline_grows() {
        // Same reasoning as header_max_width's status-pill test: the gap
        // below the underline must track its real height exactly, not a
        // number that happened to work for one asset size.
        let screen_h = 480;
        let (thin_top, thin_h) = pin_error_band(screen_h, 4);
        let (thick_top, thick_h) = pin_error_band(screen_h, 12);
        assert_eq!(thick_top - thin_top, 12 - 4);
        assert_eq!(thin_h - thick_h, 12 - 4);
    }
}
