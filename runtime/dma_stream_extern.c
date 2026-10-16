/* extern fn implementations for examples/dma_stream.phase, for both
 * builds: `--target=mps2-an505` (a real PL081, at its address) and a host
 * build (the same registers in a simulated block, and
 * phase_sim_hardware() below playing the controller).
 *
 * The "sensor" is a table of samples in flash: block k holds
 * k * 1000 + i at index i. Each transfer copies the next block into a
 * half, so check() can tell that the data really arrived, whole and in
 * order.
 */

#include "dma_stream.gen.h"

#include <stdint.h>

#define WORDS 16
#define BLOCKS 10

static const uint32_t samples[BLOCKS * WORDS] = {
#define ROW(k)                                                                                    \
    k * 1000 + 0, k * 1000 + 1, k * 1000 + 2, k * 1000 + 3, k * 1000 + 4, k * 1000 + 5,           \
        k * 1000 + 6, k * 1000 + 7, k * 1000 + 8, k * 1000 + 9, k * 1000 + 10, k * 1000 + 11,     \
        k * 1000 + 12, k * 1000 + 13, k * 1000 + 14, k * 1000 + 15
    ROW(0), ROW(1), ROW(2), ROW(3), ROW(4), ROW(5), ROW(6), ROW(7), ROW(8), ROW(9),
#undef ROW
};

static unsigned next_block = 0;
static const uint32_t *halves[2]; /* the buffer each channel fills */

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

/* ---- pointing a channel at a half ---- */

#ifndef PHASE_TARGET_HW
/* A host build's registers are 32 bits wide, its pointers 64: the
 * simulated controller keeps the real addresses here. */
static const uint32_t *sim_src[2];
static uint32_t *sim_dest[2];
#endif

static void aim(int ch, uint32_t *dest) {
    const uint32_t *src = &samples[(next_block % BLOCKS) * WORDS];
    next_block++;
    halves[ch] = dest;
#ifdef PHASE_TARGET_HW
    if (ch == 0) {
        Dma0->CSRC_0 = (uint32_t)(uintptr_t)src;
        Dma0->CDEST_0 = (uint32_t)(uintptr_t)dest;
        Dma0->CLLI_0 = 0;
    } else {
        Dma0->CSRC_1 = (uint32_t)(uintptr_t)src;
        Dma0->CDEST_1 = (uint32_t)(uintptr_t)dest;
        Dma0->CLLI_1 = 0;
    }
#else
    sim_src[ch] = src;
    sim_dest[ch] = dest;
#endif
}

void aim_ch0(uint32_t *dest, size_t dest_len) {
    (void)dest_len;
    aim(0, dest);
}

void aim_ch1(uint32_t *dest, size_t dest_len) {
    (void)dest_len;
    aim(1, dest);
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
        volatile uint32_t *control = ch == 0 ? &Dma0->CCONTROL_0 : &Dma0->CCONTROL_1;
        volatile uint32_t *config = ch == 0 ? &Dma0->CCONFIG_0 : &Dma0->CCONFIG_1;
        if (!(*config & 1u)) {
            continue;
        }
        uint32_t words = *control & 0xfffu;
        for (uint32_t i = 0; i < words; i++) {
            sim_dest[ch][i] = sim_src[ch][i];
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

/* ---- the CPU's side ---- */

void check(const uint32_t *h, size_t h_len) {
    unsigned block = h[0] / 1000;
    int whole = 1;
    for (size_t i = 0; i < h_len; i++) {
        if (h[i] != block * 1000 + i) {
            whole = 0;
        }
    }
    out_str("block ");
    out_uint(block);
    out_str(h == halves[0] ? " in lo" : " in hi");
    out_str(whole ? ": ok\n" : ": TORN\n");
}

void summarize(const uint32_t *x, size_t x_len) {
    out_str("ring: blocks ");
    out_uint(x[0] / 1000);
    out_str(" and ");
    out_uint(x[x_len - 1] / 1000);
    out_str("\n");
}
