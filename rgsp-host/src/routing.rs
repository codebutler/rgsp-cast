use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Once;

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
        cleanup_libmsettings();
        Ok(())
    }
}

// Global library handle, initialized once
// We store the pointer as a usize in an atomic so it can be shared safely across threads.
// The pointer is only written during initialization (Once) and read during use.
static LIBMSETTINGS_INIT: Once = Once::new();
static LIBMSETTINGS_HANDLE: AtomicUsize = AtomicUsize::new(0);

/// Load libmsettings at runtime and call SetAudioSink if available.
/// libmsettings is only present on the device; this is a no-op in test/build environments.
fn set_audio_sink(value: i32) {
    // Initialize library exactly once
    LIBMSETTINGS_INIT.call_once(|| {
        if let Some(handle) = load_and_init_libmsettings() {
            LIBMSETTINGS_HANDLE.store(handle as usize, Ordering::Release);
        }
    });

    // Call SetAudioSink if library is available
    let handle_ptr = LIBMSETTINGS_HANDLE.load(Ordering::Acquire) as *mut libc::c_void;
    if !handle_ptr.is_null() {
        call_set_audio_sink(handle_ptr, value);
    }
}

/// Load libmsettings and call InitSettings().
/// Returns the handle if successful, None otherwise.
fn load_and_init_libmsettings() -> Option<*mut libc::c_void> {
    use std::ffi::CStr;

    // Try to load libmsettings in order of preference:
    // 1. Bare soname: when running from pak, LD_LIBRARY_PATH contains the platform lib dir
    // 2. From environment variables: SDCARD_PATH and PLATFORM (exported by NextUI)
    // 3. Direct path: for testing outside pak or hardcoded fallback paths

    // Attempt 1: bare soname (relies on LD_LIBRARY_PATH from pak launch)
    let lib_name = CStr::from_bytes_with_nul(b"libmsettings.so\0").unwrap();
    let lib = unsafe { libc::dlopen(lib_name.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL) };
    if !lib.is_null() {
        if try_init_library(lib, "LD_LIBRARY_PATH") {
            return Some(lib);
        }
        unsafe { libc::dlclose(lib); }
    }

    // Attempt 2: from environment variables (SDCARD_PATH and PLATFORM)
    if let (Ok(sdcard), Ok(platform)) = (std::env::var("SDCARD_PATH"), std::env::var("PLATFORM")) {
        let lib_dir = format!("{}/.system/{}/lib", sdcard, platform);
        if let Some(handle) = try_load_and_init_from_dir(&lib_dir) {
            return Some(handle);
        }
    }

    // Attempt 3: fallback paths for direct testing (device families)
    let fallback_dirs = [
        "/mnt/SDCARD/.system/h700/lib",
        "/mnt/SDCARD/.system/tg5040/lib",
        "/mnt/SDCARD/.system/tg5050/lib",
    ];

    for dir in fallback_dirs.iter() {
        if let Some(handle) = try_load_and_init_from_dir(dir) {
            return Some(handle);
        }
    }

    // All attempts exhausted; log once at warn level with diagnostic info
    log_warn("libmsettings not found; external audio indicator will not light. Check SDCARD_PATH/PLATFORM env vars, device library paths, or loader search path.");
    None
}

fn try_load_and_init_from_dir(lib_dir: &str) -> Option<*mut libc::c_void> {
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
            if try_init_library(lib, &libmsettings_path) {
                return Some(lib);
            }
            unsafe { libc::dlclose(lib); }
        } else {
            let err = get_dlerror();
            log_debug(&format!("failed to load from {}: {}", libmsettings_path, err));
        }
    }

    None
}

/// Try to resolve InitSettings and SetAudioSink, then call InitSettings.
/// Returns true if both symbols resolved and InitSettings succeeded.
fn try_init_library(lib: *mut libc::c_void, path_desc: &str) -> bool {
    use std::ffi::CStr;

    // Resolve InitSettings
    let init_name = CStr::from_bytes_with_nul(b"InitSettings\0").unwrap();
    let init_sym = unsafe { libc::dlsym(lib, init_name.as_ptr()) };
    if init_sym.is_null() {
        let err = get_dlerror();
        log_debug(&format!("InitSettings not found in {}: {}", path_desc, err));
        return false;
    }

    // Resolve SetAudioSink
    let sink_name = CStr::from_bytes_with_nul(b"SetAudioSink\0").unwrap();
    let sink_sym = unsafe { libc::dlsym(lib, sink_name.as_ptr()) };
    if sink_sym.is_null() {
        let err = get_dlerror();
        log_debug(&format!("SetAudioSink not found in {}: {}", path_desc, err));
        return false;
    }

    // Call InitSettings() to initialize the shared memory segment
    unsafe {
        let init_func: extern "C" fn() = std::mem::transmute(init_sym);
        init_func();
    }

    log_debug(&format!("libmsettings initialized from {}", path_desc));
    true
}

fn call_set_audio_sink(lib: *mut libc::c_void, value: i32) {
    use std::ffi::CStr;

    let sym_name = CStr::from_bytes_with_nul(b"SetAudioSink\0").unwrap();
    let sym = unsafe { libc::dlsym(lib, sym_name.as_ptr()) };

    if !sym.is_null() {
        // SetAudioSink calls SetVolume(GetVolume()) internally, which affects the device's
        // audio mixer. This is intentional — it's how the sink change takes effect on the device.
        unsafe {
            let func: extern "C" fn(i32) = std::mem::transmute(sym);
            func(value);
        }
    }
}

fn cleanup_libmsettings() {
    use std::ffi::CStr;

    let handle_ptr = LIBMSETTINGS_HANDLE.load(Ordering::Acquire) as *mut libc::c_void;
    if !handle_ptr.is_null() {
        unsafe {
            // Try to call QuitSettings if it exists to clean up the shared memory mapping.
            // This is not strictly required (the mapping persists until process exit) but
            // follows NextUI's own cleanup pattern.
            let quit_name = CStr::from_bytes_with_nul(b"QuitSettings\0").unwrap();
            let quit_sym = libc::dlsym(handle_ptr, quit_name.as_ptr());
            if !quit_sym.is_null() {
                let quit_func: extern "C" fn() = std::mem::transmute(quit_sym);
                quit_func();
            }

            libc::dlclose(handle_ptr);
        }
    }
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
