#[test]
fn inputtino_is_not_in_the_dependency_graph() {
    // The player holds the handheld, so the input backchannel has no work to
    // do - and dropping it removes a C++ dependency from the cross build.
    let lock = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("Cargo.lock"),
    )
    .expect("Cargo.lock");
    assert!(!lock.contains("name = \"inputtino\""), "inputtino must be gone");
}
