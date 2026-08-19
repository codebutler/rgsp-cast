Copied from https://github.com/tinyalsa/tinyalsa at
df11091086b56e5fb71887f2fa320e1d2ffeff58 (tag 1.1.1), paths src/ and
include/tinyalsa/.

BSD 3-clause (Copyright 2011, The Android Open Source Project). Upstream ships
the licence text as NOTICE, not LICENSE; it is copied here verbatim.
Unmodified — update by re-copying from a newer tag, never by editing in place.

## Why this is here

NextUI's `h700/libmsettings/msettings.c` includes `<tinyalsa/mixer.h>` and calls
thirteen `mixer_*` functions; NextUI's own libmsettings makefile links
`-ltinyalsa`. Debian bookworm has no tinyalsa package at all
(`apt-cache search tinyalsa` is empty), so there is nothing for the build
container to link against. Compiling the mixer here keeps the build
self-contained and reproducible from a fresh clone, with no device on the
network and no unpinned binary.

## Why tag 1.1.1 and not v2.0.0

NextUI does not pin a tinyalsa ref in its own tree — it takes whatever its
toolchain image staged, and `workspace/h700/makefile:39` just copies
`$(PREFIX)/lib/libtinyalsa.so*`. The device is the evidence of what that was:
it ships `libtinyalsa.so.1.1.1`, so 1.1.1 is the version NextUI built h700
against. (The `libtinyalsa.so.2` in `workspace/all/minarch/makefile:111` is
inside an `ifeq ($(PLATFORM), my355)` guard and does not apply to h700.)

Matching it keeps our mixer's view of the kernel control interface identical to
the one the rest of the system uses. 1.1.1 is also the smaller dependency: its
`src/mixer.c` includes only `<tinyalsa/mixer.h>`, whereas v2.0.0 splits the
mixer across `mixer_hw.c` / `mixer_plugin.c` and pulls in `plugin.h`.

## Only the mixer is vendored

`build.rs` compiles `src/mixer.c`, and that is the whole of it — it defines all
thirteen symbols `msettings.c` needs. The PCM sources, the utils and the
examples are deliberately not copied; nothing in rgsp-ui plays audio through
tinyalsa.
