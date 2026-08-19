Copied from LoveRetro/NextUI at 39745aeefbc4993dbb4352065fe100a8f6faf1f7
(tag h700-rc8, 2026-08-09), paths workspace/all/common, workspace/h700/platform
and workspace/h700/libmsettings.

GPLv3. Linking these into rgsp-ui makes the pak a derivative work; see LICENSE.
Unmodified — update by re-copying from a newer tag, never by editing in place.

## Files beyond the minimal set, and why each is here

`sdl.h` — `api.h:3` includes it.

`displaycal.c` / `displaycal.h` — `msettings.c:15` includes the header, and
NextUI's own `h700/libmsettings/makefile` compiles `displaycal.c` into the same
library, so it must be compiled alongside `msettings.c`.

`generic_video.c`, `generic_wifi.c`, `generic_bt.c`, `led.c` — `platform.c`
textually `#include`s all four (lines 1057-1073). They must be present but must
NOT be listed as separate translation units; compiling them again would be a
duplicate-symbol error.
