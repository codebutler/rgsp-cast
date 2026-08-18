/* Unit test for draw_marker() - the pure pixel-painting helper behind
 * rgsp_capture_set_overlay(). Runs entirely on the host: draw_marker() takes
 * a plain buffer and touches no ION allocation, no /dev/fb0, no vendor libs,
 * so none of that needs to be present to test it.
 *
 * Includes src/rgsp-cast.c directly to reach the static draw_marker() rather
 * than exposing it in the public header - the same pattern already used by
 * tests/test_vendor_overspill.c to reach internals that don't belong in
 * include/rgsp_cast.h.
 *
 * The plan's original tests/test_overlay.c asserted only `with > 0` on an
 * encoded frame length, which is true whether or not anything was drawn - it
 * runs on-device and is kept as a smoke test, but it cannot substitute for
 * this: only reading the actual pixels the function wrote can show the
 * marker exists, is the right color, is the right size, is in the right
 * place, and touches nothing else.
 */
#include "../src/rgsp-cast.c"

#include <stdio.h>
#include <assert.h>

#define W 720
#define H 480
#define STRIDE (W * 4)
#define GUARD 256

/* Sentinel is 0xAA, not 0x00: a zero fill can't distinguish "drew nothing"
 * from "drew zeroes", and this codebase has already been burned once by a
 * zero-filled guard reporting a false result (see test_vendor_overspill.c).
 */
#define SENTINEL 0xAA

static unsigned char *frame;      /* W*H*4, plus a trailing guard region */
static size_t frame_bytes;

static void arm(void)
{
    memset(frame, SENTINEL, frame_bytes + GUARD);
}

static int pixel_is(int x, int y, unsigned char b, unsigned char g,
                    unsigned char r, unsigned char a)
{
    unsigned char *px = frame + (size_t)y * STRIDE + (size_t)x * 4;
    return px[0] == b && px[1] == g && px[2] == r && px[3] == a;
}

static int pixel_is_sentinel(int x, int y)
{
    unsigned char *px = frame + (size_t)y * STRIDE + (size_t)x * 4;
    return px[0] == SENTINEL && px[1] == SENTINEL &&
           px[2] == SENTINEL && px[3] == SENTINEL;
}

/* Checks every property the task requires. Returns 1 if all hold. */
static int check(const char *label)
{
    int ok = 1;

    /* Every pixel inside the 16x16 rect at (16,16) is opaque red, B,G,R,A. */
    for (int y = 16; y < 32; y++) {
        for (int x = 16; x < 32; x++) {
            if (!pixel_is(x, y, 0x00, 0x00, 0xFF, 0xFF)) {
                printf("  [%s] FAIL: (%d,%d) inside the rect is not opaque red\n",
                       label, x, y);
                ok = 0;
            }
        }
    }

    /* Pixels immediately bordering all four edges are untouched - catches an
     * off-by-one in the loop bounds in either direction. */
    for (int x = 16; x < 32; x++) {
        if (!pixel_is_sentinel(x, 15)) { printf("  [%s] FAIL: (%d,15) above the rect was touched\n", label, x); ok = 0; }
        if (!pixel_is_sentinel(x, 32)) { printf("  [%s] FAIL: (%d,32) below the rect was touched\n", label, x); ok = 0; }
    }
    for (int y = 16; y < 32; y++) {
        if (!pixel_is_sentinel(15, y)) { printf("  [%s] FAIL: (15,%d) left of the rect was touched\n", label, y); ok = 0; }
        if (!pixel_is_sentinel(32, y)) { printf("  [%s] FAIL: (32,%d) right of the rect was touched\n", label, y); ok = 0; }
    }

    /* The write stayed in bounds: the guard region past the declared buffer
     * is untouched. */
    for (int i = 0; i < GUARD; i++) {
        if (frame[frame_bytes + i] != SENTINEL) {
            printf("  [%s] FAIL: guard byte +%d past the buffer was touched (0x%02x)\n",
                   label, i, frame[frame_bytes + i]);
            ok = 0;
            break;
        }
    }

    if (ok) printf("  [%s] all checks pass\n", label);
    return ok;
}

int main(void)
{
    frame_bytes = (size_t)STRIDE * H;
    frame = malloc(frame_bytes + GUARD);
    if (!frame) { fprintf(stderr, "out of memory\n"); return 1; }

    arm();
    draw_marker(frame, W, H, STRIDE);
    int normal_ok = check("normal");

    if (!normal_ok) {
        printf("FAIL: draw_marker did not produce the expected result under "
               "its real implementation.\n");
        return 1;
    }

    printf("PASS\n");
    return 0;
}
