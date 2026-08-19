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

BINS = bin/rgsp-cast

.PHONY: all clean deploy run monitor

all: $(BINS)

librgspcast.a: src/rgsp-cast.c src/rgsp_cast_internal.h include/rgsp_cast.h
	docker run --rm --platform linux/arm64 -v "$(CURDIR)":/w -w /w $(IMAGE) \
		sh -c 'apt-get update -qq && apt-get install -y -qq gcc binutils >/dev/null 2>&1 && \
		       gcc $(CFLAGS) -c -o /tmp/rgsp-cast.o src/rgsp-cast.c && \
		       ar rcs $@ /tmp/rgsp-cast.o'

bin/rgsp-cast: src/rgsp-cast-cli.c librgspcast.a
	@mkdir -p bin
	docker run --rm --platform linux/arm64 -v "$(CURDIR)":/w -w /w $(IMAGE) \
		sh -c 'apt-get update -qq && apt-get install -y -qq gcc >/dev/null 2>&1 && \
		       gcc $(CFLAGS) -o $@ $< librgspcast.a -ldl'

bin/test-capture-api: tests/test_capture_api.c librgspcast.a
	@mkdir -p bin
	docker run --rm --platform linux/arm64 -v "$(CURDIR)":/w -w /w $(IMAGE) \
		sh -c 'apt-get update -qq && apt-get install -y -qq gcc >/dev/null 2>&1 && \
		       gcc $(CFLAGS) -o $@ $< librgspcast.a -ldl'

# Includes src/rgsp-cast.c directly to reach the vendor struct definitions and
# the dlsym'd pointers, so it does not link the archive.
bin/test-vendor-overspill: tests/test_vendor_overspill.c src/rgsp-cast.c
	@mkdir -p bin
	docker run --rm --platform linux/arm64 -v "$(CURDIR)":/w -w /w $(IMAGE) \
		sh -c 'apt-get update -qq && apt-get install -y -qq gcc >/dev/null 2>&1 && \
		       gcc $(CFLAGS) -o $@ $< -ldl'

bin/test-reopen-leak: tests/test_reopen_leak.c librgspcast.a
	@mkdir -p bin
	docker run --rm --platform linux/arm64 -v "$(CURDIR)":/w -w /w $(IMAGE) \
		sh -c 'apt-get update -qq && apt-get install -y -qq gcc >/dev/null 2>&1 && \
		       gcc $(CFLAGS) -o $@ $< librgspcast.a -ldl'

bin/test-idr-cadence: tests/test_idr_cadence.c librgspcast.a
	@mkdir -p bin
	docker run --rm --platform linux/arm64 -v "$(CURDIR)":/w -w /w $(IMAGE) \
		sh -c 'apt-get update -qq && apt-get install -y -qq gcc >/dev/null 2>&1 && \
		       gcc $(CFLAGS) -o $@ $< librgspcast.a -ldl'

deploy: $(BINS)
	ssh $(DEVICE) 'mkdir -p $(DESTDIR)'
	scp -q $(BINS) scripts/monitor.sh $(DEVICE):$(DESTDIR)/

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
		sh -c 'apt-get update -qq && apt-get install -y -qq cmake clang libasound2-dev pkg-config >/dev/null 2>&1 && \
		       cargo clean -p audiopus_sys && cargo test --workspace'

.PHONY: pak
pak: librgspcast.a bin/snd-aloop.ko
	@mkdir -p $(PAKDIR)/lib/h700
	docker run --rm --platform linux/arm64 -v "$(CURDIR)":/w -w /w rust:1-bookworm \
		sh -c 'apt-get update -qq && apt-get install -y -qq cmake clang libasound2-dev pkg-config >/dev/null 2>&1 && \
		       cargo clean --release -p audiopus_sys && \
		       cargo build --workspace --release'
	cp target/release/rgsp-host $(PAKDIR)/
	cp pak/launch.sh pak/pak.json pak/cast.png $(PAKDIR)/
	cp bin/snd-aloop.ko $(PAKDIR)/
	cp -r pak/hooks $(PAKDIR)/
	chmod +x $(PAKDIR)/launch.sh $(PAKDIR)/rgsp-host $(PAKDIR)/hooks/*/*.sh
	@echo "-> $(PAKDIR)"
	@echo "   lib/h700 is populated on the device at install time"

clean:
	rm -rf bin librgspcast.a
