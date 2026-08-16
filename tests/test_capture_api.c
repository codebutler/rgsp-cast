/* Exercises the library API end to end on the device: open, pull frames,
 * force an IDR, close. Frame 0 must be a keyframe because the encoder emits
 * SPS/PPS + IDR first. */
#include "../include/rgsp_cast.h"
#include <stdio.h>
#include <string.h>
#include <assert.h>

int main(void)
{
    rgsp_capture *c = rgsp_capture_open(720, 480, 30, 2000000);
    if (!c) { fprintf(stderr, "open failed: %s\n", rgsp_capture_last_error()); return 1; }

    const unsigned char *data; size_t len; int key;

    if (rgsp_capture_next(c, &data, &len, &key) != 0) {
        fprintf(stderr, "first frame failed: %s\n", rgsp_capture_last_error());
        return 1;
    }
    assert(len > 4);
    /* Annex-B start code */
    assert(data[0] == 0 && data[1] == 0 && data[2] == 0 && data[3] == 1);
    assert(key == 1);
    printf("frame0: %zu bytes, keyframe=%d\n", len, key);

    int keyframes = 0;
    for (int i = 0; i < 60; i++) {
        if (rgsp_capture_next(c, &data, &len, &key) != 0) {
            fprintf(stderr, "frame %d failed: %s\n", i, rgsp_capture_last_error());
            return 1;
        }
        keyframes += key;
    }
    printf("60 frames, %d keyframes\n", keyframes);

    rgsp_capture_request_idr(c);
    if (rgsp_capture_next(c, &data, &len, &key) != 0) return 1;
    assert(key == 1);
    printf("forced IDR ok\n");

    rgsp_capture_close(c);
    printf("PASS\n");
    return 0;
}
