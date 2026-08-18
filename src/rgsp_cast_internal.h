/* Internal entry points shared between librgspcast and the bundled CLI.
 *
 * These are deliberately *not* in include/rgsp_cast.h: they exist only so the
 * CLI can keep its long-standing debugging flags (-i for the NV12 reference
 * path, -S for byte strides, --dump-hdr, -v). The public API stays the small
 * surface the streaming daemon needs.
 */
#ifndef RGSP_CAST_INTERNAL_H
#define RGSP_CAST_INTERNAL_H

#include "../include/rgsp_cast.h"

/* Same as rgsp_capture_open(), plus the two knobs the CLI exposes:
 *   in_fmt        VENC_PIXEL_* — 12 (ARGB passthrough, default) or 0 (NV12)
 *   stride_bytes  non-zero: pass the fb pitch in bytes rather than pixels */
rgsp_capture *rgsp_capture_open_ex(int width, int height, int fps, int bitrate,
                                   int in_fmt, int stride_bytes);

/* As above, plus VE-side scaling: dst_w/dst_h of 0 mean "same as source". */
rgsp_capture *rgsp_capture_open_scaled_ex(int width, int height,
                                          int dst_w, int dst_h,
                                          int fps, int bitrate,
                                          int in_fmt, int stride_bytes);

void rgsp_capture_set_verbose(int v);

/* The Annex-B SPS+PPS captured after the first encoded frame, or NULL. */
const unsigned char *rgsp_capture_param_sets(rgsp_capture *c, size_t *len);

/* Cumulative per-frame timings, for the CLI's end-of-run summary. */
void rgsp_capture_stats(rgsp_capture *c, long long *convert_ns,
                        long long *encode_ns, int *short_reads);

#endif
