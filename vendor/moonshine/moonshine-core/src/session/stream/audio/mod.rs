use std::sync::Arc;

use async_shutdown::ShutdownManager;
use serde::{Deserialize, Serialize};
use strum_macros::Display;
use tokio::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Notify;
use tokio::sync::mpsc;

use crate::session::SessionKeysReceiver;
use crate::session::manager::SessionShutdownReason;

use self::encoder::AudioEncoder;
use self::host_source::spawn_pcm_bridge;

mod encoder;
mod host_source;

pub use host_source::FRAME_FRAMES;

/// The audio source emits samples at this rate to the encoder.
pub(crate) const CAPTURE_SAMPLE_RATE: u32 = 48000;

/// A buffer of interleaved f32 samples ready for Opus encoding.
pub(crate) struct AudioFrame {
	/// Interleaved f32 samples for the negotiated channel count.
	pub buf: Vec<f32>,

	/// Capture timestamp in milliseconds since process start.
	#[allow(dead_code)]
	pub capture_ts_ms: u64,
}

/// Configuration for the audio stream.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioStreamConfig {
	/// Port to use for streaming audio data.
	pub port: u16,
}

impl Default for AudioStreamConfig {
	fn default() -> Self {
		Self { port: 48000 }
	}
}

/// Number of audio channels requested by the client.
#[derive(Clone, Copy, Debug, Default, Display, PartialEq, PartialOrd)]
pub enum AudioChannels {
	#[default]
	Stereo = 2,
	Surround51 = 6,
	Surround71 = 8,
}

impl From<u8> for AudioChannels {
	fn from(value: u8) -> Self {
		match value {
			6 => Self::Surround51,
			8 => Self::Surround71,
			_ => Self::Stereo,
		}
	}
}

/// Opus multistream configuration for a specific channel layout.
#[derive(Clone, Debug)]
pub struct OpusStreamConfig {
	pub channels: AudioChannels,
	pub streams: u8,
	pub coupled_streams: u8,
	pub mapping: [u8; 8],
	pub bitrate: u32,
}

/// Pre-defined Opus stream configurations matching Sunshine's behavior.
pub(crate) const OPUS_STEREO: OpusStreamConfig = OpusStreamConfig {
	channels: AudioChannels::Stereo,
	streams: 1,
	coupled_streams: 1,
	mapping: [0, 1, 0, 0, 0, 0, 0, 0],
	bitrate: 96_000,
};

pub(crate) const OPUS_HIGH_STEREO: OpusStreamConfig = OpusStreamConfig {
	channels: AudioChannels::Stereo,
	streams: 1,
	coupled_streams: 1,
	mapping: [0, 1, 0, 0, 0, 0, 0, 0],
	bitrate: 512_000,
};

pub(crate) const OPUS_SURROUND51: OpusStreamConfig = OpusStreamConfig {
	channels: AudioChannels::Surround51,
	streams: 4,
	coupled_streams: 2,
	mapping: [0, 1, 4, 5, 2, 3, 0, 0],
	bitrate: 256_000,
};

pub(crate) const OPUS_HIGH_SURROUND51: OpusStreamConfig = OpusStreamConfig {
	channels: AudioChannels::Surround51,
	streams: 6,
	coupled_streams: 0,
	mapping: [0, 1, 2, 3, 4, 5, 0, 0],
	bitrate: 1_536_000,
};

pub(crate) const OPUS_SURROUND71: OpusStreamConfig = OpusStreamConfig {
	channels: AudioChannels::Surround71,
	streams: 5,
	coupled_streams: 3,
	mapping: [0, 1, 4, 5, 6, 7, 2, 3],
	bitrate: 450_000,
};

pub(crate) const OPUS_HIGH_SURROUND71: OpusStreamConfig = OpusStreamConfig {
	channels: AudioChannels::Surround71,
	streams: 8,
	coupled_streams: 0,
	mapping: [0, 1, 2, 3, 4, 5, 6, 7],
	bitrate: 2_048_000,
};

/// All standard configurations, ordered for RTSP DESCRIBE emission.
pub(crate) const ALL_AUDIO_CONFIGS: [&OpusStreamConfig; 6] = [
	&OPUS_STEREO,
	&OPUS_HIGH_STEREO,
	&OPUS_SURROUND51,
	&OPUS_HIGH_SURROUND51,
	&OPUS_SURROUND71,
	&OPUS_HIGH_SURROUND71,
];

/// Audio configuration negotiated between client and server.
#[derive(Clone, Debug)]
pub struct AudioConfig {
	pub channels: AudioChannels,
	pub channel_mask: u32,
	pub high_quality: bool,
	pub stream_config: OpusStreamConfig,
}

impl Default for AudioConfig {
	fn default() -> Self {
		Self {
			channels: AudioChannels::default(),
			channel_mask: 0x3,
			high_quality: true,
			stream_config: OPUS_HIGH_STEREO,
		}
	}
}

impl AudioConfig {
	/// Select the appropriate OpusStreamConfig based on channel count and quality.
	pub fn from_channels(channels: AudioChannels, channel_mask: u32, high_quality: bool) -> Self {
		let stream_config = match (channels, high_quality) {
			(AudioChannels::Surround51, false) => OPUS_SURROUND51,
			(AudioChannels::Surround51, true) => OPUS_HIGH_SURROUND51,
			(AudioChannels::Surround71, false) => OPUS_SURROUND71,
			(AudioChannels::Surround71, true) => OPUS_HIGH_SURROUND71,
			(_, false) => OPUS_STEREO,
			(_, true) => OPUS_HIGH_STEREO,
		};
		Self {
			channels,
			channel_mask,
			high_quality,
			stream_config,
		}
	}
}

#[derive(Clone, Default)]
pub struct AudioStreamContext {
	/// Duration of each audio packet in milliseconds, typically 20ms for Opus.
	pub packet_duration_ms: u32,
	/// Whether to enable QoS on the audio socket.
	pub qos: bool,
	/// Negotiated audio configuration for the stream.
	pub audio_config: AudioConfig,
	/// Whether the client has enabled audio encryption.
	pub encrypt_audio: bool,
}

/// Handle returned by `AudioStream::start` that gates the encoder and packet handler.
///
/// The encoder and packet handler are spawned immediately but block on a `Notify`
/// until `trigger()` is called.
/// Level-triggered start gate.
///
/// A bare `Notify` is edge-triggered and stores **at most one** permit, so
/// `notify_one()` called N times before anyone awaits still releases only one
/// waiter. Upstream had two waiters here and got away with calling it twice;
/// this project added a third (the PCM bridge that replaced the deleted
/// PulseAudio server), and whichever task reached its await last - the encoder,
/// spawned last and on its own thread - could be left parked forever. The
/// symptom is silent and logs nothing: capture runs, the bridge drains PCM,
/// the packet handler answers the client's PINGs, and not one Opus packet is
/// ever produced.
///
/// The flag makes it level-triggered: a waiter that arrives after the trigger
/// sees `true` and proceeds without waiting at all, so neither the number of
/// waiters nor their scheduling order can matter.
#[derive(Clone)]
pub(crate) struct AudioStartGate {
	started: Arc<AtomicBool>,
	notify: Arc<Notify>,
}

impl AudioStartGate {
	pub(crate) fn new() -> Self {
		Self { started: Arc::new(AtomicBool::new(false)), notify: Arc::new(Notify::new()) }
	}

	pub(crate) fn open(&self) {
		self.started.store(true, Ordering::SeqCst);
		self.notify.notify_waiters();
	}

	/// Resolves once the gate is open, however many tasks wait on it.
	pub(crate) async fn wait(&self) {
		while !self.started.load(Ordering::SeqCst) {
			let waiting = self.notify.notified();
			if self.started.load(Ordering::SeqCst) {
				break;
			}
			waiting.await;
		}
	}
}

pub(crate) struct AudioStartHandle {
	gate: AudioStartGate,
}

impl AudioStartHandle {
	/// Signal the encoder, PCM bridge and packet handler to begin processing.
	pub fn trigger(&self) {
		self.gate.open();
	}

	/// Clone the start notify for external triggering (e.g. bench binary).
	pub(crate) fn clone_start_gate(&self) -> AudioStartGate {
		self.gate.clone()
	}
}

pub(crate) struct AudioStream {
	udp_socket: tokio::net::UdpSocket,
	stop: ShutdownManager<SessionShutdownReason>,
}

impl AudioStream {
	pub async fn new(
		config: AudioStreamConfig,
		address: String,
		stop: ShutdownManager<SessionShutdownReason>,
	) -> Result<Self, ()> {
		tracing::debug!("Initializing audio stream.");

		let udp_socket = UdpSocket::bind((address, config.port))
			.await
			.map_err(|e| tracing::error!("Failed to bind to UDP socket: {e}"))?;

		Ok(AudioStream { udp_socket, stop })
	}

	pub fn start(
		self,
		context: AudioStreamContext,
		keys_rx: SessionKeysReceiver,
		pcm_rx: mpsc::Receiver<Vec<i16>>,
	) -> Result<AudioStartHandle, ()> {
		// Apply QoS to UDP socket.
		if context.qos {
			let _ = self.udp_socket.set_tos_v4(224);
		}

		// Level-triggered gate shared by all three audio tasks.
		let start_gate = AudioStartGate::new();

		// Create packet channel and spawn handler — gated behind start_notify.
		let (packet_tx, packet_rx) = mpsc::channel::<Vec<u8>>(10);
		spawn_handle_audio_packets(packet_rx, self.udp_socket, start_gate.clone(), self.stop.clone());

		// Create frame channels for the audio source and encoder communication.
		// 3 frames is 15 ms of audio - too tight to absorb any scheduling
		// jitter on this device, and the bridge drops whatever does not fit.
		// 16 frames is 80 ms, still well under the video pipeline's own
		// latency, and gives the encoder room to catch up after a hiccup.
		let (frame_tx, frame_rx) = crossbeam_channel::bounded::<AudioFrame>(16);
		let (frame_recycle_tx, frame_recycle_rx) = crossbeam_channel::bounded::<AudioFrame>(16);

		// Bridge host-supplied i16 PCM (`pcm_rx`) into `AudioFrame`s for the
		// encoder — gated behind start_notify. Replaces the deleted
		// PulseAudio server as the producer; see `host_source` module docs.
		spawn_pcm_bridge(
			pcm_rx,
			context.audio_config.channels as u8,
			frame_tx,
			frame_recycle_rx,
			start_gate.clone(),
			self.stop.clone(),
		);

		// Spawn audio encoder — gated behind start_notify.
		AudioEncoder::spawn(
			CAPTURE_SAMPLE_RATE,
			&context.audio_config.stream_config,
			frame_rx,
			frame_recycle_tx,
			keys_rx,
			context.encrypt_audio,
			packet_tx,
			self.stop.clone(),
			start_gate.clone(),
		)?;

		Ok(AudioStartHandle { gate: start_gate })
	}
}

fn spawn_handle_audio_packets(
	mut packet_rx: mpsc::Receiver<Vec<u8>>,
	socket: UdpSocket,
	start: AudioStartGate,
	stop: ShutdownManager<SessionShutdownReason>,
) {
	tokio::spawn(async move {
		start.wait().await;

		let mut buf = [0; 1024];
		let mut client_address = None;
		let mut audio_packets: u32 = 0;
		let mut last_audio_report = std::time::Instant::now();

		// Trigger session shutdown when the audio packet stream stops.
		let _stop_token = stop.trigger_shutdown_token(SessionShutdownReason::AudioPacketHandlerStopped);
		let _delay_stop = stop.delay_shutdown_token();

		while !stop.is_shutdown_triggered() {
			tokio::select! {
				packet = stop.wrap_cancel(packet_rx.recv()) => {
					match packet {
						Ok(Some(packet)) => {
							// Opus packet rate, at debug: 200 frames/s plus FEC
							// parity. A rate below that means the encoder is
							// starved and audio is being dropped upstream.
							audio_packets += 1;
							if last_audio_report.elapsed() >= std::time::Duration::from_secs(10) {
								tracing::debug!("audio: {audio_packets} opus packets in 10s");
								audio_packets = 0;
								last_audio_report = std::time::Instant::now();
							}
							if let Some(client_address) = client_address
								&& let Err(e) = socket.send_to(packet.as_slice(), client_address).await {
									tracing::warn!("Failed to send packet to client: {e}");
								}
						},
						_ => {
							tracing::debug!("Audio packet channel closed.");
							break;
						},
					}
				},

				message = stop.wrap_cancel(socket.recv_from(&mut buf)) => {
					let (len, address) = match message {
						Ok(Ok((len, address))) => (len, address),
						Ok(Err(e)) => {
							tracing::warn!("Failed to receive message: {e}");
							break;
						},
						Err(_) => break,
					};

					if &buf[..len] == b"PING" {
						tracing::trace!("Received audio stream PING message from {address}.");
						client_address = Some(address);
					} else {
						tracing::warn!("Received unknown message on audio stream of length {len}.");
					}
				},
			}
		}

		tracing::debug!("Audio packet stream stopped.");
	});
}
