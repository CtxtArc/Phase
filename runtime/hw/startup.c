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

void Default_Handler(void) {
    for (;;) {
    }
}

/* The vector table: 16 system exceptions, then the board's 32 external
 * interrupts (IRQ 0 is entry 16). Each IRQn_Handler is a weak alias of
 * Default_Handler; a PHASE `interrupt fn h() @ irq N` defines IRQN_Handler
 * (in the generated C), which replaces it. */
void IRQ0_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ1_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ2_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ3_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ4_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ5_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ6_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ7_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ8_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ9_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ10_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ11_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ12_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ13_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ14_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ15_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ16_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ17_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ18_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ19_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ20_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ21_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ22_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ23_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ24_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ25_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ26_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ27_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ28_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ29_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ30_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ31_Handler(void) __attribute__((weak, alias("Default_Handler")));

__attribute__((section(".isr_vector"), used)) const uint32_t vector_table[16 + 32] = {
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
    (uint32_t)IRQ0_Handler,
    (uint32_t)IRQ1_Handler,
    (uint32_t)IRQ2_Handler,
    (uint32_t)IRQ3_Handler,
    (uint32_t)IRQ4_Handler,
    (uint32_t)IRQ5_Handler,
    (uint32_t)IRQ6_Handler,
    (uint32_t)IRQ7_Handler,
    (uint32_t)IRQ8_Handler,
    (uint32_t)IRQ9_Handler,
    (uint32_t)IRQ10_Handler,
    (uint32_t)IRQ11_Handler,
    (uint32_t)IRQ12_Handler,
    (uint32_t)IRQ13_Handler,
    (uint32_t)IRQ14_Handler,
    (uint32_t)IRQ15_Handler,
    (uint32_t)IRQ16_Handler,
    (uint32_t)IRQ17_Handler,
    (uint32_t)IRQ18_Handler,
    (uint32_t)IRQ19_Handler,
    (uint32_t)IRQ20_Handler,
    (uint32_t)IRQ21_Handler,
    (uint32_t)IRQ22_Handler,
    (uint32_t)IRQ23_Handler,
    (uint32_t)IRQ24_Handler,
    (uint32_t)IRQ25_Handler,
    (uint32_t)IRQ26_Handler,
    (uint32_t)IRQ27_Handler,
    (uint32_t)IRQ28_Handler,
    (uint32_t)IRQ29_Handler,
    (uint32_t)IRQ30_Handler,
    (uint32_t)IRQ31_Handler,
};
