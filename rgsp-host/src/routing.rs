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

    // Paths where libmsettings might exist on the device
    let library_paths = [
        "/usr/trimui/lib/libmsettings.so",
        "/usr/lib/libmsettings.so",
    ];

    for path in library_paths.iter() {
        let c_path = std::ffi::CString::new(*path).unwrap();
        let lib = unsafe { libc::dlopen(c_path.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL) };

        if !lib.is_null() {
            // Library loaded; now try to find SetAudioSink
            let sym_name = CStr::from_bytes_with_nul(b"SetAudioSink\0").unwrap();
            let sym = unsafe { libc::dlsym(lib, sym_name.as_ptr()) };

            if !sym.is_null() {
                // Found the symbol; call it
                unsafe {
                    let func: extern "C" fn(i32) = std::mem::transmute(sym);
                    func(value);
                }
            }

            unsafe {
                libc::dlclose(lib);
            }
            return; // Successfully loaded and attempted to call
        }
    }
    // If we get here, libmsettings wasn't found or the symbol wasn't available.
    // This is expected in test/container environments and is not an error.
}
