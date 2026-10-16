/* Linker script for QEMU's mps2-an505 board model (Cortex-M33 with
 * TrustZone). The CPU starts in the Secure state with its vector table at
 * 0x10000000 (the IoT kit's reset INITSVTOR0), so everything is linked at
 * the Secure aliases of the board's memories -- see qemu/hw/arm/mps2-tz.c:
 * code in SSRAM1 (0x00000000, Secure alias 0x10000000), data in SSRAM2
 * (0x28000000, Secure alias 0x38000000).
 */
MEMORY
{
    FLASH (rx)  : ORIGIN = 0x10000000, LENGTH = 4M
    RAM   (rwx) : ORIGIN = 0x38000000, LENGTH = 2M
}

ENTRY(Reset_Handler)

SECTIONS
{
    .isr_vector : {
        KEEP(*(.isr_vector))
    } > FLASH

    .text : {
        *(.text*)
        *(.rodata*)
    } > FLASH

    _sidata = LOADADDR(.data);

    .data : {
        _sdata = .;
        *(.data*)
        _edata = .;
    } > RAM AT> FLASH

    .bss : {
        _sbss = .;
        *(.bss*)
        *(COMMON)
        _ebss = .;
    } > RAM

    _estack = ORIGIN(RAM) + LENGTH(RAM);
}
