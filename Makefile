# rgsp-cast — build for the RG SP (aarch64 glibc).
#
# The device runs a glibc-2.35 Ubuntu-derived rootfs and the vendor Cedar libs
# are glibc-2.33 builds, so an arm64 ubuntu:22.04 container matches both. The
# vendor libraries are dlopen'd at runtime — nothing proprietary is linked, and
# none of it is needed to compile.

DEVICE  ?= root@192.168.180.106
DESTDIR ?= /tmp/venc
LIBDIR  ?= $(DESTDIR)/lib-trimui
IMAGE   ?= ubuntu:22.04
CFLAGS  ?= -O2 -Wall -Wextra

.PHONY: all clean deploy run monitor

all: bin/snd-aloop.ko

# The standalone capture-only binary is now the Rust CLI at
# rgsp-cedar/src/bin/rgsp-cast.rs, cross-compiled the same way `pak` builds
# rgsp-host. It has no dependency on audiopus_sys, so no cache-busting clean
# is needed here.
deploy:
	docker run --rm --platform linux/arm64 -v "$(CURDIR)":/w -w /w rust:1-bookworm \
		sh -c 'apt-get update -qq && apt-get install -y -qq cmake clang libasound2-dev pkg-config >/dev/null 2>&1 && \
		       cargo build --release -p rgsp-cedar --bin rgsp-cast'
	ssh $(DEVICE) 'mkdir -p $(DESTDIR)'
	scp -q target/release/rgsp-cast scripts/monitor.sh $(DEVICE):$(DESTDIR)/

# make run DURATION=30 OUT=session.h264
DURATION ?= 30
OUT      ?= cast.h264
run: deploy
	ssh $(DEVICE) 'cd $(DESTDIR) && LD_LIBRARY_PATH=$(LIBDIR) \
		./rgsp-cast -o $(OUT) -d $(DURATION) -f 30'
	scp -q $(DEVICE):$(DESTDIR)/$(OUT) .
	@scp -q $(DEVICE):$(DESTDIR)/$(OUT).pcm . 2>/dev/null || true
	@# Video is stream-copied either way; audio (if the ALSA tee is installed)
	@# is raw s16le/48k/stereo and gets encoded to AAC. Explicit -map is
	@# required — ffmpeg's automatic stream selection drops the raw input.
	@if [ -f $(OUT).pcm ]; then \
		echo "muxing video + audio"; \
		ffmpeg -v error -y -r 30 -i $(OUT) -f s16le -ar 48000 -ac 2 -i $(OUT).pcm \
			-map 0:v -map 1:a -c:v copy -c:a aac -b:a 128k \
			-movflags +faststart $(basename $(OUT)).mp4; \
	else \
		echo "no audio track (install the ALSA tee for sound)"; \
		ffmpeg -v error -y -r 30 -i $(OUT) -c:v copy \
			-movflags +faststart $(basename $(OUT)).mp4; \
	fi
	@echo "-> $(basename $(OUT)).mp4"

# Sample CPU/GPU/thermals for SECS while something else runs.
SECS ?= 33
monitor: deploy
	ssh $(DEVICE) 'cd $(DESTDIR) && sh monitor.sh $(SECS) $(DESTDIR)/mon.log'
	scp -q $(DEVICE):$(DESTDIR)/mon.log .

PAKDIR = dist/Tools/h700/Cast.pak

# Built out-of-tree against the BSP kernel, not by this Makefile: it needs an
# arm64 container and a 4.9 source tree, and the result is committed-adjacent
# but gitignored. Fail with a useful pointer rather than make's "No rule to
# make target".
bin/snd-aloop.ko:
	@echo "bin/snd-aloop.ko is missing - build it once with:" >&2
	@echo "    ./scripts/build-snd-aloop.sh" >&2
	@false

# NOTE: no libopus-dev here. With it present, audiopus_sys links Opus
# dynamically against the container's libopus.so.0 - the device has no
# libopus, so the binary dies at startup with "cannot open shared object
# file: libopus.so.0". Without libopus-dev, audiopus_sys can't find it via
# pkg-config and instead builds Opus from source (needs cmake) and links it
# statically, which is what the device needs.
#
# The `cargo clean --release -p audiopus_sys` is required even so: a target/release
# populated by an earlier build *with* libopus-dev leaves cached artifacts
# that were compiled against the dynamic lib. Once libopus-dev is absent,
# reusing that cache fails to link ("cannot find -lopus") until audiopus_sys
# is rebuilt from scratch. Forcing just that one crate is far cheaper than
# wiping all of target/release (moonshine-core, tokio, etc. stay cached).
# Run the Rust suite in the same arm64 container the pak is built in.
#
# The `cargo clean -p audiopus_sys` matters here for the same reason it does in
# `pak`, but for the *debug* profile: `cargo clean --release -p ...` below only
# touches target/release, so a target/debug populated by an earlier build that
# had libopus-dev present still carries the dynamic-opus link and every test
# binary fails with "cannot find -lopus". Deliberately no libopus-dev in the
# package list, so the tests link the way the shipped binary does.
.PHONY: test-rust
test-rust:
	docker run --rm --platform linux/arm64 -v "$(CURDIR)":/w -w /w rust:1-bookworm \
		sh -c 'apt-get update -qq && apt-get install -y -qq cmake clang libasound2-dev libsdl2-dev libsdl2-image-dev libsdl2-ttf-dev libsamplerate0-dev libgles-dev libclang-dev pkg-config >/dev/null 2>&1 && rustup -q component add clippy && \
		       cargo clean -p audiopus_sys && cargo test --workspace && \
		       cargo clippy --workspace --all-targets'

.PHONY: pak
pak: bin/snd-aloop.ko
	@mkdir -p $(PAKDIR)/lib/h700
	docker run --rm --platform linux/arm64 -v "$(CURDIR)":/w -w /w rust:1-bookworm \
		sh -c 'apt-get update -qq && apt-get install -y -qq cmake clang libasound2-dev libsdl2-dev libsdl2-image-dev libsdl2-ttf-dev libsamplerate0-dev libgles-dev libclang-dev pkg-config >/dev/null 2>&1 && \
		       cargo clean --release -p audiopus_sys && \
		       cargo build --workspace --release'
	cp target/release/rgsp-host target/release/rgsp-ui $(PAKDIR)/
	cp pak/launch.sh pak/pak.json $(PAKDIR)/
	cp bin/snd-aloop.ko $(PAKDIR)/
	cp -r pak/hooks $(PAKDIR)/
	@# macOS drops .DS_Store into any directory Finder has looked at, and
	@# `cp -r` carries them into the pak and then onto the device. They are
	@# junk on a handheld, and they make verify-pak.sh's manifest disagree
	@# with what install-pak.sh actually copies.
	find $(PAKDIR) -name '.DS_Store' -delete
	chmod +x $(PAKDIR)/launch.sh $(PAKDIR)/rgsp-host $(PAKDIR)/rgsp-ui $(PAKDIR)/hooks/*/*.sh
	@echo "-> $(PAKDIR)"
	@echo "   lib/h700 is populated on the device at install time"

clean:
	rm -rf bin
