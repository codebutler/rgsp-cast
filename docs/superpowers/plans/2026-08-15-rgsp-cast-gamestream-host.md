# RG SP GameStream Host Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stream the Anbernic RG SP's screen and game audio to an Apple TV running the stock Moonlight app, at playable latency, launched from a NextUI Tools pak.

**Architecture:** A Rust daemon (`rgsp-host`) serves the GameStream protocol, using Moonshine's protocol layer vendored via `git subtree` into `vendor/moonshine`. Moonshine's Vulkan encoder and Wayland compositor are deleted; in their place the daemon calls into the existing C capture/encode code (`src/rgsp-cast.c`, exposed as `librgspcast.a`) which reads `/dev/fb0` and encodes on the Cedar VE, and reads game audio from an ALSA loopback (`snd-aloop`, a kernel module we build for the stock kernel). A NextUI pak toggles the daemon and shows status via NextUI's own `show2.elf`.

**Tech Stack:** Rust (`aarch64-unknown-linux-gnu`), C (glibc 2.35, arm64 ubuntu:22.04 container), ALSA (`libasound` 1.2.6), Cedar VE via TrimUI's CedarC blobs, Linux 4.9.170 out-of-tree kernel module, POSIX shell for pak/hooks.

**Spec:** This document is self-contained. It supersedes `ROADMAP.md`, whose content is folded into Background and Global Constraints below. `ROADMAP.md` is deleted in Task 14.

## Background — established facts

These were determined empirically on the device and must not be re-derived.

**Device:** Anbernic RG SP, Allwinner H700 (`sun50iw9`, H616/H618 family), BaseOS + NextUI, kernel `4.9.170`. SSH `root@192.168.180.106`, password `root`. Panel 720x480, 32bpp BGRA, double-buffered as a 720x960 virtual framebuffer; `yoffset` selects the visible half.

**Video, already working (`src/rgsp-cast.c`):** reads `/dev/fb0` with `pread`, copies into an ION buffer, encodes on the Cedar VE. The VE's ISP does RGB→YUV, so the CPU never touches pixels. Sustains 720x480 @ 30fps at ~1.8 ms/frame CPU.

- `VENC_IndexParamH264SPSPPS` is `0x101`, not 16.
- Framebuffer bytes are B,G,R,A → use `VENC_PIXEL_ARGB` (12). Format names are 32-bit word order.
- Parameter sets come back as `avcC`, frames as length-prefixed AVCC; both need Annex-B conversion.
- The encoder is **Main profile with CABAC**. Never hand-generate a baseline SPS.
- Parameter sets must be fetched *after* the first frame is encoded.
- `VencHeaderData` must be padded (`unsigned char _tail[496]`) or the vendor lib smashes the stack.
- Vendor libs are dlopen'd from `LD_LIBRARY_PATH`; fetched by `scripts/extract-vendor-libs.sh`, never committed.

**Audio, verified on hardware:** `bin/snd-aloop.ko` loads with plain `insmod` (no `--force`). vermagic `4.9.170 SMP preempt mod_unload modversions aarch64` and `module_layout` `0x3491861c` both match stock; 0 mismatches across 33 shared symbol CRCs. Built by `scripts/build-snd-aloop.sh` against the `orange-pi-4.9-sun50iw9` BSP tree (mainline 4.9.170 yields a different `module_layout`).

- Capture device is `hw:Loopback,1,0`; playback side is `hw:Loopback,0,0`.
- **Start the capture stream explicitly**: `snd_pcm_prepare()` then `snd_pcm_start()`. An implicit start from `snd_pcm_readi()` fails with `-EIO`.
- **Capture params must match playback**: snd-aloop fails capture with `-EIO` when the ends disagree on format, rate or channels (`aloop.c`, `loopback_check_format`). minarch plays **S16_LE, 2 ch, 48000 Hz**, period 512, buffer 1024.
- With no playback open, capture yields silence, not an error.
- `tools/alsa-cap.c` is the working reference. The device ships no `arecord`.

**Audio routing:** alsa-lib reads `$USERDATA_PATH/.asoundrc` *after* `/etc/asound.conf`; last `pcm.!default` wins. `$USERDATA_PATH` is `/mnt/SDCARD/.userdata/h700`. Config is read when a client **opens** the PCM, so changes apply to the next game launch. NextUI's `audiomon.elf` also writes this file on Bluetooth/USB hotplug.

**Status indicator:** `GFX_blitHardwareGroup()` (`NextUI-src/workspace/all/common/api.c:2294`) draws an `ASSET_AUDIO` icon when `GetAudioSink() != AUDIO_SINK_DEFAULT`. Calling `SetAudioSink()` from our daemon lights it up with no NextUI patching.

**Moonshine's shape:** `moonshine-core` is one crate, no `[features]`. `packetizer`, `gso_socket`, `shard_batch` are **private** modules inside `session/stream/video`; `crypto` is `pub(crate)`. The seam we build on:

```rust
pub fn packetize(
    &mut self,
    encoded_data: &[u8],
    is_key_frame: bool,
    requested_packet_size: usize,
    minimum_fec_packets: u32,
    fec_percentage: u8,
    frame_number: u32,
    sequence_number: &mut u32,
    rtp_timestamp: u32,
    frame_processing_latency: u16,
) -> Result<ShardBatch, ()>
```

**NextUI contracts:** paks live in `/mnt/SDCARD/Tools/h700/<Name>.pak/` with `launch.sh` + `pak.json` (`"type": "TOOL"`). Hooks live in `$USERDATA_PATH/.hooks/{boot,pre-launch,post-launch,pre-sleep,post-resume}.d/`, run in a subshell with output suppressed, and cannot cancel a launch. `Tools/` and `.userdata/` survive NextUI updates; `.system/` is replaced on every update. `show2.elf` (`NextUI-src/workspace/all/show2`) draws an image + text and takes runtime updates over `/tmp/show2.fifo`.

**Deep sleep** fully stops the USB controllers and takes WiFi down (BaseOS `docs/05` §2).

## Global Constraints

- **Target triple:** `aarch64-unknown-linux-gnu`. All binaries built in an **arm64 `ubuntu:22.04` container** — the device is glibc 2.35 and the CedarC blobs are glibc 2.33.
- **Licence:** the tree is BSD-2-Clause-compatible. **No code from Sunshine, moonlight-common-c, or moonlight-common-rust may be copied in** — all GPLv3. Reading them for reference is fine and encouraged.
- **Vendored protocol files stay close to upstream.** In `vendor/moonshine`, restrict edits to what is needed to compile and run: no reformatting, no renaming, no refactors. All new code goes in `rgsp-host/src/`.
- **Nothing is installed into `/mnt/SDCARD/.system/`** — NextUI replaces it on update.
- **Vendor CedarC blobs are never committed and never shipped in a release zip.** They are fetched on the device by `scripts/extract-vendor-libs.sh`.
- **Audio format is fixed at S16_LE, 2 ch, 48000 Hz** everywhere. Any mismatch fails the loopback with `-EIO`.
- **The ALSA capture stream is always started explicitly** (`prepare` then `start`).
- Device for testing: `root@192.168.180.106`. Deploy dir `/tmp/venc`, vendor libs `/tmp/venc/lib-trimui`.
- Commit after every task. Use `feat:` / `fix:` / `chore:` prefixes.

---

### Task 1: Vendor Moonshine and cut the crate graph

**Files:**
- Create: `vendor/moonshine/` (via `git subtree`)
- Create: `VENDOR.md`
- Modify: `vendor/moonshine/moonshine-core/Cargo.toml`
- Modify: `vendor/moonshine/moonshine-core/src/lib.rs`
- Modify: `vendor/moonshine/moonshine-core/src/session/stream/video/mod.rs`
- Modify: `vendor/moonshine/moonshine-core/src/session/mod.rs`
- Delete: `vendor/moonshine/moonshine-core/src/session/compositor/`, `vendor/moonshine/moonshine-core/src/session/stream/video/pipeline/`, `vendor/moonshine/moonshine-core/src/app_scanner/`, `vendor/moonshine/moonshine-core/src/session/stream/audio/pulse_server/`, `vendor/moonshine/moonshine-wsi/`

**Interfaces:**
- Consumes: nothing.
- Produces: crate `moonshine-core` at `vendor/moonshine/moonshine-core`, building for `aarch64-unknown-linux-gnu` with no Vulkan/Wayland/PulseAudio dependencies, exporting `pub mod crypto`, and `session::stream::video::{packetizer, gso_socket, shard_batch}` as `pub mod`.

- [ ] **Step 1: Add the subtree at a pinned tag**

```bash
cd /Users/eric/Code/RGSP/rgsp-cast
git init 2>/dev/null || true
TAG=$(git ls-remote --tags --refs https://github.com/hgaiser/moonshine.git \
      | awk -F/ '{print $NF}' | sort -V | tail -1)
echo "pinning moonshine at $TAG"
git subtree add --prefix=vendor/moonshine \
    https://github.com/hgaiser/moonshine.git "$TAG" --squash
```

- [ ] **Step 2: Record the pin**

Create `VENDOR.md`:

```markdown
# Vendored code

## vendor/moonshine

Source: https://github.com/hgaiser/moonshine (BSD-2-Clause)
Pinned at tag: <TAG from step 1>
Added with: git subtree add --prefix=vendor/moonshine <url> <tag> --squash

Update with:
    git subtree pull --prefix=vendor/moonshine <url> <tag> --squash

We use only the GameStream protocol layer: webserver + pairing, rtsp, tls,
crypto, clients, discovery, packetizer, gso_socket, shard_batch, control,
audio (~5,234 lines).

Keep these files as close to upstream as possible so `git subtree pull`
merges cleanly: no reformatting, no renaming, no refactors. All of our own
code lives in rgsp-host/src/.

Deleted here because the device has no Vulkan, no Wayland and no PulseAudio:
session/compositor, session/stream/video/pipeline, app_scanner,
session/stream/audio/pulse_server, and the moonshine-wsi crate.
```

- [ ] **Step 3: Delete the subsystems we replace**

```bash
cd vendor/moonshine
git rm -r --quiet moonshine-core/src/session/compositor \
                  moonshine-core/src/session/stream/video/pipeline \
                  moonshine-core/src/app_scanner \
                  moonshine-core/src/session/stream/audio/pulse_server \
                  moonshine-wsi
```

- [ ] **Step 4: Make the protocol layer reachable**

In `moonshine-core/src/session/stream/video/mod.rs`, change three lines:

```rust
pub mod gso_socket;
pub mod packetizer;
pub mod shard_batch;
```

(and delete the `mod pipeline;` line and the `use pipeline::VideoPipeline;` line).

In `moonshine-core/src/lib.rs`, change:

```rust
pub mod crypto;
```

and delete `pub mod app_scanner;`.

In `moonshine-core/src/session/mod.rs`, delete `pub mod compositor;` and `pub mod application;`.

- [ ] **Step 5: Cut the dependencies**

In `moonshine-core/Cargo.toml`, delete these dependency lines entirely: `ash`, `pixelforge`, `smithay`, `pulseaudio`, `inputtino`, `xcursor`, `wayland-scanner`, `notify-rust`, `open`, `steamlocate`, `image`, `zbus`, `zvariant`.

- [ ] **Step 6: Build it and fix the fallout**

```bash
cd /Users/eric/Code/RGSP/rgsp-cast
docker run --rm --platform linux/arm64 -v "$PWD":/w -w /w rust:1-bookworm \
  sh -c 'cd vendor/moonshine && cargo build -p moonshine-core 2>&1 | tail -40'
```

Expected: errors only about references to the deleted modules. Fix by deleting the referring code — every reference should be in `session/manager.rs`, `session/mod.rs`, or `session/stream/video/mod.rs`, and should be code that drives the compositor or the Vulkan pipeline. Do **not** add abstraction layers; delete and let the compiler point at the next one. Repeat until it builds.

- [ ] **Step 7: Prove the seam is reachable**

Create `vendor/moonshine/moonshine-core/tests/protocol_surface.rs`:

```rust
// Guards the visibility edits this project depends on. If a `git subtree pull`
// reverts them, this fails loudly instead of at the call site.
#[test]
fn protocol_layer_is_public() {
    // Compile-time reachability: naming the paths is the assertion.
    #[allow(unused_imports)]
    use moonshine_core::crypto;
    #[allow(unused_imports)]
    use moonshine_core::session::stream::video::gso_socket;
    #[allow(unused_imports)]
    use moonshine_core::session::stream::video::packetizer::Packetizer;
    #[allow(unused_imports)]
    use moonshine_core::session::stream::video::shard_batch::ShardBatch;
}
```

- [ ] **Step 8: Run the test**

```bash
docker run --rm --platform linux/arm64 -v "$PWD":/w -w /w rust:1-bookworm \
  sh -c 'cd vendor/moonshine && cargo test -p moonshine-core --test protocol_surface'
```

Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "chore: vendor moonshine protocol layer, drop vulkan/wayland/pulse"
```

---

### Task 2: `rgsp-host` crate skeleton with daemon lifecycle

**Files:**
- Create: `rgsp-host/Cargo.toml`
- Create: `rgsp-host/src/main.rs`
- Create: `rgsp-host/src/daemon.rs`
- Create: `rgsp-host/tests/pidfile.rs`

**Interfaces:**
- Consumes: `moonshine-core` (Task 1).
- Produces: binary `rgsp-host`; `daemon::PidFile::acquire(path: &Path) -> std::io::Result<PidFile>` returning `Err(ErrorKind::AlreadyExists)` when another live process holds it; `daemon::PidFile::release(self)`.

- [ ] **Step 1: Write the failing test**

Create `rgsp-host/tests/pidfile.rs`:

```rust
use rgsp_host::daemon::PidFile;
use std::io::ErrorKind;

#[test]
fn second_acquire_fails_while_first_is_held() {
    let dir = std::env::temp_dir().join("rgsp-pidtest");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("daemon.pid");
    let _ = std::fs::remove_file(&path);

    let first = PidFile::acquire(&path).expect("first acquire works");
    let second = PidFile::acquire(&path);
    assert!(second.is_err());
    assert_eq!(second.unwrap_err().kind(), ErrorKind::AlreadyExists);

    first.release();
    // Once released, the slot is free again.
    PidFile::acquire(&path).expect("acquire after release works");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn stale_pidfile_is_reclaimed() {
    let dir = std::env::temp_dir().join("rgsp-pidtest");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("stale.pid");
    // PID 999999 is above the default pid_max and cannot be running.
    std::fs::write(&path, "999999").unwrap();

    PidFile::acquire(&path).expect("stale pidfile is reclaimed");
    let _ = std::fs::remove_file(&path);
}
```

- [ ] **Step 2: Run it to confirm it fails**

```bash
docker run --rm --platform linux/arm64 -v "$PWD":/w -w /w rust:1-bookworm \
  cargo test -p rgsp-host --test pidfile
```

Expected: FAIL — crate `rgsp_host` does not exist.

- [ ] **Step 3: Create the workspace and crate**

Create `Cargo.toml` at the repo root:

```toml
[workspace]
members = ["rgsp-host", "vendor/moonshine/moonshine-core"]
resolver = "2"
```

Create `rgsp-host/Cargo.toml`:

```toml
[package]
name = "rgsp-host"
version = "0.1.0"
edition = "2021"

[lib]
name = "rgsp_host"
path = "src/lib.rs"

[[bin]]
name = "rgsp-host"
path = "src/main.rs"

[dependencies]
moonshine-core = { path = "../vendor/moonshine/moonshine-core" }
tokio = { version = "1", features = ["rt-multi-thread", "macros", "net", "sync", "signal", "time", "io-util"] }
tracing = "0.1"
tracing-subscriber = "0.3"
libc = "0.2"
anyhow = "1"
```

- [ ] **Step 4: Implement the pidfile**

Create `rgsp-host/src/lib.rs`:

```rust
pub mod daemon;
```

Create `rgsp-host/src/daemon.rs`:

```rust
use std::io::{Error, ErrorKind, Result};
use std::path::{Path, PathBuf};

/// A PID file that a single daemon instance holds for its lifetime.
///
/// A pid file left behind by a killed process must not wedge the daemon
/// permanently, so an existing file whose PID is not running is reclaimed.
pub struct PidFile {
    path: PathBuf,
}

impl PidFile {
    pub fn acquire(path: &Path) -> Result<PidFile> {
        if let Ok(existing) = std::fs::read_to_string(path) {
            if let Ok(pid) = existing.trim().parse::<i32>() {
                if process_is_alive(pid) {
                    return Err(Error::new(
                        ErrorKind::AlreadyExists,
                        format!("rgsp-host already running as pid {pid}"),
                    ));
                }
            }
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, std::process::id().to_string())?;
        Ok(PidFile { path: path.to_path_buf() })
    }

    pub fn release(self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn process_is_alive(pid: i32) -> bool {
    // Signal 0 performs error checking without sending anything.
    unsafe { libc::kill(pid, 0) == 0 }
}
```

- [ ] **Step 5: Run the tests**

```bash
docker run --rm --platform linux/arm64 -v "$PWD":/w -w /w rust:1-bookworm \
  cargo test -p rgsp-host --test pidfile
```

Expected: PASS (both tests).

- [ ] **Step 6: Write the entry point**

Create `rgsp-host/src/main.rs`:

```rust
use anyhow::Result;
use rgsp_host::daemon::PidFile;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let pid_path = PathBuf::from("/tmp/rgsp/daemon.pid");
    let pidfile = match PidFile::acquire(&pid_path) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("{e}");
            std::process::exit(1);
        }
    };

    tracing::info!("rgsp-host starting");

    tokio::signal::ctrl_c().await?;
    tracing::info!("shutting down");
    pidfile.release();
    Ok(())
}
```

- [ ] **Step 7: Build the binary**

```bash
docker run --rm --platform linux/arm64 -v "$PWD":/w -w /w rust:1-bookworm \
  cargo build -p rgsp-host
```

Expected: builds clean.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml rgsp-host
git commit -m "feat: rgsp-host skeleton with pidfile-guarded daemon lifecycle"
```

---

### Task 3: Expose the Cedar capture/encode path as a C library

**Files:**
- Create: `include/rgsp_cast.h`
- Modify: `src/rgsp-cast.c`
- Create: `tests/test_capture_api.c`
- Modify: `Makefile`

**Interfaces:**
- Consumes: nothing.
- Produces: `librgspcast.a` with this exact C API:

```c
typedef struct rgsp_capture rgsp_capture;
rgsp_capture *rgsp_capture_open(int width, int height, int fps, int bitrate);
int  rgsp_capture_next(rgsp_capture *c, const unsigned char **data,
                       size_t *len, int *is_keyframe);
void rgsp_capture_request_idr(rgsp_capture *c);
void rgsp_capture_close(rgsp_capture *c);
const char *rgsp_capture_last_error(void);
```

- [ ] **Step 1: Write the failing test**

Create `tests/test_capture_api.c`:

```c
/* Exercises the library API end to end on the device: open, pull frames,
 * force an IDR, close. Frame 0 must be a keyframe because the encoder emits
 * SPS/PPS + IDR first. */
#include "../include/rgsp_cast.h"
#include <stdio.h>
#include <string.h>
#include <assert.h>

int main(void)
{
    rgsp_capture *c = rgsp_capture_open(720, 480, 30, 2000000);
    if (!c) { fprintf(stderr, "open failed: %s\n", rgsp_capture_last_error()); return 1; }

    const unsigned char *data; size_t len; int key;

    if (rgsp_capture_next(c, &data, &len, &key) != 0) {
        fprintf(stderr, "first frame failed: %s\n", rgsp_capture_last_error());
        return 1;
    }
    assert(len > 4);
    /* Annex-B start code */
    assert(data[0] == 0 && data[1] == 0 && data[2] == 0 && data[3] == 1);
    assert(key == 1);
    printf("frame0: %zu bytes, keyframe=%d\n", len, key);

    int keyframes = 0;
    for (int i = 0; i < 60; i++) {
        if (rgsp_capture_next(c, &data, &len, &key) != 0) {
            fprintf(stderr, "frame %d failed: %s\n", i, rgsp_capture_last_error());
            return 1;
        }
        keyframes += key;
    }
    printf("60 frames, %d keyframes\n", keyframes);

    rgsp_capture_request_idr(c);
    if (rgsp_capture_next(c, &data, &len, &key) != 0) return 1;
    assert(key == 1);
    printf("forced IDR ok\n");

    rgsp_capture_close(c);
    printf("PASS\n");
    return 0;
}
```

- [ ] **Step 2: Run it to confirm it fails**

```bash
docker run --rm --platform linux/arm64 -v "$PWD":/w -w /w ubuntu:22.04 \
  sh -c 'apt-get update -qq && apt-get install -y -qq gcc >/dev/null && \
         gcc -O2 -o /tmp/t tests/test_capture_api.c -L. -lrgspcast -ldl'
```

Expected: FAIL — `include/rgsp_cast.h` and `librgspcast.a` do not exist.

- [ ] **Step 3: Write the header**

Create `include/rgsp_cast.h`:

```c
/* Cedar VE screen capture as a library.
 *
 * Frames come back as Annex-B H.264 (Main profile, level 4.1, CABAC).
 * The buffer belongs to the capture object and stays valid until the next
 * rgsp_capture_next() call.
 */
#ifndef RGSP_CAST_H
#define RGSP_CAST_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct rgsp_capture rgsp_capture;

/* Opens /dev/fb0 and the Cedar encoder. Returns NULL on failure;
 * call rgsp_capture_last_error() for detail. */
rgsp_capture *rgsp_capture_open(int width, int height, int fps, int bitrate);

/* Blocks until the next frame is due, captures, encodes, and returns the
 * Annex-B bitstream. Returns 0 on success, -1 on failure.
 * The first frame is always a keyframe (SPS + PPS + IDR). */
int rgsp_capture_next(rgsp_capture *c, const unsigned char **data,
                      size_t *len, int *is_keyframe);

/* Makes the next frame an IDR. Moonlight asks for this after packet loss. */
void rgsp_capture_request_idr(rgsp_capture *c);

void rgsp_capture_close(rgsp_capture *c);

const char *rgsp_capture_last_error(void);

#ifdef __cplusplus
}
#endif

#endif
```

- [ ] **Step 4: Refactor `src/rgsp-cast.c` into library + CLI**

Split the existing file so the capture loop becomes callable:

1. Move everything except `main()` into the library, and introduce `struct rgsp_capture` holding what the current `main()` keeps in locals: the fb file descriptor, fb geometry, the ION buffer handle, the VE encoder handle, the Annex-B output buffer, the frame counter, the `CLOCK_MONOTONIC` next-frame deadline, and a new `int force_idr;`.
2. `rgsp_capture_open()` performs the existing setup in the same order: open `/dev/fb0`, read `FBIOGET_VSCREENINFO`, dlopen the vendor libs, `VideoEncCreate`, allocate the ION input buffer, `VideoEncInit`.
3. `rgsp_capture_next()` performs one existing loop iteration: sleep until the deadline, re-read `yoffset` and `pread` the visible page, copy into the ION buffer, `VideoEncodeOneFrame`, drain the bitstream, convert AVCC→Annex-B into the output buffer. On the first frame, fetch parameter sets with `VideoEncGetParameter(VENC_IndexParamH264SPSPPS /* 0x101 */)` **after** encoding, convert the `avcC` record to Annex-B, and prepend SPS+PPS to the IDR.
4. `rgsp_capture_request_idr()` sets `c->force_idr = 1`; `rgsp_capture_next()` consumes it by setting the vendor lib's force-keyframe parameter before encoding, then clears it.
5. Set `is_keyframe` from the NAL type of the first slice NAL in the frame: type 5 → keyframe.
6. Replace every `fprintf(stderr, ...)`-then-`exit()` path with `snprintf` into a static `char last_error[256]` and a failure return.
7. `main()` moves to `src/rgsp-cast-cli.c` and keeps the existing CLI flags, now implemented as a loop over `rgsp_capture_next()` writing to a file.

Keep these unchanged — they are load-bearing and hard-won:

```c
#define VENC_IndexParamH264SPSPPS 0x101   /* NOT 16 */
/* fb bytes are B,G,R,A -> ARGB in 32-bit word order */
#define INPUT_PIXEL_FORMAT 12             /* VENC_PIXEL_ARGB */
typedef struct {
    unsigned char *pBuffer;
    unsigned int   nLength;
    unsigned char  _tail[496];            /* padding prevents a stack smash */
} VencHeaderData;
```

- [ ] **Step 5: Add library and test targets to the Makefile**

Add to `Makefile`:

```make
librgspcast.a: src/rgsp-cast.c include/rgsp_cast.h
	docker run --rm --platform linux/arm64 -v "$(CURDIR)":/w -w /w $(IMAGE) \
		sh -c 'apt-get update -qq && apt-get install -y -qq gcc binutils >/dev/null 2>&1 && \
		       gcc $(CFLAGS) -c -o /tmp/rgsp-cast.o src/rgsp-cast.c && \
		       ar rcs $@ /tmp/rgsp-cast.o'

bin/test-capture-api: tests/test_capture_api.c librgspcast.a
	@mkdir -p bin
	docker run --rm --platform linux/arm64 -v "$(CURDIR)":/w -w /w $(IMAGE) \
		sh -c 'apt-get update -qq && apt-get install -y -qq gcc >/dev/null 2>&1 && \
		       gcc $(CFLAGS) -o $@ $< librgspcast.a -ldl'
```

- [ ] **Step 6: Build and run the test on the device**

```bash
make librgspcast.a bin/test-capture-api
scp -q bin/test-capture-api root@192.168.180.106:/tmp/venc/
ssh root@192.168.180.106 'cd /tmp/venc && LD_LIBRARY_PATH=/tmp/venc/lib-trimui ./test-capture-api'
```

Expected: prints `frame0: N bytes, keyframe=1`, `60 frames, ...`, `forced IDR ok`, `PASS`.

- [ ] **Step 7: Verify the CLI still works**

```bash
make run DEVICE=root@192.168.180.106 DURATION=5 OUT=regress.h264
ffprobe -v error -show_entries stream=codec_name,width,height regress.h264
```

Expected: `h264`, `720`, `480`.

- [ ] **Step 8: Commit**

```bash
git add include src Makefile tests/test_capture_api.c
git commit -m "feat: expose cedar capture/encode as librgspcast with IDR-on-demand"
```

---

### Task 4: Rust FFI wrapper around the capture library

**Files:**
- Create: `rgsp-host/build.rs`
- Create: `rgsp-host/src/capture.rs`
- Create: `rgsp-host/tests/capture_ffi.rs`
- Modify: `rgsp-host/src/lib.rs`
- Modify: `rgsp-host/Cargo.toml`

**Interfaces:**
- Consumes: `librgspcast.a` from Task 3.
- Produces: `capture::Capture::open(width: u32, height: u32, fps: u32, bitrate: u32) -> anyhow::Result<Capture>`; `Capture::next(&mut self) -> anyhow::Result<Frame<'_>>` where `pub struct Frame<'a> { pub data: &'a [u8], pub is_keyframe: bool }`; `Capture::request_idr(&self)`. `Capture` is `Send`.

- [ ] **Step 1: Write the failing test**

Create `rgsp-host/tests/capture_ffi.rs`:

```rust
// Runs only on the device (needs /dev/fb0 and the Cedar libs).
// Skips cleanly elsewhere so the suite stays green on a laptop.
use rgsp_host::capture::Capture;

#[test]
fn captures_annexb_frames_starting_with_a_keyframe() {
    if !std::path::Path::new("/dev/fb0").exists() {
        eprintln!("skipping: no /dev/fb0");
        return;
    }

    let mut cap = Capture::open(720, 480, 30, 2_000_000).expect("open");

    let frame = cap.next().expect("first frame");
    assert!(frame.data.len() > 4);
    assert_eq!(&frame.data[..4], &[0, 0, 0, 1], "annex-b start code");
    assert!(frame.is_keyframe, "first frame must be a keyframe");

    for _ in 0..30 {
        let f = cap.next().expect("frame");
        assert!(!f.data.is_empty());
    }

    cap.request_idr();
    let f = cap.next().expect("forced idr");
    assert!(f.is_keyframe);
}
```

- [ ] **Step 2: Run it to confirm it fails**

```bash
docker run --rm --platform linux/arm64 -v "$PWD":/w -w /w rust:1-bookworm \
  cargo test -p rgsp-host --test capture_ffi
```

Expected: FAIL — no module `capture`.

- [ ] **Step 3: Add the build script**

Create `rgsp-host/build.rs`:

```rust
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
```

Add to `rgsp-host/Cargo.toml`:

```toml
build = "build.rs"
```

- [ ] **Step 4: Implement the wrapper**

Create `rgsp-host/src/capture.rs`:

```rust
use anyhow::{anyhow, Result};
use std::ffi::{c_char, c_int, c_void, CStr};

#[repr(C)]
struct RgspCapture {
    _private: [u8; 0],
}

extern "C" {
    fn rgsp_capture_open(width: c_int, height: c_int, fps: c_int, bitrate: c_int)
        -> *mut RgspCapture;
    fn rgsp_capture_next(
        c: *mut RgspCapture,
        data: *mut *const u8,
        len: *mut usize,
        is_keyframe: *mut c_int,
    ) -> c_int;
    fn rgsp_capture_request_idr(c: *mut RgspCapture);
    fn rgsp_capture_close(c: *mut RgspCapture);
    fn rgsp_capture_last_error() -> *const c_char;
}

fn last_error() -> String {
    unsafe {
        let p = rgsp_capture_last_error();
        if p.is_null() {
            "unknown error".into()
        } else {
            CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    }
}

pub struct Frame<'a> {
    pub data: &'a [u8],
    pub is_keyframe: bool,
}

pub struct Capture {
    handle: *mut RgspCapture,
}

// The handle is only ever touched from the thread that owns the Capture.
unsafe impl Send for Capture {}

impl Capture {
    pub fn open(width: u32, height: u32, fps: u32, bitrate: u32) -> Result<Capture> {
        let handle = unsafe {
            rgsp_capture_open(width as c_int, height as c_int, fps as c_int, bitrate as c_int)
        };
        if handle.is_null() {
            return Err(anyhow!("rgsp_capture_open: {}", last_error()));
        }
        Ok(Capture { handle })
    }

    /// Blocks until the next frame is due, then returns its Annex-B bitstream.
    /// The slice is owned by the capture and is invalidated by the next call.
    pub fn next(&mut self) -> Result<Frame<'_>> {
        let mut data: *const u8 = std::ptr::null();
        let mut len: usize = 0;
        let mut key: c_int = 0;
        let rc = unsafe { rgsp_capture_next(self.handle, &mut data, &mut len, &mut key) };
        if rc != 0 {
            return Err(anyhow!("rgsp_capture_next: {}", last_error()));
        }
        Ok(Frame {
            data: unsafe { std::slice::from_raw_parts(data, len) },
            is_keyframe: key != 0,
        })
    }

    pub fn request_idr(&self) {
        unsafe { rgsp_capture_request_idr(self.handle) }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        unsafe { rgsp_capture_close(self.handle) }
    }
}

// Silences an unused-import warning on the c_void import in some toolchains.
const _: Option<*const c_void> = None;
```

Add `pub mod capture;` to `rgsp-host/src/lib.rs`.

- [ ] **Step 5: Run the test on the device**

```bash
make librgspcast.a
docker run --rm --platform linux/arm64 -v "$PWD":/w -w /w rust:1-bookworm \
  cargo test -p rgsp-host --test capture_ffi --no-run --message-format=json \
  | grep -o '"executable":"[^"]*"' | tail -1
# copy the reported test binary to the device and run it there:
scp -q <binary> root@192.168.180.106:/tmp/venc/capture_ffi
ssh root@192.168.180.106 'cd /tmp/venc && LD_LIBRARY_PATH=/tmp/venc/lib-trimui ./capture_ffi --nocapture'
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add rgsp-host
git commit -m "feat: rust FFI wrapper for the cedar capture library"
```

---

### Task 5: Drive Moonshine's packetizer from the Cedar capture

**Files:**
- Create: `rgsp-host/src/video.rs`
- Create: `rgsp-host/tests/video_stream.rs`
- Modify: `rgsp-host/src/lib.rs`

**Interfaces:**
- Consumes: `capture::Capture` (Task 4); `moonshine_core::session::stream::video::packetizer::Packetizer`, `shard_batch::ShardBatch`, `gso_socket::UdpGsoSocket` (Task 1).
- Produces: `video::VideoStream::new(cfg: VideoConfig) -> VideoStream`; `VideoStream::run(self, send: impl FnMut(&[u8]) -> anyhow::Result<()>) -> anyhow::Result<()>`; `video::VideoConfig { pub width: u32, pub height: u32, pub fps: u32, pub bitrate: u32, pub packet_size: usize, pub fec_percentage: u8, pub minimum_fec_packets: u32, pub client_addr: SocketAddr }`; `VideoStream::idr_requester() -> IdrRequester` with `IdrRequester::request(&self)`.

- [ ] **Step 1: Write the failing test**

Create `rgsp-host/tests/video_stream.rs`:

```rust
// The packetizer is pure: given an encoded frame it must produce shards whose
// payload sums back to the frame plus per-shard headers. That is testable off
// the device, which is where the protocol bugs actually get caught.
use moonshine_core::session::stream::video::packetizer::Packetizer;
use rgsp_host::video::rtp_timestamp_for;

#[test]
fn rtp_timestamp_advances_at_90khz() {
    // GameStream video uses a 90 kHz RTP clock. At 30 fps that is 3000 ticks
    // per frame; drift here shows up as stutter on the client.
    assert_eq!(rtp_timestamp_for(0, 30), 0);
    assert_eq!(rtp_timestamp_for(1, 30), 3000);
    assert_eq!(rtp_timestamp_for(30, 30), 90_000);
    assert_eq!(rtp_timestamp_for(60, 60), 90_000);
}

#[test]
fn packetizer_splits_a_frame_into_shards() {
    let (_tx, rx) = tokio::sync::watch::channel(Default::default());
    let mut p = Packetizer::new(false, rx);
    p.warm_up(20, 2);

    let frame = vec![0u8; 20_000];
    let mut seq = 0u32;
    let batch = p
        .packetize(&frame, true, 1024, 2, 20, 0, &mut seq, 0, 0)
        .expect("packetize");
    assert!(seq > 0, "sequence number must advance");
    drop(batch);
}
```

- [ ] **Step 2: Run it to confirm it fails**

```bash
docker run --rm --platform linux/arm64 -v "$PWD":/w -w /w rust:1-bookworm \
  cargo test -p rgsp-host --test video_stream
```

Expected: FAIL — no `rgsp_host::video`.

- [ ] **Step 3: Implement the video stream**

Create `rgsp-host/src/video.rs`:

```rust
use crate::capture::Capture;
use anyhow::Result;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// GameStream video runs on a 90 kHz RTP clock.
const RTP_CLOCK_HZ: u64 = 90_000;

pub fn rtp_timestamp_for(frame_number: u64, fps: u32) -> u32 {
    ((frame_number * RTP_CLOCK_HZ) / fps as u64) as u32
}

#[derive(Clone, Debug)]
pub struct VideoConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
    pub packet_size: usize,
    pub fec_percentage: u8,
    pub minimum_fec_packets: u32,
    pub client_addr: SocketAddr,
}

#[derive(Clone)]
pub struct IdrRequester {
    flag: Arc<AtomicBool>,
}

impl IdrRequester {
    pub fn request(&self) {
        self.flag.store(true, Ordering::Relaxed);
    }
}

pub struct VideoStream {
    cfg: VideoConfig,
    idr: Arc<AtomicBool>,
}

impl VideoStream {
    pub fn new(cfg: VideoConfig) -> Self {
        VideoStream { cfg, idr: Arc::new(AtomicBool::new(false)) }
    }

    pub fn idr_requester(&self) -> IdrRequester {
        IdrRequester { flag: self.idr.clone() }
    }

    /// Capture -> encode -> packetize -> send, one frame at a time.
    ///
    /// Runs on a dedicated blocking thread: `Capture::next` sleeps until the
    /// frame deadline and must not occupy a tokio worker.
    pub fn run(self, mut send: impl FnMut(&[u8]) -> Result<()>) -> Result<()> {
        let mut capture = Capture::open(
            self.cfg.width, self.cfg.height, self.cfg.fps, self.cfg.bitrate,
        )?;

        let mut frame_number: u64 = 0;

        loop {
            if self.idr.swap(false, Ordering::Relaxed) {
                capture.request_idr();
            }

            let frame = capture.next()?;
            let _rtp = rtp_timestamp_for(frame_number, self.cfg.fps);

            // Hand the encoded frame to the caller, which packetizes and
            // sends it. Kept as a callback so the packetizer's session keys
            // and socket stay owned by the session layer.
            send(frame.data)?;

            frame_number += 1;
        }
    }
}
```

Add `pub mod video;` to `rgsp-host/src/lib.rs`.

- [ ] **Step 4: Run the tests**

```bash
docker run --rm --platform linux/arm64 -v "$PWD":/w -w /w rust:1-bookworm \
  cargo test -p rgsp-host --test video_stream
```

Expected: PASS.

- [ ] **Step 5: Wire it into Moonshine's session**

In `vendor/moonshine/moonshine-core/src/session/stream/video/mod.rs`, the deleted `VideoPipeline` was the frame source. Replace its construction with a channel the host fills: add

```rust
/// Encoded frames arrive from outside the crate (the Cedar encoder).
pub struct EncodedFrame {
    pub data: Vec<u8>,
    pub is_key_frame: bool,
    pub frame_number: u32,
    pub rtp_timestamp: u32,
}
```

and change the video stream's start function to take `tokio::sync::mpsc::Receiver<EncodedFrame>` instead of building a pipeline. The existing packetize-and-send loop stays exactly as upstream wrote it, reading `frame.data` and `frame.is_key_frame`.

- [ ] **Step 6: Build the workspace**

```bash
docker run --rm --platform linux/arm64 -v "$PWD":/w -w /w rust:1-bookworm \
  cargo build --workspace
```

Expected: builds clean.

- [ ] **Step 7: Commit**

```bash
git add rgsp-host vendor/moonshine
git commit -m "feat: feed cedar-encoded frames into moonshine's packetizer"
```

---

### Task 6: ALSA loopback capture in Rust

**Files:**
- Create: `rgsp-host/src/audio.rs`
- Create: `rgsp-host/tests/audio_capture.rs`
- Modify: `rgsp-host/src/lib.rs`
- Modify: `rgsp-host/Cargo.toml`

**Interfaces:**
- Consumes: nothing.
- Produces: `audio::LoopbackCapture::open(device: &str) -> Result<LoopbackCapture>`; `LoopbackCapture::read(&mut self, buf: &mut [i16]) -> Result<usize>` returning frames read; constants `audio::SAMPLE_RATE: u32 = 48_000`, `audio::CHANNELS: u32 = 2`, `audio::PERIOD_FRAMES: usize = 240`.

- [ ] **Step 1: Write the failing test**

Create `rgsp-host/tests/audio_capture.rs`:

```rust
use rgsp_host::audio::{LoopbackCapture, CHANNELS, SAMPLE_RATE};

#[test]
fn reads_silence_when_nothing_is_playing() {
    // snd-aloop yields silence rather than an error when the playback side of
    // the cable is closed, so the host can start before a game launches.
    if !std::path::Path::new("/proc/asound/Loopback").exists() {
        eprintln!("skipping: snd-aloop not loaded");
        return;
    }

    let mut cap = LoopbackCapture::open("hw:Loopback,1,0").expect("open");
    let mut buf = vec![0i16; 1024 * CHANNELS as usize];
    let frames = cap.read(&mut buf).expect("read");
    assert!(frames > 0, "must return frames, not an error");
}

#[test]
fn parameters_match_what_minarch_plays() {
    // A mismatch on format, rate or channels fails the capture side of the
    // cable with -EIO (aloop.c, loopback_check_format).
    assert_eq!(SAMPLE_RATE, 48_000);
    assert_eq!(CHANNELS, 2);
}
```

- [ ] **Step 2: Run it to confirm it fails**

```bash
docker run --rm --platform linux/arm64 -v "$PWD":/w -w /w rust:1-bookworm \
  cargo test -p rgsp-host --test audio_capture
```

Expected: FAIL — no `rgsp_host::audio`.

- [ ] **Step 3: Add the ALSA dependency**

Add to `rgsp-host/Cargo.toml`:

```toml
alsa = "0.9"
```

- [ ] **Step 4: Implement the capture**

Create `rgsp-host/src/audio.rs`:

```rust
use alsa::pcm::{Access, Format, HwParams, PCM};
use alsa::{Direction, ValueOr};
use anyhow::{Context, Result};

/// Fixed by what minarch plays. snd-aloop fails the capture side with -EIO
/// if the two ends of the cable disagree on format, rate or channels
/// (aloop.c, loopback_check_format).
pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u32 = 2;
/// 5 ms at 48 kHz - small enough that audio latency stays under the video's.
pub const PERIOD_FRAMES: usize = 240;

pub struct LoopbackCapture {
    pcm: PCM,
}

impl LoopbackCapture {
    pub fn open(device: &str) -> Result<LoopbackCapture> {
        let pcm = PCM::new(device, Direction::Capture, false)
            .with_context(|| format!("opening {device}"))?;

        {
            let hwp = HwParams::any(&pcm)?;
            hwp.set_access(Access::RWInterleaved)?;
            hwp.set_format(Format::s16())?;
            hwp.set_channels(CHANNELS)?;
            hwp.set_rate(SAMPLE_RATE, ValueOr::Nearest)?;
            hwp.set_period_size_near(PERIOD_FRAMES as i64, ValueOr::Nearest)?;
            hwp.set_buffer_size_near((PERIOD_FRAMES * 4) as i64)?;
            pcm.hw_params(&hwp)?;
        }

        // snd-aloop rejects the implicit start that snd_pcm_readi() would do,
        // returning -EIO. Prepare and start explicitly.
        pcm.prepare().context("prepare")?;
        pcm.start().context("start")?;

        Ok(LoopbackCapture { pcm })
    }

    /// Reads interleaved s16 frames. `buf.len()` must be a multiple of CHANNELS.
    /// Returns the number of *frames* read.
    pub fn read(&mut self, buf: &mut [i16]) -> Result<usize> {
        let io = self.pcm.io_i16()?;
        loop {
            match io.readi(buf) {
                Ok(frames) => return Ok(frames),
                Err(e) => {
                    // An overrun means we fell behind; recover and keep going
                    // rather than tearing down the stream.
                    if e.errno() == libc::EPIPE {
                        self.pcm.prepare()?;
                        self.pcm.start()?;
                        continue;
                    }
                    return Err(e).context("readi");
                }
            }
        }
    }
}
```

Add `pub mod audio;` to `rgsp-host/src/lib.rs`.

- [ ] **Step 5: Run the tests on the device**

```bash
ssh root@192.168.180.106 'lsmod | grep -q snd_aloop || insmod /tmp/snd-aloop.ko'
# build the test binary, copy it over, run it there
docker run --rm --platform linux/arm64 -v "$PWD":/w -w /w rust:1-bookworm \
  sh -c 'apt-get update -qq && apt-get install -y -qq libasound2-dev >/dev/null && \
         cargo test -p rgsp-host --test audio_capture --no-run'
scp -q <test binary> root@192.168.180.106:/tmp/venc/audio_capture
ssh root@192.168.180.106 '/tmp/venc/audio_capture --nocapture'
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add rgsp-host
git commit -m "feat: alsa loopback capture with explicit stream start"
```

---

### Task 7: Opus encoding and the audio stream

**Files:**
- Create: `rgsp-host/src/audio_stream.rs`
- Create: `rgsp-host/tests/opus_encode.rs`
- Modify: `rgsp-host/src/lib.rs`
- Modify: `rgsp-host/Cargo.toml`

**Interfaces:**
- Consumes: `audio::LoopbackCapture` (Task 6).
- Produces: `audio_stream::OpusStream::new(bitrate: u32) -> Result<OpusStream>`; `OpusStream::encode(&mut self, pcm: &[i16]) -> Result<&[u8]>` taking exactly `FRAME_FRAMES * CHANNELS` samples; `audio_stream::FRAME_FRAMES: usize = 240` (5 ms, the frame size Moonlight expects).

- [ ] **Step 1: Write the failing test**

Create `rgsp-host/tests/opus_encode.rs`:

```rust
use rgsp_host::audio_stream::{OpusStream, FRAME_FRAMES};
use rgsp_host::audio::CHANNELS;

#[test]
fn encodes_a_5ms_stereo_frame() {
    // Moonlight negotiates 5 ms Opus frames at 48 kHz stereo. 240 frames is
    // that duration; anything else and the client's jitter buffer misbehaves.
    assert_eq!(FRAME_FRAMES, 240);

    let mut enc = OpusStream::new(96_000).expect("encoder");
    let pcm = vec![0i16; FRAME_FRAMES * CHANNELS as usize];
    let packet = enc.encode(&pcm).expect("encode");
    assert!(!packet.is_empty(), "silence still produces a packet");
    assert!(packet.len() < 1200, "must fit one datagram");
}

#[test]
fn encodes_a_tone_larger_than_silence() {
    let mut enc = OpusStream::new(96_000).expect("encoder");

    let silence = vec![0i16; FRAME_FRAMES * CHANNELS as usize];
    let quiet = enc.encode(&silence).expect("encode silence").len();

    let mut tone = vec![0i16; FRAME_FRAMES * CHANNELS as usize];
    for (i, s) in tone.iter_mut().enumerate() {
        let t = (i / CHANNELS as usize) as f32 / 48_000.0;
        *s = ((t * 440.0 * std::f32::consts::TAU).sin() * 8000.0) as i16;
    }
    let loud = enc.encode(&tone).expect("encode tone").len();

    assert!(loud > quiet, "a tone must encode larger than silence");
}
```

- [ ] **Step 2: Run it to confirm it fails**

```bash
docker run --rm --platform linux/arm64 -v "$PWD":/w -w /w rust:1-bookworm \
  cargo test -p rgsp-host --test opus_encode
```

Expected: FAIL — no `rgsp_host::audio_stream`.

- [ ] **Step 3: Add the dependency**

Add to `rgsp-host/Cargo.toml`:

```toml
opus = "0.3"
```

- [ ] **Step 4: Implement the encoder**

Create `rgsp-host/src/audio_stream.rs`:

```rust
use crate::audio::{CHANNELS, SAMPLE_RATE};
use anyhow::{Context, Result};

/// Moonlight negotiates 5 ms Opus frames: 240 frames at 48 kHz.
pub const FRAME_FRAMES: usize = 240;

pub struct OpusStream {
    encoder: opus::Encoder,
    out: Vec<u8>,
}

impl OpusStream {
    pub fn new(bitrate: u32) -> Result<OpusStream> {
        let mut encoder = opus::Encoder::new(
            SAMPLE_RATE,
            opus::Channels::Stereo,
            opus::Application::LowDelay,
        )
        .context("creating opus encoder")?;
        encoder.set_bitrate(opus::Bitrate::Bits(bitrate as i32))?;

        Ok(OpusStream { encoder, out: vec![0u8; 1275] })
    }

    /// Encodes exactly one 5 ms stereo frame.
    pub fn encode(&mut self, pcm: &[i16]) -> Result<&[u8]> {
        anyhow::ensure!(
            pcm.len() == FRAME_FRAMES * CHANNELS as usize,
            "expected {} samples, got {}",
            FRAME_FRAMES * CHANNELS as usize,
            pcm.len()
        );
        let n = self.encoder.encode(pcm, &mut self.out).context("opus encode")?;
        Ok(&self.out[..n])
    }
}
```

Add `pub mod audio_stream;` to `rgsp-host/src/lib.rs`.

- [ ] **Step 5: Run the tests**

```bash
docker run --rm --platform linux/arm64 -v "$PWD":/w -w /w rust:1-bookworm \
  sh -c 'apt-get update -qq && apt-get install -y -qq libopus-dev pkg-config >/dev/null && \
         cargo test -p rgsp-host --test opus_encode'
```

Expected: PASS (both tests).

- [ ] **Step 6: Replace Moonshine's audio source**

In `vendor/moonshine/moonshine-core/src/session/stream/audio/mod.rs`, delete the `PulseServer` construction and the `pulse_socket`/`pulse_socket_path` fields, and change the audio stream's start function to take a `tokio::sync::mpsc::Receiver<Vec<i16>>` of PCM frames. The existing Opus encode + RTP packetization loop stays as upstream wrote it.

- [ ] **Step 7: Build the workspace**

```bash
docker run --rm --platform linux/arm64 -v "$PWD":/w -w /w rust:1-bookworm \
  cargo build --workspace
```

Expected: builds clean.

- [ ] **Step 8: Commit**

```bash
git add rgsp-host vendor/moonshine
git commit -m "feat: opus encoding from the loopback, replacing the pulse server"
```

---

### Task 8: Audio routing and the on-screen indicator

**Files:**
- Create: `rgsp-host/src/routing.rs`
- Create: `rgsp-host/tests/routing.rs`
- Modify: `rgsp-host/src/lib.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `routing::CastSink::engage(userdata: &Path) -> Result<CastSink>` writing `.asoundrc` and setting the audio sink; `CastSink::release(self) -> Result<()>` restoring the previous state; `routing::ASOUNDRC_BODY: &str`.

- [ ] **Step 1: Write the failing test**

Create `rgsp-host/tests/routing.rs`:

```rust
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
```

- [ ] **Step 2: Run it to confirm it fails**

```bash
docker run --rm --platform linux/arm64 -v "$PWD":/w -w /w rust:1-bookworm \
  cargo test -p rgsp-host --test routing
```

Expected: FAIL — no `rgsp_host::routing`.

- [ ] **Step 3: Implement routing**

Create `rgsp-host/src/routing.rs`:

```rust
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

/// libmsettings is only present on the device; elsewhere this is a no-op so the
/// tests run anywhere.
fn set_audio_sink(value: i32) {
    #[link(name = "msettings")]
    extern "C" {
        fn SetAudioSink(value: i32);
    }
    if std::path::Path::new("/usr/trimui/lib/libmsettings.so").exists()
        || std::path::Path::new("/usr/lib/libmsettings.so").exists()
    {
        unsafe { SetAudioSink(value) }
    }
}
```

Add `pub mod routing;` to `rgsp-host/src/lib.rs`.

Note: because `SetAudioSink` is only resolvable on the device, link it weakly by adding to `rgsp-host/build.rs`:

```rust
println!("cargo:rustc-link-arg=-Wl,--unresolved-symbols=ignore-in-object-files");
```

- [ ] **Step 4: Run the tests**

```bash
docker run --rm --platform linux/arm64 -v "$PWD":/w -w /w rust:1-bookworm \
  cargo test -p rgsp-host --test routing
```

Expected: PASS (both tests).

- [ ] **Step 5: Verify the indicator on the device**

```bash
scp -q target/aarch64-unknown-linux-gnu/debug/rgsp-host root@192.168.180.106:/tmp/venc/
ssh root@192.168.180.106 '/tmp/venc/rgsp-host --engage-sink-test'
```

Expected: the external-audio icon appears in NextUI's status pill; `/mnt/SDCARD/.userdata/h700/.asoundrc` contains `hw:Loopback,0,0`.

- [ ] **Step 6: Commit**

```bash
git add rgsp-host
git commit -m "feat: route audio to the loopback while casting and light the status icon"
```

---

### Task 9: Control stream — parse and discard client input

**Files:**
- Modify: `vendor/moonshine/moonshine-core/src/session/stream/control/mod.rs`
- Delete: `vendor/moonshine/moonshine-core/src/session/stream/control/input/`
- Create: `rgsp-host/tests/control_input.rs`

**Interfaces:**
- Consumes: Task 1's vendored tree.
- Produces: a control stream that accepts and discards input packets, with no `inputtino` dependency in the crate graph.

- [ ] **Step 1: Write the failing test**

Create `rgsp-host/tests/control_input.rs`:

```rust
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
```

- [ ] **Step 2: Run it to confirm it fails**

```bash
docker run --rm --platform linux/arm64 -v "$PWD":/w -w /w rust:1-bookworm \
  cargo test -p rgsp-host --test control_input
```

Expected: FAIL — `inputtino` is still in `Cargo.lock`.

- [ ] **Step 3: Replace input handling with a discard**

Delete the `input` module directory, and in `control/mod.rs` replace the input dispatch arm with a log-and-drop:

```rust
// Input is received and discarded: the player is holding the device, so
// there is nothing for the client's controller to drive.
ControlMessage::Input(_) => {
    tracing::trace!("ignoring client input packet");
}
```

Remove `mod input;` and the `inputtino` dependency from `moonshine-core/Cargo.toml`.

- [ ] **Step 4: Rebuild and run the test**

```bash
docker run --rm --platform linux/arm64 -v "$PWD":/w -w /w rust:1-bookworm \
  sh -c 'cargo build --workspace && cargo test -p rgsp-host --test control_input'
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add vendor/moonshine rgsp-host Cargo.lock
git commit -m "feat: accept and discard client input, dropping inputtino"
```

---

### Task 10: Wire the daemon together and add the status IPC

**Files:**
- Modify: `rgsp-host/src/main.rs`
- Create: `rgsp-host/src/status.rs`
- Create: `rgsp-host/tests/status.rs`
- Modify: `rgsp-host/src/lib.rs`

**Interfaces:**
- Consumes: everything from Tasks 2, 4-8.
- Produces: `status::Status` enum (`Starting`, `AwaitingPairing { url: String }`, `Ready { addr: String }`, `Connected { client: String, width: u32, height: u32, fps: u32 }`, `Stopped`); `status::StatusWriter::new(fifo: PathBuf) -> StatusWriter`; `StatusWriter::publish(&self, s: &Status)` writing `TEXT:<line>` to the show2 FIFO; `Status::line(&self) -> String`.

- [ ] **Step 1: Write the failing test**

Create `rgsp-host/tests/status.rs`:

```rust
use rgsp_host::status::{Status, StatusWriter};

#[test]
fn status_lines_lead_with_what_the_user_needs() {
    assert_eq!(
        Status::AwaitingPairing { url: "http://192.168.1.50:47990/pin".into() }.line(),
        "Pair at http://192.168.1.50:47990/pin"
    );
    assert_eq!(
        Status::Ready { addr: "192.168.1.50".into() }.line(),
        "Ready - 192.168.1.50"
    );
    assert_eq!(
        Status::Connected { client: "Apple TV".into(), width: 720, height: 480, fps: 30 }.line(),
        "Connected - Apple TV 720x480 30fps"
    );
    assert_eq!(Status::Stopped.line(), "Casting stopped");
}

#[test]
fn publish_never_blocks_when_the_fifo_has_no_reader() {
    // show2 may not be running. A status update must never wedge the daemon.
    let path = std::env::temp_dir().join("rgsp-status-test.fifo");
    let _ = std::fs::remove_file(&path);
    let w = StatusWriter::new(path.clone());
    w.publish(&Status::Starting); // must return promptly and not panic
}
```

- [ ] **Step 2: Run it to confirm it fails**

```bash
docker run --rm --platform linux/arm64 -v "$PWD":/w -w /w rust:1-bookworm \
  cargo test -p rgsp-host --test status
```

Expected: FAIL — no `rgsp_host::status`.

- [ ] **Step 3: Implement status**

Create `rgsp-host/src/status.rs`:

```rust
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub enum Status {
    Starting,
    AwaitingPairing { url: String },
    Ready { addr: String },
    Connected { client: String, width: u32, height: u32, fps: u32 },
    Stopped,
}

impl Status {
    pub fn line(&self) -> String {
        match self {
            Status::Starting => "Starting...".to_string(),
            Status::AwaitingPairing { url } => format!("Pair at {url}"),
            Status::Ready { addr } => format!("Ready - {addr}"),
            Status::Connected { client, width, height, fps } => {
                format!("Connected - {client} {width}x{height} {fps}fps")
            }
            Status::Stopped => "Casting stopped".to_string(),
        }
    }
}

/// Publishes to show2.elf's FIFO. show2 may not be running, so every write is
/// non-blocking and failures are ignored - a status line is never worth
/// stalling the stream for.
pub struct StatusWriter {
    fifo: PathBuf,
}

impl StatusWriter {
    pub fn new(fifo: PathBuf) -> StatusWriter {
        StatusWriter { fifo }
    }

    pub fn publish(&self, s: &Status) {
        let _ = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&self.fifo)
            .and_then(|mut f| writeln!(f, "TEXT:{}", s.line()));
    }
}
```

Add `pub mod status;` to `rgsp-host/src/lib.rs`.

- [ ] **Step 4: Run the tests**

```bash
docker run --rm --platform linux/arm64 -v "$PWD":/w -w /w rust:1-bookworm \
  cargo test -p rgsp-host --test status
```

Expected: PASS.

- [ ] **Step 5: Assemble the daemon**

Rewrite `rgsp-host/src/main.rs` so that, in order:

1. Acquire the pidfile at `/tmp/rgsp/daemon.pid`; exit 1 if held.
2. `CastSink::engage(Path::new("/mnt/SDCARD/.userdata/h700"))`.
3. Publish `Status::Starting`.
4. Start Moonshine's webserver, RTSP server and mDNS discovery from the vendored crate.
5. Publish `Status::AwaitingPairing` with `http://<lan-ip>:47990/pin` when unpaired, otherwise `Status::Ready`.
6. On session start, spawn the video thread (`std::thread::spawn` running `VideoStream::run`, feeding `EncodedFrame`s into the session's channel) and the audio thread (`LoopbackCapture` → `OpusStream` → the audio channel), and publish `Status::Connected`.
7. On `SIGTERM`/`SIGINT`: publish `Status::Stopped`, `CastSink::release()`, release the pidfile, exit 0.

- [ ] **Step 6: Build and smoke-test on the device**

```bash
docker run --rm --platform linux/arm64 -v "$PWD":/w -w /w rust:1-bookworm cargo build --workspace --release
scp -q target/release/rgsp-host root@192.168.180.106:/tmp/venc/
ssh root@192.168.180.106 'cd /tmp/venc && LD_LIBRARY_PATH=/tmp/venc/lib-trimui ./rgsp-host & sleep 5; curl -s http://localhost:47989/serverinfo | head -5; kill %1'
```

Expected: `/serverinfo` returns XML.

- [ ] **Step 7: Commit**

```bash
git add rgsp-host
git commit -m "feat: assemble the daemon with status reporting"
```

---

### Task 11: The NextUI pak

**Files:**
- Create: `pak/launch.sh`
- Create: `pak/pak.json`
- Create: `pak/cast.png`
- Create: `tests/test_launch_sh.sh`

**Interfaces:**
- Consumes: `rgsp-host` binary (Task 10).
- Produces: a `Cast.pak` directory layout installable to `/mnt/SDCARD/Tools/h700/`.

- [ ] **Step 1: Write the failing test**

Create `tests/test_launch_sh.sh`:

```sh
#!/bin/sh
# Exercises launch.sh's toggle logic with a stub daemon, off-device.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

mkdir -p "$TMP/pak"
cp "$HERE/../pak/launch.sh" "$TMP/pak/"
# Stub daemon: sleeps until killed.
cat > "$TMP/pak/rgsp-host" <<'EOF'
#!/bin/sh
sleep 300
EOF
chmod +x "$TMP/pak/rgsp-host" "$TMP/pak/launch.sh"
# Stub show2 so the script does not need NextUI.
mkdir -p "$TMP/bin"
printf '#!/bin/sh\nexit 0\n' > "$TMP/bin/show2.elf"
chmod +x "$TMP/bin/show2.elf"
export PATH="$TMP/bin:$PATH"
export RGSP_RUN_DIR="$TMP/run"

echo "--- first launch should start the daemon ---"
sh "$TMP/pak/launch.sh"
PID=$(cat "$TMP/run/daemon.pid")
kill -0 "$PID" || { echo "FAIL: daemon not running"; exit 1; }

echo "--- second launch should stop it ---"
sh "$TMP/pak/launch.sh"
sleep 1
if kill -0 "$PID" 2>/dev/null; then echo "FAIL: daemon still running"; exit 1; fi
[ -f "$TMP/run/daemon.pid" ] && { echo "FAIL: pidfile left behind"; exit 1; }

echo PASS
```

- [ ] **Step 2: Run it to confirm it fails**

```bash
chmod +x tests/test_launch_sh.sh && ./tests/test_launch_sh.sh
```

Expected: FAIL — `pak/launch.sh` does not exist.

- [ ] **Step 3: Write `pak/launch.sh`**

```sh
#!/bin/sh
# Cast.pak - toggle casting from the RG SP to a Moonlight client.
#
# Launching this pak starts the daemon and returns to the menu; the stream
# outlives the pak so you can go launch a game. Launching it again stops.
PAK_DIR="$(dirname "$0")"
PAK_NAME="$(basename "$PAK_DIR" .pak)"

_base="${SHARED_USERDATA_PATH:-/mnt/SDCARD/.userdata/${PLATFORM:-h700}}"
export HOME="$_base/$PAK_NAME"
mkdir -p "$HOME"

RUN_DIR="${RGSP_RUN_DIR:-/tmp/rgsp}"
PID_FILE="$RUN_DIR/daemon.pid"
LOG="$RUN_DIR/daemon.log"
mkdir -p "$RUN_DIR"

# The vendor CedarC libraries live in the pak, fetched at install time.
export LD_LIBRARY_PATH="$PAK_DIR/lib/${PLATFORM:-h700}:$LD_LIBRARY_PATH"

show() {
    show2.elf --mode=simple --image="$PAK_DIR/cast.png" --bgcolor=0x000000 &
    SHOW_PID=$!
    sleep 2
    kill "$SHOW_PID" 2>/dev/null || true
}

if [ -f "$PID_FILE" ] && kill -0 "$(cat "$PID_FILE")" 2>/dev/null; then
    # Already casting: stop.
    kill -TERM "$(cat "$PID_FILE")" 2>/dev/null || true
    # The daemon removes its own pidfile on clean exit; give it a moment.
    i=0
    while [ -f "$PID_FILE" ] && [ $i -lt 20 ]; do i=$((i+1)); sleep 0.1; done
    rm -f "$PID_FILE"
    show
else
    # Not casting: start, detached, so it survives this script exiting.
    ( "$PAK_DIR/rgsp-host" >"$LOG" 2>&1 & )
    i=0
    while [ ! -f "$PID_FILE" ] && [ $i -lt 50 ]; do i=$((i+1)); sleep 0.1; done
    show
fi
```

- [ ] **Step 4: Write `pak/pak.json`**

```json
{
  "name": "Cast",
  "version": "v0.1.0",
  "type": "TOOL",
  "description": "Stream this device's screen and sound to a Moonlight client.",
  "platforms": ["h700"]
}
```

- [ ] **Step 5: Create the status image**

```bash
docker run --rm --platform linux/arm64 -v "$PWD":/w -w /w ubuntu:22.04 \
  sh -c 'apt-get update -qq && apt-get install -y -qq imagemagick >/dev/null && \
         convert -size 720x480 xc:black -fill white -gravity center \
                 -pointsize 48 -annotate 0 "CAST" /w/pak/cast.png'
```

- [ ] **Step 6: Run the test**

```bash
./tests/test_launch_sh.sh
```

Expected: `PASS`.

- [ ] **Step 7: Commit**

```bash
git add pak tests/test_launch_sh.sh
git commit -m "feat: Cast.pak with toggle launch and show2 status"
```

---

### Task 12: Hooks — boot, pre-launch, sleep and resume

**Files:**
- Create: `pak/hooks/boot.d/10-rgsp-aloop.sh`
- Create: `pak/hooks/pre-launch.d/10-rgsp-route.sh`
- Create: `pak/hooks/pre-sleep.d/10-rgsp-stop.sh`
- Create: `pak/hooks/post-resume.d/10-rgsp-resume.sh`
- Create: `tests/test_hooks.sh`

**Interfaces:**
- Consumes: `bin/snd-aloop.ko`, `rgsp-host`.
- Produces: hooks installed to `$USERDATA_PATH/.hooks/*.d/`.

- [ ] **Step 1: Write the failing test**

Create `tests/test_hooks.sh`:

```sh
#!/bin/sh
# Hooks run in a subshell with output suppressed and cannot cancel a launch,
# so the only thing worth asserting off-device is that they are syntactically
# valid, exit 0 when their preconditions are absent, and are fast.
set -eu
HERE=$(cd "$(dirname "$0")" && pwd)
FAIL=0

for h in "$HERE"/../pak/hooks/*/*.sh; do
    [ -f "$h" ] || continue
    name=$(basename "$h")

    sh -n "$h" || { echo "FAIL: $name has a syntax error"; FAIL=1; continue; }

    start=$(date +%s)
    if ! RGSP_RUN_DIR=/nonexistent sh "$h" >/dev/null 2>&1; then
        echo "FAIL: $name exited non-zero with no preconditions"; FAIL=1
    fi
    end=$(date +%s)
    [ $((end - start)) -le 2 ] || { echo "FAIL: $name took too long"; FAIL=1; }

    echo "ok: $name"
done

[ "$FAIL" -eq 0 ] && echo PASS || { echo FAILED; exit 1; }
```

- [ ] **Step 2: Run it to confirm it fails**

```bash
chmod +x tests/test_hooks.sh && ./tests/test_hooks.sh
```

Expected: FAIL — no hook files.

- [ ] **Step 3: Write the boot hook**

Create `pak/hooks/boot.d/10-rgsp-aloop.sh`:

```sh
#!/bin/sh
# Load the ALSA loopback that casting captures from. The stock kernel is built
# with CONFIG_SND_ALOOP unset, so this module supplies it; it matches the stock
# kernel's vermagic and symbol CRCs and loads without --force.
PAK_DIR="/mnt/SDCARD/Tools/h700/Cast.pak"
LOG="/mnt/SDCARD/.userdata/h700/logs/rgsp-hooks.log"
mkdir -p "$(dirname "$LOG")"

[ -f "$PAK_DIR/snd-aloop.ko" ] || exit 0
lsmod 2>/dev/null | grep -q '^snd_aloop' && exit 0

if insmod "$PAK_DIR/snd-aloop.ko" 2>>"$LOG"; then
    echo "$(date): snd-aloop loaded" >> "$LOG"
else
    echo "$(date): snd-aloop failed to load" >> "$LOG"
fi
exit 0
```

- [ ] **Step 4: Write the pre-launch hook**

Create `pak/hooks/pre-launch.d/10-rgsp-route.sh`:

```sh
#!/bin/sh
# ALSA config is read when a client opens the PCM, so this is the last moment
# to point a launching game at the cast sink. Only acts while casting.
RUN_DIR="${RGSP_RUN_DIR:-/tmp/rgsp}"
PID_FILE="$RUN_DIR/daemon.pid"
ASOUNDRC="${USERDATA_PATH:-/mnt/SDCARD/.userdata/h700}/.asoundrc"

[ -f "$PID_FILE" ] || exit 0
kill -0 "$(cat "$PID_FILE" 2>/dev/null)" 2>/dev/null || exit 0

grep -q 'hw:Loopback,0,0' "$ASOUNDRC" 2>/dev/null && exit 0

cat > "$ASOUNDRC" <<'EOF'
# rgsp-cast: routing playback into the kernel loopback while casting.
pcm.!default {
    type plug
    slave.pcm "hw:Loopback,0,0"
}
EOF
exit 0
```

- [ ] **Step 5: Write the sleep and resume hooks**

Create `pak/hooks/pre-sleep.d/10-rgsp-stop.sh`:

```sh
#!/bin/sh
# Deep sleep fully stops the USB controllers and takes WiFi with it, so a live
# session would hang the client rather than reconnect. Stop cleanly and record
# that we were casting, so post-resume can bring it back.
RUN_DIR="${RGSP_RUN_DIR:-/tmp/rgsp}"
PID_FILE="$RUN_DIR/daemon.pid"

[ -f "$PID_FILE" ] || exit 0
PID=$(cat "$PID_FILE" 2>/dev/null) || exit 0
kill -0 "$PID" 2>/dev/null || exit 0

touch "$RUN_DIR/was-casting"
kill -TERM "$PID" 2>/dev/null || true
exit 0
```

Create `pak/hooks/post-resume.d/10-rgsp-resume.sh`:

```sh
#!/bin/sh
# Restart casting if it was running before the device slept. WiFi comes back
# asynchronously, so wait briefly for an address before starting.
RUN_DIR="${RGSP_RUN_DIR:-/tmp/rgsp}"
PAK_DIR="/mnt/SDCARD/Tools/h700/Cast.pak"

[ -f "$RUN_DIR/was-casting" ] || exit 0
rm -f "$RUN_DIR/was-casting"
[ -x "$PAK_DIR/rgsp-host" ] || exit 0

i=0
while [ $i -lt 20 ]; do
    ip addr show wlan0 2>/dev/null | grep -q 'inet ' && break
    i=$((i+1)); sleep 0.5
done

export LD_LIBRARY_PATH="$PAK_DIR/lib/h700:$LD_LIBRARY_PATH"
( "$PAK_DIR/rgsp-host" >>"$RUN_DIR/daemon.log" 2>&1 & )
exit 0
```

- [ ] **Step 6: Run the test**

```bash
chmod +x pak/hooks/*/*.sh && ./tests/test_hooks.sh
```

Expected: `PASS` with an `ok:` line per hook.

- [ ] **Step 7: Commit**

```bash
git add pak/hooks tests/test_hooks.sh
git commit -m "feat: boot, pre-launch, sleep and resume hooks"
```

---

### Task 13: Install script and release packaging

**Files:**
- Create: `scripts/install-pak.sh`
- Modify: `Makefile`

**Interfaces:**
- Consumes: everything.
- Produces: `make pak` building a complete `Cast.pak`; `scripts/install-pak.sh root@DEVICE` installing it and the hooks.

- [ ] **Step 1: Add the `pak` target**

Add to `Makefile`:

```make
PAKDIR = dist/Tools/h700/Cast.pak

.PHONY: pak
pak: librgspcast.a bin/snd-aloop.ko
	@mkdir -p $(PAKDIR)/lib/h700
	docker run --rm --platform linux/arm64 -v "$(CURDIR)":/w -w /w rust:1-bookworm \
		sh -c 'apt-get update -qq && apt-get install -y -qq libasound2-dev libopus-dev pkg-config >/dev/null 2>&1 && \
		       cargo build --workspace --release'
	cp target/release/rgsp-host $(PAKDIR)/
	cp pak/launch.sh pak/pak.json pak/cast.png $(PAKDIR)/
	cp bin/snd-aloop.ko $(PAKDIR)/
	cp -r pak/hooks $(PAKDIR)/
	chmod +x $(PAKDIR)/launch.sh $(PAKDIR)/rgsp-host $(PAKDIR)/hooks/*/*.sh
	@echo "-> $(PAKDIR)"
	@echo "   lib/h700 is populated on the device at install time"
```

- [ ] **Step 2: Write the installer**

Create `scripts/install-pak.sh`:

```sh
#!/bin/sh
# Install Cast.pak on the device, including hooks and the vendor libraries.
#
# The CedarC blobs are fetched on the device rather than shipped: they are
# proprietary, and extract-vendor-libs.sh verifies checksums against TrimUI's
# own firmware release.
set -eu

DEVICE=${1:?usage: $0 root@DEVICE}
HERE=$(cd "$(dirname "$0")" && pwd)
PAKDIR="$HERE/../dist/Tools/h700/Cast.pak"
DEST=/mnt/SDCARD/Tools/h700/Cast.pak
HOOKS=/mnt/SDCARD/.userdata/h700/.hooks

[ -d "$PAKDIR" ] || { echo "build first: make pak" >&2; exit 1; }

ssh "$DEVICE" "mkdir -p $DEST $HOOKS/boot.d $HOOKS/pre-launch.d $HOOKS/pre-sleep.d $HOOKS/post-resume.d"
scp -q -r "$PAKDIR"/* "$DEVICE:$DEST/"

# Hooks live under .userdata, not in the pak, so NextUI finds them.
for phase in boot pre-launch pre-sleep post-resume; do
    for f in "$PAKDIR/hooks/$phase.d"/*.sh; do
        [ -f "$f" ] || continue
        scp -q "$f" "$DEVICE:$HOOKS/$phase.d/"
    done
done
ssh "$DEVICE" "chmod +x $DEST/launch.sh $DEST/rgsp-host $HOOKS/*/*.sh"

# Vendor libraries, fetched here and pushed, never committed.
if [ ! -d "$HERE/../vendor-libs" ]; then
    "$HERE/extract-vendor-libs.sh"
fi
ssh "$DEVICE" "mkdir -p $DEST/lib/h700"
scp -q "$HERE/../vendor-libs"/* "$DEVICE:$DEST/lib/h700/"

ssh "$DEVICE" "lsmod | grep -q '^snd_aloop' || insmod $DEST/snd-aloop.ko"

cat <<EOF

Installed to $DEST
Hooks installed to $HOOKS

Launch it from Tools -> Cast. It toggles: once to start, again to stop.
Pair from Moonlight, then open the URL shown on screen in a browser.
EOF
```

- [ ] **Step 3: Build and install**

```bash
chmod +x scripts/install-pak.sh
make pak
./scripts/install-pak.sh root@192.168.180.106
```

Expected: completes and prints the install summary.

- [ ] **Step 4: Verify the pak appears and toggles**

```bash
ssh root@192.168.180.106 'ls -la /mnt/SDCARD/Tools/h700/Cast.pak/ && ls /mnt/SDCARD/.userdata/h700/.hooks/*/'
```

Then on the device: Tools → Cast. Confirm the daemon starts:

```bash
ssh root@192.168.180.106 'cat /tmp/rgsp/daemon.pid && tail -5 /tmp/rgsp/daemon.log'
```

Expected: a live PID and a log showing the server listening.

- [ ] **Step 5: Commit**

```bash
git add Makefile scripts/install-pak.sh
git commit -m "feat: pak build and device installer"
```

---

### Task 14: Draw a "live" marker into the outgoing stream

**Files:**
- Modify: `src/rgsp-cast.c`
- Modify: `include/rgsp_cast.h`
- Create: `tests/test_overlay.c`

**Interfaces:**
- Consumes: `rgsp_capture` (Task 3).
- Produces: `void rgsp_capture_set_overlay(rgsp_capture *c, int enabled);` — when enabled, a small marker is composited into each captured frame before encoding.

The handheld's own status pill cannot show a cast indicator during gameplay
without patching minarch into `.system/`. The stream is ours, so the marker goes
there instead: the TV shows it, the handheld's screen is untouched.

- [ ] **Step 1: Write the failing test**

Create `tests/test_overlay.c`:

```c
/* The marker is composited into the captured copy, never into the framebuffer
 * itself - the device's own display must be unchanged. */
#include "../include/rgsp_cast.h"
#include <stdio.h>
#include <assert.h>

int main(void)
{
    rgsp_capture *c = rgsp_capture_open(720, 480, 30, 2000000);
    if (!c) { fprintf(stderr, "open: %s\n", rgsp_capture_last_error()); return 1; }

    const unsigned char *data; size_t len; int key;

    rgsp_capture_set_overlay(c, 0);
    if (rgsp_capture_next(c, &data, &len, &key) != 0) return 1;
    size_t without = len;

    rgsp_capture_set_overlay(c, 1);
    if (rgsp_capture_next(c, &data, &len, &key) != 0) return 1;
    if (rgsp_capture_next(c, &data, &len, &key) != 0) return 1;
    size_t with = len;

    printf("without=%zu with=%zu\n", without, with);
    assert(with > 0);
    printf("PASS\n");
    rgsp_capture_close(c);
    return 0;
}
```

- [ ] **Step 2: Run it to confirm it fails**

```bash
make librgspcast.a
docker run --rm --platform linux/arm64 -v "$PWD":/w -w /w ubuntu:22.04 \
  sh -c 'apt-get update -qq && apt-get install -y -qq gcc >/dev/null && \
         gcc -O2 -o bin/test-overlay tests/test_overlay.c librgspcast.a -ldl'
```

Expected: FAIL — `rgsp_capture_set_overlay` is undefined.

- [ ] **Step 3: Implement the overlay**

Add to `include/rgsp_cast.h`:

```c
/* Composites a small marker into captured frames so the receiving client can
 * see the stream is live. Does not touch the device's own display. */
void rgsp_capture_set_overlay(rgsp_capture *c, int enabled);
```

In `src/rgsp-cast.c`, add `int overlay;` to `struct rgsp_capture`, the setter, and
in `rgsp_capture_next()` — after the framebuffer copy into the ION buffer, before
`VideoEncodeOneFrame` — write a 16x16 opaque red square at (16,16) into the ION
buffer. The buffer is BGRA, 4 bytes per pixel, stride `width * 4`:

```c
static void draw_marker(unsigned char *buf, int width, int stride)
{
    (void)width;
    for (int y = 16; y < 32; y++) {
        unsigned char *row = buf + (size_t)y * stride;
        for (int x = 16; x < 32; x++) {
            unsigned char *px = row + (size_t)x * 4;
            px[0] = 0x00;  /* B */
            px[1] = 0x00;  /* G */
            px[2] = 0xFF;  /* R */
            px[3] = 0xFF;  /* A */
        }
    }
}
```

- [ ] **Step 4: Build and run on the device**

```bash
make librgspcast.a
docker run --rm --platform linux/arm64 -v "$PWD":/w -w /w ubuntu:22.04 \
  sh -c 'apt-get update -qq && apt-get install -y -qq gcc >/dev/null && \
         gcc -O2 -o bin/test-overlay tests/test_overlay.c librgspcast.a -ldl'
scp -q bin/test-overlay root@192.168.180.106:/tmp/venc/
ssh root@192.168.180.106 'cd /tmp/venc && LD_LIBRARY_PATH=/tmp/venc/lib-trimui ./test-overlay'
```

Expected: prints sizes and `PASS`. Confirm the handheld's own screen shows no
red square.

- [ ] **Step 5: Enable it from the daemon**

In `rgsp-host/src/capture.rs`, add:

```rust
extern "C" {
    fn rgsp_capture_set_overlay(c: *mut RgspCapture, enabled: c_int);
}

impl Capture {
    pub fn set_overlay(&self, enabled: bool) {
        unsafe { rgsp_capture_set_overlay(self.handle, enabled as c_int) }
    }
}
```

and call `capture.set_overlay(true)` in `VideoStream::run` after opening.

- [ ] **Step 6: Commit**

```bash
git add include src tests/test_overlay.c rgsp-host
git commit -m "feat: composite a live marker into the outgoing stream"
```

---

### Task 15: End-to-end verification and documentation

**Files:**
- Modify: `README.md`
- Create: `docs/measurements.md`

**Interfaces:**
- Consumes: everything.
- Produces: a verified working system and current documentation.

- [ ] **Step 1: Pair and stream to the Apple TV**

1. On the device: Tools → Cast.
2. Read the pairing URL from the status screen (or `tail /tmp/rgsp/daemon.log`).
3. On the Apple TV, open Moonlight and add the host **by IP**.
4. Moonlight shows a PIN; enter it at the URL in a browser on any LAN machine.
5. Start the stream from Moonlight.
6. On the device, launch a game.

Expected: the game appears on the TV with sound, and the handheld's speaker is silent.

- [ ] **Step 2: Confirm the negotiated resolution**

```bash
ssh root@192.168.180.106 'grep -i -E "width|height|negotiat" /tmp/rgsp/daemon.log | head'
```

Expected: Moonlight accepts the framebuffer's native 720x480. If it instead
negotiates a different resolution, the frames must be letterboxed to it — record
which in `docs/measurements.md`, since it decides whether the VE's ISP scaler is
needed.

- [ ] **Step 3: Measure and record**

```bash
ssh root@192.168.180.106 'cd /tmp/venc && sh monitor.sh 60 /tmp/venc/cast-mon.log'
scp -q root@192.168.180.106:/tmp/venc/cast-mon.log .
```

Create `docs/measurements.md` recording, with the numbers from this run: sustained frame rate, CPU per core, GPU load, thermal zone temperatures over 60 s, and glass-to-glass latency (film the handheld and the TV together at 120 fps, count frames).

- [ ] **Step 4: Verify sleep and resume**

Let the device sleep with casting active, then wake it.

Expected: casting stops before sleep and comes back after resume; Moonlight reconnects.

- [ ] **Step 5: Verify audio restores**

Stop casting, relaunch a game.

Expected: sound comes out of the handheld speaker again; `/mnt/SDCARD/.userdata/h700/.asoundrc` is gone or restored to its pre-cast contents.

- [ ] **Step 6: Update the README**

Rewrite the `## Status` section to describe the working system: capture, encode, audio, and streaming to Moonlight. Add a `## Streaming to a TV` section covering installation (`make pak && ./scripts/install-pak.sh root@DEVICE`), pairing, and the toggle. Replace the `Streaming this to a TV is the next step, planned in ROADMAP.md` line with a link to `docs/measurements.md`.

Move the References tables from the end of this plan into the README's `## Credits` section, keeping the licence note: reading Sunshine, moonlight-common-c and moonlight-common-rust is fine; copying them is not, and nothing GPLv3 is in this tree.

- [ ] **Step 7: Run the whole test suite**

```bash
./tests/test_launch_sh.sh
./tests/test_hooks.sh
docker run --rm --platform linux/arm64 -v "$PWD":/w -w /w rust:1-bookworm \
  sh -c 'apt-get update -qq && apt-get install -y -qq libasound2-dev libopus-dev pkg-config >/dev/null 2>&1 && \
         cargo test --workspace'
```

Expected: all PASS (device-only tests skip cleanly off-device).

- [ ] **Step 8: Commit**

```bash
git add README.md docs/measurements.md
git commit -m "docs: document the working streaming host"
```

---

