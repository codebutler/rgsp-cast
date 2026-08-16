# rgsp-cast

Hardware H.264 screen capture for the **Anbernic RG SP** (Allwinner H700 /
sun50iw9) running **BaseOS + NextUI**.

Reads `/dev/fb0` and encodes it on the SoC's Cedar video engine, with game audio
captured through an ALSA loopback. Encoding is done entirely in hardware, including
the RGB→YUV colour conversion, so the whole pipeline costs about **one seventh
of one CPU core** and the emulator does not notice it is running.

```
720x480 @ 30.0 fps sustained · 1.8 ms/frame CPU · ~2 Mbps · H.264 Main level 4.1
      + 48 kHz stereo audio, measured drift within ±6 ms
```

## Status

Working and measured: **video and audio**, muxed to MP4.

Audio capture runs through the kernel loopback. `bin/snd-aloop.ko` is
**verified on hardware** — it loads with plain `insmod` and carries live game
audio (see [Audio](#audio)). Wiring `rgsp-cast` to read the capture side is the
piece in progress.

| | |
|---|---|
| Device | Anbernic RG SP, Allwinner H700 (H616/H618 family, `sun50iw9`) |
| OS | BaseOS + NextUI, kernel 4.9.170 |
| Panel | 720x480, 32bpp BGRA, double-buffered (720x960 virtual) |
| IC version | `0x3301000012011` |
| Video out | Annex-B H.264 elementary stream (Main, level 4.1) |
| Audio out | raw s16le 48 kHz stereo, muxed to AAC |

Streaming this to a TV is planned in
[docs/superpowers/plans/2026-08-15-rgsp-cast-gamestream-host.md](docs/superpowers/plans/2026-08-15-rgsp-cast-gamestream-host.md).

## Quick start

```sh
# 1. Fetch the vendor libraries (once). Downloads the pinned TrimUI firmware,
#    verifies its checksum, and extracts the CedarC libs into vendor-libs/.
./scripts/extract-vendor-libs.sh
scp -r vendor-libs root@DEVICE:/tmp/venc/lib-trimui

# 2. For sound, load the loopback module once per boot
./scripts/build-snd-aloop.sh
scp bin/snd-aloop.ko root@DEVICE:/tmp/
ssh root@DEVICE 'insmod /tmp/snd-aloop.ko'

# 3. Build (in an arm64 container) and record 30 s
make run DEVICE=root@DEVICE DURATION=30 OUT=session.h264
# -> session.mp4: video stream-copied, audio encoded to AAC
```

Already have the firmware? Pass it instead of downloading — `.zip` or `.awimg`:

```sh
./scripts/extract-vendor-libs.sh ~/Downloads/trimui_tg5040_20251128_v1.1.1.zip
```

Manually:

```sh
LD_LIBRARY_PATH=/tmp/venc/lib-trimui ./rgsp-cast -o out.h264 -d 30 -f 30
```

```
-o FILE   output Annex-B .h264        (default cast.h264)
-d SECS   duration                    (default 30)
-f FPS    target frame rate           (default 30)
-n FRAMES stop after N frames         (overrides -d)
-i FMT    input format: 12 = ARGB passthrough (default), 0 = NV12 via CPU
-a PATH   audio source to follow      (default /tmp/rgsp-audio.pcm)
-A        video only, ignore audio
-v        per-frame logging
```

Audio lands beside the video as `<output>.h264.pcm` (raw s16le 48 kHz stereo).

`make run` does the muxing for you. By hand — note the explicit `-map`, without
which ffmpeg silently drops the raw PCM:

```sh
ffmpeg -r 30 -i out.h264 -f s16le -ar 48000 -ac 2 -i out.h264.pcm \
       -map 0:v -map 1:a -c:v copy -c:a aac -movflags +faststart out.mp4
```

## Vendor libraries

**Anbernic's own firmware ships no CedarC runtime at all.** Mounting the stock
image shows `libVE.so`, `libMemAdapter.so`, `libcdc_base.so` and
`libvdecoder.so` are absent; the only trace is a dangling `DT_NEEDED`
reference from an unused vendor demo at `/bin/video/xplayerdemo2`.

So the libraries come from another device in the same VE family. **TrimUI Smart
Pro firmware v1.1.1** (H618) carries a current, glibc-2.33 build that loads on
BaseOS unmodified:

```
libvencoder.so  libvenc_base.so  libvenc_common.so
libvenc_h264.so libvenc_h265.so  libvenc_jpeg.so
libVE.so        libMemAdapter.so libcdc_base.so   libvideoengine.so
```

Download from **[trimui/firmware_smartpro releases](https://github.com/trimui/firmware_smartpro/releases)**,
release **v1.1.1** (2025-12-01). That release has two assets and only one is
usable:

| asset | size | |
|---|---|---|
| `trimui_tg5040_20251128_v1.1.1.zip` | 240 MB | ✅ unzip → `trimui_tg5040.awimg` (IMAGEWTY, contains the ext4 rootfs) |
| `sd_recovery_tg5040_smart_pro_v1.1.1_*.zip` | 2.0 GB | ❌ PhoenixCard burn image — bogus partition table, payload not mountable |

`scripts/extract-vendor-libs.sh` pulls the libraries out of the `.awimg`. These
are proprietary binaries and are deliberately not committed here.

The script hardcodes `ROOTFS_OFFSET=18095104` (`0x1141c00`), read from this
build's IMAGEWTY file table. A future firmware will move it; override with
`ROOTFS_OFFSET=<n> ./scripts/extract-vendor-libs.sh ...` after re-deriving it.

**Version matters.** Older CedarC builds refuse this silicon outright:

```
error: the driver do not support the ic 12011
```

That is a whitelist inside `libvenc_codec.so`, not a hardware limit. The blob
that fails self-identifies as `CedarC-v1.2.0, commit 15195bd, 2019-05-07` —
predating the H616/H618 entirely. The 2025 TrimUI build accepts it.

## How it works

```
/dev/fb0 ──pread──> heap buffer ──memcpy──> ION buffer ──> Cedar VE ──> H.264
   BGRA               (cached)              (IOMMU-mapped)   ISP does
   720x480                                                   RGB→YUV

snd-aloop capture side ───────────────────────────────────────────> .pcm
   the game's playback, paced by the kernel                    (s16le 48k)
```

Per frame: read the visible framebuffer page, copy it into the encoder's ION
input buffer, submit, drain the bitstream. The VE's ISP block does the colour
conversion on ingest, so the CPU never touches pixel values.

Double buffering matters: the panel has a 720x960 virtual framebuffer and
`yoffset` says which half is on screen. Reading offset 0 unconditionally — as
the reference implementation does — can capture the buffer being drawn into
rather than the one being displayed.

## Vendor ABI notes

Hard-won, and none of it is documented upstream.

### `VENC_IndexParamH264SPSPPS` is `0x101`, not 16

The H.264 parameters live in their own block starting at `0x100`
(`vencoder.h:765`), so the parameter-set index is `0x100 + 1`. Querying index
`16` returns an unrelated parameter whose value looks superficially plausible —
a pointer plus a frame-sized length — which is how the reference implementation
concluded SPS/PPS "lives in VE SRAM, not CPU-accessible" and resorted to
hardcoding parameter sets per resolution. At `0x101` the real sets come back in
ordinary CPU memory.

### Parameter sets arrive as `avcC`, frames as AVCC

`VideoEncGetParameter(0x101)` returns an AVCDecoderConfigurationRecord:

```
01 64 00 33 ff | 01 00 0a | 67 4d 00 29 96 54 05 a1 e8 80 | 01 00 04 | 68 ee 3c 80
^version       ^1 SPS,10B  ^SPS                            ^1 PPS,4B  ^PPS
```

and frame data is length-prefixed AVCC (`00 00 23 b9 65 88 …`). Both need
converting to Annex-B start codes or nothing decodes.

They must also be fetched **after** the first frame is encoded; before that the
call returns a pointer with `nLength = 0`.

### The encoder is Main profile with CABAC

Not baseline. A hand-generated baseline SPS decodes as garbage
(`cabac_init_idc overflow`, `QP out of range`) because the slice headers do not
match. Use the parameter sets the encoder gives you.

### Format names are 32-bit word order, not byte order

For a framebuffer whose bytes are B,G,R,A, the correct constant is
**`VENC_PIXEL_ARGB` (12)** — *not* `VENC_PIXEL_BGRA` (15), which produces a
blue-cast image. Determined by sweeping all four: 15 blue-cast, 13 red-cast, 14
close but wrong, 12 correct. Verified at **42.2 dB PSNR** against the CPU
conversion path on identical screen content (a channel swap scores ~10 dB).

### Vendor structs are written past their documented fields

Every struct passed to the library needs trailing padding. A bare 16-byte
`VencHeaderData` on the stack is what produced `*** stack smashing detected ***`
in the reference tool.

### Capability queries do not work

`VENC_IndexParamMAXSupportSize` (22) and `VENC_IndexParamCheckColorFormat` (23)
both return `h264 do not support this indexType` on this build, so supported
formats have to be probed by trying them. `tools/fmt-probe.c` attempts the
query and reports it unimplemented.

## Performance

Measured on-device, 30 s capture of live gameplay (GBA via `minarch.elf`), with
a 10 s no-capture baseline for comparison. Raw counters sampled at 4 Hz by
`tools/monitor.sh`; all arithmetic done off-device so sampling does not distort
the measurement.

| | baseline | during capture | delta |
|---|---|---|---|
| Total CPU | 23.8% (0.95 of 4 cores) | 27.2% (1.09 cores) | **+3.4 pp = +0.14 cores** |
| `rgsp-cast` | — | **14.2%** of one core (max 16.7%) | |
| `minarch.elf` (emulator) | 74.8% of one core | 75.9% | +1.1 pp — noise |
| CPU freq (mean) | 1142 MHz | 1290 MHz | +13% |
| GPU freq | 648 MHz pinned | 648 MHz pinned | unchanged |
| GPU power model | 47.0 mW | 51.4 mW | +9% |
| VE temp (max) | 59.5 °C | 61.1 °C | +1.6 °C |
| CPU temp (max) | 61.7 °C | 63.6 °C | +1.9 °C |

Per frame: **1.8 ms copy + 1.6 ms encode**. 900/900 frames at exactly 30.0 fps,
zero drops, ~2.1 Mbps on gameplay (≈450–510 kbps on static menus).

The emulator is unaffected — 74.8% → 75.9% of a core is within sample noise.
The +3.4 pp of system CPU is almost exactly `rgsp-cast`'s own 14.2% of one core
(14.2/4 = 3.6 pp), so the accounting closes with nothing hidden elsewhere.

The **GPU is not involved**: encoding runs on the VE, a separate block. GPU
frequency is useless as a load signal here — devfreq pins it at 648 MHz in all
conditions.

## Optimisations measured and rejected

Do not retry these without new information.

| attempt | result |
|---|---|
| **NEON colour conversion** | Unnecessary. The VE's ISP converts RGB→YUV on ingest, so the conversion stage is deleted rather than optimised: **18.28 ms → 1.55 ms** CPU per frame, an 11.8x reduction. Hand-written NEON would have optimised a stage that no longer exists. |
| **Zero-copy from framebuffer** | **Corrupt bitstream.** Prerequisites all exist — `smem_start=0xff800000`, `ve_addr_offset=0x0` — and it ran at 0.00 ms CPU, but output fails at `MB 25 0` with every frame a keyframe. The VE reaches memory through an IOMMU (`IOCTL_GET_IOMMU_ADDR`, `cedarv_iommu_buff`) that only maps ION allocations. Would need dmabuf export, which this fbdev driver lacks. |
| **mmap the fb to save one copy** | **13x worse: 19.90 ms vs 1.44 ms.** Framebuffer mappings are uncached, so each CPU read goes to DRAM individually. `pread`'s kernel-side bulk copy wins by a wide margin. The apparently wasteful pread+memcpy pair is the fast path. |

## Known issues

- **`CdcIonFree: free ion_handle err, ret -1 errno:22`** on teardown. The ION
  free ioctl differs between TrimUI's kernel and this 4.9 BSP. Allocation works;
  frees fail. Not a running leak — input buffers are allocated once, not per
  frame, and the kernel reclaims everything at process exit.
- **`iniparser: cannot open /etc/cedarc.conf`** — harmless. Drop a
  `cedarc.conf` there to silence it and to control the library's log level
  (`omx_log_level`, 2 = verbose … 6 = error only).
- **Deep sleep kills USB.** The SP kernel fully stops its USB controllers when
  suspending (BaseOS `docs/05` §2), so adb disappears mid-run. Use SSH over
  Wi-Fi for anything long.
- `tools/fmt-probe.c` reports the capability queries as unimplemented; that is
  the expected result on this build, not a failure.

## Audio

Capture is an ordinary ALSA capture device: `default` feeds the kernel loopback,
and `rgsp-cast` reads the other end.

```
default → snd-aloop  →  rgsp-cast reads the capture side
```

The kernel paces the loopback, so the timeline is continuous at 48 kHz and does
not depend on the codec's crystal. Stream parameters are **S16_LE, 2 ch,
48000 Hz**.

The stock kernel ships with `CONFIG_SND_ALOOP` unset, so the module is built
separately. `bin/snd-aloop.ko` matches the stock kernel exactly:

```sh
./scripts/build-snd-aloop.sh                 # -> bin/snd-aloop.ko
scp bin/snd-aloop.ko root@DEVICE:/tmp/
ssh root@DEVICE 'insmod /tmp/snd-aloop.ko && cat /proc/asound/cards'
```

It loads **without** `--force`, verified on the device:

```
insmod /tmp/snd-aloop.ko  ->  LOADED OK
 3 [Loopback       ]: Loopback - Loopback
```

Three checks predicted that, and are worth re-running against any future kernel:

| | built | stock |
|---|---|---|
| vermagic | `4.9.170 SMP preempt mod_unload modversions aarch64` | identical |
| `module_layout` CRC | `0x3491861c` | `0x3491861c` |
| shared symbol CRCs | 33 compared against stock modules | 0 mismatches |

The stock values come from the device itself. The kernel is built with
`CONFIG_IKCONFIG_PROC=y`, so it embeds its own gzipped config between `IKCFG_ST`
and `IKCFG_ED` markers in the disk image; that is now saved as
`reference/stock-kernel-4.9.170.config`, 4,209 lines of ground truth. The stock
modules were read out of the image directly, mounted read-only by partition
offset, and their CRCs compared.

Requirements the build has to satisfy, all encoded in the script:

- Build against the **`orange-pi-4.9-sun50iw9` BSP tree**. It is the tree whose
  `module_layout` matches; Allwinner patched the core headers relative to
  mainline.
- **Clone inside Linux.** The kernel tree contains filenames differing only by
  case (`ipt_ECN.h` vs `ipt_ecn.h`), which macOS's case-insensitive filesystem
  cannot hold.
- **`CONFIG_MOTORCOMM_PHY=y`.** OrangePi's BSP calls
  `yt8511_config_out_125m()` unconditionally from `phy_device.c`. It must be
  built in, not a module — the caller is built in.
- **Empty `.scmversion`.** `scripts/setlocalversion` otherwise appends `+` for
  an untagged git tree, giving `4.9.170+` and failing the exact vermagic match.

Also from that config: `CONFIG_MODULE_FORCE_LOAD=y` with no `CONFIG_MODULE_SIG`,
so module loading is not gated; and `CONFIG_SND_HRTIMER=y` with
`CONFIG_HIGH_RES_TIMERS=y`, the pacing infrastructure the loopback uses.

### Reading the capture side

**Start the capture stream explicitly.** `snd_pcm_readi()` on a loopback capture
device fails with `-EIO` if it has to start the stream implicitly; call
`snd_pcm_prepare()` and `snd_pcm_start()` first and it works. `tools/alsa-cap.c`
is the reference implementation, and the device ships no `arecord`.

Capture parameters must match what the playback side negotiated — snd-aloop
fails the capture side with `-EIO` when the two ends of a cable disagree on
format, rate or channels (`aloop.c`, `loopback_check_format`). The emulator
plays **S16_LE, 2 ch, 48000 Hz**, period 512, buffer 1024.

With no playback open, capture yields silence rather than an error, so a
recorder can be started before the game.

Measured on a live game, three seconds:

```
144384 frames captured (144000 requested), 287662 non-zero samples
peak -6.3 dB, RMS -19.1 dB, dynamic range 90 dB
```

### Routing

Casting is the only output — audio goes to the TV, not the handheld speaker.

NextUI switches audio routing at the ALSA config layer.
`/usr/share/alsa/alsa.conf` loads config in this order:

```
"/etc/asound.conf|||/usr/etc/asound.conf"
"~/.asoundrc"                                 <- routing lives here
```

Paks set `HOME=$USERDATA_PATH`, and the last definition of `pcm.!default` wins,
so `$USERDATA_PATH/.asoundrc` is where the sink is chosen. `audiomon.elf`
(`workspace/all/audiomon/audiomon.cpp`) writes it on BlueZ D-Bus connect and on
udev USB-audio events; `msettings.c:1003` `setHDMIAudioRoute()` handles HDMI
hotplug. Selecting the loopback while casting, and restoring afterwards, is the
same mechanism those already use.

`GetAudioSink()` in libmsettings reports which route is live
(`AUDIO_SINK_DEFAULT` / `_BLUETOOTH` / `_USBDAC`).

Bluetooth playback lags 100–200 ms behind, so it must never be the reference
when calibrating A/V sync.

### Sync

`rgsp-cast` drains audio once per video frame. Measured end-to-end difference
between the audio and video durations, across runs: **+2, +5, -6 ms**. It does
not grow with duration (same magnitude at 10 s and 30 s), so the video loop's
`CLOCK_MONOTONIC` pacing and the 48 kHz audio clock are locked — a 0.05%
mismatch would show 15 ms of drift over 30 s. Both streams start at `0.000000`.
Residual uncertainty is a constant offset bounded by roughly one ALSA period
(10.7 ms), a third of a video frame.

Caveats:

- ALSA config is read when a client **opens** the PCM, so a change takes effect
  on the next game launch.
- Muxing needs explicit `-map 0:v -map 1:a`; ffmpeg's automatic stream selection
  silently drops the raw PCM input.
- A paused emulator still writes zeros to the PCM, so a recording made while
  paused contains digital silence (-91 dB), not an error.

## Layout

```
src/rgsp-cast.c                  the capture tool
src/rgsp-audio-pump.c            ALSA-spawned audio bridge (pipe -> Unix socket)
etc/asound.conf.tee              ALSA config with the pipe-mode capture tap
scripts/extract-vendor-libs.sh   pull CedarC libs from TrimUI firmware
scripts/install-audio-tee.sh     install / remove the audio tee on the device
scripts/build-snd-aloop.sh       build snd-aloop.ko for the stock kernel
bin/snd-aloop.ko                 the built module (vermagic + CRCs match stock)
reference/stock-kernel-4.9.170.config
                                 the device's own kernel config, from IKCFG_ST
tools/alsa-cap.c                 ALSA capture -> raw s16le (the device has no arecord)
tools/fmt-probe.c                queries encoder capabilities (unimplemented on this build)
tools/monitor.sh                 raw CPU/GPU/thermal sampler
Makefile                         build / deploy / run / monitor
```

## Credits

- [carroarmato0/allwinner-cedar-tools](https://github.com/carroarmato0/allwinner-cedar-tools)
  — the reverse-engineered Cedar ABI this builds on: struct layouts, buffer
  lifecycle, and the H618 encode path. Its companion `Cast` project (live
  streaming) is referenced in its docs but is not public.
- [CalvinXu17/libcedarc](https://github.com/CalvinXu17/libcedarc) and
  [aodzip/libcedarc](https://github.com/aodzip/libcedarc) — open CedarC sources
  and `vencoder.h`, which is where the parameter index block is documented.
- [trimui/firmware_smartpro](https://github.com/trimui/firmware_smartpro) —
  source of the vendor libraries.
