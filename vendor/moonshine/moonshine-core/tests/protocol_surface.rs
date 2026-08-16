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
}
