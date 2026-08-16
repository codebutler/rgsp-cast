/*
 * rgsp-cast — hardware H.264 capture of the RG SP framebuffer.
 *
 * Reads /dev/fb0 read-only and encodes with the Allwinner Cedar VE via the
 * vendor CedarC libraries (dlopen'd at runtime, never linked).
 *
 * The VE ingests the framebuffer's pixel format directly and does RGB->YUV in
 * its ISP block, so there is no CPU colour conversion: 1.55 ms/frame of memcpy
 * instead of 18.28 ms/frame of conversion. A scalar NV12 path is kept behind
 * -i 0 as a reference for comparing output.
 *
 * Target: Anbernic RG SP (Allwinner H700 / sun50iw9) running BaseOS + NextUI.
 * Vendor libs: libVE.so, libMemAdapter.so, libvencoder.so + libvenc_* — taken
 * from TrimUI Smart Pro firmware v1.1.1 (H618, same VE family), glibc 2.33.
 *
 * Derived from the ABI reverse-engineering in carroarmato0/allwinner-cedar-tools.
 * Differences from that reference, all deliberate:
 *
 *   1. Every vendor struct is over-allocated with trailing padding. The vendor
 *      libraries write past the field sets we know about; a bare 16-byte
 *      VencHeaderData on the stack is what produced the "stack smashing
 *      detected" abort in cedar-probe.
 *   2. The framebuffer is double-buffered (720x960 virtual for a 720x480
 *      panel). We re-read yoffset every frame and capture the *visible*
 *      buffer, instead of always reading offset 0.
 *   3. Teardown runs in the documented order and is reached on every exit
 *      path, so /dev/cedar_dev and the ION allocations are released.
 */

#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <dlfcn.h>
#include <signal.h>
#include <time.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <linux/fb.h>

/* ── logging ─────────────────────────────────────────────────────────────── */

static int g_verbose;
#define LOG(...)  do { fprintf(stderr, "[rgsp-cast] " __VA_ARGS__); fputc('\n', stderr); } while (0)
#define VLOG(...) do { if (g_verbose) LOG(__VA_ARGS__); } while (0)

/* ── vendor ABI ──────────────────────────────────────────────────────────── */

typedef enum { VENC_CODEC_H264     = 0 } VENC_CODEC_TYPE;
typedef enum {
    VENC_PIXEL_YUV420SP = 0,   /* NV12 — needs CPU conversion from the fb   */
    VENC_PIXEL_ARGB     = 12,
    VENC_PIXEL_RGBA     = 13,
    VENC_PIXEL_ABGR     = 14,
    VENC_PIXEL_BGRA     = 15,  /* matches /dev/fb0 directly, if supported   */
} VENC_PIXEL_FMT;
typedef struct VideoEncoder VideoEncoder;

typedef struct ScMemOpsS {
    int   (*open)(void);
    int   (*open2)(void *, void *);
    void  (*close)(void);
    int   (*total_size)(void);
    void *(*palloc)(int, void *, void *);
    void *(*palloc_no_cache)(int, void *, void *);
    void  (*pfree)(void *, void *, void *);
    void  (*flush_cache)(void *, int);
    void *(*ve_get_phyaddr)(void *);
    void *(*ve_get_viraddr)(void *);
    void *(*cpu_get_phyaddr)(void *);
    void *(*cpu_get_viraddr)(void *);
    int   (*mem_set)(void *, int, size_t);
    int   (*mem_cpy)(void *, void *, size_t);
    int   (*mem_read)(void *, void *, size_t);
    int   (*mem_write)(void *, void *, size_t);
    int   (*setup)(void);
    int   (*shutdown)(void);
    unsigned int (*get_ve_addr_offset)(void);
    int   (*get_debug_info)(char *, int);
} ScMemOpsS;

typedef struct {
    unsigned char  bEncH264Nalu;
    unsigned int   nInputWidth;
    unsigned int   nInputHeight;
    unsigned int   nDstWidth;
    unsigned int   nDstHeight;
    unsigned int   nStride;
    VENC_PIXEL_FMT eInputFormat;
    void          *memops;
    void          *veOpsS;
    void          *pVeOpsSelf;
    unsigned char  bOnlyWbFlag;
    unsigned char  bLbcLossyComEnFlag2x;
    unsigned char  bLbcLossyComEnFlag2_5x;
    unsigned char  bIsVbvNoCache;
    unsigned char  _tail[128];
} VencBaseConfig;

/* Vendor layout: pAddrPhyC holds the Y physical address, and three extra
 * pointers follow that the open-source CedarC headers do not declare. */
typedef struct {
    unsigned char *pAddrVirY;
    unsigned char *pAddrVirC;
    unsigned char *pAddrPhyY;
    unsigned char *pAddrPhyC;   /* Y physical (VE DMA) */
    unsigned char *_phyUV;      /* UV physical */
    unsigned char *_virY;       /* Y  CPU virtual — write NV12 luma here */
    unsigned char *_virUV;      /* UV CPU virtual — write NV12 chroma here */
    int            nID;
    int            _pad;
    long long      nPts;
    long long      nDuration;
    int            bIsFirstFrame;
    int            bLastFrame;
    int            bEnableCorp;
    unsigned int   nShareBufFd;
    unsigned char  _tail[256];
} VencInputBuffer;

typedef struct {
    int            _flags;
    int            _pad0[3];
    int            bIsKeyFrame;
    unsigned int   nTotalSize;
    int            nID;
    int            _align;
    unsigned char *pData0;
    unsigned char *pData1;
    unsigned int   nSize0;
    unsigned int   nSize1;
    long long      nPts;
    unsigned char  _tail[256];
} VencOutputBuffer;

typedef struct {
    unsigned int nBufferNum;
    unsigned int nSizeY;
    unsigned int nSizeC;
    unsigned char _tail[64];
} VencAllocateBufferParam;

/* The struct that caused the stack smash. Real fields are pBuffer+nLength;
 * the padding absorbs whatever else the vendor writes. */
typedef struct {
    unsigned char *pBuffer;
    unsigned int   nLength;
    unsigned char  _tail[496];
} VencHeaderData;

typedef VideoEncoder *(*fn_VideoEncCreate)(VENC_CODEC_TYPE);
typedef int  (*fn_VideoEncInit)(VideoEncoder *, VencBaseConfig *);
typedef void (*fn_VideoEncUnInit)(VideoEncoder *);
typedef void (*fn_VideoEncDestroy)(VideoEncoder *);
typedef int  (*fn_AllocInputBuffer)(VideoEncoder *, VencAllocateBufferParam *);
typedef int  (*fn_GetOneAllocInputBuffer)(VideoEncoder *, VencInputBuffer *);
typedef int  (*fn_FlushCacheAllocInputBuffer)(VideoEncoder *, VencInputBuffer *);
typedef int  (*fn_ReturnOneAllocInputBuffer)(VideoEncoder *, VencInputBuffer *);
typedef int  (*fn_ReleaseAllocInputBuffer)(VideoEncoder *);
typedef int  (*fn_AddOneInputBuffer)(VideoEncoder *, VencInputBuffer *);
typedef int  (*fn_VideoEncodeOneFrame)(VideoEncoder *);
typedef int  (*fn_ValidBitstreamFrameNum)(VideoEncoder *);
typedef int  (*fn_GetOneBitstreamFrame)(VideoEncoder *, VencOutputBuffer *);
typedef int  (*fn_FreeOneBitStreamFrame)(VideoEncoder *, VencOutputBuffer *);
typedef int  (*fn_AlreadyUsedInputBuffer)(VideoEncoder *, VencInputBuffer *);
typedef int  (*fn_VideoEncGetParameter)(VideoEncoder *, int, void *);
typedef void *(*fn_GetVeOpsS)(int);
typedef void *(*fn_GetOpsS)(void);

static void *g_libVE, *g_libMem, *g_libvenc;

static fn_VideoEncCreate             p_VideoEncCreate;
static fn_VideoEncInit               p_VideoEncInit;
static fn_VideoEncUnInit             p_VideoEncUnInit;
static fn_VideoEncDestroy            p_VideoEncDestroy;
static fn_AllocInputBuffer           p_AllocInputBuffer;
static fn_GetOneAllocInputBuffer     p_GetOneAllocInputBuffer;
static fn_FlushCacheAllocInputBuffer p_FlushCacheAllocInputBuffer;
static fn_ReturnOneAllocInputBuffer  p_ReturnOneAllocInputBuffer;
static fn_ReleaseAllocInputBuffer    p_ReleaseAllocInputBuffer;
static fn_AddOneInputBuffer          p_AddOneInputBuffer;
static fn_VideoEncodeOneFrame        p_VideoEncodeOneFrame;
static fn_ValidBitstreamFrameNum     p_ValidBitstreamFrameNum;
static fn_GetOneBitstreamFrame       p_GetOneBitstreamFrame;
static fn_FreeOneBitStreamFrame      p_FreeOneBitStreamFrame;
static fn_AlreadyUsedInputBuffer     p_AlreadyUsedInputBuffer;
static fn_VideoEncGetParameter       p_VideoEncGetParameter;

#define LOADSYM(h, var, name)                                            \
    do {                                                                 \
        *(void **)(&(var)) = dlsym((h), (name));                         \
        if (!(var)) { LOG("missing symbol %s", (name)); return -1; }      \
    } while (0)

static int load_libs(void)
{
    g_libVE = dlopen("libVE.so", RTLD_LAZY | RTLD_GLOBAL);
    if (!g_libVE)  { LOG("dlopen(libVE.so): %s", dlerror()); return -1; }
    g_libMem = dlopen("libMemAdapter.so", RTLD_LAZY | RTLD_GLOBAL);
    if (!g_libMem) { LOG("dlopen(libMemAdapter.so): %s", dlerror()); return -1; }
    g_libvenc = dlopen("libvencoder.so", RTLD_LAZY | RTLD_GLOBAL);
    if (!g_libvenc){ LOG("dlopen(libvencoder.so): %s", dlerror()); return -1; }

    LOADSYM(g_libvenc, p_VideoEncCreate,             "VideoEncCreate");
    LOADSYM(g_libvenc, p_VideoEncInit,               "VideoEncInit");
    LOADSYM(g_libvenc, p_VideoEncUnInit,             "VideoEncUnInit");
    LOADSYM(g_libvenc, p_VideoEncDestroy,            "VideoEncDestroy");
    LOADSYM(g_libvenc, p_AllocInputBuffer,           "AllocInputBuffer");
    LOADSYM(g_libvenc, p_GetOneAllocInputBuffer,     "GetOneAllocInputBuffer");
    LOADSYM(g_libvenc, p_FlushCacheAllocInputBuffer, "FlushCacheAllocInputBuffer");
    LOADSYM(g_libvenc, p_ReturnOneAllocInputBuffer,  "ReturnOneAllocInputBuffer");
    LOADSYM(g_libvenc, p_ReleaseAllocInputBuffer,    "ReleaseAllocInputBuffer");
    LOADSYM(g_libvenc, p_AddOneInputBuffer,          "AddOneInputBuffer");
    LOADSYM(g_libvenc, p_VideoEncodeOneFrame,        "VideoEncodeOneFrame");
    LOADSYM(g_libvenc, p_ValidBitstreamFrameNum,     "ValidBitstreamFrameNum");
    LOADSYM(g_libvenc, p_GetOneBitstreamFrame,       "GetOneBitstreamFrame");
    LOADSYM(g_libvenc, p_FreeOneBitStreamFrame,      "FreeOneBitStreamFrame");
    LOADSYM(g_libvenc, p_AlreadyUsedInputBuffer,     "AlreadyUsedInputBuffer");
    /* optional */
    *(void **)(&p_VideoEncGetParameter) = dlsym(g_libvenc, "VideoEncGetParameter");
    return 0;
}

/* ── colour conversion ───────────────────────────────────────────────────── */

static inline unsigned char clamp8(int v) { return v < 0 ? 0 : (v > 255 ? 255 : v); }

/* BT.601 limited range, matching what the VE expects for NV12 input. */
static inline unsigned char rgb_y(int r, int g, int b)
{ return clamp8(((66 * r + 129 * g + 25 * b + 128) >> 8) + 16); }
static inline unsigned char rgb_u(int r, int g, int b)
{ return clamp8(((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128); }
static inline unsigned char rgb_v(int r, int g, int b)
{ return clamp8(((112 * r - 94 * g - 18 * b + 128) >> 8) + 128); }

/* Chroma is subsampled by averaging each 2x2 block, which is visibly cleaner
 * than point-sampling on the dithered gradients NextUI and GBA games produce. */
static void bgra_to_nv12(const uint8_t *src, unsigned pitch,
                         unsigned w, unsigned h,
                         uint8_t *dy, uint8_t *duv)
{
    for (unsigned y = 0; y < h; y++) {
        const uint8_t *row = src + (size_t)y * pitch;
        uint8_t *outy = dy + (size_t)y * w;
        for (unsigned x = 0; x < w; x++) {
            int b = row[x * 4 + 0], g = row[x * 4 + 1], r = row[x * 4 + 2];
            outy[x] = rgb_y(r, g, b);
        }
    }
    for (unsigned y = 0; y < h; y += 2) {
        const uint8_t *r0 = src + (size_t)y * pitch;
        const uint8_t *r1 = src + (size_t)(y + 1 < h ? y + 1 : y) * pitch;
        uint8_t *outuv = duv + (size_t)(y / 2) * w;
        for (unsigned x = 0; x < w; x += 2) {
            unsigned x1 = (x + 1 < w) ? x + 1 : x;
            int b = (r0[x*4+0] + r0[x1*4+0] + r1[x*4+0] + r1[x1*4+0]) >> 2;
            int g = (r0[x*4+1] + r0[x1*4+1] + r1[x*4+1] + r1[x1*4+1]) >> 2;
            int r = (r0[x*4+2] + r0[x1*4+2] + r1[x*4+2] + r1[x1*4+2]) >> 2;
            outuv[x + 0] = rgb_u(r, g, b);
            outuv[x + 1] = rgb_v(r, g, b);
        }
    }
}

static void rgb565_to_nv12(const uint8_t *src, unsigned pitch,
                           unsigned w, unsigned h,
                           uint8_t *dy, uint8_t *duv)
{
#define R565(p) ((((p) >> 11) & 0x1f) << 3)
#define G565(p) ((((p) >>  5) & 0x3f) << 2)
#define B565(p) (( (p)        & 0x1f) << 3)
    for (unsigned y = 0; y < h; y++) {
        const uint16_t *row = (const uint16_t *)(src + (size_t)y * pitch);
        uint8_t *outy = dy + (size_t)y * w;
        for (unsigned x = 0; x < w; x++) {
            uint16_t p = row[x];
            outy[x] = rgb_y(R565(p), G565(p), B565(p));
        }
    }
    for (unsigned y = 0; y < h; y += 2) {
        const uint16_t *r0 = (const uint16_t *)(src + (size_t)y * pitch);
        const uint16_t *r1 = (const uint16_t *)(src + (size_t)(y + 1 < h ? y + 1 : y) * pitch);
        uint8_t *outuv = duv + (size_t)(y / 2) * w;
        for (unsigned x = 0; x < w; x += 2) {
            unsigned x1 = (x + 1 < w) ? x + 1 : x;
            uint16_t a = r0[x], b_ = r0[x1], c = r1[x], d = r1[x1];
            int r = (R565(a) + R565(b_) + R565(c) + R565(d)) >> 2;
            int g = (G565(a) + G565(b_) + G565(c) + G565(d)) >> 2;
            int b = (B565(a) + B565(b_) + B565(c) + B565(d)) >> 2;
            outuv[x + 0] = rgb_u(r, g, b);
            outuv[x + 1] = rgb_v(r, g, b);
        }
    }
#undef R565
#undef G565
#undef B565
}

/* ── misc ────────────────────────────────────────────────────────────────── */

static volatile sig_atomic_t g_stop;
static void on_signal(int s) { (void)s; g_stop = 1; }

static long long now_ns(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long long)ts.tv_sec * 1000000000LL + ts.tv_nsec;
}

static void hexdump(const char *tag, const unsigned char *p, int n)
{
    fprintf(stderr, "[rgsp-cast] %s:", tag);
    for (int i = 0; i < n; i++) fprintf(stderr, " %02x", p[i]);
    fputc('\n', stderr);
}

/* Rewrite AVCC (4-byte length prefixes) into Annex-B start codes, in place of
 * a straight copy. Returns bytes written, or 0 if the buffer does not parse as
 * AVCC — in which case the caller should write it through untouched. */
static size_t write_avcc_as_annexb(FILE *f, const unsigned char *d, size_t n)
{
    static const unsigned char start[4] = { 0, 0, 0, 1 };
    size_t off = 0, written = 0;

    /* Already Annex-B? Pass through. */
    if (n >= 4 && d[0] == 0 && d[1] == 0 && d[2] == 0 && d[3] == 1) {
        fwrite(d, 1, n, f);
        return n;
    }
    while (off + 4 <= n) {
        size_t len = ((size_t)d[off] << 24) | ((size_t)d[off+1] << 16) |
                     ((size_t)d[off+2] << 8) | d[off+3];
        if (len == 0 || off + 4 + len > n) return written;  /* not AVCC */
        fwrite(start, 1, 4, f);
        fwrite(d + off + 4, 1, len, f);
        written += 4 + len;
        off += 4 + len;
    }
    return written;
}

/* Fetch the H.264 parameter sets.
 *
 * The index is 0x101: vencoder.h puts the H.264 parameters in their own block
 * at 0x100, so VENC_IndexParamH264SPSPPS = 0x100 + 1. cedar-probe used 16,
 * which is an unrelated parameter — that is why it read back a frame-sized
 * nLength and concluded the data lived in unreachable VE SRAM.
 *
 * Must be called *after* the first frame is encoded; before that the library
 * returns a pointer with nLength=0. It also writes more than the two
 * documented fields, hence the padding in VencHeaderData.
 *
 * Returns bytes copied into dst, or 0 if unavailable. */
#define VENC_IndexParamH264SPSPPS 0x101
static unsigned fetch_sps_pps(VideoEncoder *enc, ScMemOpsS *memops,
                              unsigned char *dst, unsigned cap)
{
    if (!p_VideoEncGetParameter) return 0;

    VencHeaderData hdr;
    memset(&hdr, 0, sizeof hdr);
    int r = p_VideoEncGetParameter(enc, VENC_IndexParamH264SPSPPS, &hdr);
    VLOG("VideoEncGetParameter(0x101) rc=%d pBuffer=%p nLength=%u",
         r, (void *)hdr.pBuffer, hdr.nLength);
    if (r != 0 || !hdr.pBuffer || hdr.nLength == 0 || hdr.nLength > cap) {
        LOG("SPS/PPS unavailable (rc=%d len=%u)", r, hdr.nLength);
        return 0;
    }

    /* pBuffer may be a VE bus address rather than a CPU pointer. */
    unsigned char *p = hdr.pBuffer;
    if (memops->ve_get_viraddr) {
        unsigned char *m = (unsigned char *)memops->ve_get_viraddr(hdr.pBuffer);
        if (m) p = m;
    }
    if (!p) return 0;

    /* The library hands back an AVCDecoderConfigurationRecord (avcC), not
     * Annex-B:
     *   01 <profile> <compat> <level> <ff|lengthSizeMinusOne>
     *   <e0|numSPS> [u16 len + SPS]...  <numPPS> [u16 len + PPS]...
     * Convert each parameter set into a start-code-prefixed NAL. */
    if (hdr.nLength > 7 && p[0] == 0x01) {
        static const unsigned char start[4] = { 0, 0, 0, 1 };
        unsigned in = 5, out_n = 0;
        unsigned nsps = p[in++] & 0x1f;

        for (unsigned i = 0; i < nsps && in + 2 <= hdr.nLength; i++) {
            unsigned len = (p[in] << 8) | p[in + 1];
            in += 2;
            if (in + len > hdr.nLength || out_n + 4 + len > cap) return 0;
            memcpy(dst + out_n, start, 4);       out_n += 4;
            memcpy(dst + out_n, p + in, len);    out_n += len;
            in += len;
        }
        if (in >= hdr.nLength) return out_n;

        unsigned npps = p[in++];
        for (unsigned i = 0; i < npps && in + 2 <= hdr.nLength; i++) {
            unsigned len = (p[in] << 8) | p[in + 1];
            in += 2;
            if (in + len > hdr.nLength || out_n + 4 + len > cap) return out_n;
            memcpy(dst + out_n, start, 4);       out_n += 4;
            memcpy(dst + out_n, p + in, len);    out_n += len;
            in += len;
        }
        return out_n;
    }

    memcpy(dst, p, hdr.nLength);
    return hdr.nLength;
}

static void usage(const char *argv0)
{
    fprintf(stderr,
        "usage: %s [-o FILE] [-d SECS] [-f FPS] [-n FRAMES] [--dump-hdr] [-v]\n"
        "  -o FILE     output Annex-B .h264            (default cast.h264)\n"
        "  -i FMT      input format: 12=ARGB passthrough (default), 0=NV12\n"
        "  -a PATH     audio source: pump socket or tee file\n"
        "              (default /tmp/rgsp-audio.sock)\n"
        "  -A          video only, ignore audio\n"
        "  -d SECS     capture duration in seconds     (default 30)\n"
        "  -f FPS      target frame rate               (default 30)\n"
        "  -n FRAMES   stop after N frames             (overrides -d)\n"
        "  --dump-hdr  dump the raw SPS/PPS parameter struct and exit\n"
        "  -v          verbose per-frame logging\n",
        argv0);
}

/* ── main ────────────────────────────────────────────────────────────────── */

int main(int argc, char **argv)
{
    const char *out_path = "cast.h264";
    int duration = 30, fps = 30, max_frames = 0, dump_hdr = 0;
    /* Default: hand the framebuffer to the VE untouched. Allwinner names the
     * formats by 32-bit word order, so VENC_PIXEL_ARGB (12) is the one whose
     * byte layout is B,G,R,A — exactly /dev/fb0. Verified against the CPU
     * conversion path at 42.2 dB PSNR on identical screen content.
     * Use -i 0 for the NV12 reference path (11.8x more CPU). */
    int in_fmt = VENC_PIXEL_ARGB;
    int stride_bytes = 0;               /* -S: pass stride in bytes not pixels */
    const char *audio_tee = "/tmp/rgsp-audio.sock"; /* -a: pump socket or tee file */
    int audio_off = 0;                  /* -A: video only */
    int rc = 1;

    for (int i = 1; i < argc; i++) {
        if      (!strcmp(argv[i], "-o") && i + 1 < argc) out_path   = argv[++i];
        else if (!strcmp(argv[i], "-d") && i + 1 < argc) duration   = atoi(argv[++i]);
        else if (!strcmp(argv[i], "-f") && i + 1 < argc) fps        = atoi(argv[++i]);
        else if (!strcmp(argv[i], "-n") && i + 1 < argc) max_frames = atoi(argv[++i]);
        else if (!strcmp(argv[i], "-i") && i + 1 < argc) in_fmt     = atoi(argv[++i]);
        else if (!strcmp(argv[i], "-a") && i + 1 < argc) audio_tee  = argv[++i];
        else if (!strcmp(argv[i], "-A"))                 audio_off  = 1;
        else if (!strcmp(argv[i], "-S"))                 stride_bytes = 1;
        else if (!strcmp(argv[i], "--dump-hdr"))         dump_hdr   = 1;
        else if (!strcmp(argv[i], "-v"))                 g_verbose  = 1;
        else { usage(argv[0]); return 2; }
    }
    if (fps <= 0) fps = 30;
    if (max_frames <= 0) max_frames = duration * fps;

    signal(SIGINT, on_signal);
    signal(SIGTERM, on_signal);

    /* Resources, released in reverse order at `done`. */
    int             fb_fd  = -1;
    FILE           *out    = NULL;
    uint8_t        *fb_buf = NULL;
    VideoEncoder   *enc    = NULL;
    int             audio_fd = -1;
    FILE           *audio_out = NULL;
    char            audio_path[512] = {0};
    long long       audio_bytes = 0;
    ScMemOpsS      *memops = NULL;
    int             mem_open = 0, buffers_alloced = 0, enc_inited = 0;
    VencInputBuffer *held  = NULL;      /* input buffer currently checked out */
    VencInputBuffer  inbuf, used;

    if (load_libs() != 0) goto done;

    /* ── framebuffer ─────────────────────────────────────────────────── */
    fb_fd = open("/dev/fb0", O_RDONLY);
    if (fb_fd < 0) { LOG("open(/dev/fb0): %s", strerror(errno)); goto done; }

    struct fb_var_screeninfo vinfo;
    struct fb_fix_screeninfo finfo;
    if (ioctl(fb_fd, FBIOGET_VSCREENINFO, &vinfo) < 0 ||
        ioctl(fb_fd, FBIOGET_FSCREENINFO, &finfo) < 0) {
        LOG("FBIOGET_*SCREENINFO: %s", strerror(errno));
        goto done;
    }

    unsigned w = vinfo.xres, h = vinfo.yres, bpp = vinfo.bits_per_pixel;
    unsigned pitch = finfo.line_length;
    if (bpp != 32 && bpp != 16) { LOG("unsupported bpp %u", bpp); goto done; }
    /* The VE wants 16-aligned dimensions; 720x480 already satisfies this. */
    if (w % 16 || h % 16) LOG("warning: %ux%u is not 16-aligned, VE may reject it", w, h);

    size_t frame_bytes = (size_t)pitch * h;
    fb_buf = malloc(frame_bytes);
    if (!fb_buf) { LOG("out of memory for framebuffer copy"); goto done; }

    /* Two optimisations were measured here and both are worse — do not retry
     * them without new information:
     *
     *  - Zero-copy (encode straight from fb physical memory, smem_start):
     *    produces a corrupt bitstream. The VE reaches memory through an IOMMU
     *    that only maps ION allocations, so a raw framebuffer address is
     *    meaningless to it. Would need dmabuf export, which this fbdev driver
     *    does not provide.
     *  - mmap the framebuffer and copy from it, saving one copy: 19.90 ms per
     *    frame versus 1.44 ms for pread. Framebuffer mappings are uncached, so
     *    CPU reads go to DRAM one access at a time; pread's kernel-side bulk
     *    copy is dramatically faster.
     *
     * pread into a heap buffer, then memcpy into ION, is the fast path. */

    LOG("framebuffer %ux%u %ubpp pitch=%u virtual=%ux%u",
        w, h, bpp, pitch, vinfo.xres_virtual, vinfo.yres_virtual);
    /* smem_start is the framebuffer's physical address. If it is exposed, the
     * VE may be able to DMA straight out of it and skip the copy into ION. */
    LOG("fb physical: smem_start=0x%lx smem_len=%u",
        (unsigned long)finfo.smem_start, finfo.smem_len);

    /* ── encoder ─────────────────────────────────────────────────────── */
    fn_GetVeOpsS get_ve = (fn_GetVeOpsS)dlsym(g_libVE, "GetVeOpsS");
    fn_GetOpsS   get_mem = (fn_GetOpsS)dlsym(g_libMem, "MemAdapterGetOpsS");
    if (!get_ve || !get_mem) { LOG("GetVeOpsS / MemAdapterGetOpsS missing"); goto done; }

    void *veops = get_ve(0);
    memops = (ScMemOpsS *)get_mem();
    if (!veops || !memops) { LOG("ops NULL"); goto done; }
    if (memops->open() < 0) { LOG("CdcMemOpen failed"); goto done; }
    mem_open = 1;

    if (memops->get_ve_addr_offset)
        LOG("ve_addr_offset=0x%x", memops->get_ve_addr_offset());

    enc = p_VideoEncCreate(VENC_CODEC_H264);
    if (!enc) { LOG("VideoEncCreate failed"); goto done; }

    VencBaseConfig bcfg;
    memset(&bcfg, 0, sizeof bcfg);
    /* Ask the encoder for NALU output, which emits SPS/PPS in-band ahead of
     * the IDR. Without it the parameter sets live in VE SRAM that the CPU
     * cannot read (VideoEncGetParameter hands back a VE bus address), and the
     * stream is undecodable without hardcoding them per resolution. */
    bcfg.bEncH264Nalu = 1;
    bcfg.nInputWidth = w; bcfg.nInputHeight = h;
    bcfg.nDstWidth   = w; bcfg.nDstHeight   = h;
    bcfg.nStride     = stride_bytes ? pitch : w;
    bcfg.eInputFormat = (VENC_PIXEL_FMT)in_fmt;
    bcfg.memops = memops; bcfg.veOpsS = veops; bcfg.pVeOpsSelf = NULL;

    if (p_VideoEncInit(enc, &bcfg) != 0) { LOG("VideoEncInit failed"); goto done; }
    enc_inited = 1;

    VencAllocateBufferParam bp;
    memset(&bp, 0, sizeof bp);
    int rgb_in = (in_fmt >= VENC_PIXEL_ARGB && in_fmt <= VENC_PIXEL_BGRA);
    bp.nBufferNum = 1;
    bp.nSizeY     = rgb_in ? w * h * 4 : w * h;
    bp.nSizeC     = rgb_in ? 0         : w * h / 2;
    if (p_AllocInputBuffer(enc, &bp) != 0) { LOG("AllocInputBuffer failed"); goto done; }
    buffers_alloced = 1;

    LOG("encoder ready: %ux%u fmt=%d (%s) stride=%u -> H.264 @ %d fps",
        w, h, in_fmt, rgb_in ? "RGB passthrough" : "NV12 via CPU convert",
        bcfg.nStride, fps);

    /* SPS/PPS is fetched after the first frame is encoded — see fetch_sps_pps()
     * below. The parameter set does not exist until then: querying beforehand
     * returns a pointer with nLength=0. */
    unsigned char sps_pps[512];
    unsigned sps_pps_len = 0;
    int sps_pps_written = 0;

    out = fopen(out_path, "wb");
    if (!out) { LOG("fopen(%s): %s", out_path, strerror(errno)); goto done; }

    /* Audio comes from an ALSA `type file` tee that the sound server writes
     * continuously (see scripts/install-audio-tee.sh). Seek to the end at
     * capture start so we copy only what plays during this recording, then
     * follow the file as it grows. */
    if (!audio_off) {
        /* Two sources are supported. A Unix socket is rgsp-audio-pump, which
         * ALSA spawns and which streams live audio with nothing on disk. A
         * regular file is the older `type file` tee, followed from EOF. */
        struct stat ast;
        int is_sock = (stat(audio_tee, &ast) == 0) && S_ISSOCK(ast.st_mode);

        if (is_sock) {
            audio_fd = socket(AF_UNIX, SOCK_STREAM, 0);
            if (audio_fd >= 0) {
                struct sockaddr_un sa;
                memset(&sa, 0, sizeof sa);
                sa.sun_family = AF_UNIX;
                snprintf(sa.sun_path, sizeof sa.sun_path, "%s", audio_tee);
                if (connect(audio_fd, (struct sockaddr *)&sa, sizeof sa) < 0) {
                    LOG("audio: connect(%s): %s (recording video only)",
                        audio_tee, strerror(errno));
                    close(audio_fd); audio_fd = -1;
                } else {
                    fcntl(audio_fd, F_SETFL, O_NONBLOCK);
                }
            }
        } else {
            audio_fd = open(audio_tee, O_RDONLY);
            if (audio_fd >= 0) lseek(audio_fd, 0, SEEK_END);
        }

        if (audio_fd < 0) {
            LOG("audio: %s: %s (recording video only)", audio_tee, strerror(errno));
        } else {
            snprintf(audio_path, sizeof audio_path, "%s.pcm", out_path);
            audio_out = fopen(audio_path, "wb");
            if (!audio_out) {
                LOG("audio: fopen(%s): %s", audio_path, strerror(errno));
                close(audio_fd); audio_fd = -1;
            } else {
                LOG("audio: %s %s -> %s (s16le 48000 Hz stereo)",
                    is_sock ? "streaming from" : "following",
                    audio_tee, audio_path);
            }
        }
    }
    if (sps_pps_len) fwrite(sps_pps, 1, sps_pps_len, out);

    /* ── capture loop ────────────────────────────────────────────────── */
    memset(&inbuf, 0, sizeof inbuf);
    if (p_GetOneAllocInputBuffer(enc, &inbuf) != 0) { LOG("GetOneAllocInputBuffer failed"); goto done; }
    held = &inbuf;

    const long frame_ns = 1000000000L / fps;
    long long t_start = now_ns(), next = t_start;
    long long bytes_out = 0, encode_ns = 0, convert_ns = 0;
    int frames = 0, keyframes = 0, short_reads = 0;

    while (!g_stop && frames < max_frames) {
        /* Capture the *visible* buffer: with double buffering, yoffset tells
         * us which half of the virtual framebuffer is currently on screen. */
        unsigned yoff = 0;
        if (ioctl(fb_fd, FBIOGET_VSCREENINFO, &vinfo) == 0) yoff = vinfo.yoffset;
        off_t fb_off = (off_t)yoff * pitch;

        ssize_t n = pread(fb_fd, fb_buf, frame_bytes, fb_off);
        if (n != (ssize_t)frame_bytes) { short_reads++; if (n <= 0) break; }
        const uint8_t *fb_src = fb_buf;

        long long t0 = now_ns();
        if (rgb_in) {
            /* No conversion: the VE ingests the framebuffer format as-is.
             * Still one copy, because the encoder reads from ION memory. */
            memcpy(inbuf._virY, fb_src, frame_bytes);
        } else if (bpp == 32) {
            bgra_to_nv12(fb_src, pitch, w, h, inbuf._virY, inbuf._virUV);
        } else {
            rgb565_to_nv12(fb_src, pitch, w, h, inbuf._virY, inbuf._virUV);
        }
        long long t1 = now_ns();
        convert_ns += t1 - t0;

        inbuf.nPts          = (long long)frames * (1000000LL / fps);
        inbuf.bIsFirstFrame = (frames == 0);

        p_FlushCacheAllocInputBuffer(enc, &inbuf);
        if (p_AddOneInputBuffer(enc, &inbuf) != 0) { LOG("AddOneInputBuffer failed"); break; }
        if (p_VideoEncodeOneFrame(enc) != 0)       { LOG("VideoEncodeOneFrame failed at frame %d", frames); break; }
        encode_ns += now_ns() - t1;

        /* Parameter sets exist only once a frame has been encoded, so grab
         * them after the first one and emit them ahead of any frame data. */
        if (!sps_pps_written) {
            sps_pps_len = fetch_sps_pps(enc, memops, sps_pps, sizeof sps_pps);
            if (sps_pps_len) {
                fwrite(sps_pps, 1, sps_pps_len, out);
                bytes_out += sps_pps_len;
                LOG("SPS/PPS: %u bytes", sps_pps_len);
                if (g_verbose)
                    hexdump("sps/pps", sps_pps, (int)(sps_pps_len > 32 ? 32 : sps_pps_len));
            } else {
                LOG("warning: no SPS/PPS - the file will not decode standalone");
            }
            sps_pps_written = 1;
            if (dump_hdr) { rc = 0; goto done; }
        }

        while (p_ValidBitstreamFrameNum(enc) > 0) {
            VencOutputBuffer ob;
            memset(&ob, 0, sizeof ob);
            if (p_GetOneBitstreamFrame(enc, &ob) != 0) break;

            /* The vendor exposes the frame as up to two segments of a ring
             * buffer (pData0/nSize0 + pData1/nSize1), with nTotalSize the sum.
             * Only trust the split when it adds up; otherwise treat pData0 as
             * one contiguous run of nTotalSize bytes, which is what
             * cedar-probe does and what this build appears to produce. */
            if (ob.pData0 && ob.nSize0 &&
                ob.nSize0 + ob.nSize1 == ob.nTotalSize) {
                bytes_out += write_avcc_as_annexb(out, ob.pData0, ob.nSize0);
                if (ob.pData1 && ob.nSize1)
                    bytes_out += write_avcc_as_annexb(out, ob.pData1, ob.nSize1);
            } else if (ob.pData0 && ob.nTotalSize) {
                size_t n2 = write_avcc_as_annexb(out, ob.pData0, ob.nTotalSize);
                if (n2 == 0) {   /* not AVCC after all — emit verbatim */
                    fwrite(ob.pData0, 1, ob.nTotalSize, out);
                    n2 = ob.nTotalSize;
                }
                bytes_out += n2;
            }

            if (ob.bIsKeyFrame) keyframes++;
            VLOG("frame %d: total=%u size0=%u size1=%u%s", frames,
                 ob.nTotalSize, ob.nSize0, ob.nSize1, ob.bIsKeyFrame ? " (key)" : "");
            p_FreeOneBitStreamFrame(enc, &ob);
        }

        /* Recycle the input buffer for the next frame. */
        memset(&used, 0, sizeof used);
        if (p_AlreadyUsedInputBuffer(enc, &used) == 0)
            p_ReturnOneAllocInputBuffer(enc, &used);
        memset(&inbuf, 0, sizeof inbuf);
        if (p_GetOneAllocInputBuffer(enc, &inbuf) != 0) { LOG("GetOneAllocInputBuffer failed at frame %d", frames); held = NULL; break; }

        if (audio_fd >= 0) {
            /* Copy however much the tee has written since last frame. A short
             * read just means no new audio yet. */
            unsigned char abuf[16384];
            ssize_t an;
            while ((an = read(audio_fd, abuf, sizeof abuf)) > 0) {
                fwrite(abuf, 1, (size_t)an, audio_out);
                audio_bytes += an;
            }
        }

        frames++;

        next += frame_ns;
        long long slack = next - now_ns();
        if (slack > 0) {
            struct timespec ts = { .tv_sec = slack / 1000000000LL,
                                   .tv_nsec = slack % 1000000000LL };
            nanosleep(&ts, NULL);
        }
    }

    /* Audio reaches the tee one ALSA buffer behind the wall clock (1024 frames
     * at 48 kHz = 21.3 ms). Waiting exactly that long before the final drain
     * lands the captured audio on the same end time as the last video frame;
     * draining immediately loses the tail, waiting longer overshoots it. */
    if (audio_fd >= 0) {
        struct timespec settle = { .tv_sec = 0, .tv_nsec = 21333333L };
        nanosleep(&settle, NULL);
        unsigned char abuf[16384];
        ssize_t an;
        while ((an = read(audio_fd, abuf, sizeof abuf)) > 0 && audio_out) {
            fwrite(abuf, 1, (size_t)an, audio_out);
            audio_bytes += an;
        }
    }

    {
        double secs = (now_ns() - t_start) / 1e9;
        LOG("captured %d frames (%d keyframes) in %.1f s = %.1f fps",
            frames, keyframes, secs, secs > 0 ? frames / secs : 0.0);
        LOG("output %lld bytes = %.0f kbps average",
            bytes_out, secs > 0 ? (bytes_out * 8.0 / 1000.0) / secs : 0.0);
        if (frames) {
            LOG("per frame: %s %.2f ms, encode %.2f ms",
            rgb_in ? "copy   " : "convert",
                convert_ns / 1e6 / frames, encode_ns / 1e6 / frames);
        }
        if (short_reads) LOG("warning: %d short framebuffer reads", short_reads);
        if (audio_bytes) {
            double asecs = audio_bytes / (48000.0 * 2 * 2);
            LOG("audio %lld bytes = %.1f s (%.2f s vs video; drift %+.0f ms)",
                audio_bytes, asecs, secs, (asecs - secs) * 1000.0);
        } else if (!audio_off) {
            LOG("audio: nothing captured - is the ALSA tee installed and a game running?");
        }
    }
    rc = 0;

done:
    /* Documented teardown order. Reached on every exit path, so the VE and
     * its ION allocations are always released. */
    if (audio_fd >= 0) close(audio_fd);
    if (audio_out) fclose(audio_out);
    if (out) fclose(out);
    if (enc) {
        if (held) {
            memset(&used, 0, sizeof used);
            if (p_AlreadyUsedInputBuffer && p_AlreadyUsedInputBuffer(enc, &used) == 0 &&
                p_ReturnOneAllocInputBuffer)
                p_ReturnOneAllocInputBuffer(enc, &used);
        }
        if (buffers_alloced && p_ReleaseAllocInputBuffer) p_ReleaseAllocInputBuffer(enc);
        if (enc_inited && p_VideoEncUnInit)               p_VideoEncUnInit(enc);
        if (p_VideoEncDestroy)                            p_VideoEncDestroy(enc);
    }
    if (mem_open && memops && memops->close) memops->close();
    free(fb_buf);
    if (fb_fd >= 0) close(fb_fd);
    if (g_libvenc) dlclose(g_libvenc);
    if (g_libMem)  dlclose(g_libMem);
    if (g_libVE)   dlclose(g_libVE);
    return rc;
}
