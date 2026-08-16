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

#[derive(Clone)]
pub struct IdrRequester {
    flag: Arc<AtomicBool>,
}

impl IdrRequester {
    pub fn request(&self) {
        self.flag.store(true, Ordering::Relaxed);
    }
}

pub struct VideoStream {
    cfg: VideoConfig,
    idr: Arc<AtomicBool>,
}

impl VideoStream {
    pub fn new(cfg: VideoConfig) -> Self {
        VideoStream {
            cfg,
            idr: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn idr_requester(&self) -> IdrRequester {
        IdrRequester {
            flag: self.idr.clone(),
        }
    }

    /// Capture -> encode -> send, one frame at a time.
    ///
    /// Runs on a dedicated blocking thread: `Capture::next` sleeps until the
    /// frame deadline and must not occupy a tokio worker.
    ///
    /// `send` receives the raw encoded (Annex-B) bitstream for one frame per
    /// call. Packetizing (RTP + FEC + AES-GCM) happens on the other side of
    /// `send`, inside moonshine-core's protocol layer, which also owns the
    /// per-frame metadata (keyframe flag, frame number, RTP timestamp) needed
    /// to build an `EncodedFrame` — this loop only supplies bytes.
    ///
    /// The clamp to the panel's fixed 720x480 geometry happens here, at the
    /// `Capture::open` call: `self.cfg.width`/`height` (the negotiated
    /// Moonlight resolution) are intentionally not passed through.
    pub fn run(self, mut send: impl FnMut(&[u8]) -> Result<()>) -> Result<()> {
        let mut capture = Capture::open(PANEL_WIDTH, PANEL_HEIGHT, self.cfg.fps, self.cfg.bitrate)
            .map_err(|e| anyhow!("Capture::open: {e}"))?;

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
            if self.idr.swap(false, Ordering::Relaxed) {
                capture.request_idr();
            }

            // A failure from `Capture::next` is terminal: the capture is
            // dead and must simply be dropped, not retried.
            let frame = capture.next()?;
            let _rtp = rtp_timestamp_for(frame_number, self.cfg.fps);

            send(frame.data)?;

            frame_number += 1;
        }
    }
}
