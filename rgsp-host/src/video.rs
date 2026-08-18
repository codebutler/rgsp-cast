use crate::capture::Capture;
use anyhow::{anyhow, Result};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// GameStream video runs on a 90 kHz RTP clock.
const RTP_CLOCK_HZ: u64 = 90_000;

/// The Cedar video engine on the RG SP is fixed at 720x480: `Capture::open`
/// validates against this and fails loudly on anything else (VE scaling is
/// an unexercised path). Moonlight clients routinely negotiate 1280x720, so
/// the panel geometry — not the negotiated resolution — is what `Capture`
/// actually opens with.
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
    pub packet_size: usize,
    pub fec_percentage: u8,
    pub minimum_fec_packets: u32,
    pub client_addr: SocketAddr,
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
    /// The clamp to the panel's fixed 720x480 geometry happens here, at the
    /// `Capture::open` call: `self.cfg.width`/`height` (the negotiated
    /// Moonlight resolution) are intentionally not passed through.
    pub fn run(self, mut send: impl FnMut(EncodedFrameRef<'_>) -> Result<()>) -> Result<()> {
        let mut capture = Capture::open(PANEL_WIDTH, PANEL_HEIGHT, self.cfg.fps, self.cfg.bitrate)
            .map_err(|e| anyhow!("Capture::open: {e}"))?;

        // The handheld's own status pill can't show a cast indicator during
        // gameplay without patching minarch into `.system/`, so the marker
        // goes into the outgoing stream instead: only the TV sees it.
        capture.set_overlay(true);

        if (self.cfg.width, self.cfg.height) != (PANEL_WIDTH, PANEL_HEIGHT) {
            tracing::info!(
                "negotiated resolution {}x{} clamped to panel geometry {}x{}",
                self.cfg.width,
                self.cfg.height,
                PANEL_WIDTH,
                PANEL_HEIGHT,
            );
        }

        let mut frame_number: u64 = 0;

        loop {
            if self.reset.swap(false, Ordering::Relaxed) {
                frame_number = 0;
                capture.request_idr();
            } else if self.idr.swap(false, Ordering::Relaxed) {
                capture.request_idr();
            }

            // A failure from `Capture::next` is terminal: the capture is
            // dead and must simply be dropped, not retried.
            let frame = capture.next()?;
            let rtp_timestamp = rtp_timestamp_for(frame_number, self.cfg.fps);

            send(EncodedFrameRef {
                data: frame.data,
                is_keyframe: frame.is_keyframe,
                frame_number: frame_number as u32,
                rtp_timestamp,
            })?;

            frame_number += 1;
        }
    }
}
