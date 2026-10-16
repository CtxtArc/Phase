#ifndef PHASE_RUNTIME_H
#define PHASE_RUNTIME_H

#include <stddef.h>

/* Wait for a simulated DMA/device transfer associated with the named
 * entity to complete.
 *
 * In this simulation model everything lives in one address space --
 * there's no real separate DMA/device memory to wait on. What's real is
 * the *timing*: device_capture()/device_playback() (see
 * radio_pipeline_extern.c) record a short simulated transfer delay when
 * they run, and these wait functions block until that delay has elapsed,
 * so `sync` has genuine, observable sequencing behavior rather than being
 * a no-op. `entity_name` and `buf`/`nbytes` are used only for the trace
 * message; a real hardware backend would replace the body of these two
 * functions with an actual wait-for-completion (e.g. polling a DMA
 * controller register or an interrupt flag) without the generated code
 * or the PHASE source needing to change at all.
 */
void sim_dma_wait(const char *entity_name, const void *buf, size_t nbytes);
void sim_device_wait(const char *entity_name, const void *buf, size_t nbytes);

/* v2.4: `address` registers. `volatile_write(Dma.DEST, address_of(buf))`
 * writes buf's address to the register. On the target that is the whole
 * story, and phase_addr_get reads it back. A host build's pointers are
 * wider than a 32-bit register, so phase_addr_set also keeps the real
 * pointer, and phase_addr_get -- what a simulated device calls to find the
 * memory it should read or write -- returns it. */
#include <stdint.h>
#ifdef PHASE_TARGET_HW
static inline void phase_addr_set(volatile uint32_t *reg, void *p) {
    *reg = (uint32_t)(uintptr_t)p;
}
static inline void *phase_addr_get(const volatile uint32_t *reg) {
    return (void *)(uintptr_t)*reg;
}
#else
void phase_addr_set(volatile uint32_t *reg, void *p);
void *phase_addr_get(const volatile uint32_t *reg);
#endif

#endif /* PHASE_RUNTIME_H */
