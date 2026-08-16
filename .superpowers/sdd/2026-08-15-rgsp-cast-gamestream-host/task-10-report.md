# Task 10 report — assemble the daemon, add the status IPC

## What was wired, and how

`rgsp-host/src/main.rs` is now the only place the vendored protocol layer and the
hardware modules meet. Ordering follows the brief's Step 5:

1. **Pidfile** `/tmp/rgsp/daemon.pid` via `PidFile::acquire`; exit 1 if held.
   Taken first, before anything with a side effect.
2. **`CastSink::engage(userdata)`** — `$RGSP_USERDATA`, default
   `/mnt/SDCARD/.userdata/h700`.
3. **`Status::Starting`** to the show2 FIFO (`$RGSP_STATUS_FIFO`, default
   `/tmp/show2.fifo`).
4. **Protocol layer** — `tls::load_or_create_certificate`, `SessionManager`,
   `ClientManager`, `RtspServer`, `Webserver`, `MdnsDiscovery`, assembled in the
   same order as the deleted upstream `Moonshine::new`
   (`git show f23d52e^:vendor/moonshine/src/main.rs`), minus the Vulkan
   healthcheck and D-Bus wait that no longer apply.
5. **`AwaitingPairing` / `Ready`** chosen from whether any client is paired,
   with the LAN IP found by connecting a UDP socket to TEST-NET-1 (no packets
   sent) and the **actual** configured HTTP port.
6. **Session pump** (`session_pump`): polls `video_frame_sender()` every 250 ms,
   and on a session start takes `audio_frame_sender()`,
   `encoder_control_receiver()` and `active_video_context()`, spawns the two
   hardware loops, and publishes `Connected`.
7. **SIGTERM/SIGINT** → `ShutdownManager::trigger_shutdown(AppQuit)` → `serve`
   returns → `Stopped`, `CastSink::release()`, pidfile released, exit 0.

Data paths:

- **Video**: `VideoStream::run` → callback builds an owned
  `moonshine_core::…::EncodedFrame` (`data.to_vec()`, `is_keyframe` →
  `is_key_frame`) and `blocking_send`s it. Runs on a blocking thread as the
  module docs require.
- **Audio**: `LoopbackCapture::open("hw:Loopback,1,0")` → `Vec<i16>` of exactly
  `PERIOD_FRAMES * CHANNELS` = 480 interleaved samples → `blocking_send`. No
  conversion, no encoding. **Short reads are accumulated**, never sent: the
  vendored bridge drops any chunk ≠ 480 samples at warn level, so a short send
  after an overrun recovery would degrade audio with no error anywhere.
- **Control**: `EncoderControl::Idr` and `Invalidate{..}` both →
  `IdrRequester::request()` (Cedar has no reference-invalidation API);
  `Reset` → `ResetRequester::request()`.
- **`supported_codecs = 0x1`** (H.264 only). Not guessed — recovered from the
  deleted probe, `git show f23d52e^:…/healthcheck.rs:55-64`
  (CODEC_H264 = 0x1, CODEC_HEVC = 0x100, CODEC_AV1_MAIN8 = 0x10000). Verified on
  the device: `/serverinfo` reports `<ServerCodecModeSupport>1</…>` and
  `<MaxLumaPixelsHEVC>0</…>`. `hdr_supported` is passed `false`.
- **Audio frame-size assertion** (correction #3): `const _: () = assert!(…)` in
  `rgsp-host/src/audio.rs`, compile-time.

`Capture` single-instance and terminal-failure rules are respected: the capture
is opened per session inside the video loop, there is no retry, and **both**
loops are awaited to completion before the pump comes round again.

## TDD evidence (real output)

**RED** — `cargo test -p rgsp-host --test status` before `status.rs` existed:

```
   Compiling rgsp-host v0.1.0 (/w/rgsp-host)
error[E0432]: unresolved import `rgsp_host::status`
 --> rgsp-host/tests/status.rs:1:16
  |
1 | use rgsp_host::status::{Status, StatusWriter};
  |                ^^^^^^ could not find `status` in `rgsp_host`

error: could not compile `rgsp-host` (test "status") due to 1 previous error
```

**GREEN** — after adding `status.rs` and `pub mod status;`:

```
     Running tests/status.rs (target/debug/deps/status-146aa9041d58f95f)

running 2 tests
test status_lines_lead_with_what_the_user_needs ... ok
test publish_never_blocks_when_the_fifo_has_no_reader ... ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

**The compile-time audio assertion is live** — proven by temporarily setting
`PERIOD_FRAMES = 241` and rebuilding (then reverting):

```
error[E0080]: evaluation panicked: capture period must equal Moonshine's Opus frame size
  --> rgsp-host/src/audio.rs:42:15
   |
42 |   const _: () = assert!(
43 | |     PERIOD_FRAMES == moonshine_core::session::stream::audio::FRAME_FRAMES,
44 | |     "capture period must equal Moonshine's Opus frame size"
   | |_^ evaluation of `audio::_` failed here
```

**Whole workspace** — `cargo test --workspace`, arm64 `rust:1-bookworm` +
`cmake clang libopus-dev libasound2-dev pkg-config`:

```
moonshine-core lib .............. 20 passed; 0 failed
protocol_surface ................  1 passed; 0 failed
rgsp-host audio_capture .........  3 passed; 0 failed; 1 ignored
rgsp-host capture_ffi ...........  2 passed; 0 failed
rgsp-host control_input .........  1 passed; 0 failed
rgsp-host pidfile ...............  4 passed; 0 failed
rgsp-host routing ...............  3 passed; 0 failed
rgsp-host status ................  2 passed; 0 failed
rgsp-host video_stream ..........  2 passed; 0 failed
```

**Clippy**: `cargo clippy --workspace --all-targets -- -D warnings` is **clean**.

It was red on arrival at findings in files this task did not write, surfaced by
a newer clippy than earlier tasks ran (the `rust:1-bookworm` tag floats; it is
1.97.1 today): `capture.rs:87` (`should_implement_trait`), five
`manual_c_str_literals` in `routing.rs`, and two unused-binding errors in the
`#[ignore]`d loopback test. Fixed on the controller's ruling in a separate
commit (`7a461d0`), with no behaviour change:

- `c"InitSettings"` and friends are the same `&'static CStr` that
  `CStr::from_bytes_with_nul(b"InitSettings\0").unwrap()` produced.
- `Capture::next` gets an `#[allow]`, since the `Frame` it returns borrows
  `self` for its lifetime and `Iterator` cannot express that.
- The unused binding becomes `_cap`, which still holds the capture end of the
  cable open — the same shape the sibling test at line 58 already used.

The `c""` literals feed exactly one thing: the `dlopen`/`dlsym` chain into
NextUI's libmsettings, which no container test can exercise. Re-verified on the
device rather than argued from the diff:

```
[DEBUG] USERDATA_PATH set to /mnt/SDCARD/.userdata/h700
[DEBUG] libmsettings initialized from /mnt/SDCARD/.system/h700/lib/libmsettings.so
```

That matches Task 8's evidence line for line.

## Device smoke test (real output, 192.168.180.106)

First deploy failed and the failure is worth recording:

```
./rgsp-host: error while loading shared libraries: libopus.so.0: cannot open shared object file
```

The device has `libasound.so.2` but **no libopus**. With `libopus-dev` installed
in the build container, `audiopus_sys` links Opus dynamically; **without** it,
the crate builds Opus from source and links it statically. Rebuilt with
`cmake clang libasound2-dev pkg-config` (no `libopus-dev`) and the dependency is
gone:

```
 (NEEDED) Shared library: [libasound.so.2]
 (NEEDED) Shared library: [libgcc_s.so.1]
 (NEEDED) Shared library: [libm.so.6]
 (NEEDED) Shared library: [libc.so.6]
 (NEEDED) Shared library: [ld-linux-aarch64.so.1]
```

**This changes the build recipe for Tasks 13/15: drop `libopus-dev`.** Keeping it
produces a binary that cannot start on the device.

Smoke test with that binary (run under `RGSP_USERDATA=/tmp/rgsp-smoke` so it
could not disturb the user's real `.asoundrc`):

```
=== pidfile contents: 25995 / shell PID 25995
=== serverinfo:
<root status_code="200"><hostname>RG SP</hostname><appversion>7.1.431.-1</appversion>
<GfeVersion>3.23.0.74</GfeVersion><uniqueid>7609627d-0456-4405-98d9-1f80d6f6155e</uniqueid>
<HttpsPort>47984</HttpsPort><ExternalPort></ExternalPort><mac>00:00:00:00:00:00</mac>
<MaxLumaPixelsHEVC>0</MaxLumaPixelsHEVC><LocalIP>127.0.0.1</LocalIP>
<ServerCodecModeSupport>1</ServerCodecModeSupport><SupportedDisplayMode></SupportedDisplayMode>
<PairStatus>0</PairStatus><currentgame>0</currentgame><state>MOONSHINE_SERVER_FREE</state></root>
=== asoundrc while casting:
# rgsp-cast: routing playback into the kernel loopback while casting.
# Removed automatically when casting stops.
pcm.!default {
    type plug
    slave.pcm "hw:Loopback,0,0"
}
=== files created:
/tmp/rgsp-smoke/rgsp-cast: cert.pem  config.toml  key.pem  moonshine/state.toml
=== second instance refused?
ERROR rgsp_host: rgsp-host is already running (could not acquire lock on /tmp/rgsp/daemon.pid)
exit=1
=== alive after SIGTERM? GONE
=== asoundrc after SIGTERM: ls: /tmp/rgsp-smoke/.asoundrc: No such file or directory
=== pidfile after SIGTERM: ls: /tmp/rgsp/daemon.pid: No such file or directory
=== daemon log:
 INFO rgsp_host::status: status: Starting...
 INFO rgsp_host: wrote a default configuration to /tmp/rgsp-smoke/rgsp-cast/config.toml
 INFO moonshine_core::tls: No certificate found, creating a new one.
 INFO rgsp_host::status: status: Pair at http://192.168.180.106:47989/pin
 INFO rgsp_host: rgsp-host is ready and waiting for connections
 INFO rgsp_host: received SIGTERM, shutting down
 INFO rgsp_host::status: status: Casting stopped
```

`/serverinfo` returns XML; SIGTERM publishes `Stopped`, removes the cast
`.asoundrc` (correctly leaving it absent, which is what it was before) and drops
the pidfile. The real `/mnt/SDCARD/.userdata/h700/.asoundrc` was absent before
and after; the smoke directory was deleted afterwards. What is **not** covered:
an actual paired Moonlight session — that is Task 15's end-to-end verification.

## What happens on SIGTERM

`spawn_signal_handler` selects over SIGINT and SIGTERM and triggers
`ShutdownReason::AppQuit`. `serve()` then **stops any active session
explicitly** before returning — `SessionManagerInner::drop` calls
`Handle::block_on` when a session is still live, which panics if it runs from
inside an async context, and that panic would unwind past `main`'s teardown and
leave `.asoundrc` pointed at the loopback: a dead speaker, which is precisely
the window the dispatch asked me to close. `stop_session()` first leaves `Drop`
with nothing to do, and it closes the frame channels so the blocking loops exit
well inside the 2 s timeout. `serve()` returns, the pump is aborted, then
`runtime.shutdown_timeout(2s)` (dropping a `Runtime` otherwise *waits* for
blocking tasks, and a capture loop parked in ALSA or Cedar must not delay
restoring the speaker). Then, unconditionally: `Status::Stopped` →
`CastSink::release()` → `PidFile::release()` → exit 0. The same `shutdown()`
function runs on every startup failure after `engage()` succeeded (runtime
creation, config, TLS, session manager, client manager, webserver), so the
"daemon exits with the speaker still muted" window is closed for every path
except a crash or SIGKILL — which is Task 12's hook territory, as the ledger
already records. A hard panic elsewhere, or SIGKILL, still skips `release()`;
that residual is Task 12's hooks.

**Verification limit, stated plainly**: the SIGTERM path was device-tested
**idle only** — no Moonlight client was available to establish a session. The
mid-session teardown is structurally guaranteed by the explicit `stop_session()`
above rather than empirically verified; Task 15's end-to-end run covers it.

## Vendor files touched, and why

All four are additive plumbing; three of them were sent to the controller as
questions before coding (no ruling had arrived by the time the work was done, so
I proceeded with the proposals as stated and they are trivially revertible).

| File | Change | Why |
|---|---|---|
| `audio/host_source.rs` | `pub(crate) const FRAME_FRAMES` → `pub` + doc | Correction #3's assertion is impossible otherwise. Project-created file. |
| `audio/mod.rs` | one line, `pub use host_source::FRAME_FRAMES;` | Mirrors video/mod.rs:15's existing `pub use`. |
| `session/manager.rs` | `active_video_context` field + accessor (+3 plumbing lines) | The negotiated fps and bitrate are consumed by `start_session` and were unreachable; `SessionContext::refresh_rate` is the launch-mode value, not the ANNOUNCE fps, so pacing RTP against it would be wrong. Same lifecycle as `video_frame_tx`. |
| `state.rs` | `pub fn has_any_client()` | `has_client` is `pub(crate)`; the alternative was re-implementing `dirs::data_dir()` path resolution and parsing `state.toml` in rgsp-host. |
| `tests/protocol_surface.rs` | 4 guard lines | So a `git subtree pull` that re-narrows any of the above fails loudly. |

No vendored logic changed; no upstream behaviour changed.

## Files changed

- `rgsp-host/src/main.rs` (rewritten), `rgsp-host/src/status.rs` (new),
  `rgsp-host/tests/status.rs` (new), `rgsp-host/src/lib.rs`,
  `rgsp-host/src/audio.rs` (assertion only), `rgsp-host/Cargo.toml`
  (`async-shutdown`, `toml`), `Cargo.lock`.
- Vendor: the five files in the table above.

## Self-review

- **Deviation from the brief, disclosed**: the hardware loops use
  `tokio::task::spawn_blocking` rather than bare `std::thread::spawn`. Both are
  dedicated OS threads (the "must not occupy a tokio worker" requirement holds),
  but `spawn_blocking` yields an awaitable handle, and awaiting it is what
  guarantees the `Capture` has been dropped before the next session opens one —
  `Capture` is single-instance per process, so a detached thread would break
  every subsequent session. Note that dropping a `JoinHandle` *detaches* a
  blocking task rather than cancelling it, which is why the select arms await
  the survivor explicitly instead of letting it drop.
- The pump calls `stop_session()` as soon as *either* loop stops, so a terminal
  capture failure tears the session down rather than leaving a half-dead one.
- `Status::Connected`'s `client` is the fixed string `"Moonlight"`: no client
  name or address exists anywhere in the tree (`SessionContext` carries neither),
  and adding vendor plumbing for a cosmetic string was not worth it.
- `Connected` reports 720x480, the panel geometry the client actually decodes,
  not the negotiated size; the negotiated size is logged next to it.
- The pairing URL uses the configured HTTP port (47989), not the brief's 47990 —
  47990 is Sunshine's port; `webserver/pairing.rs:176` builds its URL from the
  HTTP port, and this device's `/pin` is served there.
- Config/cert/pairing state live under `<userdata>/rgsp-cast/`; `XDG_DATA_HOME`
  is set there **only if unset**, so `PersistentState` (which resolves via
  `dirs::data_dir()`) keeps pairing on the SD card rather than the rootfs.

## Concerns

1. **Surround audio would be silent.** The vendored bridge computes
   `expected_samples = FRAME_FRAMES * channels` from the session's negotiated
   `AudioChannels`; the host always sends 480 stereo samples. A client that
   negotiates 5.1 would have every chunk dropped — silent audio, warn-level
   only. Nothing pins negotiation to stereo. Moonlight defaults to stereo, so
   this is not a blocker, but it is a real hole. Recorded in the source at the
   chunk-size check (`0c102ce`) so the next person meets it there.
2. **Build recipe change** (above): `libopus-dev` must be *absent* from the
   build container for the release binary that ships, or it cannot start on the
   device. Tasks 13/15. Note the artifact cache carries the choice: a `target/`
   populated by a `libopus-dev` build fails to link once the package is removed
   (`cannot find -lopus`) until that package is rebuilt. Container test runs are
   unaffected either way — this only matters for the binary that is deployed.
3. ~~Clippy gate red~~ — **resolved** in `7a461d0` on the controller's ruling;
   the gate is green and the dlopen path was re-verified on device.
4. **The idle status line is recomputed after each session**, so a first-time
   user does not see "Pair at …" again after pairing, and the address refreshes
   with it. It is still computed once at startup, so a DHCP change before the
   first session leaves a stale address until a session ends.
5. **250 ms session poll**: `SessionManager` has no session-start notification,
   so first frame can lag PLAY by up to that. A `Notify` on the vendor side
   would remove it if it ever matters.
6. The AV1 `rtpmap` over-advertisement from Task 5 is still open; unchanged here.
