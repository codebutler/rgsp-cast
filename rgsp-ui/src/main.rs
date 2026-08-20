use std::path::PathBuf;
use std::time::{Duration, Instant};

use rgsp_ui::rpc::{CastState, Control, PinOutcome};
use rgsp_ui::screens::home::{Home, HomeAction};
use rgsp_ui::screens::message::Message;
use rgsp_ui::screens::pin::{Pin, PinAction};
use rgsp_ui::service::Service;
use rgsp_ui::ui::Ui;

/// Minimum spacing between reconnect attempts while the daemon is down.
///
/// `Control::connect` doesn't just probe the socket -- it builds and, on
/// failure, tears down a full tokio current-thread runtime (see
/// `rpc::Control::connect`). While the service is stopped, the normal idle
/// state of this screen, calling it every frame would construct and drop
/// that runtime dozens of times a second on a battery-powered handheld. The
/// socket refusal itself is cheap; the runtime around it is not. 500ms is
/// far below what a person can perceive as latency here, but keeps the
/// retry off the per-frame hot path.
const RECONNECT_INTERVAL: Duration = Duration::from_millis(500);

/// Whether enough time has passed since the last reconnect attempt
/// (`last_attempt`, or never if `None`) to try again at `now`. Pulled out
/// of the main loop as a plain function so the throttle policy is
/// unit-testable rather than buried in the loop's control flow.
fn should_reconnect(last_attempt: Option<Instant>, now: Instant) -> bool {
    match last_attempt {
        None => true,
        Some(t) => now.duration_since(t) >= RECONNECT_INTERVAL,
    }
}

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
    /// A pairing attempt ended (rejected, or the connection was lost) and
    /// there is nothing left to do but tell the user and wait for `B`.
    Message(Message),
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

    // `scripts/smoke-ui.sh`'s hook. There is no input injection on the
    // device, so the only way to exercise `Service::start` from a process
    // that is genuinely holding /dev/fb0 -- which is the whole point, since
    // the leak being guarded against is the UI's own display descriptors
    // being inherited by the daemon -- is a non-interactive entry point that
    // brings the display up first (`Ui::new()` above, already done), starts
    // the daemon, and exits. The daemon outlives this process by design; if
    // it kept an fd on the framebuffer, the smoke test's fb0 assertion is
    // what catches it.
    if std::env::args().any(|a| a == "--smoke-start-daemon") {
        tracing::info!("--smoke-start-daemon: starting the daemon, then exiting");
        service.start()?;
        return Ok(());
    }

    // `Control` has no reconnect: a dropped or never-established connection
    // is retried below by constructing a fresh one, once per frame, rather
    // than waiting on this one to heal itself (it never will).
    let mut control = Control::connect(&socket_path.to_string_lossy()).ok();
    let mut last_reconnect_attempt = Some(Instant::now());
    let mut state = CastState { casting: false, client: None, pending: Vec::new() };
    let mut screen = Screen::Home(Home::new());

    loop {
        let buttons = ui.poll();

        let connected = control.as_ref().is_some_and(Control::is_connected);
        if !connected {
            // Until it reconnects, the service reads as stopped and pending
            // clients are unknown rather than stale. See
            // `RECONNECT_INTERVAL` for why this doesn't retry every frame.
            let now = Instant::now();
            if should_reconnect(last_reconnect_attempt, now) {
                control = Control::connect(&socket_path.to_string_lossy()).ok();
                last_reconnect_attempt = Some(now);
                if control.is_none() {
                    state = CastState { casting: false, client: None, pending: Vec::new() };
                }
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

        // Every arm produces the screen for *this* frame's draw below -- an
        // owned match on `screen` itself, not `&mut screen` plus a side
        // `next_screen` slot, so a transition can move the struct it's
        // carrying (e.g. `Pin`'s typed-in digits) into the new state
        // instead of rebuilding it from scratch.
        screen = match screen {
            Screen::Home(mut home) => match home.update(&buttons, &state) {
                HomeAction::None => Screen::Home(home),
                HomeAction::Toggle => {
                    let starting = !connected;
                    let message = if starting { "Starting..." } else { "Stopping..." };
                    // `service.start()`/`stop()` below block the frame loop
                    // for up to 5s/15s (their documented timeouts) with no
                    // way to interleave drawing, so on a handheld that reads
                    // as a hang rather than "working" unless something is
                    // drawn and presented *before* the call. One begin/end
                    // is enough: `PLAT_flip` (`generic_video.c:1657`)
                    // uploads the whole surface and presents on every call,
                    // so there is no alternating half-drawn buffer to worry
                    // about -- draw once, flip once. (An earlier version of
                    // this drew and presented twice, on the theory that the
                    // panel needed both buffers refreshed; that theory was
                    // wrong -- the blank captures that suggested it came
                    // from `scripts/screenshot.sh` reading `/dev/fb0` at a
                    // fixed offset while the visible page is whichever
                    // `yoffset` selects, a bug in the capture tool, since
                    // fixed, not in the display.)
                    ui.begin();
                    let chrome_w = ui.hardware_group();
                    ui.header("Cast", chrome_w);
                    ui.row("Service", Some(message), 0, true);
                    ui.hints(&[]);
                    ui.end();
                    let result = if starting { service.start() } else { service.stop() };
                    if let Err(e) = result {
                        tracing::error!("service toggle failed: {e:#}");
                    }
                    // Force an immediate reconnect attempt next frame instead
                    // of waiting out `RECONNECT_INTERVAL`. Without this, a
                    // successful start can still read as "Stopped" for up to
                    // that long, because `connected` won't flip true until
                    // the throttle lets a new `Control::connect` through --
                    // the same "flashes then goes back to Stopped" symptom
                    // this whole change exists to fix, just shrunk.
                    last_reconnect_attempt = None;
                    Screen::Home(home)
                }
                HomeAction::Pair(id) => {
                    let entry = state.pending.iter().find(|p| p.id == id);
                    let name = entry.and_then(|p| p.name.clone());
                    let address = entry.and_then(|p| p.address.clone());
                    Screen::Pin(Pin::new(id, name, address))
                }
                HomeAction::Exit => break,
            },
            Screen::Pin(mut pin) => match pin.update(&buttons) {
                PinAction::None => Screen::Pin(pin),
                PinAction::Back => Screen::Home(Home::new()),
                PinAction::Submit(code) => {
                    let id = pin.client_id().to_string();
                    // `submit_pin` below blocks the frame loop for up to
                    // `SUBMIT_PIN_TIMEOUT` (5s) with no way to interleave
                    // drawing -- draw and present the full-screen
                    // "Pairing…" message *before* making the call, same
                    // reasoning as `HomeAction::Toggle` above (one
                    // begin/end is enough; see its comment). No button
                    // group: while blocked, `Ui::poll()` never runs, so a
                    // `B Cancel` drawn here would do nothing until the call
                    // already returned on its own -- showing no button is
                    // the honest answer to "can the user act right now",
                    // not a fake one. A non-blocking submit (start/poll, a
                    // genuine Cancel) was built and then reverted -- the
                    // owner preferred no button over the extra concurrency
                    // it required.
                    ui.begin();
                    ui.full_screen_message("Pairing...");
                    ui.hints(&[]);
                    ui.end();
                    match control.as_mut().map(|c| c.submit_pin(&id, &code)) {
                        Some(Ok(PinOutcome::Paired)) => Screen::Home(Home::new()),
                        // Transient: the daemon is still starting up, not a
                        // bad PIN. The same PIN will work shortly, so
                        // return to the digits already typed rather than a
                        // terminal screen.
                        Some(Ok(PinOutcome::NotReady)) => {
                            pin.set_error("Not ready yet, try again");
                            Screen::Pin(pin)
                        }
                        // Wrong PIN: Moonlight has usually already given up
                        // on this attempt by the time the daemon can tell
                        // us it was wrong, so there is nothing left to
                        // retry here -- the user re-initiates pairing from
                        // Moonlight.
                        Some(Ok(PinOutcome::Rejected)) => {
                            Screen::Message(Message::new("Wrong PIN. Pair again from Moonlight."))
                        }
                        Some(Err(e)) => {
                            tracing::warn!("submit_pin failed: {e:#}");
                            Screen::Message(Message::new("Connection lost."))
                        }
                        None => Screen::Message(Message::new("Not connected to the service.")),
                    }
                }
            },
            Screen::Message(message) => {
                if message.update(&buttons) {
                    Screen::Home(Home::new())
                } else {
                    Screen::Message(message)
                }
            }
        };

        match &screen {
            Screen::Home(home) => home.draw(&mut ui, &state, connected),
            Screen::Pin(pin) => pin.draw(&mut ui),
            Screen::Message(message) => message.draw(&mut ui),
        }
        ui.end();
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_attempt_is_never_throttled() {
        assert!(should_reconnect(None, Instant::now()));
    }

    #[test]
    fn an_attempt_right_after_the_last_one_is_throttled() {
        let last = Instant::now();
        assert!(!should_reconnect(Some(last), last));
    }

    #[test]
    fn an_attempt_just_shy_of_the_interval_is_still_throttled() {
        let last = Instant::now();
        let almost = last + RECONNECT_INTERVAL - Duration::from_millis(1);
        assert!(!should_reconnect(Some(last), almost));
    }

    #[test]
    fn an_attempt_at_or_past_the_interval_is_allowed() {
        let last = Instant::now();
        assert!(should_reconnect(Some(last), last + RECONNECT_INTERVAL));
        assert!(should_reconnect(Some(last), last + RECONNECT_INTERVAL + Duration::from_millis(1)));
    }
}
