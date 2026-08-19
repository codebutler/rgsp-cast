use crate::capture::Capture;
use anyhow::{anyhow, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// GameStream video runs on a 90 kHz RTP clock.
const RTP_CLOCK_HZ: u64 = 90_000;

/// The Cedar video engine on the RG SP is fixed at 720x480: `Capture::open`
/// validates against this and fails loudly on anything else (VE scaling is
/// an unexercised path). Moonlight clients routinely negotiate 1280x720, so
/// the panel geometry — not the negotiated resolution — is what `Capture`
/// actually opens with.
/// Ceiling for the encoder bitrate, regardless of what the client negotiates.
/// 6 Mbps is already generous for 720x480 content and keeps keyframes small
/// enough to survive the handheld's WiFi.
pub const BITRATE_CEILING: u32 = 6_000_000;

pub const PANEL_WIDTH: u32 = 720;
pub const PANEL_HEIGHT: u32 = 480;

pub fn rtp_timestamp_for(frame_number: u64, fps: u32) -> u32 {
    ((frame_number * RTP_CLOCK_HZ) / fps as u64) as u32
}

#[derive(Clone, Debug)]
pub struct VideoConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
    // No packet_size / fec_percentage / minimum_fec_packets / client_addr here:
    // packetizing and the sockets belong to moonshine-core, which takes those
    // from `config.stream.video` and the RTSP-negotiated context. They used to
    // be carried here as well, written once and never read - and the stale
    // `fec_percentage: 0` in particular read as "FEC is off" when the live
    // value is 20.
}

/// One encoded frame plus the metadata moonshine-core's packetizer needs.
///
/// A borrowed view (`data: &'a [u8]`, no allocation) into the `Capture`
/// frame that produced it — valid only for the duration of the `send` call
/// in `VideoStream::run`. The wiring that hands this to
/// `moonshine_core::session::manager::SessionManager::video_frame_sender()`
/// (not part of this task; see `run`'s doc comment) will need to clone
/// `data` into an owned `EncodedFrame` before sending across that channel.
pub struct EncodedFrameRef<'a> {
    pub data: &'a [u8],
    pub is_keyframe: bool,
    pub frame_number: u32,
    pub rtp_timestamp: u32,
}

/// Task 10's wiring for both requesters below: spawn a task that awaits
/// `moonshine_core::session::manager::SessionManager::encoder_control_receiver()`
/// (an `mpsc::Receiver<EncoderControl>`, taken once — symmetric with
/// `video_frame_sender()`) and on each message received, map:
/// - `EncoderControl::Idr` -> `IdrRequester::request()`
/// - `EncoderControl::Invalidate { .. }` -> `IdrRequester::request()` too —
///   Cedar has no reference-invalidation API, so this project degrades a
///   partial-loss recovery to a full keyframe rather than a cheaper partial
///   one. The `{first, last}` frame range is intentionally discarded.
/// - `EncoderControl::Reset` -> `ResetRequester::request()`
#[derive(Clone)]
pub struct IdrRequester {
    flag: Arc<AtomicBool>,
}

impl IdrRequester {
    /// Flags the next `run()` iteration to call `Capture::request_idr()`.
    pub fn request(&self) {
        self.flag.store(true, Ordering::Relaxed);
    }
}

/// See `IdrRequester`'s doc comment for the wiring this pairs with.
#[derive(Clone)]
pub struct ResetRequester {
    flag: Arc<AtomicBool>,
}

impl ResetRequester {
    /// Flags the next `run()` iteration to restart the frame counter from
    /// zero and force an IDR — a resuming client is a fresh Moonlight
    /// session that expects frame numbers to start at 1; without the reset
    /// it sees the running counter as a huge frame gap and reports a poor
    /// connection. It also needs a decodable starting frame, hence the IDR.
    pub fn request(&self) {
        self.flag.store(true, Ordering::Relaxed);
    }
}

pub struct VideoStream {
    cfg: VideoConfig,
    idr: Arc<AtomicBool>,
    reset: Arc<AtomicBool>,
}

impl VideoStream {
    pub fn new(cfg: VideoConfig) -> Self {
        VideoStream {
            cfg,
            idr: Arc::new(AtomicBool::new(false)),
            reset: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn idr_requester(&self) -> IdrRequester {
        IdrRequester {
            flag: self.idr.clone(),
        }
    }

    pub fn reset_requester(&self) -> ResetRequester {
        ResetRequester {
            flag: self.reset.clone(),
        }
    }

    /// Capture -> encode -> send, one frame at a time.
    ///
    /// Runs on a dedicated blocking thread: `Capture::next` sleeps until the
    /// frame deadline and must not occupy a tokio worker.
    ///
    /// `send` receives one `EncodedFrameRef` per call: the raw encoded
    /// (Annex-B) bitstream plus the keyframe flag, frame number, and RTP
    /// timestamp this loop already tracks. Packetizing (RTP + FEC + AES-GCM)
    /// happens on the other side of `send`, inside moonshine-core's
    /// protocol layer — the eventual wiring is
    /// `moonshine_core::session::manager::SessionManager::video_frame_sender()`,
    /// which is not called from this task's files (see module docs).
    ///
    /// The panel is always 720x480, but the stream must come out at the
    /// resolution the client negotiated: a GameStream client rejects every
    /// frame of a differently-sized stream, sits in "Waiting for IDR frame"
    /// and drops the connection - verified against moonlight-qt, where the
    /// same host and binary streams fine at 720x480 and fails at 1080p.
    /// The VE scales input->destination in hardware, so the capture stays at
    /// panel geometry and the encoder emits the negotiated size.
    pub fn run(self, mut send: impl FnMut(EncodedFrameRef<'_>) -> Result<()>) -> Result<()> {
        // Moonlight asks for a bitrate sized to the resolution it negotiated -
        // 20 Mbps at 1080p by default. The actual picture is a 720x480 panel
        // upscaled, so there is no detail above roughly SD to spend those bits
        // on: all a higher ceiling buys is enormous keyframes that this
        // device's WiFi then drops, which is what makes the stream freeze.
        // Cap it at a rate generous for the real source resolution.
        let bitrate = self.cfg.bitrate.min(BITRATE_CEILING);
        if bitrate != self.cfg.bitrate {
            tracing::info!(
                "requested {} bps capped to {} bps for a {}x{} source",
                self.cfg.bitrate,
                bitrate,
                PANEL_WIDTH,
                PANEL_HEIGHT,
            );
        }

        let mut capture = Capture::open_scaled(
            PANEL_WIDTH,
            PANEL_HEIGHT,
            self.cfg.width,
            self.cfg.height,
            self.cfg.fps as i32,
            bitrate as i32,
        )
        .map_err(|e| anyhow!("Capture::open: {e}"))?;

        if (self.cfg.width, self.cfg.height) != (PANEL_WIDTH, PANEL_HEIGHT) {
            tracing::info!(
                "panel {}x{} scaled by the VE to the negotiated {}x{}",
                PANEL_WIDTH,
                PANEL_HEIGHT,
                self.cfg.width,
                self.cfg.height,
            );
        }

        let mut frame_number: u64 = 0;
        let mut last_latency_log = Instant::now();

        // A keyframe at the negotiated resolution is one to two orders of
        // magnitude larger than a P-frame (measured: 85 KB / 75 packets at
        // 1080p versus ~2 KB). A client that loses any one of those packets
        // asks for another IDR, which is just as likely to lose a packet, so
        // honouring every request turns one dropped packet into a permanent
        // storm of huge frames and the picture freezes. Coalesce: at most one
        // forced IDR per interval, and let the requests in between fall on the
        // floor - the client re-asks anyway if it still needs one.
        const MIN_IDR_INTERVAL: Duration = Duration::from_millis(750);
        let mut last_idr = Instant::now() - MIN_IDR_INTERVAL;

        loop {
            if self.reset.swap(false, Ordering::Relaxed) {
                // A reset is a new session, not loss recovery: always honour it.
                frame_number = 0;
                capture.request_idr();
                last_idr = Instant::now();
            } else if self.idr.swap(false, Ordering::Relaxed) {
                if last_idr.elapsed() >= MIN_IDR_INTERVAL {
                    capture.request_idr();
                    last_idr = Instant::now();
                } else {
                    tracing::trace!("IDR request coalesced");
                }
            }

            // A failure from `Capture::next` is terminal: the capture is
            // dead and must simply be dropped, not retried.
            let capture_started = Instant::now();
            let frame = capture.next()?;

            // Frame numbers on the wire are 1-based. Upstream moonshine's
            // pipeline increments before use (pipeline/mod.rs:307 then :322),
            // and the client half treats index 0 as "no frame yet" - see the
            // `first.max(1) - 1` display-order mapping. Sending a frame 0
            // means the client never accepts the first IDR, so the decoder
            // is never initialised and no picture is ever shown, even though
            // packets keep arriving and it keeps asking for another IDR.
            frame_number += 1;
            let rtp_timestamp = rtp_timestamp_for(frame_number, self.cfg.fps);

            // Host-side latency budget, sampled once a second: how long the
            // frame spent being captured and encoded, and how long the send
            // below then blocked because the packet queue was full. If both
            // are small, the delay the viewer sees is in the network or the
            // client's own buffering, not here.
            let encode_ms = capture_started.elapsed().as_secs_f32() * 1000.0;
            let send_started = Instant::now();

            send(EncodedFrameRef {
                data: frame.data,
                is_keyframe: frame.is_keyframe,
                frame_number: frame_number as u32,
                rtp_timestamp,
            })?;

            let blocked_ms = send_started.elapsed().as_secs_f32() * 1000.0;
            if last_latency_log.elapsed() >= Duration::from_secs(30) {
                tracing::debug!("latency: encode {encode_ms:.1} ms, queue wait {blocked_ms:.1} ms");
                last_latency_log = Instant::now();
            }
        }
    }
}
