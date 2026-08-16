use alsa::pcm::{Access, Format, HwParams, PCM};
use alsa::{Direction, ValueOr};
use anyhow::{Context, Result};

/// Fixed by what minarch plays. snd-aloop fails the capture side with -EIO
/// if the two ends of the cable disagree on format, rate or channels
/// (aloop.c, loopback_check_format).
pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u32 = 2;
/// 5 ms at 48 kHz - small enough that audio latency stays under the video's.
pub const PERIOD_FRAMES: usize = 240;

pub struct LoopbackCapture {
    pcm: PCM,
}

impl LoopbackCapture {
    pub fn open(device: &str) -> Result<LoopbackCapture> {
        let pcm = PCM::new(device, Direction::Capture, false)
            .with_context(|| format!("opening {device}"))?;

        {
            let hwp = HwParams::any(&pcm)?;
            hwp.set_access(Access::RWInterleaved)?;
            hwp.set_format(Format::s16())?;
            hwp.set_channels(CHANNELS)?;
            hwp.set_rate(SAMPLE_RATE, ValueOr::Nearest)?;
            hwp.set_period_size_near(PERIOD_FRAMES as i64, ValueOr::Nearest)?;
            hwp.set_buffer_size_near((PERIOD_FRAMES * 4) as i64)?;
            pcm.hw_params(&hwp)?;
        }

        // snd-aloop rejects the implicit start that snd_pcm_readi() would do,
        // returning -EIO. Prepare and start explicitly.
        pcm.prepare().context("prepare")?;
        pcm.start().context("start")?;

        Ok(LoopbackCapture { pcm })
    }

    /// Reads interleaved s16 frames. `buf.len()` must be a multiple of CHANNELS.
    /// Returns the number of *frames* read.
    pub fn read(&mut self, buf: &mut [i16]) -> Result<usize> {
        let io = self.pcm.io_i16()?;
        loop {
            match io.readi(buf) {
                Ok(frames) => return Ok(frames),
                Err(e) => {
                    // An overrun means we fell behind; recover and keep going
                    // rather than tearing down the stream.
                    if e.errno() == libc::EPIPE {
                        self.pcm.prepare()?;
                        self.pcm.start()?;
                        continue;
                    }
                    return Err(e).context("readi");
                }
            }
        }
    }
}
