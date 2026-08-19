use std::path::PathBuf;

use rgsp_ui::rpc::{CastState, Control, PinOutcome};
use rgsp_ui::screens::home::{Home, HomeAction};
use rgsp_ui::screens::pin::{Pin, PinAction};
use rgsp_ui::service::Service;
use rgsp_ui::ui::Ui;

/// Where the daemon puts its pidfile, log, and control socket. Mirrors the
/// pak's own hooks (`pak/hooks/*/10-rgsp-*.sh`), which read the same
/// variable with the same `/tmp/rgsp` fallback.
fn run_dir() -> PathBuf {
    std::env::var("RGSP_RUN_DIR").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("/tmp/rgsp"))
}

/// Where the pak's own `rgsp-host` binary lives, for [`Service::start`].
/// Mirrors `pak/hooks/*/10-rgsp-*.sh`'s `RGSP_PAK_DIR` fallback.
fn pak_dir() -> PathBuf {
    std::env::var("RGSP_PAK_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/mnt/SDCARD/Tools/h700/Cast.pak"))
}

/// The screen currently receiving input and being drawn.
enum Screen {
    Home(Home),
    Pin(Pin),
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    // `Ui::new()`/`Drop` bracket the display: Drop runs even if a panic
    // unwinds through the loop below, which is what keeps a bug here from
    // leaving /dev/fb0 held open after this process exits.
    let mut ui = Ui::new()?;
    let (w, h) = ui.size();
    tracing::info!(w, h, "display up");

    let run_dir = run_dir();
    let socket_path = run_dir.join("control.sock");
    let service = Service::new(pak_dir(), run_dir);

    // `Control` has no reconnect: a dropped or never-established connection
    // is retried below by constructing a fresh one, once per frame, rather
    // than waiting on this one to heal itself (it never will).
    let mut control = Control::connect(&socket_path.to_string_lossy()).ok();
    let mut state = CastState { casting: false, client: None, pending: Vec::new() };
    let mut screen = Screen::Home(Home::new());

    loop {
        let buttons = ui.poll();

        let connected = control.as_ref().is_some_and(Control::is_connected);
        if !connected {
            // A refused connect on a Unix socket returns fast, so retrying
            // every frame while the daemon is down is cheap. Until it
            // reconnects, the service reads as stopped and pending clients
            // are unknown rather than stale.
            control = Control::connect(&socket_path.to_string_lossy()).ok();
            if control.is_none() {
                state = CastState { casting: false, client: None, pending: Vec::new() };
            }
        }
        if let Some(c) = control.as_mut()
            && let Some(new_state) = c.poll_state()
        {
            state = new_state;
        }

        // The panel is double-buffered: a single `GFX_flip` updates only one
        // of the two buffers, so every frame must redraw in full or the
        // other buffer goes stale and visibly wrong.
        ui.begin();

        let mut next_screen = None;
        match &mut screen {
            Screen::Home(home) => match home.update(&buttons, &state) {
                HomeAction::None => {}
                HomeAction::Toggle => {
                    let starting = !state.casting;
                    let message = if starting { "Starting..." } else { "Stopping..." };
                    // `service.start()`/`stop()` below block the frame loop
                    // for up to 5s/15s (their documented timeouts) with no
                    // way to interleave drawing, so on a handheld that reads
                    // as a hang rather than "working" unless something is
                    // drawn and presented *before* the call. The panel is
                    // double-buffered, so a single begin/end only updates one
                    // of the two buffers -- draw and present twice to make
                    // sure the message is actually on screen, not sitting in
                    // the buffer that won't be shown until the next flip.
                    for _ in 0..2 {
                        ui.begin();
                        ui.header("Cast");
                        ui.row("Service", Some(message), 0, true);
                        ui.hints(&[]);
                        ui.end();
                    }
                    let result = if starting { service.start() } else { service.stop() };
                    if let Err(e) = result {
                        tracing::error!("service toggle failed: {e:#}");
                    }
                }
                HomeAction::Pair(id) => {
                    let name = state.pending.iter().find(|p| p.id == id).and_then(|p| p.name.clone());
                    next_screen = Some(Screen::Pin(Pin::new(id, name)));
                }
                HomeAction::Exit => break,
            },
            Screen::Pin(pin) => match pin.update(&buttons) {
                PinAction::None => {}
                PinAction::Back => next_screen = Some(Screen::Home(Home::new())),
                PinAction::Submit(code) => {
                    let id = pin.client_id().to_string();
                    match control.as_mut().map(|c| c.submit_pin(&id, &code)) {
                        Some(Ok(PinOutcome::Paired)) => next_screen = Some(Screen::Home(Home::new())),
                        // Wrong PIN: the user should retype it.
                        Some(Ok(PinOutcome::Rejected)) => pin.set_error("PIN rejected"),
                        // The daemon is still starting up, not a bad PIN --
                        // reporting it as one would send the user retyping a
                        // correct PIN in confusion.
                        Some(Ok(PinOutcome::NotReady)) => pin.set_error("Not ready yet, try again"),
                        Some(Err(e)) => {
                            tracing::warn!("submit_pin failed: {e:#}");
                            pin.set_error("Connection lost");
                        }
                        None => pin.set_error("Not connected"),
                    }
                }
            },
        }
        if let Some(next) = next_screen {
            screen = next;
        }

        match &screen {
            Screen::Home(home) => home.draw(&mut ui, &state),
            Screen::Pin(pin) => pin.draw(&mut ui),
        }
        ui.end();
    }

    Ok(())
}
