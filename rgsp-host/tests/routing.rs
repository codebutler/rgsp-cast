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

/// The pre-launch hook writes its own copy of the routing config when a game
/// starts mid-cast, and that copy has never carried the "Removed
/// automatically" line the daemon's `ASOUNDRC_BODY` has.
///
/// `release()` used to compare the whole body, so the hook's file looked like
/// a foreign audio manager's and was deliberately left in place - the handheld
/// then had no speaker audio until the next boot. Reachable any time someone
/// launches a game while casting.
#[test]
fn release_restores_after_the_pre_launch_hook_rewrote_the_file() {
    let dir = std::env::temp_dir().join(format!("rgsp-hookbody-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let asoundrc = dir.join(".asoundrc");
    let _ = std::fs::remove_file(&asoundrc);

    let sink = CastSink::engage(&dir).expect("engage");

    // Verbatim from pak/hooks/pre-launch.d/10-rgsp-route.sh - note it has no
    // "Removed automatically" line, which is exactly what broke the old check.
    std::fs::write(
        &asoundrc,
        "# rgsp-cast: routing playback into the kernel loopback while casting.\n\
         pcm.!default {\n    type plug\n    slave.pcm \"hw:Loopback,0,0\"\n}\n",
    )
    .unwrap();

    sink.release().expect("release");

    assert!(
        !asoundrc.exists(),
        "the hook's copy is still ours and must be cleaned up, or the speaker stays silent"
    );
}

/// A cast killed with SIGKILL leaves our config behind. The next engage must
/// not snapshot that as the "previous" config to restore, or every later clean
/// stop faithfully rewrites the loopback route and the fault latches.
#[test]
fn a_leftover_config_is_not_snapshotted_as_the_users_own() {
    let dir = std::env::temp_dir().join(format!("rgsp-latch-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let asoundrc = dir.join(".asoundrc");

    // Simulate the aftermath of a SIGKILL mid-cast.
    std::fs::write(&asoundrc, rgsp_host::routing::ASOUNDRC_BODY).unwrap();

    let sink = CastSink::engage(&dir).expect("engage over a leftover");
    sink.release().expect("release");

    assert!(
        !asoundrc.exists(),
        "a leftover of our own must be cleared, not restored as if the user had put it there"
    );
}

/// A genuine foreign config - a Bluetooth manager's, say - must survive a
/// cast untouched.
#[test]
fn a_foreign_config_is_preserved_across_a_cast() {
    let dir = std::env::temp_dir().join(format!("rgsp-foreign-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let asoundrc = dir.join(".asoundrc");

    let foreign = "pcm.!default { type plug slave.pcm \"bluealsa\" }\n";
    std::fs::write(&asoundrc, foreign).unwrap();

    let sink = CastSink::engage(&dir).expect("engage");
    sink.release().expect("release");

    assert_eq!(
        std::fs::read_to_string(&asoundrc).unwrap(),
        foreign,
        "someone else's config must come back exactly as it was"
    );
}
