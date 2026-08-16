fn main() {
    // librgspcast.a is produced by `make librgspcast.a` at the repo root.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    println!("cargo:rustc-link-search=native={}", root.display());
    println!("cargo:rustc-link-lib=static=rgspcast");
    println!("cargo:rustc-link-lib=dylib=dl");
    println!("cargo:rerun-if-changed={}/librgspcast.a", root.display());
}
