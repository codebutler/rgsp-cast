//! The rgsp-cast daemon: Moonshine's GameStream protocol layer driven by the
//! RG SP's Cedar hardware encoder and the kernel ALSA loopback.
//!
//! Ownership split, since it is not obvious from the crate layout:
//! - `moonshine-core` (vendored) owns pairing, RTSP, RTP+FEC, encryption,
//!   Opus encoding and mDNS. It has no video encoder and no audio source.
//! - `rgsp_host::{capture, video, audio}` own the hardware: H.264 frames out
//!   of the Cedar VE, raw PCM out of the loopback.
//! - This file is the only place the two meet.

use std::path::{Path, PathBuf};

use async_shutdown::ShutdownManager;
use moonshine_core::ShutdownReason;
use moonshine_core::clients::ClientManager;
use moonshine_core::config::Config;
use moonshine_core::discovery::MdnsDiscovery;
use moonshine_core::rtsp::RtspServer;
use moonshine_core::session::manager::SessionManager;
use moonshine_core::session::stream::video::{EncodedFrame, EncoderControl};
use moonshine_core::webserver::Webserver;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;

use rgsp_host::audio::{CHANNELS, LoopbackCapture, PERIOD_FRAMES};
use rgsp_host::control::{ControlHandle, PendingEntry};
use rgsp_host::daemon::PidFile;
use rgsp_host::routing::CastSink;
use rgsp_host::video::{IdrRequester, ResetRequester, VideoConfig, VideoStream};

/// Moonlight's codec bitmask, as probed by the Vulkan healthcheck this project
/// deleted (`git show f23d52e^:.../healthcheck.rs`, CODEC_H264 = 0x1,
/// CODEC_HEVC = 0x100, CODEC_AV1_MAIN8 = 0x10000). Cedar encodes H.264 only,
/// so exactly one bit is advertised — Task 5 found two other places where the
/// vendored tree offered capabilities this device does not have.
const SUPPORTED_CODECS: u32 = 0x0000_0001;

/// The capture end of the kernel loopback cable. `.asoundrc` (written by
/// `CastSink::engage`) points playback at the other end, `hw:Loopback,0,0`.
const LOOPBACK_CAPTURE_DEVICE: &str = "hw:Loopback,1,0";

/// NextUI's per-platform userdata directory on the RG SP (h700). Holds the
/// `.asoundrc` the routing swaps, and — under `rgsp-cast/` — our config,
/// TLS keypair and pairing state, so all of it survives a reboot on the SD
/// card rather than living on the rootfs.
const DEFAULT_USERDATA: &str = "/mnt/SDCARD/.userdata/h700";

/// Where the pidfile and the UI's control socket live when `RGSP_RUN_DIR`
/// says nothing. Every other piece of the tree — `pak/launch.sh`, the
/// `pak/hooks/*` scripts, and `rgsp-ui` — reads `RGSP_RUN_DIR` with this same
/// fallback, so the daemon has to as well: a UI that spawns us with a custom
/// `RGSP_RUN_DIR` then probes that directory for our socket, and a daemon
/// that ignored the variable would bind somewhere the UI never looks.
const DEFAULT_RUN_DIR: &str = "/tmp/rgsp";

/// The directory holding the pidfile and the control socket.
fn run_dir() -> PathBuf {
    std::env::var_os("RGSP_RUN_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(DEFAULT_RUN_DIR))
}

/// How often to check whether a Moonlight client has started a session.
/// `SessionManager` has no "session started" notification; the senders simply
/// become `Some` once RTSP PLAY has built the streams.
const SESSION_POLL: std::time::Duration = std::time::Duration::from_millis(250);

/// Exercise the virtual gamepad on its own: create it, then press and release
/// A once a second. Used to check that an emulator already running picks up a
/// device that appears after it started - the one thing about this path that
/// cannot be settled by reading code.
fn input_selftest() -> anyhow::Result<()> {
    let mut pad = rgsp_host::input::VirtualPad::open()?;
    tracing::info!("pressing BTN_SOUTH (A) once a second; ctrl-c to stop");
    let mut down = false;
    loop {
        down = !down;
        let mut state = rgsp_host::input::PadState::default();
        state.set_key(304, down);
        pad.apply(state)?;
        tracing::info!("A {}", if down { "down" } else { "up" });
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    if std::env::args().any(|a| a == "--input-selftest") {
        return match input_selftest() {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                tracing::error!("input selftest failed: {e:#}");
                std::process::ExitCode::FAILURE
            },
        };
    }

    let userdata = PathBuf::from(
        std::env::var("RGSP_USERDATA").unwrap_or_else(|_| DEFAULT_USERDATA.to_string()),
    );

    // Step 1: the pidfile is the mutual exclusion for the whole daemon, so it
    // is taken before anything with a side effect — including binding the
    // control socket, which unlinks whatever is already at that path before
    // it binds. Without the pidfile held first, a losing second instance
    // could unlink a first instance's *live* socket out from under it.
    let run_dir = run_dir();
    let pid_path = run_dir.join("daemon.pid");
    let pidfile = match PidFile::acquire(&pid_path) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("rgsp-host is already running ({e})");
            return std::process::ExitCode::FAILURE;
        }
    };

    // Step 2: route game audio into the loopback. From here on, EVERY exit
    // path must run `release()` — while this is engaged the handheld's
    // speaker is silent, and nothing else restores it.
    let cast_sink = match CastSink::engage(&userdata) {
        Ok(sink) => sink,
        Err(e) => {
            tracing::error!("failed to route audio to the loopback: {e:#}");
            pidfile.release();
            return std::process::ExitCode::FAILURE;
        }
    };

    // Loaded before the runtime exists: it sets `XDG_DATA_HOME`, and mutating
    // the environment with other threads running is the pattern Rust 2024 made
    // `unsafe`. Nothing here needs async.
    let config = match load_config(&userdata) {
        Ok(config) => config,
        Err(e) => {
            tracing::error!("{e:#}");
            return shutdown(cast_sink, pidfile, std::process::ExitCode::FAILURE);
        }
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!("failed to start the tokio runtime: {e}");
            return shutdown(cast_sink, pidfile, std::process::ExitCode::FAILURE);
        }
    };

    // Step 3: serve the control socket, now that the pidfile guarantees we're
    // the only instance that could be unlinking a stale path there.
    // `ControlHandle::serve` unlinks the path before binding, so it must run
    // *after* the pidfile is held — the flock is what makes a stale path from
    // a dead daemon safe to unlink, and what stops a second, losing instance
    // from unlinking a live one's socket out from under it. The run dir
    // itself already exists: `PidFile::acquire` created it.
    let control_socket_path = run_dir.join("control.sock");
    let control = ControlHandle::new();
    let control_socket = match runtime.block_on(control.clone().serve(&control_socket_path.to_string_lossy())) {
        Ok(handle) => handle,
        Err(e) => {
            tracing::error!("failed to serve the control socket: {e:#}");
            return shutdown(cast_sink, pidfile, std::process::ExitCode::FAILURE);
        }
    };

    let outcome = runtime.block_on(serve(config, &control));
    // Signal the control socket's server task to stop *before* tearing down
    // the runtime below -- that task only exists to receive this signal
    // while the runtime is still alive, so calling `stop()` after
    // `shutdown_timeout` (as this used to) would find nothing left
    // listening: a no-op whose `Err` gets silently discarded. Ordered here,
    // it actually asks the RPC server to wind down, and `shutdown_timeout`
    // then gets to wait for that to happen instead of just aborting it.
    let _ = control_socket.stop();
    // The hardware loops run as blocking tasks, and dropping a `Runtime`
    // waits for those to finish. A capture loop parked in ALSA or the Cedar
    // driver must not delay restoring the speaker, so give them a moment and
    // then leave them to the process exit that follows.
    runtime.shutdown_timeout(std::time::Duration::from_secs(2));

    // Step 7. `CastSink::release` is not a `Drop` impl and this process may
    // end via `ExitCode` or a signal, so teardown is explicit and shared by
    // the success and failure paths.
    let code = match outcome {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e:#}");
            std::process::ExitCode::FAILURE
        }
    };
    shutdown(cast_sink, pidfile, code)
}

/// Restore audio routing and drop the pidfile. Runs on clean shutdown, on
/// SIGTERM/SIGINT, and on every startup failure after `CastSink::engage`
/// succeeded.
fn shutdown(cast_sink: CastSink, pidfile: PidFile, code: std::process::ExitCode) -> std::process::ExitCode {
    if let Err(e) = cast_sink.release() {
        tracing::error!("failed to restore audio routing: {e:#}");
    }
    pidfile.release();
    code
}

async fn serve(config: Config, control: &ControlHandle) -> anyhow::Result<()> {
    tracing::debug!("using configuration:\n{config:#?}");

    let shutdown = ShutdownManager::new();
    spawn_signal_handler(shutdown.clone());

    // Step 4: the protocol layer. Every one of these spawns its own tasks and
    // stops when `shutdown` triggers.
    let (cert, pkey) = moonshine_core::tls::load_or_create_certificate(&config)
        .map_err(|()| anyhow::anyhow!("failed to load or create the TLS certificate"))?;

    let session_manager = SessionManager::new(
        config.stream.video.clone(),
        config.stream.audio.clone(),
        config.stream.control.clone(),
        config.address.clone(),
        config.stream.timeout,
        shutdown.clone(),
    )
    .map_err(|()| anyhow::anyhow!("failed to create the session manager"))?;

    let client_manager = ClientManager::new(cert.clone(), pkey)
        .map_err(|()| anyhow::anyhow!("failed to create the client manager"))?;

    let unique_id = client_manager
        .persistent_state()
        .get_uuid()
        .map_err(|()| anyhow::anyhow!("failed to read the server's unique id"))?;

    // The socket is already serving (Step 3 in `main`, after the pidfile), so
    // `submit_pin` calls made before this point are answered with "pairing
    // not available" rather than panicking or hanging.
    control.set_client_manager(client_manager.clone());

    let _rtsp = RtspServer::new(
        config.address.clone(),
        config.stream.port,
        config.stream.video.clone(),
        config.stream.audio.clone(),
        config.stream.control.clone(),
        session_manager.clone(),
        shutdown.clone(),
    );
    let _webserver = Webserver::new(
        config.name.clone(),
        config.address.clone(),
        config.stream.port,
        config.webserver.clone(),
        config.applications.clone(),
        SUPPORTED_CODECS,
        false, // No HDR: the panel is 720x480 SDR and Cedar encodes 8-bit H.264.
        unique_id,
        cert,
        client_manager.clone(),
        session_manager.clone(),
        shutdown.clone(),
    )
    .map_err(|()| anyhow::anyhow!("failed to start the webserver"))?;
    let _discovery = MdnsDiscovery::spawn(&config.address, config.webserver.port, &config.name);

    tracing::info!("rgsp-host is ready and waiting for connections");

    // Step 5: keep the UI's pending-pairing list current. `pending_changed`
    // fires after every mutation of the pending set, including removal on
    // successful pairing — and it can fire four times in a row at machine
    // pace during the handshake (`clients.rs:155,205,239,274`). `Notify::
    // notify_waiters` stores no permit: it only wakes waiters already
    // registered, so we register (`enable()`) before ever reading state, and
    // re-register immediately after each fire — before the next async call —
    // so a fire landing while we're mid-update is still caught rather than
    // lost outright. `pending` is a full replacement, not a diff, so a lost
    // fire wouldn't just be late; it could leave the UI stuck on a stale list
    // forever.
    let pending_watcher = {
        let control = control.clone();
        let client_manager = client_manager.clone();
        tokio::spawn(async move {
            let changed = client_manager.pending_changed();
            let notified = changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            control.set_pending(pending_entries(&client_manager));
            loop {
                notified.as_mut().await;
                notified.set(changed.notified());
                notified.as_mut().enable();
                control.set_pending(pending_entries(&client_manager));
            }
        })
    };

    // Step 6.
    let pump = tokio::spawn(session_pump(session_manager.clone(), shutdown.clone(), control.clone()));

    shutdown.wait_shutdown_triggered().await;
    pending_watcher.abort();

    // Stop the session explicitly rather than letting `SessionManager`'s
    // `Drop` do it. That `Drop` calls `Handle::block_on` when a session is
    // still active, which panics if it runs from inside this async context —
    // and a panic here would unwind past `main`'s teardown, leaving
    // `.asoundrc` pointed at the loopback and the handheld with no speaker
    // audio. Stopping first means `Drop` has nothing left to do.
    let _ = session_manager.stop_session().await;
    pump.abort();
    Ok(())
}

/// `ClientManager::pending_clients` returns moonshine-core's `PendingInfo`;
/// the control socket's wire type is `PendingEntry`. Same shape, different
/// crate, so it's a mapping rather than a shared type.
fn pending_entries(client_manager: &ClientManager) -> Vec<PendingEntry> {
    client_manager
        .pending_clients()
        .into_iter()
        .map(|p| PendingEntry { id: p.id, name: normalize_devicename(p.name), address: p.address })
        .collect()
}

/// Every Moonlight client hardcodes `devicename=roth` on every pairing
/// request — it is not the client's name. Confirmed against both upstream
/// clients' pairing code: moonlight-qt's `app/backend/nvpairingmanager.cpp`
/// sends `devicename=roth&updateState=1&...` on all five pairing POSTs, and
/// moonlight-android's `NvHTTP.java` sends
/// `"pair", "devicename=roth&updateState=1&" + ...`. `roth` is a legacy
/// GameStream constant left over from an old NVIDIA SHIELD codename, so
/// showing it in the pairing UI would read as a name when it is really a
/// protocol artifact shared by every client.
///
/// Fold it back to `None` here so the UI falls back to its short-id
/// display, same as any other pending client that sent no name. Keep
/// capturing `devicename` upstream in moonshine (do not remove it there) —
/// a non-Moonlight client could someday send something genuinely useful,
/// and only the literal constant is normalized away, not any broader
/// pattern.
fn normalize_devicename(name: Option<String>) -> Option<String> {
    name.filter(|n| n != "roth")
}

fn spawn_signal_handler(shutdown: ShutdownManager<ShutdownReason>) {
    tokio::spawn(async move {
        let mut terminate = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("failed to listen for SIGTERM: {e}");
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => tracing::info!("received SIGINT, shutting down"),
            _ = terminate.recv() => tracing::info!("received SIGTERM, shutting down"),
        }
        let _ = shutdown.trigger_shutdown(ShutdownReason::AppQuit);
    });
}

/// Drives the hardware for as long as a Moonlight session is active, and waits
/// for the next one after it ends.
///
/// The capture is opened per session, not once at startup: `Capture` is
/// single-instance per process (`CAPTURE_OPEN` in `capture.rs`) and holds the
/// Cedar VE, so it must be released between sessions. Both hardware loops are
/// therefore joined before the loop comes round again.
async fn session_pump(
    session_manager: SessionManager,
    shutdown: ShutdownManager<ShutdownReason>,
    control: ControlHandle,
) {
    loop {
        // Wait for RTSP PLAY to build the streams.
        let frame_tx = loop {
            if shutdown.is_shutdown_triggered() {
                return;
            }
            if let Some(tx) = session_manager.video_frame_sender().await {
                break tx;
            }
            tokio::time::sleep(SESSION_POLL).await;
        };

        let context = match session_manager.active_video_context().await {
            Some(context) => context,
            None => {
                // The session ended between the two accessors.
                continue;
            }
        };
        let audio_tx = session_manager.audio_frame_sender().await;
        let control_rx = session_manager.encoder_control_receiver().await;
        let input_rx = session_manager.input_receiver().await;

        // The client chooses this, and `rtp_timestamp_for` divides by it, so a
        // malformed ANNOUNCE carrying 0 would panic the video task and tear the
        // session down. Clamp once, here, and use this one value everywhere
        // below — the encoder's pacing and the 90 kHz RTP clock must never
        // disagree.
        let fps = context.fps.max(1);
        if fps != context.fps {
            tracing::warn!("client negotiated {} fps; clamped to {fps}", context.fps);
        }

        // Moonlight negotiates its own resolution (commonly 1280x720);
        // `VideoStream::run` clamps to the panel geometry and logs — the panel
        // geometry is what the client will actually decode.
        control.set_client(Some("Moonlight".to_string()));
        control.set_casting(true);
        tracing::info!(
            "session started: {}x{} @ {fps} fps, {} bps requested, codec {:?}",
            context.width,
            context.height,
            context.bitrate,
            context.video_format,
        );
        // We only ever produce H.264: the Cedar VE's H.265 path is not wired
        // up here. A client that negotiated anything else will sit in
        // "Waiting for IDR frame" forever, because its depacketizer looks for
        // that codec's parameter sets (an HEVC VPS, say) and never finds them
        // in our stream - with no error anywhere, on either side.
        if !matches!(context.video_format, moonshine_core::session::stream::video::VideoFormat::H264) {
            tracing::error!(
                "client negotiated {:?}, but this host only encodes H.264 - the client will \
                 never decode this stream. Set the client's video codec to H.264.",
                context.video_format,
            );
        }

        let video = VideoStream::new(VideoConfig {
            width: context.width,
            height: context.height,
            fps,
            bitrate: context.bitrate as u32,
        });
        let control_task = tokio::spawn(forward_encoder_control(
            control_rx,
            video.idr_requester(),
            video.reset_requester(),
        ));

        // Client input, if the pad could be created. A device without
        // /dev/uinput still streams; it just cannot be controlled remotely,
        // which is worth a warning rather than failing the session.
        let input_task = input_rx.map(|rx| tokio::spawn(run_input(rx)));

        let mut video_task = tokio::task::spawn_blocking(move || run_video(video, frame_tx));
        let mut audio_task = tokio::task::spawn_blocking(move || run_audio(audio_tx));

        // Whichever loop stops first, the session is over. Stop the session so
        // the other loop's channel closes, then wait for it: both must have
        // returned before the next iteration, because the `Capture` they hold
        // is single-instance per process and the next session reopens it.
        // (Dropping a `JoinHandle` detaches the blocking task rather than
        // cancelling it, so awaiting the survivor is not optional.)
        tokio::select! {
            result = &mut video_task => {
                report("video capture", result);
                let _ = session_manager.stop_session().await;
                report("audio capture", audio_task.await);
            },
            result = &mut audio_task => {
                report("audio capture", result);
                let _ = session_manager.stop_session().await;
                report("video capture", video_task.await);
            },
        }
        control_task.abort();
        // The pad is dropped with the task, which releases every held button.
        // A client that disconnects mid-press must not leave the emulator
        // seeing a key held down forever.
        if let Some(task) = input_task {
            task.abort();
        }

        control.set_client(None);
        control.set_casting(false);

        if shutdown.is_shutdown_triggered() {
            return;
        }
        tracing::info!("session ended, waiting for the next client");
    }
}

fn report(what: &str, result: Result<anyhow::Result<()>, tokio::task::JoinError>) {
    match result {
        Ok(Ok(())) => tracing::info!("{what} stopped"),
        Ok(Err(e)) => tracing::warn!("{what} stopped: {e:#}"),
        Err(e) => tracing::error!("{what} panicked: {e}"),
    }
}

/// Capture -> Cedar H.264 -> moonshine-core's packetizer.
///
/// Runs on a blocking thread: `Capture::next` sleeps until the frame deadline,
/// and `blocking_send` parks when the bounded channel is full.
/// Feed client input into the virtual gamepad for the life of a session.
///
/// The pad is created here and dropped when this task ends, so its buttons are
/// released and the device disappears with the session.
async fn run_input(mut input_rx: mpsc::Receiver<Vec<u8>>) {
    let mut pad = match rgsp_host::input::VirtualPad::open() {
        Ok(pad) => pad,
        Err(e) => {
            tracing::warn!("no virtual gamepad, remote input disabled: {e:#}");
            return;
        },
    };

    // One running state, updated in place: each packet carries the complete
    // set of buttons held at that instant, and `apply` emits only the edges.
    let mut state = rgsp_host::input::PadState::default();
    let mut seen: u32 = 0;
    let mut unhandled: u32 = 0;
    let mut last_report = std::time::Instant::now();
    while let Some(payload) = input_rx.recv().await {
        seen += 1;
        if rgsp_host::input_decode::apply_packet(&payload, &mut state) {
            if let Err(e) = pad.apply(state) {
                tracing::warn!("failed to inject input, stopping: {e:#}");
                return;
            }
        } else {
            // Mouse, scroll, pen, text and haptics all land here; only
            // controller and keyboard packets drive the pad.
            unhandled += 1;
            if last_report.elapsed() >= std::time::Duration::from_secs(30) {
                tracing::debug!("input: {seen} packets, {unhandled} not applicable to the pad");
                last_report = std::time::Instant::now();
            }
        }
    }
    tracing::debug!("input channel closed");
}

fn run_video(video: VideoStream, frame_tx: mpsc::Sender<EncodedFrame>) -> anyhow::Result<()> {
    video.run(|frame| {
        // Note the field-name difference across the crate boundary:
        // `is_keyframe` here, `is_key_frame` in moonshine-core.
        frame_tx
            .blocking_send(EncodedFrame {
                data: frame.data.to_vec(),
                is_key_frame: frame.is_keyframe,
                frame_number: frame.frame_number,
                rtp_timestamp: frame.rtp_timestamp,
            })
            .map_err(|_| anyhow::anyhow!("video stream closed"))
    })
}

/// Loopback PCM -> moonshine-core's Opus encoder.
///
/// Sends whole periods only. `LoopbackCapture::read` may return a short read
/// (notably right after an overrun recovery), and the vendored PCM bridge
/// *drops* any chunk that is not exactly one Opus frame — silently, at warn
/// level — so a short send would degrade audio with no error anywhere.
fn run_audio(pcm_tx: Option<mpsc::Sender<Vec<i16>>>) -> anyhow::Result<()> {
    let Some(pcm_tx) = pcm_tx else {
        anyhow::bail!("no audio channel for this session");
    };

    let mut capture = LoopbackCapture::open(LOOPBACK_CAPTURE_DEVICE)?;
    let samples = PERIOD_FRAMES * CHANNELS as usize;
    let mut last_audio_log = std::time::Instant::now();
    let mut loudest: u16 = 0;
    let mut periods: u32 = 0;

    loop {
        let mut buf = vec![0i16; samples];
        let mut filled = 0usize;
        while filled < PERIOD_FRAMES {
            let frames = capture.read(&mut buf[filled * CHANNELS as usize..])?;
            if frames == 0 {
                anyhow::bail!("loopback capture returned no frames");
            }
            filled += frames;
        }

        // Is there actually sound in what we captured? The loopback data path
        // has no automated coverage (see the note at the top of audio.rs: an
        // in-process test read all zeros while aplay->arecord through the same
        // cable was fine), so report the peak amplitude periodically. Silence
        // here means the capture side is not receiving the game's audio;
        // non-zero here means the problem is downstream, in Opus or the
        // client's audio stream.
        let peak = buf.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
        loudest = loudest.max(peak);
        periods += 1;
        if last_audio_log.elapsed() >= std::time::Duration::from_secs(30) {
            tracing::debug!("audio: {periods} periods captured, peak amplitude {loudest}");
            last_audio_log = std::time::Instant::now();
            loudest = 0;
            periods = 0;
        }

        pcm_tx
            .blocking_send(buf)
            .map_err(|_| anyhow::anyhow!("audio stream closed"))?;
    }
}

/// Client recovery requests -> the capture's IDR / reset flags.
///
/// Cedar has no reference-frame invalidation API, so `Invalidate` degrades to
/// a full IDR: more expensive than the partial recovery upstream's Vulkan
/// encoder could do, and the only thing this hardware can do.
async fn forward_encoder_control(
    control_rx: Option<mpsc::Receiver<EncoderControl>>,
    idr: IdrRequester,
    reset: ResetRequester,
) {
    let Some(mut control_rx) = control_rx else {
        tracing::warn!("no encoder control channel for this session; client recovery is disabled");
        return;
    };

    while let Some(control) = control_rx.recv().await {
        match control {
            EncoderControl::Idr => {
                tracing::debug!("client requested an IDR frame");
                idr.request();
            }
            EncoderControl::Invalidate { first, last } => {
                tracing::debug!("client could not decode frames {first}..={last}; forcing an IDR");
                idr.request();
            }
            EncoderControl::Reset => {
                tracing::debug!("client resumed the session; resetting the frame counter");
                reset.request();
            }
        }
    }
}

/// Loads (creating on first run) the Moonshine config, with this device's
/// paths and identity rather than upstream's desktop defaults.
fn load_config(userdata: &Path) -> anyhow::Result<Config> {
    let dir = userdata.join("rgsp-cast");

    // `PersistentState` (the pairing database) resolves via `dirs::data_dir`,
    // i.e. `$XDG_DATA_HOME` or `$HOME/.local/share`. On the device HOME is on
    // the rootfs, so without this the pairing is lost whenever the rootfs is
    // reflashed. Never overrides an existing value.
    if std::env::var_os("XDG_DATA_HOME").is_none() {
        std::env::set_var("XDG_DATA_HOME", &dir);
    }

    let path = match std::env::var_os("RGSP_CONFIG") {
        Some(path) => PathBuf::from(path),
        None => dir.join("config.toml"),
    };

    if !path.exists() {
        let config = Config {
            name: "RG SP".to_string(),
            webserver: moonshine_core::webserver::WebserverConfig {
                certificate: dir.join("cert.pem"),
                private_key: dir.join("key.pem"),
                ..Default::default()
            },
            // One entry so Moonlight has something to select. Launching is a
            // no-op here: the "application" is whatever is already on the
            // handheld's screen, which is what the framebuffer capture
            // streams.
            applications: vec![moonshine_core::config::ApplicationConfig {
                title: "RG SP".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };

        let serialized = toml::to_string_pretty(&config)?;
        std::fs::create_dir_all(&dir)?;
        std::fs::write(&path, serialized)?;
        tracing::info!("wrote a default configuration to {}", path.display());
    }

    Config::load_or_create(&path)
        .map_err(|()| anyhow::anyhow!("failed to load the configuration at {}", path.display()))
}

#[cfg(test)]
mod pending_entries_tests {
    use super::normalize_devicename;

    #[test]
    fn roth_placeholder_is_dropped() {
        assert_eq!(normalize_devicename(Some("roth".to_string())), None);
    }

    #[test]
    fn a_real_name_is_kept() {
        assert_eq!(
            normalize_devicename(Some("Steam Deck".to_string())),
            Some("Steam Deck".to_string())
        );
    }

    #[test]
    fn no_name_stays_none() {
        assert_eq!(normalize_devicename(None), None);
    }
}
