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
use tokio::sync::{Notify, mpsc};

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

/// Packetize each `EncodedFrame` from the host's encoder into `ShardBatch`es
/// and forward them to the packet handler.
///
/// This is a minimal replacement for the deleted Vulkan `VideoPipeline`'s
/// encode+packetize loop, covering only packetizing: the host now supplies
/// pre-encoded frames, so there is no GPU encode, HDR metadata, or per-frame
/// latency stats to produce here. IDR / reference-invalidation / reset
/// requests are not consumed by this loop — they must reach the host's
/// encoder directly, which is outside this crate.
pub(crate) fn spawn_packetize_frames(
	mut frame_rx: mpsc::Receiver<EncodedFrame>,
	config: VideoStreamConfig,
	context: VideoStreamContext,
	keys_rx: SessionKeysReceiver,
	packet_tx: mpsc::Sender<ShardBatch>,
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
			let frame = match stop_session_manager.wrap_cancel(frame_rx.recv()).await {
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
		}

		tracing::debug!("Video packetize loop stopped.");
	});
}
