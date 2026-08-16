// The packetizer is pure: given an encoded frame it must produce shards whose
// payload sums back to the frame plus per-shard headers. That is testable off
// the device, which is where the protocol bugs actually get caught.
use moonshine_core::session::SessionKeyData;
use moonshine_core::session::stream::video::packetizer::Packetizer;
use rgsp_host::video::rtp_timestamp_for;

#[test]
fn rtp_timestamp_advances_at_90khz() {
    // GameStream video uses a 90 kHz RTP clock. At 30 fps that is 3000 ticks
    // per frame; drift here shows up as stutter on the client.
    assert_eq!(rtp_timestamp_for(0, 30), 0);
    assert_eq!(rtp_timestamp_for(1, 30), 3000);
    assert_eq!(rtp_timestamp_for(30, 30), 90_000);
    assert_eq!(rtp_timestamp_for(60, 60), 90_000);
}

#[test]
fn packetizer_splits_a_frame_into_shards() {
    // SessionKeyData has no Default impl; construct it directly.
    let (_tx, rx) = tokio::sync::watch::channel(SessionKeyData {
        remote_input_key: Vec::new(),
        remote_input_key_id: 0,
    });
    let mut p = Packetizer::new(false, rx);
    p.warm_up(20, 2);

    let frame = vec![0u8; 20_000];
    let mut seq = 0u32;
    let batch = p
        .packetize(&frame, true, 1024, 2, 20, 0, &mut seq, 0, 0)
        .expect("packetize");
    assert!(seq > 0, "sequence number must advance");
    drop(batch);
}
