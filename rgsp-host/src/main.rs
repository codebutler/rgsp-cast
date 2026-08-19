//! The rgsp-cast daemon: Moonshine's GameStream protocol layer driven by the
//! RG SP's Cedar hardware encoder and the kernel ALSA loopback.
//!
//! Ownership split, since it is not obvious from the crate layout:
//! - `moonshine-core` (vendored) owns pairing, RTSP, RTP+FEC, encryption,
//!   Opus encoding and mDNS. It has no video encoder and no audio source.
//! - `rgsp_host::{capture, video, audio}` own the hardware: H.264 frames out
//!   of the Cedar VE, raw PCM out of the loopback.
//! - This file is the only place the two meet.

use std::net::{IpAddr, SocketAddr, UdpSocket};
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
use rgsp_host::daemon::PidFile;
use rgsp_host::routing::CastSink;
use rgsp_host::status::{DEFAULT_FIFO, Status, StatusWriter};
use rgsp_host::video::{
    IdrRequester, PANEL_HEIGHT, PANEL_WIDTH, ResetRequester, VideoConfig, VideoStream,
};

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
    let status = StatusWriter::new(PathBuf::from(
        std::env::var("RGSP_STATUS_FIFO").unwrap_or_else(|_| DEFAULT_FIFO.to_string()),
    ));

    // Step 1: the pidfile is the mutual exclusion for the whole daemon, so it
    // is taken before anything with a side effect.
    let pid_path = PathBuf::from("/tmp/rgsp/daemon.pid");
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

    // Step 3.
    status.publish(&Status::Starting);

    // Loaded before the runtime exists: it sets `XDG_DATA_HOME`, and mutating
    // the environment with other threads running is the pattern Rust 2024 made
    // `unsafe`. Nothing here needs async.
    let config = match load_config(&userdata) {
        Ok(config) => config,
        Err(e) => {
            tracing::error!("{e:#}");
            return shutdown(status, cast_sink, pidfile, std::process::ExitCode::FAILURE);
        }
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!("failed to start the tokio runtime: {e}");
            return shutdown(status, cast_sink, pidfile, std::process::ExitCode::FAILURE);
        }
    };

    let outcome = runtime.block_on(serve(config, &status));
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
    shutdown(status, cast_sink, pidfile, code)
}

/// Publish `Stopped`, restore audio routing, drop the pidfile. Runs on clean
/// shutdown, on SIGTERM/SIGINT, and on every startup failure after
/// `CastSink::engage` succeeded.
fn shutdown(
    status: StatusWriter,
    cast_sink: CastSink,
    pidfile: PidFile,
    code: std::process::ExitCode,
) -> std::process::ExitCode {
    status.publish(&Status::Stopped);
    if let Err(e) = cast_sink.release() {
        tracing::error!("failed to restore audio routing: {e:#}");
    }
    pidfile.release();
    code
}

async fn serve(config: Config, status: &StatusWriter) -> anyhow::Result<()> {
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
    let paired = client_manager.persistent_state().has_any_client();

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

    // Step 5.
    let idle = idle_status(paired, config.webserver.port);
    status.publish(&idle);
    tracing::info!("rgsp-host is ready and waiting for connections");

    // Step 6.
    let pump = tokio::spawn(session_pump(
        session_manager.clone(),
        client_manager,
        config.webserver.port,
        shutdown.clone(),
        status.clone(),
    ));

    shutdown.wait_shutdown_triggered().await;

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

fn idle_status(paired: bool, http_port: u16) -> Status {
    let addr = lan_ip()
        .map(|ip| ip.to_string())
        .unwrap_or_else(|| "no network".to_string());
    if paired {
        Status::Ready { addr }
    } else {
        // Moonlight's pairing flow appends its own `?uniqueid=`; this is the
        // page the user opens to type the PIN in
        // (`webserver/pairing.rs` logs the full URL once a client starts).
        Status::AwaitingPairing {
            url: format!("http://{addr}:{http_port}/pin"),
        }
    }
}

/// The address a client on the LAN would reach us on. A connected UDP socket
/// picks the interface the kernel would route through without sending
/// anything, which beats guessing at interface names.
fn lan_ip() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    socket.local_addr().ok().map(|addr| addr.ip())
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
    client_manager: ClientManager,
    http_port: u16,
    shutdown: ShutdownManager<ShutdownReason>,
    status: StatusWriter,
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
        // `VideoStream::run` clamps to the panel geometry and logs. The status
        // line reports what the client will actually decode.
        status.publish(&Status::Connected {
            client: "Moonlight".to_string(),
            width: PANEL_WIDTH,
            height: PANEL_HEIGHT,
            fps,
        });
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
            packet_size: context.packet_size,
            fec_percentage: 0,
            minimum_fec_packets: context.minimum_fec_packets,
            // Packetizing moved into moonshine-core, which owns the sockets;
            // nothing in `VideoStream` sends to this address.
            client_addr: SocketAddr::from(([0, 0, 0, 0], 0)),
        });
        let control = tokio::spawn(forward_encoder_control(
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
        control.abort();
        // The pad is dropped with the task, which releases every held button.
        // A client that disconnects mid-press must not leave the emulator
        // seeing a key held down forever.
        if let Some(task) = input_task {
            task.abort();
        }

        if shutdown.is_shutdown_triggered() {
            return;
        }
        tracing::info!("session ended, waiting for the next client");
        // Recomputed rather than cached: a first-time user is unpaired at
        // startup and paired by the time their first session ends, so a cached
        // line would tell them to pair again. This also refreshes the address
        // if DHCP moved us.
        status.publish(&idle_status(
            client_manager.persistent_state().has_any_client(),
            http_port,
        ));
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
