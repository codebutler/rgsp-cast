/*
 * rgsp-audio-pump — bridge game audio from ALSA to rgsp-cast, without touching
 * the filesystem.
 *
 * ALSA's `type file` plugin runs this as a command (a `file "|..."` entry) and
 * feeds the playback stream to our stdin. We hand it to whoever is connected to
 * a Unix socket, and discard it otherwise.
 *
 * Why a pump rather than a plain file or FIFO:
 *
 *   - A file grows without bound. Filling the 256 MB tmpfs makes ALSA writes
 *     fail with ENOSPC, and pcm_file.c propagates that straight to the
 *     application (:441-446 -> :493-495) — which silently kills game audio.
 *     There is no "ignore errors" option in the plugin.
 *   - A FIFO has no writer-side protection either: opening one O_WRONLY blocks
 *     until a reader exists, and a reader that goes away turns writes into
 *     EPIPE, which the plugin reports as EIO. Same outcome.
 *   - In pipe mode ALSA spawns this process itself, so a reader always exists
 *     for as long as the PCM is open.
 *
 * Nothing here may ever block or fail in a way that reaches ALSA:
 *   - stdin is drained on every wakeup regardless of client state
 *   - client sockets are non-blocking; a slow client drops samples
 *   - client errors only ever close that client
 *   - SIGPIPE is ignored
 *
 * Audio is whatever the PCM carries — s16le 48 kHz stereo for NextUI/minarch.
 */

#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <poll.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>

#define MAX_CLIENTS 4
#define CHUNK       16384

static const char *sock_path = "/tmp/rgsp-audio.sock";

int main(int argc, char **argv)
{
    if (argc > 1) sock_path = argv[1];

    /* A client that disappears must not kill us. */
    signal(SIGPIPE, SIG_IGN);

    int lfd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (lfd < 0) { perror("socket"); return 1; }

    struct sockaddr_un sa;
    memset(&sa, 0, sizeof sa);
    sa.sun_family = AF_UNIX;
    snprintf(sa.sun_path, sizeof sa.sun_path, "%s", sock_path);
    unlink(sock_path);                       /* stale node from a previous run */

    if (bind(lfd, (struct sockaddr *)&sa, sizeof sa) < 0) { perror("bind"); return 1; }
    if (listen(lfd, MAX_CLIENTS) < 0) { perror("listen"); return 1; }
    chmod(sock_path, 0666);
    fcntl(lfd, F_SETFL, O_NONBLOCK);

    int clients[MAX_CLIENTS];
    for (int i = 0; i < MAX_CLIENTS; i++) clients[i] = -1;

    unsigned char buf[CHUNK];
    unsigned long long total = 0, dropped = 0;

    for (;;) {
        struct pollfd pfd[2 + MAX_CLIENTS];
        int n = 0;
        pfd[n].fd = 0;    pfd[n].events = POLLIN; n++;   /* stdin from ALSA */
        pfd[n].fd = lfd;  pfd[n].events = POLLIN; n++;   /* new clients      */
        for (int i = 0; i < MAX_CLIENTS; i++)
            if (clients[i] >= 0) { pfd[n].fd = clients[i]; pfd[n].events = 0; n++; }

        if (poll(pfd, n, 1000) < 0) {
            if (errno == EINTR) continue;
            break;
        }

        if (pfd[1].revents & POLLIN) {
            int c = accept(lfd, NULL, NULL);
            if (c >= 0) {
                int slot = -1;
                for (int i = 0; i < MAX_CLIENTS; i++)
                    if (clients[i] < 0) { slot = i; break; }
                if (slot < 0) close(c);           /* full: refuse politely */
                else {
                    fcntl(c, F_SETFL, O_NONBLOCK);
                    clients[slot] = c;
                }
            }
        }

        if (pfd[0].revents & (POLLIN | POLLHUP)) {
            ssize_t got = read(0, buf, sizeof buf);
            if (got == 0) break;                  /* PCM closed: we are done */
            if (got < 0) {
                if (errno == EINTR || errno == EAGAIN) continue;
                break;
            }
            total += (unsigned long long)got;

            for (int i = 0; i < MAX_CLIENTS; i++) {
                if (clients[i] < 0) continue;
                ssize_t off = 0;
                while (off < got) {
                    ssize_t w = write(clients[i], buf + off, (size_t)(got - off));
                    if (w > 0) { off += w; continue; }
                    if (w < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) {
                        /* Consumer is behind. Drop the rest of this chunk
                         * rather than stall the game's audio thread. */
                        dropped += (unsigned long long)(got - off);
                        break;
                    }
                    close(clients[i]);            /* gone or broken */
                    clients[i] = -1;
                    break;
                }
            }
        }
    }

    for (int i = 0; i < MAX_CLIENTS; i++) if (clients[i] >= 0) close(clients[i]);
    close(lfd);
    unlink(sock_path);
    fprintf(stderr, "[rgsp-audio-pump] %llu bytes in, %llu dropped\n", total, dropped);
    return 0;
}
