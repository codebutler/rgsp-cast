// Guards the visibility edits this project depends on. If a `git subtree pull`
// reverts them, this fails loudly instead of at the call site.
#[test]
fn protocol_layer_is_public() {
	// Compile-time reachability: naming the paths is the assertion.
	#[allow(unused_imports)]
	use moonshine_core::crypto;
	#[allow(unused_imports)]
	use moonshine_core::session::stream::video::gso_socket;
	#[allow(unused_imports)]
	use moonshine_core::session::stream::video::packetizer::Packetizer;
	#[allow(unused_imports)]
	use moonshine_core::session::stream::video::shard_batch::ShardBatch;

	// Individual symbols consumed by rgsp-host, named directly so a
	// `git subtree pull` that narrows visibility fails here instead of at
	// the call site.
	#[allow(unused_imports)]
	use moonshine_core::session::stream::video::EncodedFrame;
	#[allow(unused_imports)]
	use moonshine_core::session::stream::video::EncoderControl;
	#[allow(unused_imports)]
	use moonshine_core::session::stream::video::gso_socket::UdpGsoSocket;
	#[allow(unused_imports)]
	use moonshine_core::session::stream::video::packetizer::MAX_SHARDS;
	#[allow(unused_imports)]
	use moonshine_core::session::stream::video::shard_batch::ShardBuf;
	// A crypto function, not just the module.
	let _ = moonshine_core::crypto::encrypt;
	// The seam this task added: SessionManager's accessor for the encoded-frame
	// channel that feeds the video stream's packetize loop.
	let _ = moonshine_core::session::manager::SessionManager::video_frame_sender;
	// Symmetric accessor for client recovery requests (IDR / reference
	// invalidation / reset), reaching the host encoder from outside the crate.
	let _ = moonshine_core::session::manager::SessionManager::encoder_control_receiver;
}
