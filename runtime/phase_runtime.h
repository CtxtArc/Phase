#ifndef PHASE_RUNTIME_H
#define PHASE_RUNTIME_H

#include <stddef.h>

/* Wait for a simulated DMA/device transfer associated with the named
 * entity to complete.
 *
 * In this v0.1 simulation model everything lives in one address space --
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

#endif /* PHASE_RUNTIME_H */
