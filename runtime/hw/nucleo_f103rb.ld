/* Linker script for the ST NUCLEO-F103RB (STM32F103RB, medium-density):
 * 128 KB of flash at 0x08000000, where the chip boots from (BOOT0 low, as
 * the board ties it), and 20 KB of SRAM at 0x20000000 -- RM0008 section 3.3
 * and the datasheet's memory map. The F103 has one DMA controller (DMA1)
 * and no CCM RAM.
 */
MEMORY
{
    FLASH (rx)  : ORIGIN = 0x08000000, LENGTH = 128K
    RAM   (rwx) : ORIGIN = 0x20000000, LENGTH = 20K
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
