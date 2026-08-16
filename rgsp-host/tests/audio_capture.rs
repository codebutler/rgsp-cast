use rgsp_host::audio::{LoopbackCapture, CHANNELS, SAMPLE_RATE};

#[test]
fn reads_silence_when_nothing_is_playing() {
    // snd-aloop yields silence rather than an error when the playback side of
    // the cable is closed, so the host can start before a game launches.
    if !std::path::Path::new("/proc/asound/Loopback").exists() {
        eprintln!("skipping: snd-aloop not loaded");
        return;
    }

    let mut cap = LoopbackCapture::open("hw:Loopback,1,0").expect("open");
    let mut buf = vec![0i16; 1024 * CHANNELS as usize];
    let frames = cap.read(&mut buf).expect("read");
    assert!(frames > 0, "must return frames, not an error");
}

#[test]
fn parameters_match_what_minarch_plays() {
    // A mismatch on format, rate or channels fails the capture side of the
    // cable with -EIO (aloop.c, loopback_check_format).
    assert_eq!(SAMPLE_RATE, 48_000);
    assert_eq!(CHANNELS, 2);
}
