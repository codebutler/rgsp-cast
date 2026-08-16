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
