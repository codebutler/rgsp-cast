/* Writes a loud 440 Hz sine into the loopback playback side, forever. */
#include <alsa/asoundlib.h>
#include <math.h>
#include <stdio.h>
int main(int argc, char **argv) {
    const char *dev = argc > 1 ? argv[1] : "hw:Loopback,0,0";
    snd_pcm_t *pcm; int err;
    if ((err = snd_pcm_open(&pcm, dev, SND_PCM_STREAM_PLAYBACK, 0)) < 0) {
        fprintf(stderr, "open %s: %s\n", dev, snd_strerror(err)); return 1; }
    if ((err = snd_pcm_set_params(pcm, SND_PCM_FORMAT_S16_LE, SND_PCM_ACCESS_RW_INTERLEAVED,
                                  2, 48000, 1, 100000)) < 0) {
        fprintf(stderr, "params: %s\n", snd_strerror(err)); return 1; }
    short buf[480*2]; double ph = 0;
    for (int i = 0; i < 2000; i++) {
        for (int f = 0; f < 480; f++) {
            short v = (short)(20000.0 * sin(ph)); ph += 2*M_PI*440.0/48000.0;
            buf[f*2] = v; buf[f*2+1] = v;
        }
        snd_pcm_sframes_t n = snd_pcm_writei(pcm, buf, 480);
        if (n < 0) { snd_pcm_recover(pcm, n, 1); }
    }
    snd_pcm_close(pcm); return 0;
}
