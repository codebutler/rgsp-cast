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

/* Capture the panel but encode at dst_w x dst_h, scaled by the VE.
 * GameStream clients reject a stream whose resolution is not the one they
 * negotiated, so the host encodes at the negotiated size rather than the
 * panel's. dst_w/dst_h of 0 mean "no scaling". */
rgsp_capture *rgsp_capture_open_scaled(int width, int height,
                                       int dst_w, int dst_h,
                                       int fps, int bitrate);

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

void rgsp_capture_close(rgsp_capture *c);

const char *rgsp_capture_last_error(void);

#ifdef __cplusplus
}
#endif

#endif
