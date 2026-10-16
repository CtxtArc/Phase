/* extern fn implementations for examples/dma_stream.phase, for both
 * builds: `--target=mps2-an505` (a real PL081, at its address) and a host
 * build (the same registers in a simulated block, and
 * phase_sim_hardware() below playing the controller).
 *
 * produce() fills a block with sample number k * 1000 + i at index i, for
 * the next block number k; the controller copies it into a half of the
 * ring; check() verifies the copy -- every word, so a torn or missing
 * transfer shows. The PHASE side does everything in between, including
 * handing the controller the buffers' addresses.
 */

#include "dma_stream.gen.h"
#include "phase_runtime.h"

#include <stdint.h>

static unsigned next_block = 0;
static const uint32_t *ring_lo; /* where the low half is, for messages */

/* ---- output: the board's UART0, or stdout ---- */

#ifdef PHASE_TARGET_HW
static void out_char(char c) {
    while (Uart0->STATE & CmsdkUart_STATE_TXFULL_Msk) {
    }
    Uart0->DATA = (uint32_t)(unsigned char)c;
}
#else
#include <stdio.h>
static void out_char(char c) { putchar(c); }
#endif

static void out_str(const char *s) {
    while (*s) {
        out_char(*s++);
    }
}

static void out_uint(unsigned v) {
    char buf[12];
    int n = 0;
    do {
        buf[n++] = (char)('0' + v % 10);
        v /= 10;
    } while (v);
    while (n) {
        out_char(buf[--n]);
    }
}

/* ---- the CPU's side ---- */

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

/* ---- the simulated PL081 (host build only) ---- */

#ifndef PHASE_TARGET_HW
void phase_sim_hardware(void) {
    /* Write-1-to-clear. */
    Dma0->INTTCSTATUS &= ~Dma0->INTTCCLEAR;
    Dma0->RAWINTTCSTATUS &= ~Dma0->INTTCCLEAR;
    Dma0->INTTCCLEAR = 0;
    if (!(Dma0->CONFIG & Pl081_CONFIG_ENABLE_Msk)) {
        return;
    }
    for (int ch = 0; ch < 2; ch++) {
        volatile uint32_t *src = ch == 0 ? &Dma0->CSRC_0 : &Dma0->CSRC_1;
        volatile uint32_t *dest = ch == 0 ? &Dma0->CDEST_0 : &Dma0->CDEST_1;
        volatile uint32_t *control = ch == 0 ? &Dma0->CCONTROL_0 : &Dma0->CCONTROL_1;
        volatile uint32_t *config = ch == 0 ? &Dma0->CCONFIG_0 : &Dma0->CCONFIG_1;
        if (!(*config & 1u)) {
            continue;
        }
        const uint32_t *from = phase_addr_get(src);
        uint32_t *to = phase_addr_get(dest);
        uint32_t words = *control & 0xfffu;
        for (uint32_t i = 0; i < words; i++) {
            to[i] = from[i];
        }
        *control &= ~0xfffu;
        *config &= ~1u; /* the channel disables itself when done */
        if ((*control & (1u << 31)) && (*config & (1u << 15))) {
            Dma0->RAWINTTCSTATUS |= 1u << ch;
            Dma0->INTTCSTATUS |= 1u << ch;
        }
    }
}
#endif
