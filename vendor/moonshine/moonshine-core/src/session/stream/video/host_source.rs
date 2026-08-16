//! Frame source for hosts that encode outside this crate (e.g. the Cedar
//! video engine on the Anbernic RG SP), replacing the deleted Vulkan
//! `VideoPipeline`.
//!
//! Kept in its own file, separate from `mod.rs`, so future `git subtree
//! pull`s of upstream Moonshine never have to merge through it: `mod.rs`
//! only gains a `mod host_source;` line and a call site, both additions
//! upstream cannot also touch.

use std::sync::Arc;

use async_shutdown::ShutdownManager;
use tokio::sync::{Notify, broadcast, mpsc};

use crate::session::SessionKeysReceiver;
use crate::session::manager::SessionShutdownReason;

use super::packetizer::Packetizer;
use super::shard_batch::ShardBatch;
use super::{VideoStreamConfig, VideoStreamContext};

/// One encoded video frame, produced outside the crate by the host's
/// hardware encoder and fed into `VideoStream::start`'s `frame_rx` for
/// packetizing.
///
/// Replaces the deleted Vulkan `VideoPipeline`'s `ExportedFrame` as the
/// frame source: the host now supplies pre-encoded bytes directly instead
/// of exporting DMA-BUFs for GPU encode.
pub struct EncodedFrame {
	pub data: Vec<u8>,
	pub is_key_frame: bool,
	pub frame_number: u32,
	pub rtp_timestamp: u32,
}

/// A client recovery request, mapped from Moonlight's three independent
/// signals (IDR, reference-frame invalidation, stream reset) into one
/// channel for the host's encoder to consume.
///
/// Collapsed to one enum instead of exposing the three raw broadcast
/// channels `VideoStreamHandle` uses internally, because the Cedar video
/// engine (the only encoder this project drives) can't act on them
/// differently anyway — see the variant docs below.
#[derive(Debug, Clone, Copy)]
pub enum EncoderControl {
	/// Client reported it can't decode; encode the next frame as a
	/// keyframe. Maps directly to `Capture::request_idr()`.
	Idr,
	/// Client reported the inclusive `[first, last]` frame-index range it
	/// couldn't decode. Upstream's Vulkan encoder could drop just the
	/// affected reference frames and predict from a surviving one; the C
	/// capture layer has no equivalent API, so this degrades to a full IDR
	/// — more expensive than upstream's cheap path, but correct.
	Invalidate { first: u32, last: u32 },
	/// A client is resuming an already-running session. The host's frame
	/// counter (owned by `rgsp_host::video::VideoStream`, not this crate)
	/// must restart from zero, and the resumed client needs a decodable
	/// starting frame — so this also implies an IDR.
	Reset,
}

/// Packetize each `EncodedFrame` from the host's encoder into `ShardBatch`es
/// and forward them to the packet handler; also the sole consumer of the
/// IDR / reference-invalidation / reset broadcast channels, mapping each to
/// an `EncoderControl` and forwarding it to the host via `control_tx`.
///
/// This is a minimal replacement for the deleted Vulkan `VideoPipeline`'s
/// encode+packetize loop, covering only packetizing and control-request
/// relay: the host now supplies pre-encoded frames, so there is no GPU
/// encode, HDR metadata, or per-frame latency stats to produce here.
#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_packetize_frames(
	mut frame_rx: mpsc::Receiver<EncodedFrame>,
	config: VideoStreamConfig,
	context: VideoStreamContext,
	keys_rx: SessionKeysReceiver,
	packet_tx: mpsc::Sender<ShardBatch>,
	mut idr_rx: broadcast::Receiver<()>,
	mut invalidate_rx: broadcast::Receiver<(u32, u32)>,
	mut reset_rx: broadcast::Receiver<()>,
	control_tx: mpsc::Sender<EncoderControl>,
	start: Arc<Notify>,
	stop_session_manager: ShutdownManager<SessionShutdownReason>,
) {
	tokio::spawn(async move {
		start.notified().await;

		let mut packetizer = Packetizer::new(context.encrypt_video, keys_rx);
		packetizer.warm_up(config.fec_percentage, context.minimum_fec_packets);
		let mut sequence_number: u32 = 0;

		// Trigger session shutdown if we exit unexpectedly.
		let _stop_token = stop_session_manager.trigger_shutdown_token(SessionShutdownReason::VideoEncoderStopped);
		let _delay_stop = stop_session_manager.delay_shutdown_token();

		while !stop_session_manager.is_shutdown_triggered() {
			tokio::select! {
				frame = stop_session_manager.wrap_cancel(frame_rx.recv()) => {
					let frame = match frame {
						Ok(Some(frame)) => frame,
						Ok(None) => {
							tracing::debug!("Encoded frame channel closed.");
							break;
						},
						Err(_) => break,
					};

					let batch = match packetizer.packetize(
						&frame.data,
						frame.is_key_frame,
						context.packet_size,
						context.minimum_fec_packets,
						config.fec_percentage,
						frame.frame_number,
						&mut sequence_number,
						frame.rtp_timestamp,
						0,
					) {
						Ok(batch) => batch,
						Err(()) => {
							tracing::warn!("Failed to packetize frame {}.", frame.frame_number);
							continue;
						},
					};

					match stop_session_manager.wrap_cancel(packet_tx.send(batch)).await {
						Ok(Ok(())) => {},
						Ok(Err(_)) | Err(_) => {
							tracing::debug!("Packet handler stopped, exiting packetize loop.");
							break;
						},
					}
				},

				idr = idr_rx.recv() => {
					match idr {
						Ok(()) => forward_control(&control_tx, EncoderControl::Idr),
						Err(broadcast::error::RecvError::Lagged(n)) => {
							tracing::warn!("Missed {n} IDR request(s) while lagging; forwarding one now.");
							forward_control(&control_tx, EncoderControl::Idr);
						},
						Err(broadcast::error::RecvError::Closed) => {},
					}
				},

				invalidate = invalidate_rx.recv() => {
					match invalidate {
						Ok((first, last)) => forward_control(&control_tx, EncoderControl::Invalidate { first, last }),
						Err(broadcast::error::RecvError::Lagged(n)) => {
							tracing::warn!(
								"Missed {n} reference-invalidation request(s) while lagging; forcing an IDR."
							);
							forward_control(&control_tx, EncoderControl::Idr);
						},
						Err(broadcast::error::RecvError::Closed) => {},
					}
				},

				reset = reset_rx.recv() => {
					match reset {
						Ok(()) => forward_control(&control_tx, EncoderControl::Reset),
						Err(broadcast::error::RecvError::Lagged(n)) => {
							tracing::warn!("Missed {n} reset request(s) while lagging; forwarding one now.");
							forward_control(&control_tx, EncoderControl::Reset);
						},
						Err(broadcast::error::RecvError::Closed) => {},
					}
				},
			}
		}

		tracing::debug!("Video packetize loop stopped.");
	});
}

/// Best-effort forward: a full or not-yet-consumed control channel (no host
/// consumer wired up until Task 10) must not block or kill the packetize
/// loop — packetizing real frames takes priority over control-request
/// delivery.
fn forward_control(control_tx: &mpsc::Sender<EncoderControl>, control: EncoderControl) {
	if let Err(e) = control_tx.try_send(control) {
		tracing::debug!("Dropping encoder control signal {control:?}: {e}");
	}
}
