/* examples/nucleo_adc_dma.phase's C side: report one round of samples.
 *
 * On the board each sample is ADC1's reading of VREFINT, the chip's
 * internal reference (1.21 V typical). ST measures it once per chip at
 * the factory, with the supply (VDDA) at exactly 3.3 V, and stores the
 * reading at VREFINT_CAL (0x1FFF7A2A, STM32F401 datasheet, "Reference
 * voltage"). Comparing the two gives the supply voltage actually present:
 * VDDA = 3300 mV * VREFINT_CAL / reading. A round is plausible when its
 * samples sit close together and the supply comes out near 3.3 V.
 *
 * A host build has no ADC: phase_sim_hardware() below plays ADC1 and DMA2
 * stream 0, filling the buffer with readings around a made-up
 * calibration value, then clearing the stream's EN bit as the real one
 * does -- which is what `sync` waits for. */
#define PHASE_NO_SHORT_NAMES 1
#include "nucleo_adc_dma.gen.h"
#include "phase_runtime.h"

#ifdef PHASE_TARGET_HW
#include "phase_hw.h"
#define VREFINT_CAL (*(const volatile uint16_t *)0x1FFF7A2Au)
static void say(const char *s) { phase_hw_puts(s); }
static void say_u32(uint32_t v) { phase_hw_put_u32(v); }
#else
#include <stdio.h>
#define VREFINT_CAL ((uint16_t)1503u)
static void say(const char *s) { fputs(s, stdout); }
static void say_u32(uint32_t v) { printf("%u", (unsigned)v); }
#endif

static uint32_t round_no;

void report(const uint16_t *samples, size_t samples_len) {
    uint32_t sum = 0, lo = 0xFFFF, hi = 0;
    for (size_t i = 0; i < samples_len; i++) {
        uint32_t s = samples[i];
        sum += s;
        lo = s < lo ? s : lo;
        hi = s > hi ? s : hi;
    }
    uint32_t avg = (sum + (uint32_t)samples_len / 2) / (uint32_t)samples_len;
    uint32_t vdda = avg ? (3300u * VREFINT_CAL + avg / 2) / avg : 0;
    int plausible = avg > 0 && hi - lo < 64 && vdda > 3000 && vdda < 3600;
    say("round ");
    say_u32(++round_no);
    say(": VREFINT ");
    say_u32(avg);
    say(" (");
    say_u32(lo);
    say("..");
    say_u32(hi);
    say("), factory value ");
    say_u32(VREFINT_CAL);
    say(", supply ");
    say_u32(vdda);
    say(plausible ? " mV: ok\n" : " mV: IMPLAUSIBLE\n");
}

#ifndef PHASE_TARGET_HW
/* ADC1 converting VREFINT into DMA2 stream 0, simulated. */
void phase_sim_hardware(void) {
    phase_devtype_Stm32Dma_regs *dma = PHASE_DEV_Dma2;
    if (!(dma->CR_0 & PHASE_Stm32Dma_CR_0_EN_Msk)) {
        return;
    }
    if (!(PHASE_DEV_Adc1->CR2 & PHASE_Adc1_CR2_DMA_Msk) ||
        !(PHASE_DEV_Adc1->CR2 & PHASE_Adc1_CR2_ADON_Msk)) {
        return;
    }
    /* The stream reads from PAR (the ADC's DR) into M0AR. */
    const volatile uint32_t *dr = phase_addr_get(&dma->PAR_0);
    uint16_t *to = phase_addr_get(&dma->M0AR_0);
    static uint32_t noise = 1;
    for (uint32_t i = 0; i < dma->NDTR_0; i++) {
        noise = noise * 1103515245u + 12345u;
        PHASE_DEV_Adc1->DR = 1500u + (noise >> 16) % 7u; /* one conversion */
        to[i] = (uint16_t)*dr;
    }
    dma->NDTR_0 = 0;
    dma->CR_0 &= ~PHASE_Stm32Dma_CR_0_EN_Msk; /* the stream is done */
}
#endif
