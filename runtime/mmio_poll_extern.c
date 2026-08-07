/* extern fn implementation for examples/mmio_poll.phase (see spec §3.9,
 * §5.2). The compiler only ever generates *prototypes* for `extern fn`
 * -- this is the hand-written body.
 *
 * This is the M11 demo: it exists to prove `volatile_read` compiled
 * directly into a `while` condition (see phase_pir::PirExpr::VolatileRead
 * and spec §9, Milestone M11) actually re-polls real (simulated)
 * hardware state across multiple iterations -- not zero, not once, not
 * forever, exactly as many as it takes the simulated device to become
 * ready.
 *
 * READY and DONE are declared `extern volatile uint32_t` in
 * mmio_poll.gen.h (every M6 MMIO register is) and *defined* in
 * mmio_poll.gen.c -- ordinary C external linkage is what lets this file
 * and the compiler-generated one share the same two globals.
 */

#include "mmio_poll.gen.h"

#include <stdio.h>

static int calls = 0;

void arm_if_ready(void) {
    calls++;
    printf("[sim] arm_if_ready: call #%d\n", calls);
    if (calls >= 3) {
        /* Real hardware would set this from an interrupt handler or a
         * completed DMA transfer; here it's just "the device took a
         * few cycles to come up." The *loop that notices* is 100%
         * compiler-generated PHASE code, not this file. */
        READY = 1;
    }
}
