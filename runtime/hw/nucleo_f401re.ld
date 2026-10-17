/* Linker script for the ST NUCLEO-F401RE (STM32F401RE): 512 KB of flash
 * at 0x08000000, where the chip boots from (BOOT0 low, as the board ties
 * it), and 96 KB of SRAM at 0x20000000 -- RM0368 section 2.3, and
 * FLASH_BASE / SRAM1_BASE in ST's stm32f401xe.h. All of the SRAM is
 * reachable by both DMA controllers (the F401 has no CCM RAM).
 */
MEMORY
{
    FLASH (rx)  : ORIGIN = 0x08000000, LENGTH = 512K
    RAM   (rwx) : ORIGIN = 0x20000000, LENGTH = 96K
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
