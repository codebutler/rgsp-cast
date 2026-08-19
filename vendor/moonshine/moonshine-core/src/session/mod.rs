use std::sync::Arc;

use async_shutdown::ShutdownManager;
use manager::SessionShutdownReason;
use tokio::sync::{mpsc, watch};

use crate::session::stream::audio::AudioChannels;
use crate::session::stream::audio::AudioStream;
use crate::session::stream::audio::AudioStreamContext;
use crate::session::stream::control::ControlStream;
use crate::session::stream::control::ControlStreamContext;
use crate::session::stream::video::EncodedFrame;
use crate::session::stream::video::EncoderControl;
use crate::session::stream::video::FrameStats;
use crate::session::stream::video::HdrModeState;
use crate::session::stream::video::VideoStream;
use crate::session::stream::video::VideoStreamContext;
use crate::session::stream::video::VideoStreamHandle;

use self::stream::audio::AudioStreamConfig;
use self::stream::control::ControlStreamConfig;
use self::stream::video::VideoStreamConfig;

pub mod application;
pub mod manager;
pub mod stream;

/// Timeout in seconds for the HTTP launch endpoint to wait for the session to launch.
pub(crate) const APP_LAUNCH_HTTP_TIMEOUT_SECS: u64 = 60;

/// Raw session encryption key data.
#[derive(Clone, Debug)]
pub struct SessionKeyData {
	/// AES GCM key used for encoding video / audio / control messages.
	pub remote_input_key: Vec<u8>,

	/// AES GCM initialization vector for video / audio / control messages.
	pub remote_input_key_id: i64,
}

pub(crate) type SessionKeysReceiver = watch::Receiver<SessionKeyData>;
pub(crate) type SessionKeysSender = watch::Sender<SessionKeyData>;

/// Session keys — either raw keys or a watch receiver.
#[derive(Clone, Debug)]
pub enum SessionKeys {
	Keys(SessionKeyData),
	Rx(SessionKeysReceiver),
}

impl SessionKeys {
	pub(crate) fn new(remote_input_key: Vec<u8>, remote_input_key_id: i64) -> Self {
		Self::Keys(SessionKeyData {
			remote_input_key,
			remote_input_key_id,
		})
	}

	pub(crate) fn clone_rx(&self) -> Option<SessionKeysReceiver> {
		match self {
			Self::Rx(rx) => Some(rx.clone()),
			_ => None,
		}
	}
}

/// Context for a session.
///
/// This is created at launch time and contains all the information about the session
/// that is needed to start the streams.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct SessionContext {
	/// ID of the application as reported to the client.
	pub application_id: i32,

	/// Resolution of the video stream (width, height).
	pub resolution: (u32, u32),

	/// Refresh rate of the video stream (in Hz).
	pub refresh_rate: u32,

	/// Encryption keys for encoding traffic.
	pub keys: SessionKeys,

	/// Audio channel count (2, 6, or 8).
	pub audio_channels: AudioChannels,

	/// Audio channel mask.
	pub audio_channel_mask: u32,

	/// If true, the session is launched with HDR support.
	pub hdr: bool,
}

/// The state of the session. This enum enforces the session lifecycle:
///
/// 1. `Initialized` — Session created; not yet launched.
/// 2. `Launched` — Session launched; waiting for RTSP negotiation.
/// 3. `Active` — Streams are active.
enum SessionState {
	/// Session initialized; not yet launched.
	Initialized(InitializedSession),
	/// Session launched; waiting for RTSP PLAY.
	Launched(LaunchedSession),
	/// Streams active.
	Active(ActiveSession),
}

impl SessionState {
	fn context(&self) -> &SessionContext {
		match self {
			Self::Initialized(session) => session.context(),
			Self::Launched(launched) => launched.context(),
			Self::Active(active) => active.context(),
		}
	}
}

/// Initialized session state — components created, session not yet launched.
pub(crate) struct InitializedSession {
	context: SessionContext,
	audio_stream: AudioStream,
	video_stream: VideoStream,
	control_stream: ControlStream,
	hdr_metadata_rx: watch::Receiver<HdrModeState>,
	stop: ShutdownManager<SessionShutdownReason>,
}

impl InitializedSession {
	#[allow(clippy::too_many_arguments)]
	pub(crate) async fn new(
		video_config: VideoStreamConfig,
		audio_config: AudioStreamConfig,
		control_config: ControlStreamConfig,
		address: String,
		context: SessionContext,
		stop: ShutdownManager<SessionShutdownReason>,
		stats_tx: tokio::sync::broadcast::Sender<FrameStats>,
	) -> Result<Self, ()> {
		// Create HDR metadata watch channel.
		let (hdr_metadata_tx, hdr_metadata_rx) = watch::channel(HdrModeState::new(context.hdr));

		// Create audio stream, video stream, and control stream.
		let audio = AudioStream::new(audio_config, address.clone(), stop.clone()).await?;
		let video_stream =
			VideoStream::new(video_config.clone(), address.clone(), hdr_metadata_tx, stop.clone(), stats_tx).await?;
		let control_stream = ControlStream::new(control_config, address, stop.clone())?;

		Ok(Self {
			context,
			audio_stream: audio,
			video_stream,
			control_stream,
			hdr_metadata_rx,
			stop,
		})
	}

	pub(crate) fn context(&self) -> &SessionContext {
		&self.context
	}

	/// Launch the session, but do not start streams.
	pub(crate) async fn launch(self) -> Result<LaunchedSession, ()> {
		let Self {
			context,
			audio_stream: audio,
			video_stream,
			control_stream,
			hdr_metadata_rx,
			stop: _stop,
		} = self;

		Ok(LaunchedSession {
			context,
			video_stream,
			audio,
			control_stream,
			hdr_metadata_rx,
		})
	}
}

/// Launched session state — waiting for RTSP negotiation.
pub(crate) struct LaunchedSession {
	context: SessionContext,
	video_stream: VideoStream,
	audio: AudioStream,
	control_stream: ControlStream,
	hdr_metadata_rx: watch::Receiver<HdrModeState>,
}

impl LaunchedSession {
	pub(crate) fn context(&self) -> &SessionContext {
		&self.context
	}

	#[allow(clippy::too_many_arguments)]
	pub(crate) async fn start(
		self,
		video_config: VideoStreamConfig,
		stream_timeout: u64,
		video_ctx: VideoStreamContext,
		audio_ctx: AudioStreamContext,
		frame_rx: mpsc::Receiver<EncodedFrame>,
		control_tx: mpsc::Sender<EncoderControl>,
		pcm_rx: mpsc::Receiver<Vec<i16>>,
		stop: ShutdownManager<SessionShutdownReason>,
	) -> Result<(ActiveSession, Arc<tokio::sync::Notify>, crate::session::stream::audio::AudioStartGate), ()> {
		let Self {
			context,
			audio,
			video_stream,
			control_stream,
			hdr_metadata_rx,
		} = self;

		let hdr_effective = context.hdr;

		// Extract the watch receiver for streams.
		let keys_rx = context.keys.clone_rx().ok_or_else(|| {
			tracing::error!("Session keys not initialized");
		})?;

		// Start video stream — gated, returns VideoStreamHandle. control_tx
		// carries client recovery requests out to the host, mapped by the
		// packetize loop.
		let video_handle = video_stream
			.start(video_config, video_ctx, keys_rx.clone(), frame_rx, control_tx, stop.clone())
			.map_err(|()| tracing::error!("Failed to start video stream"))?;

		// Start audio stream — gated, returns AudioStartHandle. `pcm_rx`
		// carries raw i16 PCM in from the host's capture; the audio stream's
		// `host_source` bridges it into Opus-ready `AudioFrame`s.
		let audio_trigger = audio
			.start(audio_ctx, keys_rx, pcm_rx)
			.map_err(|()| tracing::error!("Failed to start audio stream"))?;

		// Clone the start notifies for external triggering (e.g. bench binary).
		let video_start_notify = video_handle.clone_start_notify();
		let audio_start_notify = audio_trigger.clone_start_gate();

		// Keep a handle to the video stream so a resuming client can reset its
		// frame counters (see `ActiveSession::reset_video_stream`).
		let video_handle_for_resume = video_handle.clone();

		// Start control stream — receives both handles.
		let control_ctx = ControlStreamContext::new(&context, hdr_effective);
		control_stream.start(
			stream_timeout,
			control_ctx,
			video_handle,
			audio_trigger,
			hdr_metadata_rx,
		);

		Ok((
			ActiveSession {
				context,
				video_handle: video_handle_for_resume,
			},
			video_start_notify,
			audio_start_notify,
		))
	}
}

/// Active session state — streams are active.
pub(crate) struct ActiveSession {
	context: SessionContext,
	video_handle: VideoStreamHandle,
}

impl ActiveSession {
	pub(crate) fn context(&self) -> &SessionContext {
		&self.context
	}

	/// Reset the video stream's frame counters and force an IDR for a resuming client.
	pub(crate) fn reset_video_stream(&self) {
		self.video_handle.request_reset();
	}
}
