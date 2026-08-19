# Golden fixtures for the Cedar-VE bitstream conversion

These fixtures were captured off a real Anbernic RG SP, from the pre-port C
capture library (`src/rgsp-cast.c`), which was temporarily instrumented with
an `RGSP_DUMP_FIXTURES` env var to dump, per frame, both the vendor's raw
output and the C's converted output. That instrumentation is still present
in the working tree at the time of this commit; it is deleted in a later,
unrelated commit along with the rest of the C file, once the Rust port lands.

**These are the C's own output, not independently derived ground truth.**
They are a *characterization* baseline: they lock in the current, working
behaviour of the C conversion so the Rust port can be checked byte-for-byte
against it. If the C had a latent bug, these fixtures reproduce that bug
faithfully. Correctness was validated separately by streaming to real
clients (see `CLAUDE.md`), not by these files.

## What's here

- `avcc_record.bin` — the raw `AVCDecoderConfigurationRecord` returned by
  `VideoEncGetParameter(0x101)` on stream start. 25 bytes. Contains the
  SPS/PPS parameter sets in AVCC form (2-byte length-prefixed).

- `frame_key_raw.bin` / `frame_key_expected.bin` — the first frame of a
  plain-run capture (`fx1`, frame 0). `_raw` is exactly what the vendor's
  `GetOneBitstreamFrame` returned: AVCC, with 4-byte big-endian length
  prefixes instead of Annex-B start codes. `_expected` is exactly what the
  C produced from it: Annex-B (start codes `00 00 00 01`), with the 22-byte
  SPS/PPS parameter sets (derived from `avcc_record.bin`) prepended, because
  this is a keyframe.

- `frame_delta_raw.bin` / `frame_delta_expected.bin` — the next frame in the
  same run (`fx1`, frame 1). A P-frame; no parameter sets prepended, so
  `_expected` is simply `_raw` with length prefixes swapped for start codes.

- `frame_forced_idr_raw.bin` / `frame_forced_idr_expected.bin` — frame 137 of
  a 168-frame run (`fx2`) in which a client-forced IDR was injected mid-stream.

- `frame_forced_idr2_raw.bin` / `frame_forced_idr2_expected.bin` — frame 167
  of the same `fx2` run, a second client-forced IDR.

## Why the forced-IDR cases matter

When the client requests an IDR mid-stream (e.g. on a Moonlight client
reconnect or a decoder resync), the Cedar VE emits it as a bare type-5 NAL
with no parameter sets attached — only the very first keyframe of a stream
carries them in the vendor's raw output. A software H.264 decoder doesn't
care: it already has SPS/PPS cached from the first keyframe and just resets
its reference state. But VideoToolbox — the decoder path used by every Apple
client (macOS Moonlight with `--video-decoder hardware`, iOS, tvOS) — will
not resume decoding on a keyframe that lacks parameter sets. It sits at
"Waiting for IDR frame" forever, streaming garbage or nothing until the
session gives up.

That's why the C prepends the 22-byte parameter set block ahead of *every*
keyframe it emits, forced or not, not just the stream's first one.
`frame_forced_idr*` are exactly this case, captured from the device: the
raw vendor output has no parameter sets, and the expected (converted)
output does.

## Byte layout notes (for whoever writes the Rust conversion)

`avcc_record.bin`, converted to the 22-byte Annex-B parameter-set block, is
laid out as: 5 header bytes, then `numSPS & 0x1f`, then per SPS a 2-byte
big-endian length + payload, then `numPPS`, then per PPS a 2-byte big-endian
length + payload. Each parameter set is emitted as `00 00 00 01` + payload.

Frame raw-to-Annex-B conversion: each 4-byte big-endian length prefix in the
vendor's AVCC output is replaced by a `00 00 00 01` start code; NAL payloads
are otherwise passed through unchanged.
