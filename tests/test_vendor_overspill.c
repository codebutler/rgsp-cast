/* How far past each vendor struct's end do the CedarC libraries write?
 *
 * This exists because the answer is not zero, and the one case where it is not
 * cost a segfault. VencInputBuffer overruns its declared extent — its
 * _tail[256] padding included — by 24 bytes on every frame. As stack locals in
 * the old CLI that spill landed on scratch and nobody noticed for months; the
 * moment the extraction promoted the struct to a member of rgsp_capture, it
 * started zeroing live fields.
 *
 * So: changing a vendor struct's storage class is a hazard in this codebase,
 * and "it passes" is not evidence — the CLI passed throughout. Run this before
 * moving one, and after adding a new vendor call.
 *
 * Method. Wrap the struct in { T s; unsigned char sentinel[4096]; }, fill the
 * sentinel with 0xAA, make the vendor call that writes it, then find the
 * highest byte that changed. The fill must NOT be zeroes: most of what the
 * vendor writes past the end *is* zeroes, so a zero-filled guard scanned for
 * non-zero bytes reported +5 when the truth was +24. That false reading is the
 * reason this file specifies the sentinel value so loudly.
 *
 * VencInputBuffer doubles as the positive control. If it measures 0 the
 * harness is not observing anything and the run fails, whatever the other
 * structs report.
 *
 * The setup below deliberately mirrors rgsp_capture_open_ex()'s sequence
 * rather than calling it: VideoEncInit and AllocInputBuffer each write a struct
 * we want to measure, and they only run on a fresh encoder. Keep it in step
 * with the library if that sequence changes.
 *
 * Includes the library source directly to reach the real struct definitions and
 * the dlsym'd vendor pointers — measuring a re-declared copy would silently
 * stop meaning anything the moment the two drifted.
 */
#include "../src/rgsp-cast.c"

#define GUARD 4096

#define GUARDED(T) struct { T s; unsigned char sentinel[GUARD]; }

static void arm(unsigned char *sentinel)
{
    memset(sentinel, 0xAA, GUARD);
}

/* Bytes past the struct's end that the vendor modified. */
static int extent(const unsigned char *sentinel)
{
    int last = -1;
    for (int i = 0; i < GUARD; i++)
        if (sentinel[i] != 0xAA) last = i;
    return last + 1;
}

static void report(const char *name, size_t declared, int spill)
{
    printf("  %-24s declared %4zu bytes, vendor wrote +%d past end%s\n",
           name, declared, spill, spill ? "" : "  (clean)");
}

int main(void)
{
    int rc = 1;
    int spill_base = -1, spill_alloc = -1, spill_in = -1, spill_out = -1;

    if (load_libs() != 0) {
        fprintf(stderr, "load_libs: %s\n", g_last_error);
        return 1;
    }

    int fb = open("/dev/fb0", O_RDONLY);
    if (fb < 0) { perror("/dev/fb0"); return 1; }
    struct fb_var_screeninfo vinfo;
    struct fb_fix_screeninfo finfo;
    if (ioctl(fb, FBIOGET_VSCREENINFO, &vinfo) < 0 ||
        ioctl(fb, FBIOGET_FSCREENINFO, &finfo) < 0) {
        perror("FBIOGET"); close(fb); return 1;
    }
    unsigned w = vinfo.xres, h = vinfo.yres;

    fn_GetVeOpsS get_ve  = (fn_GetVeOpsS)dlsym(g_libVE, "GetVeOpsS");
    fn_GetOpsS   get_mem = (fn_GetOpsS)dlsym(g_libMem, "MemAdapterGetOpsS");
    if (!get_ve || !get_mem) { fprintf(stderr, "ops missing\n"); close(fb); return 1; }

    void *veops = get_ve(0);
    ScMemOpsS *memops = (ScMemOpsS *)get_mem();
    if (!veops || !memops || memops->open() < 0) {
        fprintf(stderr, "mem open failed\n"); close(fb); return 1;
    }

    VideoEncoder *enc = p_VideoEncCreate(VENC_CODEC_H264);
    if (!enc) { fprintf(stderr, "VideoEncCreate failed\n"); goto out_mem; }

    printf("vendor struct overspill, %ux%u, sentinel 0xAA over %d bytes\n\n", w, h, GUARD);

    /* ── VencBaseConfig, written by VideoEncInit ─────────────────────── */
    GUARDED(VencBaseConfig) bc;
    memset(&bc.s, 0, sizeof bc.s);
    arm(bc.sentinel);
    bc.s.bEncH264Nalu = 1;
    bc.s.nInputWidth = w; bc.s.nInputHeight = h;
    bc.s.nDstWidth   = w; bc.s.nDstHeight   = h;
    bc.s.nStride     = w;
    bc.s.eInputFormat = VENC_PIXEL_ARGB;
    bc.s.memops = memops; bc.s.veOpsS = veops; bc.s.pVeOpsSelf = NULL;
    if (p_VideoEncInit(enc, &bc.s) != 0) {
        fprintf(stderr, "VideoEncInit failed\n"); goto out_enc;
    }
    spill_base = extent(bc.sentinel);

    /* ── VencAllocateBufferParam, written by AllocInputBuffer ─────────── */
    GUARDED(VencAllocateBufferParam) bp;
    memset(&bp.s, 0, sizeof bp.s);
    arm(bp.sentinel);
    bp.s.nBufferNum = 1;
    bp.s.nSizeY     = w * h * 4;
    bp.s.nSizeC     = 0;
    if (p_AllocInputBuffer(enc, &bp.s) != 0) {
        fprintf(stderr, "AllocInputBuffer failed\n"); goto out_uninit;
    }
    spill_alloc = extent(bp.sentinel);

    /* ── VencInputBuffer — the positive control ───────────────────────── */
    GUARDED(VencInputBuffer) in;
    memset(&in.s, 0, sizeof in.s);
    arm(in.sentinel);
    if (p_GetOneAllocInputBuffer(enc, &in.s) != 0) {
        fprintf(stderr, "GetOneAllocInputBuffer failed\n"); goto out_release;
    }

    /* Encode one frame so there is a bitstream to retrieve. */
    memset(in.s._virY, 0, (size_t)w * h * 4);
    in.s.nPts = 0;
    in.s.bIsFirstFrame = 1;
    p_FlushCacheAllocInputBuffer(enc, &in.s);
    if (p_AddOneInputBuffer(enc, &in.s) != 0 || p_VideoEncodeOneFrame(enc) != 0) {
        fprintf(stderr, "encode failed\n"); goto out_release;
    }

    /* ── VencOutputBuffer, written by GetOneBitstreamFrame ────────────── */
    if (p_ValidBitstreamFrameNum(enc) > 0) {
        GUARDED(VencOutputBuffer) ob;
        memset(&ob.s, 0, sizeof ob.s);
        arm(ob.sentinel);
        if (p_GetOneBitstreamFrame(enc, &ob.s) == 0) {
            spill_out = extent(ob.sentinel);
            p_FreeOneBitStreamFrame(enc, &ob.s);
        }
    }

    /* The measurement that matters: the recycle pair is what corrupted
     * rgsp_capture's fields when this struct became a member. */
    GUARDED(VencInputBuffer) used;
    memset(&used.s, 0, sizeof used.s);
    arm(used.sentinel);
    if (p_AlreadyUsedInputBuffer(enc, &used.s) == 0)
        p_ReturnOneAllocInputBuffer(enc, &used.s);
    spill_in = extent(used.sentinel);

    report("VencBaseConfig",          sizeof(VencBaseConfig),          spill_base);
    report("VencAllocateBufferParam", sizeof(VencAllocateBufferParam), spill_alloc);
    report("VencOutputBuffer",        sizeof(VencOutputBuffer),        spill_out < 0 ? 0 : spill_out);
    report("VencInputBuffer",         sizeof(VencInputBuffer),         spill_in);

    if (spill_out < 0)
        printf("\n  note: no bitstream frame was available, VencOutputBuffer not measured\n");

    printf("\n");
    if (spill_in <= 0) {
        printf("FAIL: VencInputBuffer measured +0, but it is known to overrun by 24.\n"
               "      The harness is not observing the vendor's writes - do not trust\n"
               "      the other numbers above.\n");
        rc = 1;
    } else {
        printf("PASS: control (VencInputBuffer) shows +%d, so the harness observes\n"
               "      vendor writes; the numbers above are meaningful.\n", spill_in);
        rc = 0;
    }

out_release:
    p_ReleaseAllocInputBuffer(enc);
out_uninit:
    p_VideoEncUnInit(enc);
out_enc:
    p_VideoEncDestroy(enc);
out_mem:
    if (memops->close) memops->close();
    close(fb);
    return rc;
}
