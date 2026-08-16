/*
 * rgsp-cast — CLI front end for librgspcast.
 *
 * Captures the framebuffer to an Annex-B .h264 file, optionally alongside the
 * PCM the ALSA tee is producing, and prints a timing summary. All of the
 * Cedar VE work lives in src/rgsp-cast.c behind include/rgsp_cast.h; this file
 * is the loop, the flags and the file I/O.
 */

#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <signal.h>
#include <time.h>
#include <sys/stat.h>
#include <sys/socket.h>
#include <sys/un.h>

#include "rgsp_cast_internal.h"

#define LOG(...)  do { fprintf(stderr, "[rgsp-cast] " __VA_ARGS__); fputc('\n', stderr); } while (0)

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

static void usage(const char *argv0)
{
    fprintf(stderr,
        "usage: %s [-o FILE] [-d SECS] [-f FPS] [-n FRAMES] [--dump-hdr] [-v]\n"
        "  -o FILE     output Annex-B .h264            (default cast.h264)\n"
        "  -i FMT      input format: 12=ARGB passthrough (default), 0=NV12\n"
        "  -b BPS      target bitrate                  (default: encoder's)\n"
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

int main(int argc, char **argv)
{
    const char *out_path = "cast.h264";
    int duration = 30, fps = 30, max_frames = 0, dump_hdr = 0;
    /* Default: hand the framebuffer to the VE untouched (VENC_PIXEL_ARGB=12,
     * whose byte layout is B,G,R,A — exactly /dev/fb0).
     * Use -i 0 for the NV12 reference path (11.8x more CPU). */
    int in_fmt = 12;
    int bitrate = 0;                    /* -b: 0 leaves the encoder default */
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
        else if (!strcmp(argv[i], "-b") && i + 1 < argc) bitrate    = atoi(argv[++i]);
        else if (!strcmp(argv[i], "-a") && i + 1 < argc) audio_tee  = argv[++i];
        else if (!strcmp(argv[i], "-A"))                 audio_off  = 1;
        else if (!strcmp(argv[i], "-S"))                 stride_bytes = 1;
        else if (!strcmp(argv[i], "--dump-hdr"))         dump_hdr   = 1;
        else if (!strcmp(argv[i], "-v"))                 rgsp_capture_set_verbose(1);
        else { usage(argv[0]); return 2; }
    }
    if (fps <= 0) fps = 30;
    if (max_frames <= 0) max_frames = duration * fps;

    signal(SIGINT, on_signal);
    signal(SIGTERM, on_signal);

    /* Resources, released in reverse order at `done`. */
    rgsp_capture *cap    = NULL;
    FILE         *out    = NULL;
    int           audio_fd = -1;
    FILE         *audio_out = NULL;
    char          audio_path[512] = {0};
    long long     audio_bytes = 0;
    int           rgb_in = (in_fmt >= 12 && in_fmt <= 15);

    cap = rgsp_capture_open_ex(0, 0, fps, bitrate, in_fmt, stride_bytes);
    if (!cap) { LOG("%s", rgsp_capture_last_error()); goto done; }

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

    /* ── capture loop ────────────────────────────────────────────────── */
    long long t_start = now_ns();
    long long bytes_out = 0;
    int frames = 0, keyframes = 0;

    while (!g_stop && frames < max_frames) {
        const unsigned char *data;
        size_t len;
        int key;

        if (rgsp_capture_next(cap, &data, &len, &key) != 0) {
            LOG("%s", rgsp_capture_last_error());
            break;
        }
        if (len) fwrite(data, 1, len, out);
        bytes_out += (long long)len;
        keyframes += key;

        if (dump_hdr) {
            size_t hn = 0;
            const unsigned char *hp = rgsp_capture_param_sets(cap, &hn);
            if (hp) hexdump("sps/pps", hp, (int)(hn > 32 ? 32 : hn));
            rc = 0;
            frames++;
            goto done;
        }

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
        long long convert_ns = 0, encode_ns = 0;
        int short_reads = 0;
        rgsp_capture_stats(cap, &convert_ns, &encode_ns, &short_reads);

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
    if (audio_fd >= 0) close(audio_fd);
    if (audio_out) fclose(audio_out);
    if (out) fclose(out);
    rgsp_capture_close(cap);
    return rc;
}
