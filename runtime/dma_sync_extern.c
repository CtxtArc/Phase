/* extern fns for examples/dma_sync.phase: fill a block with a new pattern,
 * and verify the copy. On the board (QEMU's AN505) the PL081 does the
 * copy; in a host build, phase_sim_hardware() below plays it, called
 * while `sync` polls the channel. */
#include "dma_sync.gen.h"
#include "phase_runtime.h"

static uint32_t round_no;

#ifdef PHASE_TARGET_HW
#include "phase_hw.h"
static void say(const char *s) { phase_hw_puts(s); }
static void say_u32(uint32_t v) { phase_hw_put_u32(v); }
#else
#include <stdio.h>
static void say(const char *s) { fputs(s, stdout); }
static void say_u32(uint32_t v) { printf("%u", (unsigned)v); }
#endif

void fill(uint32_t *b, size_t b_len) {
    round_no++;
    for (size_t i = 0; i < b_len; i++) {
        b[i] = round_no * 0x10000u + (uint32_t)i;
    }
}

void verify(const uint32_t *b, size_t b_len) {
    size_t good = 0;
    for (size_t i = 0; i < b_len; i++) {
        good += b[i] == round_no * 0x10000u + (uint32_t)i;
    }
    say("copy ");
    say_u32(round_no);
    say(good == b_len ? ": all 64 words\n" : ": WRONG\n");
}

#ifndef PHASE_TARGET_HW
/* The PL081, simulated: an enabled channel copies, then turns itself off. */
void phase_sim_hardware(void) {
    phase_devtype_Pl081_regs *dma = PHASE_DEV_Dma0;
    if (!(dma->CCONFIG_0 & 1u)) {
        return;
    }
    const uint32_t *from = phase_addr_get(&dma->CSRC_0);
    uint32_t *to = phase_addr_get(&dma->CDEST_0);
    for (uint32_t i = 0; i < (dma->CCONTROL_0 & 0xfffu); i++) {
        to[i] = from[i];
    }
    dma->CCONFIG_0 &= ~1u;
}
#endif
