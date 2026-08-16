/*
 * alsa-cap — read an ALSA capture device and write raw s16le to a file.
 *
 * The device ships no arecord, and this is the read side rgsp-cast needs for
 * loopback audio anyway.
 *
 *   alsa-cap [-D device] [-d seconds] [-r rate] [-c channels] -o out.raw
 *
 * Defaults match what the emulator plays: s16le, 48 kHz, stereo.
 */
#define _GNU_SOURCE
#include <alsa/asoundlib.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

int main(int argc, char **argv)
{
    const char *dev = "hw:Loopback,1,0";
    const char *out = NULL;
    unsigned rate = 48000, secs = 3;
    unsigned channels = 2;
    int c;

    while ((c = getopt(argc, argv, "D:d:r:c:o:")) != -1) {
        switch (c) {
        case 'D': dev = optarg; break;
        case 'd': secs = (unsigned)atoi(optarg); break;
        case 'r': rate = (unsigned)atoi(optarg); break;
        case 'c': channels = (unsigned)atoi(optarg); break;
        case 'o': out = optarg; break;
        default:
            fprintf(stderr, "usage: %s [-D dev] [-d secs] [-r rate] [-c ch] -o out\n", argv[0]);
            return 2;
        }
    }
    if (!out) { fprintf(stderr, "-o is required\n"); return 2; }

    snd_pcm_t *pcm;
    int err = snd_pcm_open(&pcm, dev, SND_PCM_STREAM_CAPTURE, 0);
    if (err < 0) {
        fprintf(stderr, "open %s: %s\n", dev, snd_strerror(err));
        return 1;
    }

    unsigned actual_rate = rate;
    err = snd_pcm_set_params(pcm, SND_PCM_FORMAT_S16_LE,
                             SND_PCM_ACCESS_RW_INTERLEAVED,
                             channels, actual_rate,
                             1,          /* allow resampling */
                             200000);    /* 200 ms latency */
    if (err < 0) {
        fprintf(stderr, "set_params: %s\n", snd_strerror(err));
        snd_pcm_close(pcm);
        return 1;
    }

    /* Report what was actually negotiated: snd-aloop fails the capture side
     * with -EIO when the two ends of the cable disagree on format, rate or
     * channels (aloop.c loopback_check_format), so the negotiated values are
     * the first thing worth seeing. */
    {
        snd_pcm_hw_params_t *hw;
        snd_pcm_hw_params_alloca(&hw);
        if (snd_pcm_hw_params_current(pcm, hw) == 0) {
            unsigned r = 0, ch = 0; int dir = 0;
            snd_pcm_format_t fmt;
            snd_pcm_uframes_t period = 0, bufsz = 0;
            snd_pcm_hw_params_get_rate(hw, &r, &dir);
            snd_pcm_hw_params_get_channels(hw, &ch);
            snd_pcm_hw_params_get_format(hw, &fmt);
            snd_pcm_hw_params_get_period_size(hw, &period, &dir);
            snd_pcm_hw_params_get_buffer_size(hw, &bufsz);
            fprintf(stderr, "negotiated: %s %u Hz %u ch period=%lu buffer=%lu\n",
                    snd_pcm_format_name(fmt), r, ch,
                    (unsigned long)period, (unsigned long)bufsz);
        }
    }

    fprintf(stderr, "state after params: %s\n",
            snd_pcm_state_name(snd_pcm_state(pcm)));

    err = snd_pcm_prepare(pcm);
    fprintf(stderr, "prepare: %s (state %s)\n",
            err < 0 ? snd_strerror(err) : "ok",
            snd_pcm_state_name(snd_pcm_state(pcm)));

    err = snd_pcm_start(pcm);
    fprintf(stderr, "start: %s (state %s)\n",
            err < 0 ? snd_strerror(err) : "ok",
            snd_pcm_state_name(snd_pcm_state(pcm)));

    FILE *f = fopen(out, "wb");
    if (!f) { perror("fopen"); snd_pcm_close(pcm); return 1; }

    const snd_pcm_uframes_t chunk = 1024;
    short *buf = malloc(chunk * channels * sizeof(short));
    unsigned long want = (unsigned long)rate * secs, got = 0;
    unsigned long long nonzero = 0;

    while (got < want) {
        snd_pcm_sframes_t n = snd_pcm_readi(pcm, buf, chunk);
        if (n == -EPIPE) {                 /* overrun: the writer outran us */
            snd_pcm_prepare(pcm);
            continue;
        }
        if (n < 0) {
            fprintf(stderr, "readi: %s\n", snd_strerror((int)n));
            break;
        }
        fwrite(buf, sizeof(short) * channels, (size_t)n, f);
        for (snd_pcm_sframes_t i = 0; i < n * (snd_pcm_sframes_t)channels; i++)
            if (buf[i]) nonzero++;
        got += (unsigned long)n;
    }

    fprintf(stderr, "captured %lu frames (%lu requested), %llu non-zero samples\n",
            got, want, nonzero);

    free(buf);
    fclose(f);
    snd_pcm_close(pcm);
    return got ? 0 : 1;
}
