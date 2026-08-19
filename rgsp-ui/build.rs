use std::{
    env,
    path::{Path, PathBuf},
};

fn main() {
    // FIXED_SCALE is a compile-time 2 only on h700; on tg5040 it is a runtime
    // ternary. A wrong constant is worse than a missing one, so refuse to build
    // anywhere else rather than silently scaling by the wrong factor.
    println!("cargo:rerun-if-env-changed=RGSP_PLATFORM");
    let platform = env::var("RGSP_PLATFORM").unwrap_or_else(|_| "h700".into());
    assert_eq!(platform, "h700", "rgsp-ui supports only PLATFORM=h700");

    let vendor = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("vendor");
    let root = vendor.join("nextui");
    let common = root.join("common");
    let plat = root.join("h700");
    let tinyalsa = vendor.join("tinyalsa");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());

    let sdl = pkg_config::probe_library("sdl2").expect("sdl2 dev headers missing");

    build_msettings(&common, &plat, &tinyalsa, &out);

    let mut build = cc::Build::new();
    build
        .std("gnu99")
        .include(&common)
        .include(&plat)
        .define("PLATFORM", "\"h700\"")
        .define("USE_SDL2", None)
        .define("USE_GLES", None)
        .define("GL_GLEXT_PROTOTYPES", None)
        // generic_video.c, generic_wifi.c, generic_bt.c and led.c are absent on
        // purpose: platform.c #includes them textually (platform.c:1057-1073),
        // so compiling them again would define every symbol twice.
        .files([
            common.join("api.c"),
            common.join("utils.c"),
            common.join("config.c"),
            common.join("scaler.c"),
            plat.join("platform.c"),
        ]);
    for p in &sdl.include_paths {
        build.include(p);
    }
    // NextUI's own code, not ours: its warnings are not actionable here.
    build.warnings(false).compile("nextui");

    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=dylib=msettings");
    for lib in [
        "SDL2",
        "SDL2_image",
        "SDL2_ttf",
        "GLESv2",
        "samplerate",
        "pthread",
        "dl",
        "m",
        "z",
    ] {
        println!("cargo:rustc-link-lib={lib}");
    }

    let mut builder = bindgen::Builder::default()
        // defines.h first, exactly as api.c includes them: api.h:512 sizes an
        // array with BTN_ID_COUNT, which is a defines.h enumerator, so api.h
        // alone does not parse.
        .header(common.join("defines.h").to_str().unwrap())
        .header(common.join("api.h").to_str().unwrap())
        // InitSettings/QuitSettings live in msettings.h, which api.h does not
        // include — api.c pulls it in separately.
        .header(plat.join("msettings.h").to_str().unwrap())
        .clang_args(["-I", common.to_str().unwrap(), "-I", plat.to_str().unwrap()])
        .clang_args(["-DPLATFORM=\"h700\"", "-DUSE_SDL2", "-DUSE_GLES"])
        .allowlist_function("GFX_.*|PLAT_.*|PWR_.*|PAD_.*|SND_.*|InitSettings|QuitSettings")
        .allowlist_type("GFX_Fonts")
        .allowlist_var("font|FIXED_SCALE|PILL_SIZE|BUTTON_SIZE|MODE_MENU|BTN_.*|ASSET_.*")
        // SDL types come from sdl2-sys; a second copy would be a second ABI to
        // keep correct by hand. TTF_Font is deliberately NOT blocked: sdl2-sys
        // only defines it under its "ttf" feature, which we do not enable, so
        // blocking it would leave GFX_Fonts naming a type that does not exist.
        // It is an opaque handle either way.
        .blocklist_type("SDL_.*|_SDL.*")
        .raw_line("use sdl2_sys::*;");
    for p in &sdl.include_paths {
        builder = builder.clang_arg(format!("-I{}", p.display()));
    }
    builder
        .generate()
        .expect("bindgen failed")
        .write_to_file(out.join("nextui.rs"))
        .expect("write bindings");

    println!("cargo:rerun-if-changed=vendor/nextui");
    println!("cargo:rerun-if-changed=vendor/tinyalsa");
}

/// Build `libmsettings.so` into `out`, the way NextUI builds it.
///
/// It has to be a shared library, not part of the static archive: `msettings.c`
/// defines its own `getInt`, `putInt`, `putFile`, `touch` and `exactMatch`,
/// which collide at link time with the identically-named functions in
/// `utils.c`. Upstream hits the same wall and solves it the same way — every
/// NextUI app compiles `utils.c` and links `-lmsettings`.
///
/// `displaycal.c` goes in alongside it because NextUI's own libmsettings
/// makefile builds both into the one library. tinyalsa is linked in statically
/// here: `msettings.c` needs thirteen `mixer_*` symbols and bookworm ships no
/// tinyalsa package, so the mixer is vendored and compiled rather than found.
/// It is pinned to tag 1.1.1, the version the device ships — see
/// `vendor/tinyalsa/PROVENANCE.md`.
///
/// The device already ships its own `libmsettings.so` on `LD_LIBRARY_PATH`, and
/// that is the copy loaded at runtime; this one exists so the container has
/// something to link against without needing a device on the network.
fn build_msettings(common: &Path, plat: &Path, tinyalsa: &Path, out: &Path) {
    let mut cfg = cc::Build::new();
    cfg.std("gnu99")
        .include(common)
        .include(plat)
        .include(tinyalsa.join("include"))
        // Third-party code, not ours: its warnings are not actionable here.
        .warnings(false);

    let so = out.join("libmsettings.so");
    let mut cmd = cfg.get_compiler().to_command();
    cmd.args(["-shared", "-fPIC"])
        .arg("-o")
        .arg(&so)
        .args([
            plat.join("msettings.c"),
            common.join("displaycal.c"),
            // mixer.c alone defines all thirteen mixer_* symbols msettings.c
            // needs; nothing here plays PCM audio.
            tinyalsa.join("src/mixer.c"),
        ])
        .args(["-ldl", "-lrt", "-lm"]);

    let status = cmd
        .status()
        .unwrap_or_else(|e| panic!("could not run the C compiler for libmsettings.so: {e}"));
    assert!(status.success(), "building {} failed", so.display());
}
