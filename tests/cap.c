/* Reads the loopback capture side the same way rgsp-host does: explicit
 * prepare+start, because snd-aloop rejects readi()'s implicit start. */
#include <alsa/asoundlib.h>
#include <stdio.h>
int main(int argc, char **argv) {
    const char *dev = argc > 1 ? argv[1] : "hw:Loopback,1,0";
    snd_pcm_t *pcm; int err;
    if ((err = snd_pcm_open(&pcm, dev, SND_PCM_STREAM_CAPTURE, 0)) < 0) {
        fprintf(stderr, "open %s: %s\n", dev, snd_strerror(err)); return 1; }
    if ((err = snd_pcm_set_params(pcm, SND_PCM_FORMAT_S16_LE, SND_PCM_ACCESS_RW_INTERLEAVED,
                                  2, 48000, 1, 100000)) < 0) {
        fprintf(stderr, "params: %s\n", snd_strerror(err)); return 1; }
    snd_pcm_prepare(pcm); snd_pcm_start(pcm);
    short buf[480*2]; long total = 0; int peak = 0;
    for (int i = 0; i < 200; i++) {
        snd_pcm_sframes_t n = snd_pcm_readi(pcm, buf, 480);
        if (n < 0) { snd_pcm_recover(pcm, n, 1); continue; }
        total += n;
        for (int j = 0; j < n*2; j++) { int a = buf[j] < 0 ? -buf[j] : buf[j]; if (a > peak) peak = a; }
    }
    printf("CAPTURE %s: frames=%ld peak=%d\n", dev, total, peak);
    snd_pcm_close(pcm); return 0;
}
