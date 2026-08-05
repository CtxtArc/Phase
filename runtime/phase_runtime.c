/* Must come before any system headers: exposes usleep() under -std=c11,
 * which otherwise hides POSIX extensions in strict mode. */
#define _POSIX_C_SOURCE 200809L

/* Generic simulated-hardware wait functions, shared by every PHASE demo
 * program (see spec §5). Deliberately program-agnostic -- unlike
 * runtime/<stem>_extern.c (one per demo program, implementing that
 * program's specific `extern fn`s), this file only implements the two
 * functions `sync` calls into regardless of which program generated the
 * call, so it never needs to include a program-specific generated header
 * and can always be linked.
 */

#include "phase_runtime.h"

#include <stdio.h>
#include <time.h>

/* v0.1 keeps everything in one address space (spec §5.1), so there is no
 * real separate memory to wait on -- a program's extern fns already
 * finish their work synchronously before returning. What these functions
 * simulate is the *timing*: a short, visible delay plus a trace message,
 * standing in for where a real driver would block on a DMA-complete
 * interrupt or a device-ready register. Swapping in a real
 * wait-for-completion here is the entire job of porting to real hardware
 * (spec §5.3) -- nothing in the generated code or the PHASE source needs
 * to change.
 */

static void simulated_transfer_delay(void) {
    struct timespec ts = { .tv_sec = 0, .tv_nsec = 2 * 1000 * 1000 }; /* 2ms */
    nanosleep(&ts, NULL);
}

void sim_dma_wait(const char *entity_name, const void *buf, size_t nbytes) {
    (void)buf;
    printf("[sim] sim_dma_wait(\"%s\"): waiting for DMA transfer to complete (%zu bytes)...\n",
           entity_name, nbytes);
    simulated_transfer_delay();
    printf("[sim] sim_dma_wait(\"%s\"): DMA transfer complete\n", entity_name);
}

void sim_device_wait(const char *entity_name, const void *buf, size_t nbytes) {
    (void)buf;
    printf("[sim] sim_device_wait(\"%s\"): waiting for device transfer to complete (%zu bytes)...\n",
           entity_name, nbytes);
    simulated_transfer_delay();
    printf("[sim] sim_device_wait(\"%s\"): device transfer complete\n", entity_name);
}
