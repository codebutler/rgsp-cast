//! PCM source for hosts that capture audio outside this crate (e.g. the
//! ALSA loopback capture on the Anbernic RG SP), replacing the deleted
//! PulseAudio server as the `AudioFrame` producer.
//!
//! Unlike video, Moonshine's own Opus encoder (`encoder.rs`) was never
//! deleted: it still takes raw f32 PCM `AudioFrame`s and does the real
//! Opus encode, CBR, FEC and RTP packetization exactly as upstream wrote
//! it. Only the *producer* — PulseAudio, which mixed application audio —
//! had no replacement once it was removed. This file is that replacement:
//! it converts i16 PCM handed in from the host into `AudioFrame`s and
//! drives the same recycle-pool contract the deleted PulseAudio server
//! did (see `git show f23d52e^:.../pulse_server/mod.rs::clock_tick`,
//! preserved in history as the upstream-blessed reference).
//!
//! Kept in its own file, separate from `mod.rs`, so future `git subtree
//! pull`s of upstream Moonshine never have to merge through it: `mod.rs`
//! only gains a `mod host_source;` line and a call site, both additions
//! upstream cannot also touch.


use async_shutdown::ShutdownManager;
use tokio::sync::mpsc;

use crate::session::manager::SessionShutdownReason;

use super::{AudioFrame, CAPTURE_SAMPLE_RATE};

/// Duration of one Opus frame Moonlight negotiates, in milliseconds.
const FRAME_DURATION_MS: u32 = 5;

/// Number of PCM frames (samples per channel) in one Opus frame. Derived
/// from `CAPTURE_SAMPLE_RATE` rather than hardcoded, so a future change to
/// either constant can't silently desync from the other and mistime frames
/// at the client.
///
/// `pub` (re-exported from `audio/mod.rs`) so the host's capture period can
/// be checked against it at compile time: `rgsp_host::audio::PERIOD_FRAMES`
/// must equal this, or every chunk the host sends is dropped by
/// `chunk_len_is_valid` below — silent audio, no error.
pub const FRAME_FRAMES: usize = (CAPTURE_SAMPLE_RATE / 1000 * FRAME_DURATION_MS) as usize;

/// Convert one PCM chunk into an `AudioFrame`, reusing a buffer from the
/// recycle pool if one is ready, falling back to a stashed `spare_frame` (to
/// avoid losing an allocation from a previous full `frame_tx`), and only
/// allocating fresh if both are exhausted. Mirrors the deleted PulseAudio
/// server's `clock_tick`, whose contract this replaces.
///
/// Normalizes by dividing by `i16::MAX` (32767), the conventional approach:
/// symmetric divisor, no branch. This means `i16::MIN` (-32768) maps to
/// slightly more negative than -1.0 (~-1.0000305) rather than exactly -1.0 —
/// negligible in practice, and matches what most PCM-to-float converters do.
fn build_frame(
	pcm: &[i16],
	frame_recycle_rx: &crossbeam_channel::Receiver<AudioFrame>,
	spare_frame: &mut Option<AudioFrame>,
) -> AudioFrame {
	let mut frame = frame_recycle_rx
		.try_recv()
		.ok()
		.or_else(|| spare_frame.take())
		.unwrap_or(AudioFrame {
			buf: Vec::new(),
			capture_ts_ms: 0,
		});

	frame.buf.clear();
	frame.buf.extend(pcm.iter().map(|&s| s as f32 / i16::MAX as f32));
	// Not tracked on this path — `capture_ts_ms` is `#[allow(dead_code)]`
	// downstream (the encoder never reads it), and the host doesn't hand in
	// a capture timestamp for the simple `Vec<i16>` channel this bridges from.
	frame.capture_ts_ms = 0;
	frame
}

/// Hand a built frame to the encoder, stashing it as `spare_frame` instead of
/// dropping it if the channel is full — same tradeoff the deleted PulseAudio
/// server made: keep the allocation, drop the (now stale) samples instead.
/// Returns `false` once the encoder has disconnected and the bridge should stop.
fn send_frame(
	frame_tx: &crossbeam_channel::Sender<AudioFrame>,
	frame: AudioFrame,
	spare_frame: &mut Option<AudioFrame>,
) -> bool {
	match frame_tx.try_send(frame) {
		Ok(()) => true,
		Err(crossbeam_channel::TrySendError::Full(frame)) => {
			*spare_frame = Some(frame);
			true
		},
		Err(crossbeam_channel::TrySendError::Disconnected(_)) => false,
	}
}

/// Whether a PCM chunk is exactly one Opus frame's worth of samples for the
/// given channel count. A short or long chunk would desync Opus's fixed
/// 5 ms framing, so anything else must be rejected rather than encoded.
///
/// KNOWN LIMITATION: `expected_samples` scales with the *negotiated* channel
/// count, but the host's capture is fixed at stereo. A client that negotiates
/// 5.1 therefore has every chunk rejected here — silent audio, with nothing
/// louder than the warning below to say so. Moonlight defaults to stereo, so
/// this has not bitten; fixing it means upmixing host-side or pinning the
/// negotiation to stereo.
fn chunk_len_is_valid(pcm: &[i16], expected_samples: usize) -> bool {
	pcm.len() == expected_samples
}

/// Bridge host-supplied interleaved i16 PCM into the encoder's `AudioFrame`
/// channel. `channels` sizes the expected chunk length; chunks of any other
/// size are dropped with a warning rather than fed to the encoder (see
/// `chunk_len_is_valid`).
pub(crate) fn spawn_pcm_bridge(
	mut pcm_rx: mpsc::Receiver<Vec<i16>>,
	channels: u8,
	frame_tx: crossbeam_channel::Sender<AudioFrame>,
	frame_recycle_rx: crossbeam_channel::Receiver<AudioFrame>,
	start: super::AudioStartGate,
	stop: ShutdownManager<SessionShutdownReason>,
) {
	tokio::spawn(async move {
		start.wait().await;

		// Trigger session shutdown if we exit unexpectedly.
		let _stop_token = stop.trigger_shutdown_token(SessionShutdownReason::AudioSourceStopped);
		let _delay_stop = stop.delay_shutdown_token();

		let expected_samples = FRAME_FRAMES * channels as usize;
		let mut spare_frame: Option<AudioFrame> = None;

		while !stop.is_shutdown_triggered() {
			let pcm = match stop.wrap_cancel(pcm_rx.recv()).await {
				Ok(Some(pcm)) => pcm,
				Ok(None) => {
					tracing::debug!("PCM source channel closed.");
					break;
				},
				Err(_) => break,
			};

			if !chunk_len_is_valid(&pcm, expected_samples) {
				tracing::warn!(
					"Dropping PCM chunk of {} sample(s), expected {expected_samples}.",
					pcm.len()
				);
				continue;
			}

			let frame = build_frame(&pcm, &frame_recycle_rx, &mut spare_frame);
			if !send_frame(&frame_tx, frame, &mut spare_frame) {
				tracing::debug!("Audio encoder stopped, exiting PCM bridge.");
				break;
			}
		}

		tracing::debug!("PCM bridge stopped.");
	});
}

#[cfg(test)]
mod tests {
	use super::*;

	fn frame_of(buf: Vec<f32>) -> AudioFrame {
		AudioFrame { buf, capture_ts_ms: 0 }
	}

	#[test]
	fn frame_frames_is_five_ms_derived_from_capture_rate() {
		assert_eq!(
			FRAME_FRAMES,
			(CAPTURE_SAMPLE_RATE / 1000 * FRAME_DURATION_MS) as usize,
			"must be derived from the vendored constants, not a bare literal"
		);
		assert_eq!(FRAME_FRAMES, 240, "Moonlight negotiates 5 ms Opus frames at 48 kHz");
	}

	#[test]
	fn chunk_len_is_valid_accepts_exact_length() {
		assert!(chunk_len_is_valid(&[0i16; 480], 480));
	}

	#[test]
	fn chunk_len_is_valid_rejects_short_chunk() {
		assert!(!chunk_len_is_valid(&[0i16; 479], 480));
	}

	#[test]
	fn chunk_len_is_valid_rejects_long_chunk() {
		assert!(!chunk_len_is_valid(&[0i16; 481], 480));
	}

	#[test]
	fn chunk_len_is_valid_rejects_empty_slice() {
		assert!(!chunk_len_is_valid(&[], 480));
	}

	#[test]
	fn converts_i16_to_normalized_f32() {
		let (_recycle_tx, recycle_rx) = crossbeam_channel::bounded::<AudioFrame>(1);
		let mut spare = None;
		let pcm = [i16::MAX, i16::MIN, 0];
		let frame = build_frame(&pcm, &recycle_rx, &mut spare);

		assert_eq!(frame.buf.len(), pcm.len());
		assert!((frame.buf[0] - 1.0).abs() < 1e-6, "max i16 should normalize to ~1.0");
		assert!((frame.buf[1] - (-1.0)).abs() < 1e-4, "min i16 should normalize to ~-1.0 (see build_frame's doc comment)");
		assert_eq!(frame.buf[2], 0.0);
	}

	#[test]
	fn reuses_a_recycled_buffer_before_allocating() {
		let (recycle_tx, recycle_rx) = crossbeam_channel::bounded::<AudioFrame>(1);
		recycle_tx.send(frame_of(vec![9.0; 8])).unwrap();
		let mut spare = None;
		let pcm = [1i16, 2, 3, 4];

		let frame = build_frame(&pcm, &recycle_rx, &mut spare);

		assert_eq!(frame.buf.len(), 4, "stale recycled contents must not leak into the new frame");
		assert!(recycle_rx.try_recv().is_err(), "the recycled buffer should have been drained");
	}

	#[test]
	fn falls_back_to_spare_frame_when_recycle_pool_is_empty() {
		let (_recycle_tx, recycle_rx) = crossbeam_channel::bounded::<AudioFrame>(1);
		let mut spare = Some(frame_of(vec![0.0; 100]));
		let pcm = [5i16, 6];

		let frame = build_frame(&pcm, &recycle_rx, &mut spare);

		assert_eq!(frame.buf.len(), 2);
		assert!(spare.is_none(), "the spare frame should be consumed, not left stashed");
	}

	#[test]
	fn allocates_fresh_when_both_recycle_pool_and_spare_are_empty() {
		let (_recycle_tx, recycle_rx) = crossbeam_channel::bounded::<AudioFrame>(1);
		let mut spare = None;
		let pcm = [1i16, 2, 3];

		let frame = build_frame(&pcm, &recycle_rx, &mut spare);

		assert_eq!(frame.buf.len(), 3);
	}

	#[test]
	fn stashes_frame_as_spare_when_encoder_channel_is_full() {
		let (frame_tx, frame_rx) = crossbeam_channel::bounded::<AudioFrame>(1);
		frame_tx.try_send(frame_of(vec![0.0; 1])).unwrap();
		let mut spare = None;

		let ok = send_frame(&frame_tx, frame_of(vec![1.0, 2.0]), &mut spare);

		assert!(ok, "a full encoder channel is not a fatal error for the bridge");
		assert!(spare.is_some(), "the frame's allocation should be stashed rather than dropped");
		assert_eq!(spare.unwrap().buf, vec![1.0, 2.0]);
		drop(frame_rx);
	}

	#[test]
	fn stops_when_encoder_channel_is_disconnected() {
		let (frame_tx, frame_rx) = crossbeam_channel::bounded::<AudioFrame>(1);
		drop(frame_rx);
		let mut spare = None;

		let ok = send_frame(&frame_tx, frame_of(vec![1.0]), &mut spare);

		assert!(!ok, "a disconnected encoder channel must stop the bridge");
	}
}
