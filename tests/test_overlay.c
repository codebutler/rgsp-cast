/* On-device smoke test for rgsp_capture_set_overlay(). Confirms the overlay
 * path runs end-to-end against the real encoder and ION buffers - something
 * tests/test_draw_marker.c cannot do, since that one runs on the host
 * against a plain synthetic buffer with no device involved.
 *
 * What this test CAN show: the library doesn't crash or fail with the
 * overlay on, and the encoded output measurably changes when the overlay is
 * turned on (`with != without`), which a static square painted into every
 * captured frame should reliably do.
 *
 * What this test CANNOT show: that the marker is the right color, the right
 * size, in the right place, or that surrounding pixels are undisturbed - an
 * H.264 byte count changing is consistent with a correct 16x16 red square,
 * but also with almost any other change to the frame. Pixel-level
 * correctness is what tests/test_draw_marker.c proves, on the host, by
 * reading the ION-equivalent buffer directly before it goes to the encoder.
 * This test is only a wiring check that draw_marker() is actually being
 * called from rgsp_capture_next() and that overlay=1 reaches the real
 * device's encoder without failing.
 *
 * The marker is composited into the captured copy, never into the framebuffer
 * itself - the device's own display must be unchanged. (Not automatically
 * checked here - confirm visually that the handheld's own screen shows no
 * red square while this runs.)
 */
#include "../include/rgsp_cast.h"
#include <stdio.h>
#include <assert.h>

int main(void)
{
    rgsp_capture *c = rgsp_capture_open(720, 480, 30, 2000000);
    if (!c) { fprintf(stderr, "open: %s\n", rgsp_capture_last_error()); return 1; }

    const unsigned char *data; size_t len; int key;

    rgsp_capture_set_overlay(c, 0);
    if (rgsp_capture_next(c, &data, &len, &key) != 0) {
        fprintf(stderr, "next (overlay off): %s\n", rgsp_capture_last_error());
        return 1;
    }
    size_t without = len;

    rgsp_capture_set_overlay(c, 1);
    /* First frame with overlay on can still carry SPS/PPS sized for the
     * pre-overlay stream; take the second to compare like with like. */
    if (rgsp_capture_next(c, &data, &len, &key) != 0) {
        fprintf(stderr, "next (overlay on, warmup): %s\n", rgsp_capture_last_error());
        return 1;
    }
    if (rgsp_capture_next(c, &data, &len, &key) != 0) {
        fprintf(stderr, "next (overlay on): %s\n", rgsp_capture_last_error());
        return 1;
    }
    size_t with = len;

    printf("without=%zu with=%zu\n", without, with);
    assert(with > 0);
    assert(with != without);
    printf("PASS (wiring only - see file header for what this does and does not prove)\n");
    rgsp_capture_close(c);
    return 0;
}
