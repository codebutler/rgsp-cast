# Vendored code

## vendor/moonshine

Source: https://github.com/hgaiser/moonshine (BSD-2-Clause)
Pinned at tag: v0.15.0
Added with: git subtree add --prefix=vendor/moonshine https://github.com/hgaiser/moonshine.git v0.15.0 --squash

Update with:
    git subtree pull --prefix=vendor/moonshine https://github.com/hgaiser/moonshine.git <tag> --squash

We use only the GameStream protocol layer: webserver + pairing, rtsp, tls,
crypto, clients, discovery, packetizer, gso_socket, shard_batch, control,
audio (~5,234 lines).

Keep these files as close to upstream as possible so `git subtree pull`
merges cleanly: no reformatting, no renaming, no refactors. All of our own
code lives in rgsp-host/src/.

Deleted here because the device has no Vulkan, no Wayland and no PulseAudio:
session/compositor, session/stream/video/pipeline, app_scanner,
session/stream/audio/pulse_server, and the moonshine-wsi crate.

### Full deletion list

Subsystems the device cannot support:

- `moonshine-core/src/session/compositor/` — Wayland (smithay) + DRM/GBM.
- `moonshine-core/src/session/stream/video/pipeline/` — Vulkan video encode (ash, pixelforge).
- `moonshine-core/src/session/stream/audio/pulse_server/` — embedded PulseAudio server.
- `moonshine-core/src/session/stream/audio/buffer.rs` — mixing buffers built on the `pulseaudio` crate's protocol types.
- `moonshine-core/src/app_scanner/` — Steam/Heroic/Lutris/desktop scanning (steamlocate, walkdir, .desktop parsing).
- `moonshine-core/src/session/stream/control/input/` — gamepad/keyboard/mouse injection via inputtino, feeding the compositor.
- `moonshine-core/src/session/inhibit.rs` — logind sleep inhibitor over zbus.
- `moonshine-core/src/healthcheck.rs` — Vulkan encoder probe via pixelforge.
- `moonshine-wsi/` — Vulkan WSI layer.

Reduced rather than deleted:

- `moonshine-core/src/session/application.rs` — kept only `ApplicationConfig`
  (the applist/launch HTTP endpoints need it); the systemd/zbus launcher is gone.

Vendored workspace scaffolding, removed so the repo root can own the workspace:

- `Cargo.toml` (the vendored `[workspace]` root), `Cargo.lock`, `src/` (the
  `moonshine` binary), `moonshine-tools/`, `flake.nix`, `flake.lock`, `nix/`,
  `nfpm.yaml`, `dist/`.
  `moonshine-core/Cargo.toml` is now a standalone manifest with `version`
  inlined instead of inherited.

### Visibility changes

`moonshine-core/src/lib.rs`: `crypto` is `pub` (was `pub(crate)`).
`moonshine-core/src/session/stream/video/mod.rs`: `gso_socket`, `packetizer`
and `shard_batch` are `pub` (were private). Their items were promoted from
`pub(crate)` to `pub` so they are usable from outside the crate.

`moonshine-core/tests/protocol_surface.rs` guards these: it fails to compile
if a `git subtree pull` reverts them.

### Behaviour changes made to drop dependencies

- Pairing no longer raises a desktop notification with the PIN URL
  (notify-rust + open); it logs the URL instead.
- `/appasset` serves the configured boxart file verbatim instead of decoding
  and rescaling it to 600x801 (the `image` crate).
- `HdrMetadata` / `HdrModeState` moved from `session/compositor/frame.rs` to
  `session/stream/video/mod.rs`; `AudioFrame` and `CAPTURE_SAMPLE_RATE` moved
  from `session/stream/audio/pulse_server/` to `session/stream/audio/mod.rs`.
  These are protocol-side types whose defining modules were deleted.
- `VideoStream::start` and `AudioStream::start` create their packet/frame
  channels with no in-crate producer — the Vulkan pipeline and PulseAudio
  server that fed them are gone. The host binary supplies encoded video and
  audio samples; the injection seam is defined in a later task.
- Control-stream `InputData` messages are parsed and dropped rather than
  injected into the compositor.
