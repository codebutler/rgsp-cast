# Working in this repo

A GameStream host for the Anbernic RG SP. `README.md` describes what it does;
this file is about how to work on it without breaking things.

## The device

```
ssh root@192.168.180.106        # password: root
```

Everything ships to `/mnt/SDCARD/Tools/h700/Cast.pak/`. The daemon writes
`/tmp/rgsp/daemon.log` and `/tmp/rgsp/daemon.pid`.

The device sleeps and drops off the network. If SSH times out, it is asleep —
ask for it to be woken rather than assuming a fault.

## Building

Everything cross-compiles in an arm64 container; there is no toolchain on the
device and none on the host.

```sh
make librgspcast.a          # the C capture library
make pak                    # the full pak, ready to install
make test-rust              # the Rust suite, in the container
./tests/test_launch_sh.sh   # pak toggle behaviour
./tests/test_hooks.sh       # NextUI hook behaviour
```

**`libopus-dev` must not be installed in the build container.** With it,
`audiopus_sys` links Opus dynamically; the device has no libopus and the binary
dies at startup. Without it, Opus is built from source and linked statically.
The correct package set is `cmake clang libasound2-dev pkg-config`. This
choice is cached in `target/`, so switching requires
`cargo clean -p audiopus_sys` — `make pak` and `make test-rust` both do it.

**After changing any C in `src/`, rebuild `librgspcast.a` before building
`rgsp-host`.** Cargo links the archive but does not know how to rebuild it, so
a Rust-only build silently ships the previous C code.

## Deploying

The daemon runs as `./rgsp-host` with a relative path, so `pkill -f
Cast.pak/rgsp-host` does not match it. Kill it by PID:

```sh
P=$(ssh root@DEVICE 'ps | grep "[r]gsp-host" | awk "{print \$1}"')
ssh root@DEVICE "kill -9 $P"
```

An `scp` over a running binary fails with `dest open ... Failure`, so stop it
first. **Verify what is actually on the device after deploying** — compare
`md5 -q target/release/rgsp-host` against `md5sum` on the device. Testing a
stale binary produces conclusions that are entirely wrong.

Start it through `launch.sh`, never directly: it exports `LD_LIBRARY_PATH` for
the vendor CedarC libs, and without it `dlopen(libVE.so)` fails and capture
never opens. `launch.sh` is a toggle — running it twice stops the daemon.

## Testing against a client

Do not iterate through the TV; it yields one bit of information per attempt.
Use `moonlight-qt` on the Mac, which logs the client's own reasoning:

```sh
/Applications/Moonlight.app/Contents/MacOS/Moonlight pair 192.168.180.106 --pin 1234
ssh root@DEVICE 'wget -qO- --post-data="uniqueid=0123456789ABCDEF&pin=1234" \
  --header="Content-Type: application/x-www-form-urlencoded" \
  "http://127.0.0.1:47989/submit-pin"'

/Applications/Moonlight.app/Contents/MacOS/Moonlight stream 192.168.180.106 "RG SP" \
  --720 --fps 60 --bitrate 5000 --display-mode windowed --video-decoder hardware
```

Its log lands in `/tmp/Moonlight-*.log`. `Waiting for IDR frame`, `Reached
consecutive drop limit` and `Received first audio/video packet` are the lines
that matter.

`--video-decoder hardware` matters: it uses VideoToolbox, like every Apple
client, and is far stricter than the software decoder. A stream can work with
`software` and fail on an Apple TV.

**Kill the client when finished** (`pkill -9 -f Moonlight`, which the `stream`
child alone does not satisfy). The host serves one session and a lingering
client silently holds it, which looks exactly like a broken host to whoever
tries next.

## Diagnostics

All at debug level:

```sh
RUST_LOG=rgsp_host=debug,moonshine_core::session::stream=debug
```

- `latency: encode N ms, queue wait N ms` — host-side budget. Encode includes
  the frame-pacing sleep, so ~17 ms at 60 fps is idle, not work.
- `audio: N periods captured, peak amplitude N` — peak 0 means silence is being
  captured; non-zero means the fault is downstream.
- `audio encoder behind: N PCM frame(s) dropped` — audible as crackle.

## Things that will mislead you

- **`pgrep -f <pattern>` matches your own SSH command line.** Use `ps | grep
  "[r]gsp-host"`.
- **`find` with escaped parens through SSH** errors into `/dev/null` and looks
  like "no results". Verify a negative result before believing it.
- **BSD `sed -i` needs an explicit backup suffix** (`sed -i ''`). Without one it
  fails and the file is untouched, so a fault injection silently does nothing
  and the test "passes".
- **`.asoundrc` only affects processes that open ALSA afterwards.** A game
  already running when casting starts is not routed.
- **snd-aloop rejects the implicit start** that `readi()` would do; capture
  needs an explicit `prepare()` + `start()`.

## The vendored tree

`vendor/moonshine/` is a `git subtree`. Keep new logic in the `host_source.rs`
files on each side so pulls stay mechanical. Where upstream files are edited,
the reason is in a comment at the edit.

Upstream assumptions that no longer hold here are worth checking before trusting
them: it was written for a Vulkan encoder on a desktop, and this device has
neither the throughput nor the codec support that implies.

## Reference sources

Not vendored, but the authority when behaviour is unclear. Clone and read them
rather than inferring:

- `libcedarc` — the real `vencoder.h`: parameter indices, struct layouts
- `moonlight-common-c` — what the client actually requires
- `moonlight-ios` — the Apple decoder path, shared by tvOS

Reading is fine; copying is not.
