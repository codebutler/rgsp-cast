use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Audio routing via ALSA config file manipulation.
///
/// This module routes audio to the TV's loopback device while casting, then restores
/// the previous routing when done. The routing is implemented by writing an ALSA
/// configuration file that points pcm.!default at the loopback sink.
///
/// # On-Screen Indicator (Deferred)
///
/// NextUI's status pill can show an external-audio icon when SetAudioSink() is called
/// with a non-DEFAULT value. This was attempted but encountered issues:
///
/// - SetAudioSink is exported by libmsettings.so, located at:
///   `/mnt/SDCARD/.system/<platform>/lib/libmsettings.so` (h700, tg5040, tg5050)
///
/// - The library depends on libtinyalsa.so.1 in the same directory. That dependency
///   is not on the default library loader search path, requiring RTLD_GLOBAL preload.
///
/// - InitSettings() must be called before any SetAudioSink() call, because it maps
///   the shared memory segment that SetAudioSink dereferences. But InitSettings does:
///   `sprintf(SettingsPath, "%s/msettings.bin", getenv("USERDATA_PATH"))`
///   without NULL checking — so if USERDATA_PATH is unset (happens when started from
///   boot.d/post-resume.d hooks, not from a pak), the getenv returns NULL and the
///   sprintf is undefined behaviour.
///
/// - Even after fixing all three above issues (correct paths, dependency preload,
///   ensuring USERDATA_PATH), the call still crashes on device with an unknown cause.
///   SetAudioSink also calls SetVolume(GetVolume()) → SetRawVolume() + SaveSettings(),
///   which touches the real mixer and writes msettings.bin to disk; the remaining
///   fault is plausibly there.
///
/// The routing (writing .asoundrc) is the core feature and works correctly. The
/// indicator is cosmetic and not worth the crash risk in a daemon meant to survive
/// gaming sessions. This can be revisited if NextUI provides a safer indicator API.

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
        Ok(())
    }
}
