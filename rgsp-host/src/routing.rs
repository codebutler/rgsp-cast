use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Once;

// Audio routing via ALSA config file manipulation, with on-screen indicator.
//
// This module routes audio to the TV's loopback device while casting, then restores
// the previous routing when done. The routing is implemented by writing an ALSA
// configuration file. An on-screen indicator is lit via SetAudioSink from NextUI's
// libmsettings.
//
// SetAudioSink Integration Notes
// ==============================
//
// The indicator is driven by SetAudioSink() from libmsettings.so, located at
// `/mnt/SDCARD/.system/<platform>/lib/libmsettings.so` (h700, tg5040, tg5050).
//
// Fixed issues:
// - libmsettings depends on libtinyalsa.so.1 in the same directory, which is not on
//   the default loader search path. We preload it with RTLD_GLOBAL before loading
//   libmsettings.
// - InitSettings() must be called before any SetAudioSink() call because it maps the
//   shared memory segment that SetAudioSink dereferences. InitSettings itself does
//   `sprintf(..., getenv("USERDATA_PATH"))` without NULL checking, so it crashes when
//   started from boot.d/post-resume.d hooks. We ensure USERDATA_PATH is set first.
//
// Known issues:
// - QuitSettings() is not called on cleanup because it appears to be the cause of an
//   exit-time crash. The shared-memory segment is owned by NextUI and outlives us; the
//   kernel unmaps our view on process exit anyway.
//
// SetAudioSink is not cosmetic: it calls SetVolume(GetVolume()) → SetRawVolume() +
// SaveSettings(), which touches the real mixer and persists the setting to msettings.bin.

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

// Ensure USERDATA_PATH is set before InitSettings is called
// InitSettings dereferences getenv("USERDATA_PATH") without NULL checking
static USERDATA_PATH_INIT: Once = Once::new();

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

    // Ensure USERDATA_PATH is set before calling InitSettings.
    // InitSettings does: sprintf(SettingsPath, "%s/msettings.bin", getenv("USERDATA_PATH"))
    // without checking for NULL, so unset USERDATA_PATH → undefined behaviour segfault.
    // This happens when daemon is started outside a pak (boot.d, post-resume.d hooks).
    ensure_userdata_path();

    // Call InitSettings() to initialize the shared memory segment that stores audio settings
    unsafe {
        let init_func: extern "C" fn() = std::mem::transmute(init_sym);
        init_func();
    }

    log_debug(&format!("libmsettings initialized from {}", path_desc));
    true
}

/// Ensure USERDATA_PATH environment variable is set.
/// InitSettings dereferences getenv("USERDATA_PATH") without a NULL check.
/// If called from a pak, NextUI exports it. If called from boot.d/post-resume.d hooks,
/// it won't be set, so we provide the device default.
fn ensure_userdata_path() {
    USERDATA_PATH_INIT.call_once(|| {
        // Only set if not already set; if we are launched from a pak, use NextUI's value
        if std::env::var("USERDATA_PATH").is_err() {
            // Try to derive from SDCARD_PATH and PLATFORM if available
            let default_path = if let (Ok(sdcard), Ok(platform)) =
                (std::env::var("SDCARD_PATH"), std::env::var("PLATFORM"))
            {
                format!("{}/.userdata/{}", sdcard, platform)
            } else {
                // Fallback: h700 is the RG SP, the only device we support directly
                "/mnt/SDCARD/.userdata/h700".to_string()
            };

            std::env::set_var("USERDATA_PATH", &default_path);
            log_debug(&format!("USERDATA_PATH set to {}", default_path));
        }
    });
}

fn call_set_audio_sink(lib: *mut libc::c_void, value: i32) {
    use std::ffi::CStr;

    let sym_name = CStr::from_bytes_with_nul(b"SetAudioSink\0").unwrap();
    let sym = unsafe { libc::dlsym(lib, sym_name.as_ptr()) };

    if !sym.is_null() {
        // SetAudioSink is not cosmetic — it:
        // 1. Updates audiosink in the shared settings struct
        // 2. Calls SetVolume(GetVolume()) to apply the change to the mixer
        // 3. Calls SaveSettings() to write msettings.bin to disk
        // This is real: the mixer level changes and the setting persists across reboots.
        unsafe {
            let func: extern "C" fn(i32) = std::mem::transmute(sym);
            func(value);
        }
    }
}

fn cleanup_libmsettings() {
    let handle_ptr = LIBMSETTINGS_HANDLE.load(Ordering::Acquire) as *mut libc::c_void;
    if !handle_ptr.is_null() {
        unsafe {
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
