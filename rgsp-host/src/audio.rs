//! ALSA loopback capture for streaming game audio.
//!
//! # Known Gap: Loopback Data Path
//!
//! The loopback DATA path has no automated regression test.
//!
//! An in-process playback test was attempted and abandoned: `writei()` reports
//! frames written and the playback stream reports no error, but the capture side
//! reads all zeros (19200 samples, max magnitude 0). Tried explicit
//! prepare/start orderings, prepare→fill→start, start_threshold via sw_params,
//! several cables and subdevices, varied tone length and amplitude, and
//! accumulating across reads.
//!
//! The hardware path is known good: feeding /dev/urandom through `aplay` into
//! hw:Loopback,0,0 while capturing from hw:Loopback,1,0 yields 96255 non-zero
//! samples across 48128 frames. So this is a defect in how the alsa crate's
//! playback side is driven here, not a broken cable.
//!
//! What IS guarded automatically: parameter negotiation, via the test
//! `both_cable_ends_can_open_with_matching_params`, which exercises snd-aloop's
//! loopback_check_format—the failure mode that produces a silent -EIO when
//! capture and playback disagree on format, rate, or channels.

use alsa::pcm::{Access, Format, HwParams, PCM};
use alsa::{Direction, ValueOr};
use anyhow::{Context, Result};
use tracing::warn;

/// Fixed by what minarch plays. snd-aloop fails the capture side with -EIO
/// if the two ends of the cable disagree on format, rate or channels
/// (aloop.c, loopback_check_format).
pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u32 = 2;
/// 5 ms at 48 kHz - small enough that audio latency stays under the video's.
pub const PERIOD_FRAMES: usize = 240;

/// One capture period must be exactly one Opus frame. Moonshine's PCM bridge
/// drops any chunk whose length differs (`host_source::chunk_len_is_valid`),
/// and it drops it at warn level rather than failing — a mismatch would show
/// up as silent or jittery audio at the client, never as an error. Checked at
/// compile time so it cannot drift.
const _: () = assert!(
    PERIOD_FRAMES == moonshine_core::session::stream::audio::FRAME_FRAMES,
    "capture period must equal Moonshine's Opus frame size"
);

pub struct LoopbackCapture {
    pcm: PCM,
    overrun_count: u32,
}

/// `LoopbackCapture` can be sent to other threads because the ALSA PCM handle
/// (`pcm::PCM`) is thread-safe for the single capture stream we create. The
/// kernel loopback device is a real hardware endpoint with exclusive access
/// serialization. Unlike C state that racily accesses process-global buffers,
/// ALSA's Rust binding handles synchronization internally.
unsafe impl Send for LoopbackCapture {}

impl LoopbackCapture {
    pub fn open(device: &str) -> Result<LoopbackCapture> {
        let pcm = PCM::new(device, Direction::Capture, false)
            .with_context(|| format!("opening {device}"))?;

        {
            let hwp = HwParams::any(&pcm)?;
            hwp.set_access(Access::RWInterleaved)
                .context("setting access to RWInterleaved")?;
            hwp.set_format(Format::s16())
                .context("setting format to S16_LE")?;
            hwp.set_channels(CHANNELS)
                .context(format!("setting channels to {}", CHANNELS))?;
            hwp.set_rate(SAMPLE_RATE, ValueOr::Nearest)
                .context(format!("setting rate to {} Hz", SAMPLE_RATE))?;
            hwp.set_period_size_near(PERIOD_FRAMES as i64, ValueOr::Nearest)
                .context("setting period size")?;
            // Four periods is 20 ms of slack at 48 kHz, and this CPU is busy
            // capturing and encoding video on the same cores: any scheduling
            // hiccup longer than that overruns the capture, which costs a
            // prepare()/start() stream reset. Sustained, that is not glitchy
            // audio, it is no usable audio at all - the log fills with
            // "audio buffer overrun ... recovering" every few tens of ms.
            // Deeper buffer, same 5 ms period: reads stay small and prompt,
            // but jitter has somewhere to go. This costs nothing in latency
            // that matters, because we still consume every period as it
            // arrives; it only bounds how far behind we may briefly fall.
            hwp.set_buffer_size_near((PERIOD_FRAMES * 16) as i64)
                .context("setting buffer size")?;
            pcm.hw_params(&hwp)?;
        }

        // Verify that ALSA granted what we asked for. snd-aloop fails the capture
        // side with -EIO when the two ends of the cable disagree on format, rate or
        // channels (aloop.c loopback_check_format), so mismatches are fatal.
        {
            let hwp = pcm.hw_params_current().context("reading current hardware parameters")?;

            let actual_rate = hwp.get_rate().context("reading rate from hardware")?;
            if actual_rate != SAMPLE_RATE {
                return Err(anyhow::anyhow!(
                    "rate negotiation failed: requested {}, got {}",
                    SAMPLE_RATE,
                    actual_rate
                ));
            }

            let actual_channels = hwp.get_channels().context("reading channels from hardware")?;
            if actual_channels != CHANNELS {
                return Err(anyhow::anyhow!(
                    "channel count negotiation failed: requested {}, got {}",
                    CHANNELS,
                    actual_channels
                ));
            }

            let actual_format = hwp.get_format().context("reading format from hardware")?;
            if actual_format != Format::s16() {
                return Err(anyhow::anyhow!(
                    "format negotiation failed: requested S16_LE, got {:?}",
                    actual_format
                ));
            }

            let period_frames = hwp.get_period_size().context("reading period size from hardware")?;
            let buffer_frames = hwp.get_buffer_size().context("reading buffer size from hardware")?;
            tracing::debug!(
                "loopback capture negotiated: rate={} Hz channels={} period={} frames buffer={} frames",
                actual_rate,
                actual_channels,
                period_frames,
                buffer_frames
            );
        }

        // snd-aloop rejects the implicit start that snd_pcm_readi() would do,
        // returning -EIO. Prepare and start explicitly.
        pcm.prepare().context("prepare")?;
        pcm.start().context("start")?;

        Ok(LoopbackCapture {
            pcm,
            overrun_count: 0,
        })
    }

    /// Reads interleaved s16 frames. `buf.len()` must be a multiple of CHANNELS.
    /// Returns the number of *frames* read.
    pub fn read(&mut self, buf: &mut [i16]) -> Result<usize> {
        debug_assert_eq!(
            buf.len() % CHANNELS as usize,
            0,
            "buffer length must be a multiple of CHANNELS"
        );

        let io = self.pcm.io_i16()?;
        loop {
            match io.readi(buf) {
                Ok(frames) => return Ok(frames),
                Err(e) => {
                    // An overrun means we fell behind; recover and keep going
                    // rather than tearing down the stream.
                    if e.errno() == libc::EPIPE {
                        self.overrun_count += 1;
                        // Log the first few, then every 500th: an overrun
                        // storm used to emit thousands of identical lines,
                        // which buried the video diagnostics next to them.
                        if self.overrun_count <= 3 || self.overrun_count % 500 == 0 {
                            warn!(
                                "audio buffer overrun #{}: recovering",
                                self.overrun_count
                            );
                        }
                        self.pcm.prepare()?;
                        self.pcm.start()?;
                        continue;
                    }
                    return Err(e).context("readi");
                }
            }
        }
    }

    /// Returns the number of buffer overruns encountered during capture.
    pub fn overrun_count(&self) -> u32 {
        self.overrun_count
    }
}
