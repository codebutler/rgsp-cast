/* Does rgsp_capture_request_idr() actually reach the encoder?
 *
 * test_capture_api.c asserts that the frame after a request is a keyframe,
 * which passes spuriously if the encoder was about to emit one anyway at a GOP
 * boundary. That matters here because the vendor parameter index used by
 * request_idr() (VENC_IndexParamForceKeyFrame = 0x6) is reconstructed from the
 * standard CedarX enum ordering rather than read from a vendor header — the
 * same shape of hazard as 0x101 vs 16. A silently-wrong index would leave
 * Moonlight unable to recover from packet loss.
 *
 * So: measure the encoder's natural keyframe cadence first, then force an IDR
 * twice at different off-cadence offsets and require both to land. Two forced
 * keyframes away from any natural boundary is convincing; one is not.
 */
#include "../include/rgsp_cast.h"
#include <stdio.h>
#include <stdlib.h>

#define OBSERVE 120

static int pull(rgsp_capture *c, size_t *len_out)
{
    const unsigned char *d;
    size_t n;
    int k;

    if (rgsp_capture_next(c, &d, &n, &k) != 0) {
        fprintf(stderr, "capture failed: %s\n", rgsp_capture_last_error());
        exit(1);
    }
    if (len_out) *len_out = n;
    return k;
}

int main(void)
{
    rgsp_capture *c = rgsp_capture_open(720, 480, 30, 2000000);
    if (!c) { fprintf(stderr, "open failed: %s\n", rgsp_capture_last_error()); return 1; }

    /* ── phase 1: natural cadence, no IDR requests ───────────────────── */
    int key[OBSERVE];
    for (int i = 0; i < OBSERVE; i++) key[i] = pull(c, NULL);

    printf("phase 1: natural keyframe pattern over %d frames (K=keyframe)\n", OBSERVE);
    fputs("  ", stdout);
    for (int i = 0; i < OBSERVE; i++) putchar(key[i] ? 'K' : '.');
    putchar('\n');

    int kf = 0, first = -1, last = -1, min_gap = OBSERVE + 1;
    for (int i = 0; i < OBSERVE; i++) {
        if (!key[i]) continue;
        kf++;
        if (first < 0) first = i;
        if (last >= 0 && i - last < min_gap) min_gap = i - last;
        last = i;
    }
    printf("  %d keyframe(s); first at frame %d; ", kf, first);
    if (kf < 2) printf("no repeat in the window (natural interval >= %d frames)\n", OBSERVE);
    else        printf("smallest gap between keyframes = %d frames\n", min_gap);

    /* A forced IDR only proves anything if the encoder was not about to emit
     * one regardless. */
    int gop = (kf < 2) ? OBSERVE : min_gap;
    if (gop < 4) {
        printf("\nINCONCLUSIVE: the natural keyframe interval is %d frames, too short\n"
               "to distinguish a forced IDR from the encoder's own cadence.\n", gop);
        rgsp_capture_close(c);
        return 2;
    }

    /* ── phase 2: force an IDR twice, at two different off-cadence points ── */
    const int offsets[2] = { 17, 29 };
    int fails = 0;

    printf("\nphase 2: forcing an IDR at two off-cadence offsets\n");
    for (int t = 0; t < 2; t++) {
        size_t len_before = 0, len_forced = 0;
        int stray = 0;

        /* Run up to the request point. These frames are the control: if the
         * encoder is quiet across all of them, it is not on a boundary. */
        for (int i = 0; i < offsets[t]; i++) stray += pull(c, &len_before);

        rgsp_capture_request_idr(c);
        int k = pull(c, &len_forced);

        printf("  trial %d: %d run-up frames contained %d keyframes; "
               "frame after request: keyframe=%d\n",
               t + 1, offsets[t], stray, k);
        printf("           sizes: forced frame %zu bytes vs preceding %zu bytes\n",
               len_forced, len_before);
        if (stray)  printf("           NOTE: keyframes in the run-up weaken this trial\n");
        if (!k) { printf("           FAIL: forced IDR did not produce a keyframe\n"); fails++; }
    }

    rgsp_capture_close(c);

    if (fails) {
        printf("\nFAIL: %d of 2 forced IDRs did not land - "
               "VENC_IndexParamForceKeyFrame is probably the wrong index\n", fails);
        return 1;
    }
    printf("\nPASS: both forced IDRs landed off-cadence "
           "(natural interval >= %d frames)\n", gop);
    return 0;
}
