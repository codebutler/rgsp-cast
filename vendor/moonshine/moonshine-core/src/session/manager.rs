use std::sync::Arc;

use async_shutdown::ShutdownManager;
use tokio::sync::{Mutex, broadcast, mpsc, watch};

use crate::ShutdownReason;
use crate::session::FrameStats;
use crate::session::InitializedSession;
use crate::session::SessionContext;
use crate::session::SessionKeyData;
use crate::session::SessionKeys;
use crate::session::SessionKeysSender;
use crate::session::SessionState;
use crate::session::stream::audio::AudioStreamConfig;
use crate::session::stream::audio::AudioStreamContext;
use crate::session::stream::control::ControlStreamConfig;
use crate::session::stream::video::EncodedFrame;
use crate::session::stream::video::EncoderControl;
use crate::session::stream::video::VideoStreamConfig;
use crate::session::stream::video::VideoStreamContext;

const SESSION_SHUTDOWN_TIMEOUT_SECS: u64 = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionShutdownReason {
	/// Session manager is shutting down.
	ManagerShutdown,
	/// The session was stopped by the user.
	UserStopped,
	/// Video packet handler stopped unexpectedly.
	VideoPacketHandlerStopped,
	/// Video encoder stopped unexpectedly.
	VideoEncoderStopped,
	/// Audio packet handler stopped unexpectedly.
	AudioPacketHandlerStopped,
	/// Audio encoder stopped unexpectedly.
	AudioEncoderStopped,
	/// Audio source (the host's PCM bridge) stopped unexpectedly.
	AudioSourceStopped,
	/// Control stream stopped unexpectedly.
	ControlStreamStopped,
}

struct SessionManagerInner {
	/// Configuration for the video stream.
	video_config: VideoStreamConfig,

	/// Configuration for the audio stream.
	audio_config: AudioStreamConfig,

	/// Configuration for the control stream.
	control_config: ControlStreamConfig,

	/// Address to bind streams to.
	address: String,

	/// Time in seconds since last ping after which the stream closes.
	stream_timeout: u64,


	/// The currently active session, if any.
	session: Option<SessionState>,

	/// Shutdown manager for the active session, used to trigger session shutdown upon request.
	stop: ShutdownManager<SessionShutdownReason>,

	/// Sender for session keys, used to update keys from the webserver in different subsystems.
	///
	/// Subsystems that get updated are: video encoder, audio encoder and input handler.
	keys_tx: Option<SessionKeysSender>,

	/// Stream contexts received via RTSP ANNOUNCE, consumed by `start_session`.
	video_stream_context: Option<VideoStreamContext>,
	audio_stream_context: Option<AudioStreamContext>,

	/// Copy of the negotiated video context of the *active* session, kept
	/// because `video_stream_context` above is taken by `start_session`.
	/// The host's encoder is outside this crate and needs the negotiated fps
	/// and bitrate to configure its hardware encoder, so it reads them here
	/// alongside `video_frame_sender()`. Same lifecycle as `video_frame_tx`:
	/// set in `start_session`, cleared in `reset_session`.
	active_video_context: Option<VideoStreamContext>,

	/// Broadcast sender for per-frame encoding statistics.
	stats_tx: tokio::sync::broadcast::Sender<FrameStats>,

	/// Watchdog task for monitoring unexpected session shutdowns.
	stop_watcher: Option<tokio::task::JoinHandle<()>>,

	/// Notify to trigger the video pipeline start (used by bench / external callers).
	video_start_notify: Option<Arc<tokio::sync::Notify>>,

	/// Sender for encoded video frames, handed to the host's encoder once the
	/// video stream is active. `None` until `start_session()` succeeds.
	video_frame_tx: Option<mpsc::Sender<EncodedFrame>>,

	/// Sender for raw i16 PCM samples, handed to the host's audio capture
	/// once the audio stream is active. `None` until `start_session()`
	/// succeeds.
	audio_frame_tx: Option<mpsc::Sender<Vec<i16>>>,

	/// Receiving end of the packetize loop's `EncoderControl` channel —
	/// client recovery requests (IDR / reference invalidation / reset),
	/// mapped and forwarded from inside the crate. Taken (not cloned) by
	/// `encoder_control_receiver()`, since there is exactly one host
	/// encoder to hand it to. `None` until `start_session()` succeeds or
	/// after it has already been taken.
	encoder_control_rx: Option<mpsc::Receiver<EncoderControl>>,

	/// Notify to trigger the audio pipeline start (used by bench / external callers).
	audio_start_notify: Option<Arc<tokio::sync::Notify>>,

	/// Shutdown manager for the entire application.
	shutdown: ShutdownManager<ShutdownReason>,

	/// Trigger token for the session manager's own shutdown trigger.
	///
	/// Used to trigger an application shutdown if the session manager stops unexpectedly.
	_trigger_token: async_shutdown::TriggerShutdownToken<ShutdownReason>,

	/// Delay token for the session manager's own shutdown trigger.
	///
	/// Used to delay shutdown until the session manager has cleaned up.
	_delay_token: async_shutdown::DelayShutdownToken<ShutdownReason>,
}

impl SessionManagerInner {
	fn reset_session(&mut self) {
		if let Some(handle) = self.stop_watcher.take() {
			handle.abort();
		}
		self.session = None;
		self.keys_tx = None;
		self.video_stream_context = None;
		self.audio_stream_context = None;
		self.active_video_context = None;
		self.video_start_notify = None;
		self.audio_start_notify = None;
		self.video_frame_tx = None;
		self.audio_frame_tx = None;
		self.encoder_control_rx = None;
		self.stop = ShutdownManager::new();
	}
}

impl Drop for SessionManagerInner {
	fn drop(&mut self) {
		if let Some(handle) = self.stop_watcher.take() {
			handle.abort();
		}
		if self.session.is_some() {
			tracing::debug!("Stopping active session before shutdown.");
			let _ = self.stop.trigger_shutdown(SessionShutdownReason::ManagerShutdown);
			// Wait until shutdown completed.
			if let Ok(handle) = tokio::runtime::Handle::try_current() {
				handle.block_on(self.stop.wait_shutdown_complete());
			}
		}
	}
}

#[derive(Clone)]
pub struct SessionManager {
	inner: Arc<Mutex<SessionManagerInner>>,
	stats_tx: broadcast::Sender<FrameStats>,
}

impl SessionManager {
	#[allow(clippy::too_many_arguments)]
	pub fn new(
		video_config: VideoStreamConfig,
		audio_config: AudioStreamConfig,
		control_config: ControlStreamConfig,
		address: String,
		stream_timeout: u64,
		shutdown: ShutdownManager<ShutdownReason>,
	) -> Result<Self, ()> {
		let trigger_token = shutdown.trigger_shutdown_token(ShutdownReason::SessionManagerShutdown);
		let delay_token = shutdown.delay_shutdown_token().map_err(|e| {
			tracing::error!("Failed to create delay shutdown token: {e:?}");
		})?;

		let inner = SessionManagerInner {
			video_config,
			audio_config,
			control_config,
			address,
			stream_timeout,
			session: None,
			stop: ShutdownManager::new(),
			keys_tx: None,
			video_stream_context: None,
			audio_stream_context: None,
			active_video_context: None,
			stats_tx: tokio::sync::broadcast::channel(256).0,
			stop_watcher: None,
			video_start_notify: None,
			audio_start_notify: None,
			video_frame_tx: None,
			audio_frame_tx: None,
			encoder_control_rx: None,
			shutdown: shutdown.clone(),
			_trigger_token: trigger_token,
			_delay_token: delay_token,
		};

		let stats_tx = inner.stats_tx.clone();
		let inner = Arc::new(Mutex::new(inner));

		Ok(Self { inner, stats_tx })
	}

	/// Returns a receiver for per-frame encoding statistics.
	///
	/// Call **before** `initialize_session()` to receive stats from the start.
	/// Multiple receivers can be created — each receives a copy of every message.
	pub fn bench_stats_receiver(&self) -> tokio::sync::broadcast::Receiver<FrameStats> {
		self.stats_tx.subscribe()
	}

	/// Sender for encoded video frames, for the host's encoder to feed once
	/// the video stream is active.
	///
	/// `None` until `start_session()` has succeeded — the channel is created
	/// there, alongside the video stream that owns the receiving end.
	pub async fn video_frame_sender(&self) -> Option<mpsc::Sender<EncodedFrame>> {
		self.inner.lock().await.video_frame_tx.clone()
	}

	/// Sender for raw interleaved i16 PCM samples, for the host's audio
	/// capture to feed once the audio stream is active. Each item must be
	/// exactly one Opus frame's worth of samples (`FRAME_FRAMES` times the
	/// channel count, see `audio::host_source`); mismatched chunks are
	/// dropped with a warning rather than fed to Moonshine's own Opus
	/// encoder, which still owns the real encode (unlike video, this
	/// crate's audio encoder was never removed; only the PulseAudio
	/// producer was).
	///
	/// `None` until `start_session()` has succeeded — the channel is created
	/// there, alongside the audio stream that owns the receiving end.
	pub async fn audio_frame_sender(&self) -> Option<mpsc::Sender<Vec<i16>>> {
		self.inner.lock().await.audio_frame_tx.clone()
	}

	/// The negotiated video parameters of the active session (fps, bitrate,
	/// resolution, packet size), for the host's encoder to configure its
	/// hardware with. These arrive via RTSP ANNOUNCE and are otherwise
	/// consumed by `start_session()`.
	///
	/// `None` until `start_session()` has succeeded, matching
	/// `video_frame_sender()`.
	pub async fn active_video_context(&self) -> Option<VideoStreamContext> {
		self.inner.lock().await.active_video_context.clone()
	}

	/// Take the receiving end of client recovery requests (IDR / reference
	/// invalidation / reset), mapped to `EncoderControl` by the video
	/// stream's packetize loop — the control stream parses these off
	/// Moonlight's wire messages and forwards them internally; this is how
	/// they reach the host's encoder outside the crate.
	///
	/// Returns `Some` exactly once per session: this is a plain `mpsc`, not
	/// a broadcast, since there is one host encoder to hand it to. Returns
	/// `None` before `start_session()` succeeds or if already taken.
	/// Requests sent before the receiver is taken and polled queue up to
	/// the channel's bound; see `start_session` for that bound and its
	/// backpressure behavior.
	pub async fn encoder_control_receiver(&self) -> Option<mpsc::Receiver<EncoderControl>> {
		self.inner.lock().await.encoder_control_rx.take()
	}

	/// Trigger the video and audio pipelines to start encoding.
	///
	/// In the normal flow, this is triggered by the control stream when the
	/// client sends `StartB`. Call this from external callers (e.g. bench binary)
	/// that have no Moonlight client. Must be called after `start_session()`.
	pub async fn trigger_streams_start(&self) {
		let inner = self.inner.lock().await;
		if let Some(notify) = inner.video_start_notify.as_ref() {
			// Call notify_one() twice instead of notify_waiters() because
			// Notify only wakes tasks already .awaiting; notify_waiters()
			// is a no-op if no task is waiting yet.  notify_one() stores
			// a permit so the next notified().await completes immediately.
			notify.notify_one();
			notify.notify_one();
		}
		if let Some(notify) = inner.audio_start_notify.as_ref() {
			notify.notify_one();
			notify.notify_one();
		}
	}

	/// Set the video and audio stream contexts after receiving RTSP ANNOUNCE.
	pub async fn set_stream_context(
		&self,
		video_stream_context: VideoStreamContext,
		audio_stream_context: AudioStreamContext,
	) -> Result<(), ()> {
		let mut guard = self.inner.lock().await;
		match guard.session.as_ref() {
			Some(SessionState::Launched(_)) => {
				tracing::debug!("Stream contexts received via RTSP ANNOUNCE.");
				guard.video_stream_context = Some(video_stream_context);
				guard.audio_stream_context = Some(audio_stream_context);
				Ok(())
			},
			Some(SessionState::Initialized(_)) => {
				tracing::warn!("SetStreamContext rejected: session not yet launched (Initialized state)");
				Err(())
			},
			Some(SessionState::Active(_)) => {
				// Client is resuming an already-running session (reconnect). The video,
				// audio, and control streams are still running and re-learn the client's
				// address from its PINGs (with refreshed keys via `/resume`), so there is
				// nothing to rebuild — accept the re-ANNOUNCE without storing new contexts.
				tracing::info!("Resuming active session: accepting RTSP ANNOUNCE from reconnecting client.");
				Ok(())
			},
			None => {
				tracing::warn!("SetStreamContext rejected: no active session");
				Err(())
			},
		}
	}

	/// Get the current session context if there is an active session; otherwise return `None`.
	pub async fn get_session_context(&self) -> Result<Option<SessionContext>, ()> {
		let guard = self.inner.lock().await;
		Ok(guard.session.as_ref().map(|s| s.context().clone()))
	}

	/// Initialize a new session with the provided context.
	///
	/// The session is not launched until `launch_session` is called.
	pub async fn initialize_session(&self, mut context: SessionContext) -> Result<(), ()> {
		let mut guard = self.inner.lock().await;

		if guard.session.is_some() || guard.keys_tx.is_some() {
			tracing::warn!("Session already initialized, rejecting InitializeSession command.");
			return Err(());
		}

		// Extract the raw keys from context and create the watch channel.
		let session_keys = match context.keys {
			SessionKeys::Keys(data) => data,
			SessionKeys::Rx(_) => {
				tracing::error!("Session keys already initialized as a watch receiver");
				return Err(());
			},
		};
		let (tx, rx) = watch::channel(session_keys);
		context.keys = SessionKeys::Rx(rx);

		let video_config = guard.video_config.clone();
		let audio_config = guard.audio_config.clone();
		let control_config = guard.control_config.clone();
		let address = guard.address.clone();
		let stop = guard.stop.clone();
		let stats_tx = guard.stats_tx.clone();
		let session = InitializedSession::new(
			video_config,
			audio_config,
			control_config,
			address,
			context,
			stop,
			stats_tx,
		)
		.await?;
		guard.session = Some(SessionState::Initialized(session));

		spawn_session_watchdog(&self.inner, &mut guard);
		tracing::info!("Session initialized successfully, waiting to be launched.");

		// Set the keys sender here so that it is not set if session initialization failed.
		guard.keys_tx = Some(tx);
		Ok(())
	}

	/// Launch the session by starting the compositor and application, but don't start streams until RTSP ANNOUNCE is received.
	pub async fn launch_session(&self) -> Result<(), ()> {
		let session = {
			let mut guard = self.inner.lock().await;
			match guard.session.take() {
				Some(SessionState::Initialized(session)) => session,
				Some(SessionState::Launched(launched)) => {
					guard.session = Some(SessionState::Launched(launched));
					tracing::warn!("LaunchSession rejected: session already launched");
					return Err(());
				},
				Some(SessionState::Active(active)) => {
					guard.session = Some(SessionState::Active(active));
					tracing::warn!("LaunchSession rejected: session already active");
					return Err(());
				},
				None => {
					tracing::warn!("LaunchSession rejected: no active session");
					return Err(());
				},
			}
		};

		tracing::info!("Launching session.");
		match session.launch().await {
			Ok(launched) => {
				let mut guard = self.inner.lock().await;
				guard.session = Some(SessionState::Launched(launched));
				tracing::info!("Session launched successfully, waiting for RTSP ANNOUNCE.");
				Ok(())
			},
			Err(()) => {
				let mut guard = self.inner.lock().await;
				guard.reset_session();
				tracing::error!("Failed to launch session, waiting for new session.");
				Err(())
			},
		}
	}

	/// Start the video and audio streams.
	///
	/// Returns `Ok(())` only after all three streams (video, audio, control) are
	/// successfully constructed. Returns `Err(())` if any stream fails to initialize.
	pub async fn start_session(&self) -> Result<(), ()> {
		let (launched, video_stream_context, audio_stream_context, stop) = {
			let mut guard = self.inner.lock().await;
			let video_stream_context = guard.video_stream_context.take();
			let audio_stream_context = guard.audio_stream_context.take();
			match guard.session.take() {
				Some(SessionState::Launched(launched)) => {
					(launched, video_stream_context, audio_stream_context, guard.stop.clone())
				},
				Some(SessionState::Initialized(session)) => {
					guard.session = Some(SessionState::Initialized(session));
					tracing::warn!("StartSession rejected: session not yet launched");
					return Err(());
				},
				Some(SessionState::Active(active)) => {
					// Resume (reconnect): the streams are already running, so PLAY is a
					// no-op — the client picks up the existing streams once it PINGs.
					// The reconnecting client is a fresh Moonlight session that expects
					// frame numbers to start at 1, so reset the video frame counters and
					// force an IDR; otherwise it sees the running counter as a huge frame
					// gap and reports a poor connection.
					active.reset_video_stream();
					guard.session = Some(SessionState::Active(active));
					tracing::info!(
						"Resuming active session: resetting video frame counter and treating PLAY as no-op."
					);
					return Ok(());
				},
				None => {
					tracing::warn!("StartSession rejected: no active session");
					return Err(());
				},
			}
		};

		let video_stream_context = video_stream_context.ok_or_else(|| {
			tracing::error!("VideoStreamContext not set");
		})?;
		let audio_stream_context = audio_stream_context.ok_or_else(|| {
			tracing::error!("AudioStreamContext not set");
		})?;
		// Kept for the host's encoder, which reads the negotiated fps and
		// bitrate via `active_video_context()`; the original is moved into
		// `launched.start()` below.
		let active_video_context = video_stream_context.clone();

		tracing::info!("Starting session streams.");
		// Channel the host's encoder feeds via `video_frame_sender()`; the
		// receiving end is handed to the video stream's packetize loop.
		//
		// Bound of 4 is a starting guess, not a measured value. Backpressure
		// note: this is a bounded mpsc, so a full channel blocks the
		// *sender* — which on the host side is expected to be the capture
		// thread (`rgsp_host::video::VideoStream::run`, doc'd as running on
		// a dedicated blocking thread precisely so it's safe to block).
		// That means a stalled packetize/send path stalls capture rather
		// than dropping frames. For a live low-latency stream that is
		// usually the wrong tradeoff — dropping a stale frame and catching
		// up is better than falling behind and delaying every frame after
		// it. Left as-is pending real on-device backpressure behavior; the
		// alternative, if this turns out to bite, is a ring buffer /
		// try_send-and-drop policy on the host side rather than growing
		// this bound.
		let (frame_tx, frame_rx) = mpsc::channel::<EncodedFrame>(4);
		// Client recovery requests (IDR / reference invalidation / reset),
		// mapped to EncoderControl by the packetize loop and handed to the
		// host via encoder_control_receiver(). Small bound: these are rare,
		// human-timescale events, not a per-frame stream — forward_control()
		// in host_source.rs drops rather than blocks if it ever fills.
		let (control_tx, control_rx) = mpsc::channel::<EncoderControl>(8);
		// Channel the host's audio capture feeds via `audio_frame_sender()`;
		// the receiving end is bridged into Opus-ready `AudioFrame`s by the
		// audio stream's `host_source`. Same bound and backpressure
		// reasoning as `frame_tx` above — capture is expected to run on a
		// dedicated blocking thread, so a full channel stalling it is
		// preferable to dropping audio samples.
		let (audio_frame_tx, audio_pcm_rx) = mpsc::channel::<Vec<i16>>(4);
		let mut guard = self.inner.lock().await;
		let video_config = guard.video_config.clone();
		let stream_timeout = guard.stream_timeout;
		match launched
			.start(
				video_config,
				stream_timeout,
				video_stream_context,
				audio_stream_context,
				frame_rx,
				control_tx,
				audio_pcm_rx,
				stop,
			)
			.await
		{
			Ok((active, video_notify, audio_notify)) => {
				guard.session = Some(SessionState::Active(active));
				guard.video_start_notify = Some(video_notify);
				guard.audio_start_notify = Some(audio_notify);
				guard.active_video_context = Some(active_video_context);
				guard.video_frame_tx = Some(frame_tx);
				guard.audio_frame_tx = Some(audio_frame_tx);
				guard.encoder_control_rx = Some(control_rx);
				Ok(())
			},
			Err(()) => {
				guard.reset_session();
				tracing::error!("Failed to start session streams.");
				Err(())
			},
		}
	}

	/// Stop the session and return to Uninitialized state.
	pub async fn stop_session(&self) -> Result<(), ()> {
		let (stop, shutdown) = {
			let mut guard = self.inner.lock().await;
			match guard.session {
				Some(_) => {},
				None => return Ok(()),
			}
			let stop = guard.stop.clone();
			let shutdown = guard.shutdown.clone();

			// Drop session first, which drops the Application.
			guard.reset_session();
			(stop, shutdown)
		};

		// Then trigger shutdown of the compositor & streams.
		let _ = stop.trigger_shutdown(SessionShutdownReason::UserStopped);

		wait_for_session_shutdown(&stop, &shutdown, SESSION_SHUTDOWN_TIMEOUT_SECS).await?;
		tracing::info!("Session stopped by user, waiting for new session.");
		Ok(())
	}

	/// Update the session keys for the active session.
	pub(crate) async fn update_keys(&self, keys: SessionKeyData) -> Result<(), ()> {
		let guard = self.inner.lock().await;

		if guard.session.is_none() {
			tracing::warn!("No active session to update keys for.");
			return Err(());
		}

		if let Some(keys_tx) = &guard.keys_tx {
			keys_tx.send_replace(keys);
		} else {
			tracing::warn!("No active session to update keys for.");
		}

		Ok(())
	}
}

/// Spawn a watchdog task to monitor the session for unexpected shutdowns.
fn spawn_session_watchdog(inner: &Arc<Mutex<SessionManagerInner>>, guard: &mut SessionManagerInner) {
	if guard.stop_watcher.is_some() {
		tracing::error!("Session watchdog already running, not spawning another.");
		return;
	}

	let inner = inner.clone();
	let stop = guard.stop.clone();
	let shutdown = guard.shutdown.clone();
	let handle = tokio::spawn(async move {
		tokio::select! {
			reason = stop.wait_shutdown_triggered() => {
				if reason == SessionShutdownReason::UserStopped {
					tracing::info!("Session shutdown requested by user.");
				} else {
					tracing::warn!("Session stopped unexpectedly (reason: {reason:?}), waiting for new session.");
				}
			},
			_ = shutdown.wait_shutdown_triggered() => {
				tracing::debug!("Global shutdown triggered, stopping active session.");
				let _ = stop.trigger_shutdown(SessionShutdownReason::ManagerShutdown);
			},
		}

		// First drop the session so that the application exits as soon as possible.
		{
			inner.lock().await.reset_session();
		}

		// Then wait for the session to shut down.
		stop.wait_shutdown_complete().await;
	});
	guard.stop_watcher = Some(handle);
}

/// Wait for the session to shut down within the given timeout.
///
/// If the session does not shut down in time, triggers a global application
/// shutdown to prevent orphaned tasks and resource leaks.
async fn wait_for_session_shutdown(
	stop: &ShutdownManager<SessionShutdownReason>,
	shutdown: &ShutdownManager<ShutdownReason>,
	timeout_secs: u64,
) -> Result<(), ()> {
	match tokio::time::timeout(
		std::time::Duration::from_secs(timeout_secs),
		stop.wait_shutdown_complete(),
	)
	.await
	{
		Ok(_) => Ok(()),
		Err(_) => {
			tracing::error!("Session shutdown timed out after {timeout_secs}s — triggering application shutdown.");
			let _ = shutdown.trigger_shutdown(ShutdownReason::SessionManagerShutdown);
			Err(())
		},
	}
}
