/* extern fn implementations for examples/circular_dma.phase (see spec
 * §3.9, §5.2). Hand-written, program-specific glue: the compiler only
 * generates the prototypes.
 *
 * Simulates a DMA engine in circular mode over a two-half buffer: each
 * pass over a half writes the next block number into every sample of it.
 */

#include "circular_dma.gen.h"

#include <stdio.h>

static const Sample *lo_half = NULL;
static unsigned next_block = 0;

static const char *half_name(const Sample *h) {
    return h == lo_half ? "lo" : "hi";
}

static void engine_fill(Sample *h, size_t n) {
    unsigned block = next_block++;
    for (size_t i = 0; i < n; i++) {
        h[i].value = (float)block;
    }
}

void start_circular(Sample *lo, size_t lo_len, Sample *hi, size_t hi_len) {
    lo_half = lo;
    printf("[sim] start_circular: engine running over %zu + %zu samples\n", lo_len, hi_len);
    engine_fill(lo, lo_len);
    engine_fill(hi, hi_len);
}

void wait_half(Sample *h, size_t h_len) {
    (void)h_len;
    printf("[sim] wait_half: %s is full\n", half_name(h));
}

void requeue(Sample *h, size_t h_len) {
    engine_fill(h, h_len); /* the engine's next pass over this half */
}

void stop_circular(Sample *lo, size_t lo_len, Sample *hi, size_t hi_len) {
    (void)lo;
    (void)lo_len;
    (void)hi;
    (void)hi_len;
    printf("[sim] stop_circular: engine stopped\n");
}

void process(const Sample *h, size_t h_len) {
    static unsigned expected = 0;
    unsigned got = (unsigned)h[0].value;
    int intact = 1;
    for (size_t i = 0; i < h_len; i++) {
        if ((unsigned)h[i].value != got) {
            intact = 0;
        }
    }
    printf("[sim] process: block %u from %s%s\n", got, half_name(h),
           (got == expected && intact) ? "" : "  <-- OUT OF ORDER OR TORN");
    expected = got + 1;
}

void summarize(const Sample *x, size_t x_len) {
    printf("[sim] summarize: %zu samples, halves hold blocks %u and %u\n", x_len,
           (unsigned)x[0].value, (unsigned)x[x_len - 1].value);
}
