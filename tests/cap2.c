/* Same hw params rgsp-host uses: explicit access/format/channels/rate and a
 * pinned period/buffer, rather than letting snd_pcm_set_params choose. */
#include <alsa/asoundlib.h>
#include <stdio.h>
#include <stdlib.h>
int main(int argc, char **argv) {
    const char *dev = "hw:Loopback,1,0";
    snd_pcm_uframes_t period = argc > 1 ? (snd_pcm_uframes_t)atoi(argv[1]) : 240;
    snd_pcm_uframes_t buffer = argc > 2 ? (snd_pcm_uframes_t)atoi(argv[2]) : 3840;
    snd_pcm_t *pcm; snd_pcm_hw_params_t *hw; int err;
    if ((err = snd_pcm_open(&pcm, dev, SND_PCM_STREAM_CAPTURE, 0)) < 0) {
        fprintf(stderr, "open: %s\n", snd_strerror(err)); return 1; }
    snd_pcm_hw_params_malloc(&hw);
    snd_pcm_hw_params_any(pcm, hw);
    snd_pcm_hw_params_set_access(pcm, hw, SND_PCM_ACCESS_RW_INTERLEAVED);
    snd_pcm_hw_params_set_format(pcm, hw, SND_PCM_FORMAT_S16_LE);
    snd_pcm_hw_params_set_channels(pcm, hw, 2);
    unsigned rate = 48000; snd_pcm_hw_params_set_rate_near(pcm, hw, &rate, 0);
    snd_pcm_hw_params_set_period_size_near(pcm, hw, &period, 0);
    snd_pcm_hw_params_set_buffer_size_near(pcm, hw, &buffer);
    if ((err = snd_pcm_hw_params(pcm, hw)) < 0) {
        fprintf(stderr, "hw_params: %s\n", snd_strerror(err)); return 1; }
    printf("negotiated period=%lu buffer=%lu\n", (unsigned long)period, (unsigned long)buffer);
    snd_pcm_prepare(pcm); snd_pcm_start(pcm);
    short buf[2400*2]; long total=0; int peak=0;
    for (int i = 0; i < 400; i++) {
        snd_pcm_sframes_t n = snd_pcm_readi(pcm, buf, period);
        if (n < 0) { snd_pcm_recover(pcm, n, 1); continue; }
        total += n;
        for (int j = 0; j < n*2; j++) { int a = buf[j]<0?-buf[j]:buf[j]; if (a>peak) peak=a; }
    }
    printf("period=%lu buffer=%lu frames=%ld peak=%d\n", (unsigned long)period, (unsigned long)buffer, total, peak);
    return 0;
}
