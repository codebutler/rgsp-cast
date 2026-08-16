/*
 * fmt-probe — ask the Cedar encoder which input pixel formats it supports.
 *
 * VENC_IndexParamCheckColorFormat (23) takes a VencCheckColorFormat{index,
 * eColorFormat} and is enumerated by incrementing index until it fails. If
 * BGRA appears in the list, the VE ingests the framebuffer format directly and
 * no CPU colour conversion is needed at all.
 */
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <dlfcn.h>

typedef struct VideoEncoder VideoEncoder;
typedef enum { VENC_CODEC_H264 = 0 } VENC_CODEC_TYPE;

#define VENC_IndexParamCheckColorFormat 23
#define VENC_IndexParamMAXSupportSize   22

typedef struct { int index; int eColorFormat; unsigned char _tail[128]; } VencCheckColorFormat;
typedef struct { unsigned int nWidth, nHeight; unsigned char _tail[64]; } VencSize;

static const char *fmt_name(int f)
{
    static const char *n[] = {
        "YUV420SP(NV12)", "YVU420SP(NV21)", "YUV420P", "YVU420P",
        "YUV422SP", "YVU422SP", "YUV422P", "YVU422P",
        "YUYV422", "UYVY422", "YVYU422", "VYUY422",
        "ARGB", "RGBA", "ABGR", "BGRA",
        "TILE_32X32", "TILE_128X32", "AFBC_AW", "LBC_AW",
    };
    return (f >= 0 && f < (int)(sizeof n / sizeof n[0])) ? n[f] : "?";
}

int main(void)
{
    void *lve  = dlopen("libVE.so", RTLD_LAZY | RTLD_GLOBAL);
    void *lmem = dlopen("libMemAdapter.so", RTLD_LAZY | RTLD_GLOBAL);
    void *lenc = dlopen("libvencoder.so", RTLD_LAZY | RTLD_GLOBAL);
    if (!lve || !lmem || !lenc) { printf("dlopen failed: %s\n", dlerror()); return 1; }

    VideoEncoder *(*create)(VENC_CODEC_TYPE) = dlsym(lenc, "VideoEncCreate");
    void (*destroy)(VideoEncoder *)          = dlsym(lenc, "VideoEncDestroy");
    int (*getparam)(VideoEncoder *, int, void *) = dlsym(lenc, "VideoEncGetParameter");
    if (!create || !getparam) { printf("missing symbols\n"); return 1; }

    VideoEncoder *enc = create(VENC_CODEC_H264);
    if (!enc) { printf("VideoEncCreate failed\n"); return 1; }

    VencSize sz;
    memset(&sz, 0, sizeof sz);
    if (getparam(enc, VENC_IndexParamMAXSupportSize, &sz) == 0)
        printf("max encode size: %ux%u\n", sz.nWidth, sz.nHeight);
    else
        printf("max encode size: query failed\n");

    printf("supported input formats:\n");
    int found = 0;
    for (int i = 0; i < 24; i++) {
        VencCheckColorFormat c;
        memset(&c, 0, sizeof c);
        c.index = i;
        if (getparam(enc, VENC_IndexParamCheckColorFormat, &c) != 0) break;
        printf("  [%2d] %-2d %s\n", i, c.eColorFormat, fmt_name(c.eColorFormat));
        if (c.eColorFormat == 15) found = 1;   /* VENC_PIXEL_BGRA */
    }
    printf("%s\n", found ? "=> BGRA IS supported: no CPU conversion needed"
                         : "=> BGRA not listed");

    if (destroy) destroy(enc);
    return 0;
}
