use rgsp_cedar::vendor_lib::VendorLibs;

fn on_device() -> bool {
    std::path::Path::new("/dev/fb0").exists()
}

#[test]
fn loads_every_required_symbol() {
    if !on_device() {
        eprintln!("skipping: no /dev/fb0");
        return;
    }
    let libs = VendorLibs::load().expect("load vendor libs");
    // The optional pair: the C tolerates their absence, but this build has
    // them, and without set_parameter there is no bitrate and no forced IDR.
    assert!(libs.video_enc_set_parameter.is_some());
    assert!(libs.video_enc_get_parameter.is_some());
}

#[test]
fn loading_twice_returns_the_same_singleton() {
    if !on_device() {
        eprintln!("skipping: no /dev/fb0");
        return;
    }
    // dlclose was deliberately dropped and loading made idempotent so that a
    // capture reopen works at all - see tests/reopen_leak.rs.
    let a = VendorLibs::load().unwrap();
    let b = VendorLibs::load().unwrap();
    assert!(std::ptr::eq(a, b));
}
