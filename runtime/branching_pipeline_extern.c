/* extern fn implementations for examples/branching_pipeline.phase (see
 * spec §3.9, §5.2). The compiler only ever generates *prototypes* for
 * `extern fn` -- these are the hand-written bodies. Program-specific by
 * design: a different PHASE program needs its own
 * runtime/<stem>_extern.c.
 *
 * This is the M8 demo: it exists to prove a *branching* program builds
 * with `cc` and runs both the `if` and `else` C paths for real, not just
 * that the M4 analyzer accepts the source.
 */

#include "branching_pipeline.gen.h"

#include <stdio.h>
#include <stdlib.h>

/* Reads an environment variable so the test harness (and a curious human
 * running the binary by hand) can force either path deterministically --
 * `PHASE_DEMO_USE_DMA=0` forces the @DEVICE branch, anything else (or
 * unset) takes the @DMA branch. Standing in for a real runtime decision
 * (e.g. "is a DMA engine present on this board"), same spirit as
 * device_capture's synthetic signal in the radio_pipeline demo. */
bool should_use_dma(void) {
    const char *env = getenv("PHASE_DEMO_USE_DMA");
    bool use_dma = (env == NULL) || (env[0] != '0');
    printf("[sim] should_use_dma: %s\n", use_dma ? "true" : "false");
    return use_dma;
}

void device_capture(uint8_t *dst, size_t dst_len) {
    printf("[sim] device_capture: filling %zu bytes (if-branch taken)\n", dst_len);
    for (size_t i = 0; i < dst_len; i++) {
        dst[i] = (uint8_t)i;
    }
}

void device_stream(uint8_t *dst, size_t dst_len) {
    printf("[sim] device_stream: streaming %zu bytes (else-branch taken)\n", dst_len);
    for (size_t i = 0; i < dst_len; i++) {
        dst[i] = (uint8_t)(dst_len - i);
    }
}
