/* examples/nucleo_f103rb_blink.phase's one extern: report a tick on the console
 * (the board's, through phase_hw.h; stdout in a host build). */
#include "nucleo_f103rb_blink.gen.h"

#ifdef PHASE_TARGET_HW
#include "phase_hw.h"

void report(uint32_t n) {
    phase_hw_puts("tick ");
    phase_hw_put_u32(n);
    phase_hw_puts("\n");
}
#else
#include <stdio.h>

void report(uint32_t n) {
    printf("tick %u\n", (unsigned)n);
}
#endif
