/* Open -> capture -> close, twenty times in one process.
 *
 * The daemon does one of these cycles per Moonlight session, and nothing had
 * ever executed that path: the CLI opens once and exits, so every teardown was
 * followed immediately by process death. Two things make it worth checking.
 * `dlclose` was deliberately dropped and load_libs() made idempotent so a
 * reopen works at all; and the vendor logs `CdcIonFree ... errno:22` on every
 * run, which is EINVAL on a free — the shape of an allocation not coming back.
 * Harmless in a process that is about to exit, not harmless in a long-lived
 * daemon.
 *
 * Reports RSS and ION usage between cycles. Flat means the log noise is noise.
 */
#include "../include/rgsp_cast.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define CYCLES        20
#define FRAMES_PER    5

/* Resident set size in kB from /proc/self/status. */
static long rss_kb(void)
{
    FILE *f = fopen("/proc/self/status", "r");
    if (!f) return -1;
    char line[256];
    long kb = -1;
    while (fgets(line, sizeof line, f))
        if (!strncmp(line, "VmRSS:", 6)) { kb = strtol(line + 6, NULL, 10); break; }
    fclose(f);
    return kb;
}

/* ION accounting, read from debugfs. Each heap file is a table of
 *
 *       client              pid             size
 *   nextui.elf             1517          4153344
 *
 * followed by an "orphaned allocations" section listing buffers whose client
 * is gone — which is precisely what a failing CdcIonFree would leave behind.
 * Sums live bytes across every heap, our own process's share, and the orphan
 * total. Falls back to MemFree only if debugfs is unreadable. */
struct ion_stat { long total, mine, orphan; const char *what; };

static void ion_read_heap(const char *path, long mypid, struct ion_stat *st)
{
    FILE *f = fopen(path, "r");
    if (!f) return;
    char line[512];
    int in_orphan = 0;
    while (fgets(line, sizeof line, f)) {
        if (strstr(line, "orphaned")) { in_orphan = 1; continue; }
        char name[128];
        long pid, size;
        if (sscanf(line, "%127s %ld %ld", name, &pid, &size) != 3) continue;
        if (in_orphan) st->orphan += size;
        else {
            st->total += size;
            if (pid == mypid) st->mine += size;
        }
    }
    fclose(f);
}

static void ion_stat(long mypid, struct ion_stat *st)
{
    static const char *heaps[] = {
        "/sys/kernel/debug/ion/heaps/cma",
        "/sys/kernel/debug/ion/heaps/secure",
        "/sys/kernel/debug/ion/heaps/sys_user",
    };
    memset(st, 0, sizeof *st);
    st->what = "ion";
    for (unsigned i = 0; i < sizeof heaps / sizeof heaps[0]; i++)
        ion_read_heap(heaps[i], mypid, st);

    if (st->total == 0) {   /* debugfs unreadable — fall back to MemFree */
        FILE *f = fopen("/proc/meminfo", "r");
        if (!f) { st->what = "unavailable"; return; }
        char line[256];
        while (fgets(line, sizeof line, f))
            if (!strncmp(line, "MemFree:", 8)) {
                st->total = strtol(line + 8, NULL, 10) * 1024;
                break;
            }
        fclose(f);
        st->what = "memfree";
    }
}

int main(void)
{
    long mypid = (long)getpid();
    long rss0 = 0, ion0 = 0, mine0 = 0, orph0 = 0;
    struct ion_stat st;

    printf("cycle  rss_kB  d_rss   ion_total  d_ion   ion_mine  orphaned\n");
    for (int i = 0; i < CYCLES; i++) {
        rgsp_capture *c = rgsp_capture_open(720, 480, 30, 2000000);
        if (!c) {
            fprintf(stderr, "cycle %d: open failed: %s\n", i, rgsp_capture_last_error());
            return 1;
        }
        for (int f = 0; f < FRAMES_PER; f++) {
            const unsigned char *d; size_t n; int k;
            if (rgsp_capture_next(c, &d, &n, &k) != 0) {
                fprintf(stderr, "cycle %d frame %d: %s\n", i, f, rgsp_capture_last_error());
                rgsp_capture_close(c);
                return 1;
            }
        }
        rgsp_capture_close(c);

        long rss = rss_kb();
        ion_stat(mypid, &st);
        if (i == 0) { rss0 = rss; ion0 = st.total; mine0 = st.mine; orph0 = st.orphan; }
        printf("%5d  %6ld  %+5ld   %9ld  %+6ld  %8ld  %8ld\n",
               i, rss, rss - rss0, st.total, st.total - ion0, st.mine, st.orphan);
        fflush(stdout);
    }

    long rss = rss_kb();
    ion_stat(mypid, &st);
    printf("\n%d cycles of open/%d frames/close completed (source: %s)\n",
           CYCLES, FRAMES_PER, st.what);
    printf("RSS       %ld -> %ld kB (%+ld kB over %d cycles)\n",
           rss0, rss, rss - rss0, CYCLES);
    printf("ION total %ld -> %ld bytes (%+ld)\n", ion0, st.total, st.total - ion0);
    printf("ION ours  %ld -> %ld bytes (%+ld)\n", mine0, st.mine, st.mine - mine0);
    printf("ION orphaned %ld -> %ld bytes (%+ld)\n",
           orph0, st.orphan, st.orphan - orph0);
    /* Reopening at all is the primary result; the numbers above are the
     * evidence for whether it is sustainable. */
    printf("PASS: reopen works %d times in one process\n", CYCLES);
    return 0;
}
