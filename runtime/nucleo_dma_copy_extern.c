/* extern fn implementations for examples/nucleo_dma_copy.phase, for both
 * builds: `--target=nucleo-f401re` (the STM32's DMA2, at its address) and
 * a host build (the same registers in a simulated block, and
 * phase_sim_hardware() below playing the controller).
 *
 * produce() fills a block with k * 1000 + i at index i, for the next
 * block number k; a stream copies it into a half of the ring; check()
 * verifies every word of the copy, so a torn or missing transfer shows.
 * The PHASE side does everything in between, including handing the
 * controller the buffers' addresses. Written with the generated header's
 * PHASE_-prefixed names, as C next to a vendor's CMSIS header would be.
 */
#define PHASE_NO_SHORT_NAMES 1
#include "nucleo_dma_copy.gen.h"
#include "phase_runtime.h"

#include <stdint.h>

static unsigned next_block = 0;
static const uint32_t *ring_lo; /* where the low half is, for messages */

#ifdef PHASE_TARGET_HW
#include "phase_hw.h"
static void out_str(const char *s) { phase_hw_puts(s); }
static void out_uint(unsigned v) { phase_hw_put_u32(v); }
#else
#include <stdio.h>
static void out_str(const char *s) { fputs(s, stdout); }
static void out_uint(unsigned v) { printf("%u", v); }
#endif

void produce(uint32_t *block, size_t block_len) {
    unsigned k = next_block++;
    for (size_t i = 0; i < block_len; i++) {
        block[i] = k * 1000u + (uint32_t)i;
    }
}

void check(const uint32_t *h, size_t h_len) {
    if (ring_lo == NULL) {
        ring_lo = h; /* the first block arrives in the low half */
    }
    unsigned block = h[0] / 1000;
    int whole = 1;
    for (size_t i = 0; i < h_len; i++) {
        if (h[i] != block * 1000 + i) {
            whole = 0;
        }
    }
    out_str("block ");
    out_uint(block);
    out_str(h == ring_lo ? " in lo" : " in hi");
    out_str(whole ? ": ok\n" : ": TORN\n");
}

void summarize(const uint32_t *x, size_t x_len) {
    out_str("ring: blocks ");
    out_uint(x[0] / 1000);
    out_str(" and ");
    out_uint(x[x_len - 1] / 1000);
    out_str("\n");
}

/* ---- the simulated DMA2 (host build only) ---- */

#ifndef PHASE_TARGET_HW
void phase_sim_hardware(void) {
    phase_devtype_Stm32Dma_regs *dma = PHASE_DEV_Dma2;
    /* Write 1 to LIFCR to clear the same bit of LISR. */
    dma->LISR &= ~dma->LIFCR;
    dma->LIFCR = 0;
    struct {
        volatile uint32_t *cr, *ndtr, *par, *m0ar;
        unsigned tcif;
    } streams[2] = {
        {&dma->CR_0, &dma->NDTR_0, &dma->PAR_0, &dma->M0AR_0, PHASE_Stm32Dma_LISR_TCIF0_Pos},
        {&dma->CR_1, &dma->NDTR_1, &dma->PAR_1, &dma->M0AR_1, PHASE_Stm32Dma_LISR_TCIF1_Pos},
    };
    for (int s = 0; s < 2; s++) {
        if (!(*streams[s].cr & PHASE_Stm32Dma_CR_0_EN_Msk)) {
            continue;
        }
        /* Memory to memory: from PAR to M0AR, NDTR items. */
        const uint32_t *from = phase_addr_get(streams[s].par);
        uint32_t *to = phase_addr_get(streams[s].m0ar);
        for (uint32_t i = 0; i < *streams[s].ndtr; i++) {
            to[i] = from[i];
        }
        *streams[s].ndtr = 0;
        *streams[s].cr &= ~PHASE_Stm32Dma_CR_0_EN_Msk; /* the stream stops itself */
        dma->LISR |= 1u << streams[s].tcif;
    }
}
#endif
