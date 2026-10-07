#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

/* Same ASCII cell-diff kernel in C, C++, Rust and JS. Not a complete TUI. */
static double now(void) {
    struct timespec value;
    clock_gettime(CLOCK_MONOTONIC, &value);
    return value.tv_sec + value.tv_nsec / 1e9;
}
int main(int argc, char **argv) {
    const size_t cells = 160 * 50;
    const unsigned frames = argc > 1 ? (unsigned)strtoul(argv[1], NULL, 10) : 30000;
    uint32_t *base = (uint32_t *)calloc(cells, sizeof(uint32_t));
    uint32_t *previous = (uint32_t *)calloc(cells, sizeof(uint32_t));
    uint32_t *next = (uint32_t *)calloc(cells, sizeof(uint32_t));
    if (!base || !previous || !next) return 2;
    for (size_t i = 0; i < cells; i++) base[i] = (uint32_t)(32 + (i * 13 + i / 160 * 7) % 95) | (uint32_t)((i / 160 % 8) << 8);
    uint32_t seed = 123456789, checksum = 2166136261u;
    uint64_t changed = 0;
    const double start = now();
    for (unsigned frame = 0; frame < frames; frame++) {
        memcpy(next, base, cells * sizeof(uint32_t));
        for (unsigned k = 0; k < 8; k++) {
            seed = seed * 1664525u + 1013904223u;
            size_t pos = cells - 320 + seed % 320;
            next[pos] = 32 + ((seed >> 16) % 95) | (7u << 8);
        }
        for (size_t i = 0; i < cells; i++) {
            if (next[i] != previous[i]) {
                changed++;
                checksum = (checksum ^ next[i] ^ (uint32_t)i) * 16777619u;
            }
        }
        uint32_t *swap = previous; previous = next; next = swap;
    }
    printf("{\"frames\":%u,\"kernel_ms\":%.6f,\"checksum\":%u,\"changed\":%llu}\n", frames, (now()-start)*1000, checksum, (unsigned long long)changed);
    free(base); free(previous); free(next);
}
