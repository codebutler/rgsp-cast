Copied from https://github.com/pvaibhav/NextUI (the h700 port fork of
LoveRetro/NextUI) at 39745aeefbc4993dbb4352065fe100a8f6faf1f7 (tag h700-rc8,
2026-08-09), paths workspace/all/common, workspace/h700/platform and
workspace/h700/libmsettings.

GPLv3 — `LICENSE` at that exact SHA is the full 621-line GPL-3.0 text. Linking
these into rgsp-ui makes the pak a derivative work. Unmodified — never edit in
place.

## Do NOT update to a newer NextUI ref without a licence review

Upstream relicensed away from GPLv3 in
`ae652648548edf6ab24cbb816cf4e4194e609fb3` — "chore: License Transition —
GPL 3.0 → PolyForm Noncommercial 1.0.0" (PR #806, 2026-08-15). That commit
replaces the 621-line GPLv3 text with PolyForm Noncommercial 1.0.0 and adds a
CONTRIBUTING.md placing contributions under it.

**PolyForm Noncommercial is not an open-source licence, it forbids commercial
use, and it cannot be combined with GPLv3.** The snapshot vendored here
predates that change and is GPLv3; any newer ref is not. Re-copying from one
would change what this pak may legally be, so a newer ref is not a drop-in
replacement — treat an update as a licensing decision, not routine maintenance.

Verified: our pin is dated 2026-08-09, the relicense 2026-08-15, and
`git merge-base --is-ancestor ae65264 39745ae` reports the relicense is **not**
an ancestor of our pin. A GPLv3 grant already made on a released version cannot
be retroactively withdrawn, so this snapshot stays GPLv3.

## Files beyond the minimal set, and why each is here

`sdl.h` — `api.h:3` includes it.

`displaycal.c` / `displaycal.h` — `msettings.c:15` includes the header, and
NextUI's own `h700/libmsettings/makefile` compiles `displaycal.c` into the same
library, so it must be compiled alongside `msettings.c`.

`generic_video.c`, `generic_wifi.c`, `generic_bt.c`, `led.c` — `platform.c`
textually `#include`s all four (lines 1057-1073). They must be present but must
NOT be listed as separate translation units; compiling them again would be a
duplicate-symbol error.
