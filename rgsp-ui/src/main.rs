use rgsp_ui::sys;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    // SAFETY: single-threaded startup, and NextUI's documented init order (see
    // workspace/all/clock/clock.c). InitSettings and QuitSettings bracket the
    // whole body so the settings are released even when bring-up fails partway
    // — an early return between the two would be an unpaired acquire.
    unsafe {
        sys::InitSettings();
        let result = show_display();
        sys::QuitSettings();
        result
    }
}

/// Bring the display up, log its dimensions, clear it, and tear it back down.
///
/// Proving the teardown releases `/dev/fb0` is the whole point of this binary.
///
/// # Safety
///
/// The caller must have called `InitSettings` beforehand and must call
/// `QuitSettings` afterwards, on the same thread.
unsafe fn show_display() -> anyhow::Result<()> {
    // SAFETY: every pointer here is owned by C. `screen` is null-checked before
    // use and stays valid until GFX_quit, which is the last thing to touch it.
    unsafe {
        let screen = sys::GFX_init(sys::MODE_MENU as i32);
        anyhow::ensure!(!screen.is_null(), "GFX_init returned null");
        sys::PLAT_initInput();
        sys::PWR_init();

        tracing::info!(w = (*screen).w, h = (*screen).h, "display up");

        sys::PLAT_clearVideo(screen);
        sys::GFX_flip(screen);

        // Torn down in reverse order of init, so nothing outlives what it uses.
        sys::PWR_quit();
        sys::PLAT_quitInput();
        sys::GFX_quit();
    }
    Ok(())
}
