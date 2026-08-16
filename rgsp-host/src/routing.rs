use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// alsa-lib reads $USERDATA_PATH/.asoundrc after /etc/asound.conf and the last
/// pcm.!default wins, so this file selects the sink. `type plug` so a game
/// asking for something other than 48 kHz stereo still works.
pub const ASOUNDRC_BODY: &str = "\
# rgsp-cast: routing playback into the kernel loopback while casting.
# Removed automatically when casting stops.
pcm.!default {
    type plug
    slave.pcm \"hw:Loopback,0,0\"
}
";

/// Values from NextUI's libmsettings. Setting anything other than DEFAULT is
/// what lights up the external-audio icon in the status pill
/// (GFX_blitHardwareGroup, api.c:2294).
const AUDIO_SINK_DEFAULT: i32 = 0;
const AUDIO_SINK_USBDAC: i32 = 2;

pub struct CastSink {
    asoundrc: PathBuf,
    previous: Option<String>,
}

impl CastSink {
    pub fn engage(userdata: &Path) -> Result<CastSink> {
        let asoundrc = userdata.join(".asoundrc");
        let previous = std::fs::read_to_string(&asoundrc).ok();

        std::fs::write(&asoundrc, ASOUNDRC_BODY)
            .with_context(|| format!("writing {}", asoundrc.display()))?;

        set_audio_sink(AUDIO_SINK_USBDAC);

        Ok(CastSink { asoundrc, previous })
    }

    pub fn release(self) -> Result<()> {
        match &self.previous {
            Some(body) => std::fs::write(&self.asoundrc, body)
                .with_context(|| format!("restoring {}", self.asoundrc.display()))?,
            None => {
                let _ = std::fs::remove_file(&self.asoundrc);
            }
        }
        set_audio_sink(AUDIO_SINK_DEFAULT);
        Ok(())
    }
}

/// Load libmsettings at runtime and call SetAudioSink if available.
/// libmsettings is only present on the device; this is a no-op in test/build environments.
fn set_audio_sink(value: i32) {
    use std::ffi::CStr;

    // Try to load libmsettings in order of preference:
    // 1. Bare soname: when running from pak, LD_LIBRARY_PATH contains the platform lib dir
    // 2. From environment variables: SDCARD_PATH and PLATFORM (exported by NextUI)
    // 3. Direct path: for testing outside pak or hardcoded fallback paths
    //
    // Note: libmsettings.so depends on libtinyalsa.so.1, which sits in the same directory
    // but is not on the default loader search path. We preload it with RTLD_GLOBAL first
    // so its symbols are available when libmsettings is loaded.

    // Attempt 1: bare soname (relies on LD_LIBRARY_PATH from pak launch)
    let lib_name = CStr::from_bytes_with_nul(b"libmsettings.so\0").unwrap();
    let lib = unsafe { libc::dlopen(lib_name.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL) };
    if !lib.is_null() {
        if call_set_audio_sink(lib, value) {
            log_debug("libmsettings loaded via LD_LIBRARY_PATH");
            unsafe { libc::dlclose(lib); }
            return;
        }
        unsafe { libc::dlclose(lib); }
        let err = get_dlerror();
        log_debug(&format!("bare soname load failed: {}", err));
    }

    // Attempt 2: from environment variables (SDCARD_PATH and PLATFORM)
    if let (Ok(sdcard), Ok(platform)) = (std::env::var("SDCARD_PATH"), std::env::var("PLATFORM")) {
        let lib_dir = format!("{}/.system/{}/lib", sdcard, platform);
        if try_load_from_dir(&lib_dir, value) {
            log_debug(&format!("libmsettings loaded from {}", lib_dir));
            return;
        }
    }

    // Attempt 3: fallback paths for direct testing (device families)
    let fallback_dirs = [
        "/mnt/SDCARD/.system/h700/lib",
        "/mnt/SDCARD/.system/tg5040/lib",
        "/mnt/SDCARD/.system/tg5050/lib",
    ];

    for dir in fallback_dirs.iter() {
        if try_load_from_dir(dir, value) {
            log_debug(&format!("libmsettings loaded from {}", dir));
            return;
        }
    }

    // All attempts exhausted; log once at warn level with diagnostic info
    log_warn("libmsettings not found; external audio indicator will not light. Check SDCARD_PATH/PLATFORM env vars, device library paths, or loader search path.");
}

fn try_load_from_dir(lib_dir: &str, value: i32) -> bool {
    // Preload libtinyalsa.so.1 with RTLD_GLOBAL so its symbols are available when
    // libmsettings is loaded. Failures on this preload are ignored; if it's already
    // resolvable elsewhere, the preload is harmless.
    let tinyalsa_path = format!("{}/libtinyalsa.so.1", lib_dir);
    if let Ok(c_path) = std::ffi::CString::new(tinyalsa_path) {
        let _ = unsafe { libc::dlopen(c_path.as_ptr(), libc::RTLD_LAZY | libc::RTLD_GLOBAL) };
    }

    // Now load libmsettings from the same directory
    let libmsettings_path = format!("{}/libmsettings.so", lib_dir);
    if let Ok(c_path) = std::ffi::CString::new(libmsettings_path.clone()) {
        let lib = unsafe { libc::dlopen(c_path.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL) };
        if !lib.is_null() {
            if call_set_audio_sink(lib, value) {
                unsafe { libc::dlclose(lib); }
                return true;
            }
            unsafe { libc::dlclose(lib); }
        }
        let err = get_dlerror();
        log_debug(&format!("failed to load from {}: {}", libmsettings_path, err));
    }

    false
}

fn call_set_audio_sink(lib: *mut libc::c_void, value: i32) -> bool {
    use std::ffi::CStr;

    let sym_name = CStr::from_bytes_with_nul(b"SetAudioSink\0").unwrap();
    let sym = unsafe { libc::dlsym(lib, sym_name.as_ptr()) };

    if !sym.is_null() {
        unsafe {
            let func: extern "C" fn(i32) = std::mem::transmute(sym);
            func(value);
        }
        return true;
    }
    false
}

fn get_dlerror() -> String {
    use std::ffi::CStr;

    let err_ptr = unsafe { libc::dlerror() };
    if err_ptr.is_null() {
        "no error".to_string()
    } else {
        unsafe {
            CStr::from_ptr(err_ptr)
                .to_string_lossy()
                .into_owned()
        }
    }
}

fn log_debug(msg: &str) {
    if std::env::var("RGSP_DEBUG").is_ok() {
        eprintln!("[DEBUG] {}", msg);
    }
}

fn log_warn(msg: &str) {
    eprintln!("[WARN] {}", msg);
}
