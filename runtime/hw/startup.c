/* Minimal Cortex-M3 startup code: the two-entry vector table the CPU
 * requires (initial stack pointer + Reset_Handler address) and the
 * .data/.bss initialization every C runtime needs before `main` can rely
 * on globals being in a defined state. This is genuinely how a bare-metal
 * ARM target boots -- not a stand-in or simulation of booting.
 *
 * See mps2an385.ld for the memory layout this depends on.
 */
#include <stdint.h>

extern uint32_t _sidata; /* .data's load address in FLASH */
extern uint32_t _sdata;  /* .data's start address in RAM */
extern uint32_t _edata;  /* .data's end address in RAM */
extern uint32_t _sbss;   /* .bss start */
extern uint32_t _ebss;   /* .bss end */
extern uint32_t _estack; /* top of RAM -- initial stack pointer */

int main(void);

/* ARM semihosting SYS_EXIT (0x18) with ADP_Stopped_ApplicationExit. */
static void phase_hw_exit(void) {
    register uint32_t op __asm__("r0") = 0x18u;
    register uint32_t arg __asm__("r1") = 0x20026u;
    __asm__ volatile("bkpt 0xAB" : : "r"(op), "r"(arg) : "memory");
    for (;;) {
    }
}

void Reset_Handler(void) {
    for (uint32_t *src = &_sidata, *dst = &_sdata; dst < &_edata;) {
        *dst++ = *src++;
    }
    for (uint32_t *bss = &_sbss; bss < &_ebss; bss++) {
        *bss = 0;
    }

    main();

    /* main() returning has nowhere to go on bare metal -- there's no OS
     * to return control to. Under QEMU with semihosting enabled (as
     * `phase build --target=mps2-an385 --run` starts it), report a clean
     * exit to the host; without semihosting the BKPT faults, and the
     * HardFault handler halts -- the same end state as before. */
    phase_hw_exit();
}

static void Default_Handler(void) {
    for (;;) {
    }
}

/* Cortex-M3 only strictly requires entries 0 (initial SP) and 1 (Reset)
 * to boot; this demo never enables interrupts or triggers a fault, so
 * the remaining vector slots are never actually dispatched through, but
 * the CMSDK/ARMv7-M convention still reserves the first 16 words. */
__attribute__((section(".isr_vector"), used)) const uint32_t vector_table[16] = {
    (uint32_t)&_estack,
    (uint32_t)Reset_Handler,
    (uint32_t)Default_Handler, /* NMI */
    (uint32_t)Default_Handler, /* HardFault */
    (uint32_t)Default_Handler, /* MemManage */
    (uint32_t)Default_Handler, /* BusFault */
    (uint32_t)Default_Handler, /* UsageFault */
    0,
    0,
    0,
    0,
    (uint32_t)Default_Handler, /* SVCall */
    0,
    0,
    (uint32_t)Default_Handler, /* PendSV */
    (uint32_t)Default_Handler, /* SysTick */
};
