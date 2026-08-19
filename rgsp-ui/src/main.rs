use rgsp_ui::sys;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    // SAFETY: single-threaded startup; these are NextUI's documented init order
    // (see workspace/all/clock/clock.c), and every pointer is owned by C.
    unsafe {
        sys::InitSettings();
        // SAFETY: MODE_MENU is the mode constant GFX_init expects; the returned
        // surface is owned by NextUI and stays valid until GFX_quit.
        let screen = sys::GFX_init(sys::MODE_MENU as i32);
        anyhow::ensure!(!screen.is_null(), "GFX_init returned null");
        sys::PLAT_initInput();
        sys::PWR_init();

        // SAFETY: non-null checked directly above, and C keeps it alive.
        tracing::info!(w = (*screen).w, h = (*screen).h, "display up");

        sys::PLAT_clearVideo(screen);
        sys::GFX_flip(screen);

        // Torn down in reverse order of init, so nothing outlives what it uses.
        // Releasing the framebuffer here is the whole point of this binary.
        sys::PWR_quit();
        sys::PLAT_quitInput();
        sys::GFX_quit();
        sys::QuitSettings();
    }
    Ok(())
}
