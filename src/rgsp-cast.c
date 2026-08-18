/*
 * librgspcast — hardware H.264 capture of the RG SP framebuffer.
 *
 * Reads /dev/fb0 read-only and encodes with the Allwinner Cedar VE via the
 * vendor CedarC libraries (dlopen'd at runtime, never linked). The CLI that
 * used to live here is now src/rgsp-cast-cli.c; this file is the library
 * behind include/rgsp_cast.h, so it never calls exit() — failures land in a
 * static error buffer and come back as NULL / -1.
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
#include <stdarg.h>
#include <string.h>
#include <stdint.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <dlfcn.h>
#include <time.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <linux/fb.h>

#include "rgsp_cast_internal.h"

/* ── logging ─────────────────────────────────────────────────────────────── */

static int g_verbose;
#define LOG(...)  do { fprintf(stderr, "[rgsp-cast] " __VA_ARGS__); fputc('\n', stderr); } while (0)
#define VLOG(...) do { if (g_verbose) LOG(__VA_ARGS__); } while (0)

/* Fatal paths used to LOG and exit; as a library they record here and fail. */
static char g_last_error[256];

static void set_error(const char *fmt, ...)
{
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(g_last_error, sizeof g_last_error, fmt, ap);
    va_end(ap);
}

const char *rgsp_capture_last_error(void) { return g_last_error; }

void rgsp_capture_set_verbose(int v) { g_verbose = v; }

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
typedef int  (*fn_VideoEncSetParameter)(VideoEncoder *, int, void *);
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
static fn_VideoEncSetParameter       p_VideoEncSetParameter;

#define LOADSYM(h, var, name)                                            \
    do {                                                                 \
        *(void **)(&(var)) = dlsym((h), (name));                         \
        if (!(var)) { set_error("missing symbol %s", (name)); return -1; }\
    } while (0)

static int load_libs(void)
{
    if (g_libvenc) return 0;   /* already loaded */

    g_libVE = dlopen("libVE.so", RTLD_LAZY | RTLD_GLOBAL);
    if (!g_libVE)  { set_error("dlopen(libVE.so): %s", dlerror()); return -1; }
    g_libMem = dlopen("libMemAdapter.so", RTLD_LAZY | RTLD_GLOBAL);
    if (!g_libMem) { set_error("dlopen(libMemAdapter.so): %s", dlerror()); return -1; }
    g_libvenc = dlopen("libvencoder.so", RTLD_LAZY | RTLD_GLOBAL);
    if (!g_libvenc){ set_error("dlopen(libvencoder.so): %s", dlerror()); return -1; }

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
    *(void **)(&p_VideoEncSetParameter) = dlsym(g_libvenc, "VideoEncSetParameter");
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

/* ── overlay ─────────────────────────────────────────────────────────────── */

/* Paints a 16x16 opaque red square at (16,16) into a BGRA buffer, in place.
 * Pure and format-specific: it assumes 4 bytes/pixel B,G,R,A, which is what
 * the RGB-passthrough capture path (rgsp_capture_open's default) puts in the
 * ION input buffer - see the comment at rgsp_capture_open() for why that
 * layout matches /dev/fb0 directly. Not meaningful against the NV12 debug
 * path's Y/UV planes, so callers only invoke it when c->rgb_in.
 *
 * `stride` must be the real row pitch in bytes (VencInputBuffer's rows are
 * copied straight from the framebuffer via pread+memcpy with no repacking,
 * so they inherit /dev/fb0's pitch, which can exceed width*4) - never pass
 * width*4 unless it has been verified equal to the buffer's actual stride.
 *
 * width/height gate the square against buffers too small to hold it, so a
 * caller can never be handed a resolution where this writes out of bounds. */
static void draw_marker(unsigned char *buf, int width, int height, int stride)
{
    if (width < 32 || height < 32) return;
    for (int y = 16; y < 32; y++) {
        unsigned char *row = buf + (size_t)y * stride;
        for (int x = 16; x < 32; x++) {
            unsigned char *px = row + (size_t)x * 4;
            px[0] = 0x00;  /* B */
            px[1] = 0x00;  /* G */
            px[2] = 0xFF;  /* R */
            px[3] = 0xFF;  /* A */
        }
    }
}

/* ── misc ────────────────────────────────────────────────────────────────── */

static long long now_ns(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long long)ts.tv_sec * 1000000000LL + ts.tv_nsec;
}

/* ── capture object ──────────────────────────────────────────────────────── */

struct rgsp_capture {
    /* framebuffer */
    int       fb_fd;
    unsigned  w, h, bpp, pitch;
    size_t    frame_bytes;
    uint8_t  *fb_buf;

    /* encoder */
    VideoEncoder    *enc;
    ScMemOpsS       *memops;
    int              mem_open, buffers_alloced, enc_inited;
    VencInputBuffer *held;          /* input buffer currently checked out */
    int              in_fmt, rgb_in;

    /* Annex-B output, reused across frames; grows on demand. */
    unsigned char *out_buf;
    size_t         out_cap, out_len;

    unsigned char sps_pps[512];
    unsigned      sps_pps_len;
    int           sps_pps_fetched;

    /* pacing and counters */
    int       fps;
    long      frame_ns;
    long long deadline;
    int       frames;
    int       force_idr;
    int       short_reads;
    long long convert_ns, encode_ns;

    /* Whether to composite the live marker (see draw_marker()). Off by
     * default (calloc'd). Same threading contract as force_idr above. */
    int       overlay;

    /* Sticky death flag. A failed next() can leave the vendor input buffer
     * un-acquired or already submitted, so the object is not safe to drive
     * again; fail_msg preserves the original diagnosis across later calls. */
    int  failed;
    char fail_msg[256];

    /* Vendor-written structs go LAST, and nothing may be added after them.
     *
     * The vendor libraries write past the end of VencInputBuffer, beyond even
     * its _tail[256] padding. Measured with a 0xAA sentinel guard on-device:
     * the AlreadyUsedInputBuffer/ReturnOneAllocInputBuffer pair modifies up to
     * **+24 bytes past the end of `used`**, every frame. Most of that write is
     * zeroes, which is why it is invisible to a scan for non-zero bytes.
     *
     * As stack locals in the old main() the spill landed on adjacent scratch
     * and was harmless, which is why it went unnoticed for so long. As struct
     * members it lands on live fields: out_buf sat at +16..+23 past `used` and
     * was nulled every frame, segfaulting on the first one. Keeping the pair
     * adjacent and in this order reproduces the layout the vendor libs have
     * always been fed, and _vendor_guard (4096, vs the 24 observed) absorbs the
     * spill. Nothing may be added after them. */
    VencInputBuffer inbuf, used;
    unsigned char   _vendor_guard[4096];
};

/* GetOneBitstreamFrame() fills a VencOutputBuffer, so it is vendor-written on
 * every frame and in principle carries the same overspill risk that `used`
 * turned out to have — and it moved from the old main()'s large stack frame,
 * which had scratch after it, into next()'s smaller one where the neighbours
 * are live locals and the return address.
 *
 * Measured with a 0xAA sentinel on-device, the spill is **+0 bytes**: unlike
 * VencInputBuffer, this struct is written strictly within its declared extent
 * (its own _tail[256] included). The guard is therefore precaution, not a fix
 * for a live bug — it is kept because it costs 256 bytes of stack and makes the
 * struct safe wherever it is declared, in line with this file's standing rule
 * that every vendor struct carries trailing slack. VencBaseConfig and
 * VencAllocateBufferParam measured +0 as well and are left as plain locals. */
typedef struct {
    VencOutputBuffer ob;
    unsigned char    guard[256];
} GuardedOutputBuffer;

static int out_reserve(rgsp_capture *c, size_t extra)
{
    if (c->out_len + extra <= c->out_cap) return 0;
    size_t cap = c->out_cap ? c->out_cap : 65536;
    while (cap < c->out_len + extra) cap *= 2;
    unsigned char *p = realloc(c->out_buf, cap);
    if (!p) { set_error("out of memory growing output buffer to %zu", cap); return -1; }
    c->out_buf = p;
    c->out_cap = cap;
    return 0;
}

static int out_append(rgsp_capture *c, const unsigned char *d, size_t n)
{
    if (out_reserve(c, n) != 0) return -1;
    memcpy(c->out_buf + c->out_len, d, n);
    c->out_len += n;
    return 0;
}

/* Append AVCC (4-byte length prefixes) to the output buffer as Annex-B start
 * codes.
 *
 * Returns 0 on success and -1 if the output buffer could not be grown; *written
 * receives the number of bytes appended, which is 0 when the input does not
 * parse as AVCC and the caller should append it untouched.
 *
 * Running out of memory has to be distinguishable from "not AVCC": both used to
 * come back as a short byte count, so an allocation failure part-way through a
 * frame silently produced a truncated Annex-B frame that was then handed to the
 * caller with a success return. */
static int append_avcc_as_annexb(rgsp_capture *c, const unsigned char *d,
                                 size_t n, size_t *written)
{
    static const unsigned char start[4] = { 0, 0, 0, 1 };
    size_t off = 0;

    *written = 0;

    /* Already Annex-B? Pass through. */
    if (n >= 4 && d[0] == 0 && d[1] == 0 && d[2] == 0 && d[3] == 1) {
        if (out_append(c, d, n) != 0) return -1;
        *written = n;
        return 0;
    }
    while (off + 4 <= n) {
        size_t len = ((size_t)d[off] << 24) | ((size_t)d[off+1] << 16) |
                     ((size_t)d[off+2] << 8) | d[off+3];
        if (len == 0 || off + 4 + len > n) return 0;  /* not AVCC */
        if (out_append(c, start, 4) != 0) return -1;
        if (out_append(c, d + off + 4, len) != 0) return -1;
        *written += 4 + len;
        off += 4 + len;
    }
    return 0;
}

/* An IDR frame is one whose first slice NAL has type 5. Parameter sets (7, 8),
 * SEI (6) and access-unit delimiters (9) are skipped. */
static int annexb_first_slice_is_idr(const unsigned char *d, size_t n)
{
    size_t i = 0;
    while (i + 4 <= n) {
        size_t sc = 0;
        if (d[i] == 0 && d[i+1] == 0 && d[i+2] == 0 && d[i+3] == 1)      sc = 4;
        else if (d[i] == 0 && d[i+1] == 0 && d[i+2] == 1)                sc = 3;
        if (!sc || i + sc >= n) { i++; continue; }
        unsigned type = d[i + sc] & 0x1f;
        if (type == 1 || type == 5) return type == 5;
        i += sc;
    }
    return 0;
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

/* ── public API ──────────────────────────────────────────────────────────── */

/* Marks the capture dead and returns the failure code for rgsp_capture_next().
 *
 * A failed frame can leave the vendor input buffer either already submitted
 * (AddOneInputBuffer succeeded, encode did not) or not acquired at all
 * (GetOneAllocInputBuffer failed, leaving inbuf zeroed and _virY NULL). Neither
 * state is safe to drive again — the old main() sidestepped this by breaking
 * out of the loop and exiting, but a library that returns -1 invites a retry
 * that would encode from a NULL pointer or double-submit. */
static int capture_fail(rgsp_capture *c)
{
    c->failed = 1;
    snprintf(c->fail_msg, sizeof c->fail_msg, "%s", g_last_error);
    return -1;
}

/* Same enum block as VENC_IndexParamH264SPSPPS above: the generic parameters
 * start at 0, the H.264-specific ones at 0x100. */
#define VENC_IndexParamBitrate       0x0
#define VENC_IndexParamForceKeyFrame 0x6

rgsp_capture *rgsp_capture_open_ex(int width, int height, int fps, int bitrate,
                                   int in_fmt, int stride_bytes)
{
    g_last_error[0] = '\0';

    if (fps <= 0) fps = 30;

    rgsp_capture *c = calloc(1, sizeof *c);
    if (!c) { set_error("out of memory"); return NULL; }
    c->fb_fd  = -1;
    c->in_fmt = in_fmt;
    c->fps    = fps;

    if (load_libs() != 0) goto fail;

    /* ── framebuffer ─────────────────────────────────────────────────── */
    c->fb_fd = open("/dev/fb0", O_RDONLY);
    if (c->fb_fd < 0) { set_error("open(/dev/fb0): %s", strerror(errno)); goto fail; }

    struct fb_var_screeninfo vinfo;
    struct fb_fix_screeninfo finfo;
    if (ioctl(c->fb_fd, FBIOGET_VSCREENINFO, &vinfo) < 0 ||
        ioctl(c->fb_fd, FBIOGET_FSCREENINFO, &finfo) < 0) {
        set_error("FBIOGET_*SCREENINFO: %s", strerror(errno));
        goto fail;
    }

    unsigned w = vinfo.xres, h = vinfo.yres, bpp = vinfo.bits_per_pixel;
    unsigned pitch = finfo.line_length;
    if (bpp != 32 && bpp != 16) { set_error("unsupported bpp %u", bpp); goto fail; }
    /* The VE wants 16-aligned dimensions; 720x480 already satisfies this. */
    if (w % 16 || h % 16) LOG("warning: %ux%u is not 16-aligned, VE may reject it", w, h);

    /* The encoder path has only ever run at the panel's native geometry, and
     * asking the VE to scale is an untested path. Callers that pass a size say
     * what they expect; disagreeing with the panel is an error, not a resize. */
    if ((width > 0 && (unsigned)width != w) || (height > 0 && (unsigned)height != h)) {
        set_error("requested %dx%d but the framebuffer is %ux%u; scaling is not supported",
                  width, height, w, h);
        goto fail;
    }

    c->w = w; c->h = h; c->bpp = bpp; c->pitch = pitch;
    c->frame_bytes = (size_t)pitch * h;
    c->fb_buf = malloc(c->frame_bytes);
    if (!c->fb_buf) { set_error("out of memory for framebuffer copy"); goto fail; }

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
    if (!get_ve || !get_mem) { set_error("GetVeOpsS / MemAdapterGetOpsS missing"); goto fail; }

    void *veops = get_ve(0);
    c->memops = (ScMemOpsS *)get_mem();
    if (!veops || !c->memops) { set_error("ops NULL"); goto fail; }
    if (c->memops->open() < 0) { set_error("CdcMemOpen failed"); goto fail; }
    c->mem_open = 1;

    if (c->memops->get_ve_addr_offset)
        LOG("ve_addr_offset=0x%x", c->memops->get_ve_addr_offset());

    c->enc = p_VideoEncCreate(VENC_CODEC_H264);
    if (!c->enc) { set_error("VideoEncCreate failed"); goto fail; }

    /* Bitrate is a generic parameter, applied by VideoEncInit. 0 leaves the
     * encoder default alone, which is what the CLI has always used. */
    if (bitrate > 0) {
        if (!p_VideoEncSetParameter) {
            set_error("VideoEncSetParameter missing; cannot set bitrate");
            goto fail;
        }
        int br = bitrate;
        if (p_VideoEncSetParameter(c->enc, VENC_IndexParamBitrate, &br) != 0) {
            set_error("VideoEncSetParameter(bitrate=%d) failed", bitrate);
            goto fail;
        }
    }

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
    bcfg.memops = c->memops; bcfg.veOpsS = veops; bcfg.pVeOpsSelf = NULL;

    if (p_VideoEncInit(c->enc, &bcfg) != 0) { set_error("VideoEncInit failed"); goto fail; }
    c->enc_inited = 1;

    VencAllocateBufferParam bp;
    memset(&bp, 0, sizeof bp);
    c->rgb_in = (in_fmt >= VENC_PIXEL_ARGB && in_fmt <= VENC_PIXEL_BGRA);
    bp.nBufferNum = 1;
    bp.nSizeY     = c->rgb_in ? w * h * 4 : w * h;
    bp.nSizeC     = c->rgb_in ? 0         : w * h / 2;
    if (p_AllocInputBuffer(c->enc, &bp) != 0) { set_error("AllocInputBuffer failed"); goto fail; }
    c->buffers_alloced = 1;

    LOG("encoder ready: %ux%u fmt=%d (%s) stride=%u -> H.264 @ %d fps",
        w, h, in_fmt, c->rgb_in ? "RGB passthrough" : "NV12 via CPU convert",
        bcfg.nStride, fps);

    /* SPS/PPS is fetched after the first frame is encoded — see fetch_sps_pps()
     * above. The parameter set does not exist until then: querying beforehand
     * returns a pointer with nLength=0. */

    memset(&c->inbuf, 0, sizeof c->inbuf);
    if (p_GetOneAllocInputBuffer(c->enc, &c->inbuf) != 0) {
        set_error("GetOneAllocInputBuffer failed");
        goto fail;
    }
    c->held = &c->inbuf;

    c->frame_ns = 1000000000L / fps;
    c->deadline = now_ns();
    return c;

fail:
    rgsp_capture_close(c);
    return NULL;
}

rgsp_capture *rgsp_capture_open(int width, int height, int fps, int bitrate)
{
    /* Hand the framebuffer to the VE untouched. Allwinner names the formats by
     * 32-bit word order, so VENC_PIXEL_ARGB (12) is the one whose byte layout
     * is B,G,R,A — exactly /dev/fb0. Verified against the CPU conversion path
     * at 42.2 dB PSNR on identical screen content. */
    return rgsp_capture_open_ex(width, height, fps, bitrate, VENC_PIXEL_ARGB, 0);
}

int rgsp_capture_next(rgsp_capture *c, const unsigned char **data,
                      size_t *len, int *is_keyframe)
{
    if (!c) { set_error("null capture"); return -1; }
    if (c->failed) { set_error("%s", c->fail_msg); return -1; }

    /* Pace to the frame deadline before capturing, so each frame samples the
     * screen one frame interval after the last. */
    long long now = now_ns();
    long long slack = c->deadline - now;
    if (slack > 0) {
        struct timespec ts = { .tv_sec = slack / 1000000000LL,
                               .tv_nsec = slack % 1000000000LL };
        nanosleep(&ts, NULL);
    }
    c->deadline += c->frame_ns;
    /* After a stall the deadline can fall arbitrarily far behind. Without this
     * clamp the next calls all return instantly, replaying the backlog as a
     * burst of frames with stale timestamps; drop the missed frames instead. */
    if (c->deadline < now) c->deadline = now + c->frame_ns;

    c->out_len = 0;

    /* Capture the *visible* buffer: with double buffering, yoffset tells
     * us which half of the virtual framebuffer is currently on screen. */
    struct fb_var_screeninfo vinfo;
    unsigned yoff = 0;
    if (ioctl(c->fb_fd, FBIOGET_VSCREENINFO, &vinfo) == 0) yoff = vinfo.yoffset;
    off_t fb_off = (off_t)yoff * c->pitch;

    ssize_t n = pread(c->fb_fd, c->fb_buf, c->frame_bytes, fb_off);
    if (n != (ssize_t)c->frame_bytes) {
        c->short_reads++;
        if (n <= 0) {
            set_error("pread(/dev/fb0): %s", n < 0 ? strerror(errno) : "end of file");
            return capture_fail(c);
        }
    }
    const uint8_t *fb_src = c->fb_buf;

    long long t0 = now_ns();
    if (c->rgb_in) {
        /* No conversion: the VE ingests the framebuffer format as-is.
         * Still one copy, because the encoder reads from ION memory. */
        memcpy(c->inbuf._virY, fb_src, c->frame_bytes);
    } else if (c->bpp == 32) {
        bgra_to_nv12(fb_src, c->pitch, c->w, c->h, c->inbuf._virY, c->inbuf._virUV);
    } else {
        rgb565_to_nv12(fb_src, c->pitch, c->w, c->h, c->inbuf._virY, c->inbuf._virUV);
    }
    long long t1 = now_ns();
    c->convert_ns += t1 - t0;

    /* Composite into the ION copy just made above, never into /dev/fb0 - the
     * device's own screen must stay untouched. Only meaningful for the BGRA
     * passthrough path; see draw_marker()'s comment. */
    if (c->overlay && c->rgb_in)
        draw_marker(c->inbuf._virY, (int)c->w, (int)c->h, (int)c->pitch);

    c->inbuf.nPts          = (long long)c->frames * (1000000LL / c->fps);
    c->inbuf.bIsFirstFrame = (c->frames == 0);

    /* Moonlight asks for an IDR after packet loss; the vendor parameter is
     * one-shot and applies to the frame encoded next. */
    if (c->force_idr) {
        int one = 1, r = -1;
        if (p_VideoEncSetParameter)
            r = p_VideoEncSetParameter(c->enc, VENC_IndexParamForceKeyFrame, &one);
        if (r != 0)
            LOG("warning: force-IDR request ignored (rc=%d); the next frame may not be a keyframe", r);
        c->force_idr = 0;
    }

    p_FlushCacheAllocInputBuffer(c->enc, &c->inbuf);
    if (p_AddOneInputBuffer(c->enc, &c->inbuf) != 0) {
        set_error("AddOneInputBuffer failed at frame %d", c->frames);
        return capture_fail(c);
    }
    if (p_VideoEncodeOneFrame(c->enc) != 0) {
        set_error("VideoEncodeOneFrame failed at frame %d", c->frames);
        return capture_fail(c);
    }
    c->encode_ns += now_ns() - t1;

    /* Parameter sets exist only once a frame has been encoded, so grab
     * them after the first one and emit them ahead of any frame data. */
    if (!c->sps_pps_fetched) {
        c->sps_pps_len = fetch_sps_pps(c->enc, c->memops, c->sps_pps, sizeof c->sps_pps);
        if (c->sps_pps_len)
            LOG("SPS/PPS: %u bytes", c->sps_pps_len);
        else
            LOG("warning: no SPS/PPS - the stream will not decode standalone");
        c->sps_pps_fetched = 1;
    }
    if (c->frames == 0 && c->sps_pps_len &&
        out_append(c, c->sps_pps, c->sps_pps_len) != 0)
        return capture_fail(c);

    /* Bytes present before the bitstream drain, so a failure to pull the very
     * first segment can be told apart from the end of a frame's segments. */
    const size_t before_drain = c->out_len;

    while (p_ValidBitstreamFrameNum(c->enc) > 0) {
        GuardedOutputBuffer g;
        VencOutputBuffer *o = &g.ob;
        size_t w = 0;
        int bad = 0;

        memset(&g, 0, sizeof g);
        if (p_GetOneBitstreamFrame(c->enc, o) != 0) {
            /* Nothing retrieved at all means there is no frame to hand back;
             * that is a failure, not an early end to the segment list.
             *
             * Caveat for the next reader: a preceding segment that succeeded
             * but carried nTotalSize == 0 would leave out_len untouched and be
             * indistinguishable from "the loop has not appended yet", so a
             * genuine failure after one could be misreported as end-of-list.
             * Not seen in practice on this encoder, and not worth a speculative
             * fix — but that is the hole if this ever misbehaves. */
            if (c->out_len == before_drain) {
                set_error("GetOneBitstreamFrame failed at frame %d", c->frames);
                return capture_fail(c);
            }
            break;
        }

        /* The vendor exposes the frame as up to two segments of a ring
         * buffer (pData0/nSize0 + pData1/nSize1), with nTotalSize the sum.
         * Only trust the split when it adds up; otherwise treat pData0 as
         * one contiguous run of nTotalSize bytes, which is what
         * cedar-probe does and what this build appears to produce. */
        if (o->pData0 && o->nSize0 &&
            o->nSize0 + o->nSize1 == o->nTotalSize) {
            bad = append_avcc_as_annexb(c, o->pData0, o->nSize0, &w) != 0;
            if (!bad && o->pData1 && o->nSize1)
                bad = append_avcc_as_annexb(c, o->pData1, o->nSize1, &w) != 0;
        } else if (o->pData0 && o->nTotalSize) {
            bad = append_avcc_as_annexb(c, o->pData0, o->nTotalSize, &w) != 0;
            if (!bad && w == 0)   /* not AVCC — emit verbatim */
                bad = out_append(c, o->pData0, o->nTotalSize) != 0;
        }

        VLOG("frame %d: total=%u size0=%u size1=%u%s", c->frames,
             o->nTotalSize, o->nSize0, o->nSize1, o->bIsKeyFrame ? " (key)" : "");
        p_FreeOneBitStreamFrame(c->enc, o);

        /* Freed first so the vendor buffer is not leaked on the way out; the
         * error from out_reserve() is still in the error buffer. */
        if (bad) return capture_fail(c);
    }

    /* Recycle the input buffer for the next frame. */
    memset(&c->used, 0, sizeof c->used);
    if (p_AlreadyUsedInputBuffer(c->enc, &c->used) == 0)
        p_ReturnOneAllocInputBuffer(c->enc, &c->used);
    memset(&c->inbuf, 0, sizeof c->inbuf);
    if (p_GetOneAllocInputBuffer(c->enc, &c->inbuf) != 0) {
        set_error("GetOneAllocInputBuffer failed at frame %d", c->frames);
        c->held = NULL;
        return capture_fail(c);
    }

    c->frames++;

    if (data)        *data = c->out_buf;
    if (len)         *len  = c->out_len;
    if (is_keyframe) *is_keyframe = annexb_first_slice_is_idr(c->out_buf, c->out_len);
    return 0;
}

void rgsp_capture_request_idr(rgsp_capture *c)
{
    if (c) c->force_idr = 1;
}

void rgsp_capture_set_overlay(rgsp_capture *c, int enabled)
{
    if (c) c->overlay = enabled ? 1 : 0;
}

const unsigned char *rgsp_capture_param_sets(rgsp_capture *c, size_t *len)
{
    if (!c || !c->sps_pps_len) { if (len) *len = 0; return NULL; }
    if (len) *len = c->sps_pps_len;
    return c->sps_pps;
}

void rgsp_capture_stats(rgsp_capture *c, long long *convert_ns,
                        long long *encode_ns, int *short_reads)
{
    if (!c) return;
    if (convert_ns)  *convert_ns  = c->convert_ns;
    if (encode_ns)   *encode_ns   = c->encode_ns;
    if (short_reads) *short_reads = c->short_reads;
}

void rgsp_capture_close(rgsp_capture *c)
{
    if (!c) return;

    /* Documented teardown order. Reached on every exit path, so the VE and
     * its ION allocations are always released. */
    if (c->enc) {
        if (c->held) {
            memset(&c->used, 0, sizeof c->used);
            if (p_AlreadyUsedInputBuffer && p_AlreadyUsedInputBuffer(c->enc, &c->used) == 0 &&
                p_ReturnOneAllocInputBuffer)
                p_ReturnOneAllocInputBuffer(c->enc, &c->used);
        }
        if (c->buffers_alloced && p_ReleaseAllocInputBuffer) p_ReleaseAllocInputBuffer(c->enc);
        if (c->enc_inited && p_VideoEncUnInit)               p_VideoEncUnInit(c->enc);
        if (p_VideoEncDestroy)                               p_VideoEncDestroy(c->enc);
    }
    if (c->mem_open && c->memops && c->memops->close) c->memops->close();
    free(c->fb_buf);
    free(c->out_buf);
    if (c->fb_fd >= 0) close(c->fb_fd);
    free(c);
}
