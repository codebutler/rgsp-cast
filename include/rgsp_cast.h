/* Cedar VE screen capture as a library.
 *
 * Frames come back as Annex-B H.264 (Main profile, level 4.1, CABAC).
 * The buffer belongs to the capture object and stays valid until the next
 * rgsp_capture_next() call.
 */
#ifndef RGSP_CAST_H
#define RGSP_CAST_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct rgsp_capture rgsp_capture;

/* Opens /dev/fb0 and the Cedar encoder. Returns NULL on failure;
 * call rgsp_capture_last_error() for detail. */
rgsp_capture *rgsp_capture_open(int width, int height, int fps, int bitrate);

/* Blocks until the next frame is due, captures, encodes, and returns the
 * Annex-B bitstream. Returns 0 on success, -1 on failure.
 * The first frame is always a keyframe (SPS + PPS + IDR).
 *
 * A failure is terminal: the capture object is dead and every later call
 * returns -1 with the original error. Do not retry — only rgsp_capture_close()
 * may follow. (A failed frame can leave the encoder's input buffer submitted
 * or unacquired, so driving it again would work on inconsistent state.) */
int rgsp_capture_next(rgsp_capture *c, const unsigned char **data,
                      size_t *len, int *is_keyframe);

/* Makes the next frame an IDR. Moonlight asks for this after packet loss. */
void rgsp_capture_request_idr(rgsp_capture *c);

/* Composites a small marker into captured frames so the receiving client can
 * see the stream is live. Does not touch the device's own display. Off by
 * default - the daemon turns it on explicitly.
 *
 * Safe to call from a different thread than rgsp_capture_next(): the flag is
 * a plain int, the same pattern already used by rgsp_capture_request_idr /
 * force_idr. A torn read costs at most one frame drawn with the stale
 * setting, which is the same tolerance that pattern already accepts. */
void rgsp_capture_set_overlay(rgsp_capture *c, int enabled);

void rgsp_capture_close(rgsp_capture *c);

const char *rgsp_capture_last_error(void);

#ifdef __cplusplus
}
#endif

#endif
