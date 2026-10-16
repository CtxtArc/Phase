/* extern fn implementations for examples/circular_dma.phase (see spec
 * §3.9, §5.2). Hand-written, program-specific glue: the compiler only
 * generates the prototypes.
 *
 * Also plays the DMA controller in a host build: phase_sim_hardware() runs
 * whenever the program sleeps in a `wait`, before the interrupt handlers.
 * Each time, the engine fills the next half -- every sample with the next
 * block number -- and sets that half's STATUS bit, the way the real engine
 * raises its interrupt.
 */

#include "circular_dma.gen.h"

#include <stdio.h>

static Sample *halves[2];
static size_t half_len;
static int next_half = 0;
static unsigned next_block = 0;

static const char *half_name(const Sample *h) {
    return h == halves[0] ? "lo" : "hi";
}

void start_circular(Sample *lo, size_t lo_len, Sample *hi, size_t hi_len) {
    /* On real hardware: write the addresses and the length into the
     * engine's registers. */
    halves[0] = lo;
    halves[1] = hi;
    half_len = lo_len;
    printf("[sim] start_circular: engine set up over %zu + %zu samples\n", lo_len, hi_len);
}

#ifndef PHASE_TARGET_HW
void phase_sim_hardware(void) {
    /* Write-1-to-clear. */
    Dma0->STATUS &= ~Dma0->CLEAR;
    Dma0->CLEAR = 0;
    if (!(Dma0->CTRL & Dma0_CTRL_ENABLE_Msk) || halves[0] == NULL) {
        return;
    }
    if (Dma0->STATUS != 0) {
        return; /* the last interrupt hasn't been handled yet */
    }
    Sample *h = halves[next_half];
    unsigned block = next_block++;
    for (size_t i = 0; i < half_len; i++) {
        h[i].value = (float)block;
    }
    printf("[sim] DMA: %s is full (block %u), interrupt\n", half_name(h), block);
    Dma0->STATUS |= next_half == 0 ? Dma0_STATUS_HT_Msk : Dma0_STATUS_TC_Msk;
    next_half ^= 1;
}
#endif

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
