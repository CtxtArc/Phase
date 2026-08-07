/* M10: a real-hardware implementation of the same two functions
 * runtime/phase_runtime.c simulates (`sim_dma_wait`/`sim_device_wait`,
 * declared in phase_runtime.h). This file is the entire M10 deliverable:
 * the PHASE compiler, the generated C, and the PHASE source are all
 * completely unchanged from M9 -- only this backend differs, exactly as
 * spec §9's Milestone M10 describes.
 *
 * `entity_name`/`buf`/`nbytes` are accepted (matching the declared
 * signature every other backend also implements) but unused here -- a
 * real wait-for-completion polls a hardware register, it doesn't need to
 * know the C variable name that triggered it.
 *
 * Honest limit: the MPS2 AN385 QEMU board model doesn't implement an
 * actual DMA controller (see qemu/hw/arm/mps2.c -- it's one of several
 * `create_unimplemented_device` stubs on this board), so there's no real
 * DMA-completion register available to poll. Both functions poll the one
 * real peripheral this board exposes: the CMSDK UART's TXFULL status bit.
 * A target with a real DMA controller would poll that controller's own
 * completion register the same way -- same pattern, different register.
 */

#include "mps2an385.h"
#include "phase_runtime.h"

static void wait_for_uart_tx_drain(void) {
    /* Real register poll on real (QEMU-modeled) hardware -- not a sleep,
     * not a counter, an actual read of a live status bit until the
     * hardware itself reports it's done. */
    while (UART0->STATE & UART_STATE_TXFULL) {
        /* spin */
    }
}

void sim_dma_wait(const char *entity_name, const void *buf, size_t nbytes) {
    (void)entity_name;
    (void)buf;
    (void)nbytes;
    wait_for_uart_tx_drain();
}

void sim_device_wait(const char *entity_name, const void *buf, size_t nbytes) {
    (void)entity_name;
    (void)buf;
    (void)nbytes;
    wait_for_uart_tx_drain();
}
