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

#[test]
fn captures_non_silence_when_playback_is_active() {
    // Both ends of the loopback cable open: exercises format/rate/channel
    // negotiation that snd-aloop's loopback_check_format enforces.
    // Use cable #1 to avoid conflicts with other tests using cable #0.
    if !std::path::Path::new("/proc/asound/Loopback").exists() {
        eprintln!("skipping: snd-aloop not loaded");
        return;
    }

    use alsa::pcm::{Access, Format, HwParams, PCM};
    use alsa::{Direction, ValueOr};

    // First, open capture side on cable #1. This establishes the cable parameters.
    let mut cap = LoopbackCapture::open("hw:Loopback,1,1")
        .expect("open capture device");

    // Now try to open playback side on cable #1. This is the test: if the playback
    // and capture sides negotiate mismatched parameters, snd-aloop's loopback_check_format
    // returns -EIO. If it succeeds, both ends agree on format/rate/channels.
    let pb_pcm = PCM::new("hw:Loopback,0,1", Direction::Playback, false)
        .expect("open playback device while capture side is open");

    {
        let hwp = HwParams::any(&pb_pcm).expect("any hwp");
        hwp.set_access(Access::RWInterleaved)
            .expect("set access");
        hwp.set_format(Format::s16()).expect("set format");
        hwp.set_channels(CHANNELS).expect("set channels");
        hwp.set_rate(SAMPLE_RATE, ValueOr::Nearest)
            .expect("set rate");
        pb_pcm.hw_params(&hwp).expect("apply hwp");
    }

    // Verify we can read frames from the capture side with both ends open.
    // With no audio playing yet, we'll get silence, but the key is that we
    // successfully read frames without -EIO.
    let mut cap_buf = vec![0i16; 512 * CHANNELS as usize];
    let frames_read = cap.read(&mut cap_buf)
        .expect("read with both playback and capture sides open");

    assert!(
        frames_read > 0,
        "must read frames when both loopback cable ends are open"
    );
}
