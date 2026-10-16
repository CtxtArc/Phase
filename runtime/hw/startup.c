/* Minimal Cortex-M startup code (the M3 of the AN385 and the M33 of the
 * AN505 boot the same way): the two-entry vector table the CPU
 * requires (initial stack pointer + Reset_Handler address) and the
 * .data/.bss initialization every C runtime needs before `main` can rely
 * on globals being in a defined state. This is genuinely how a bare-metal
 * ARM target boots -- not a stand-in or simulation of booting.
 *
 * See mps2an385.ld / mps2an505.ld for the memory layouts this depends on.
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

/* The vector table: 16 system exceptions, then 128 external interrupts
 * (IRQ 0 is entry 16) -- enough for both boards: the AN385 has 32, the
 * AN505 124 (32 inside its IoT kit, then 92 from the FPGA image). Each
 * IRQn_Handler is a weak alias of Default_Handler; a PHASE
 * `interrupt fn h() @ irq N` defines IRQN_Handler (in the generated C),
 * which replaces it. */
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
void IRQ32_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ33_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ34_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ35_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ36_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ37_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ38_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ39_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ40_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ41_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ42_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ43_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ44_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ45_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ46_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ47_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ48_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ49_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ50_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ51_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ52_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ53_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ54_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ55_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ56_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ57_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ58_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ59_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ60_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ61_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ62_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ63_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ64_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ65_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ66_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ67_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ68_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ69_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ70_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ71_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ72_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ73_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ74_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ75_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ76_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ77_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ78_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ79_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ80_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ81_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ82_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ83_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ84_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ85_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ86_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ87_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ88_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ89_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ90_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ91_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ92_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ93_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ94_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ95_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ96_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ97_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ98_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ99_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ100_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ101_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ102_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ103_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ104_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ105_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ106_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ107_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ108_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ109_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ110_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ111_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ112_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ113_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ114_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ115_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ116_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ117_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ118_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ119_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ120_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ121_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ122_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ123_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ124_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ125_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ126_Handler(void) __attribute__((weak, alias("Default_Handler")));
void IRQ127_Handler(void) __attribute__((weak, alias("Default_Handler")));

__attribute__((section(".isr_vector"), used)) const uint32_t vector_table[16 + 128] = {
    (uint32_t)&_estack,
    (uint32_t)Reset_Handler,
    (uint32_t)Default_Handler, /* NMI */
    (uint32_t)Default_Handler, /* HardFault */
    (uint32_t)Default_Handler, /* MemManage */
    (uint32_t)Default_Handler, /* BusFault */
    (uint32_t)Default_Handler, /* UsageFault */
    (uint32_t)Default_Handler, /* SecureFault (Armv8-M; reserved on the M3) */
    0,
    0,
    0,
    (uint32_t)Default_Handler, /* SVCall */
    (uint32_t)Default_Handler, /* DebugMonitor */
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
    (uint32_t)IRQ32_Handler,
    (uint32_t)IRQ33_Handler,
    (uint32_t)IRQ34_Handler,
    (uint32_t)IRQ35_Handler,
    (uint32_t)IRQ36_Handler,
    (uint32_t)IRQ37_Handler,
    (uint32_t)IRQ38_Handler,
    (uint32_t)IRQ39_Handler,
    (uint32_t)IRQ40_Handler,
    (uint32_t)IRQ41_Handler,
    (uint32_t)IRQ42_Handler,
    (uint32_t)IRQ43_Handler,
    (uint32_t)IRQ44_Handler,
    (uint32_t)IRQ45_Handler,
    (uint32_t)IRQ46_Handler,
    (uint32_t)IRQ47_Handler,
    (uint32_t)IRQ48_Handler,
    (uint32_t)IRQ49_Handler,
    (uint32_t)IRQ50_Handler,
    (uint32_t)IRQ51_Handler,
    (uint32_t)IRQ52_Handler,
    (uint32_t)IRQ53_Handler,
    (uint32_t)IRQ54_Handler,
    (uint32_t)IRQ55_Handler,
    (uint32_t)IRQ56_Handler,
    (uint32_t)IRQ57_Handler,
    (uint32_t)IRQ58_Handler,
    (uint32_t)IRQ59_Handler,
    (uint32_t)IRQ60_Handler,
    (uint32_t)IRQ61_Handler,
    (uint32_t)IRQ62_Handler,
    (uint32_t)IRQ63_Handler,
    (uint32_t)IRQ64_Handler,
    (uint32_t)IRQ65_Handler,
    (uint32_t)IRQ66_Handler,
    (uint32_t)IRQ67_Handler,
    (uint32_t)IRQ68_Handler,
    (uint32_t)IRQ69_Handler,
    (uint32_t)IRQ70_Handler,
    (uint32_t)IRQ71_Handler,
    (uint32_t)IRQ72_Handler,
    (uint32_t)IRQ73_Handler,
    (uint32_t)IRQ74_Handler,
    (uint32_t)IRQ75_Handler,
    (uint32_t)IRQ76_Handler,
    (uint32_t)IRQ77_Handler,
    (uint32_t)IRQ78_Handler,
    (uint32_t)IRQ79_Handler,
    (uint32_t)IRQ80_Handler,
    (uint32_t)IRQ81_Handler,
    (uint32_t)IRQ82_Handler,
    (uint32_t)IRQ83_Handler,
    (uint32_t)IRQ84_Handler,
    (uint32_t)IRQ85_Handler,
    (uint32_t)IRQ86_Handler,
    (uint32_t)IRQ87_Handler,
    (uint32_t)IRQ88_Handler,
    (uint32_t)IRQ89_Handler,
    (uint32_t)IRQ90_Handler,
    (uint32_t)IRQ91_Handler,
    (uint32_t)IRQ92_Handler,
    (uint32_t)IRQ93_Handler,
    (uint32_t)IRQ94_Handler,
    (uint32_t)IRQ95_Handler,
    (uint32_t)IRQ96_Handler,
    (uint32_t)IRQ97_Handler,
    (uint32_t)IRQ98_Handler,
    (uint32_t)IRQ99_Handler,
    (uint32_t)IRQ100_Handler,
    (uint32_t)IRQ101_Handler,
    (uint32_t)IRQ102_Handler,
    (uint32_t)IRQ103_Handler,
    (uint32_t)IRQ104_Handler,
    (uint32_t)IRQ105_Handler,
    (uint32_t)IRQ106_Handler,
    (uint32_t)IRQ107_Handler,
    (uint32_t)IRQ108_Handler,
    (uint32_t)IRQ109_Handler,
    (uint32_t)IRQ110_Handler,
    (uint32_t)IRQ111_Handler,
    (uint32_t)IRQ112_Handler,
    (uint32_t)IRQ113_Handler,
    (uint32_t)IRQ114_Handler,
    (uint32_t)IRQ115_Handler,
    (uint32_t)IRQ116_Handler,
    (uint32_t)IRQ117_Handler,
    (uint32_t)IRQ118_Handler,
    (uint32_t)IRQ119_Handler,
    (uint32_t)IRQ120_Handler,
    (uint32_t)IRQ121_Handler,
    (uint32_t)IRQ122_Handler,
    (uint32_t)IRQ123_Handler,
    (uint32_t)IRQ124_Handler,
    (uint32_t)IRQ125_Handler,
    (uint32_t)IRQ126_Handler,
    (uint32_t)IRQ127_Handler,
};
