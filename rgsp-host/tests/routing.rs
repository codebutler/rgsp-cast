use rgsp_host::routing::CastSink;

#[test]
fn engage_writes_the_loopback_default_and_release_restores() {
    let dir = std::env::temp_dir().join("rgsp-routing-test");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let asoundrc = dir.join(".asoundrc");

    let sink = CastSink::engage(&dir).expect("engage");
    let written = std::fs::read_to_string(&asoundrc).expect("asoundrc written");
    assert!(written.contains("hw:Loopback,0,0"));
    assert!(written.contains("pcm.!default"));

    sink.release().expect("release");
    assert!(!asoundrc.exists(), "release removes the file when there was none before");
}

#[test]
fn release_restores_a_preexisting_asoundrc() {
    // audiomon writes this file for bluetooth and USB. If one of those was
    // active when casting started, casting must hand it back untouched.
    let dir = std::env::temp_dir().join("rgsp-routing-test2");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let asoundrc = dir.join(".asoundrc");
    let original = "pcm.!default { type plug slave.pcm { type bluealsa } }\n";
    std::fs::write(&asoundrc, original).unwrap();

    let sink = CastSink::engage(&dir).expect("engage");
    assert!(std::fs::read_to_string(&asoundrc).unwrap().contains("Loopback"));

    sink.release().expect("release");
    assert_eq!(std::fs::read_to_string(&asoundrc).unwrap(), original);
}

#[test]
fn repeated_engage_release_cycles() {
    // Test that engage/release cycles work correctly when called multiple times.
    // Note: This test cannot verify the libmsettings path (SetAudioSink/release on device)
    // because the library doesn't exist in containers. It only locks in the file-level
    // behavior. The use-after-free fix (not calling dlclose) would manifest on device
    // during the second cycle if the fix regressed.
    let dir = std::env::temp_dir().join("rgsp-routing-test3");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let asoundrc = dir.join(".asoundrc");

    // First cycle
    let sink1 = CastSink::engage(&dir).expect("first engage");
    let written1 = std::fs::read_to_string(&asoundrc).expect("first cycle: asoundrc written");
    assert!(written1.contains("hw:Loopback,0,0"));

    sink1.release().expect("first release");
    assert!(!asoundrc.exists(), "first release: file removed when there was none before");

    // Second cycle: should work identically
    let sink2 = CastSink::engage(&dir).expect("second engage");
    let written2 = std::fs::read_to_string(&asoundrc).expect("second cycle: asoundrc written");
    assert!(written2.contains("hw:Loopback,0,0"));
    assert_eq!(written1, written2, "second cycle: written content matches first cycle");

    sink2.release().expect("second release");
    assert!(!asoundrc.exists(), "second release: file removed when there was none before");
}
