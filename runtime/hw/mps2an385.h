/* Real hardware register map for the ARM MPS2 AN385 (Cortex-M3) target
 * board's CMSDK APB UART0 peripheral. Not synthetic -- these are the
 * actual documented register offsets and bit positions for the CMSDK APB
 * UART, cross-checked against QEMU's own model of this exact FPGA image
 * (qemu/hw/char/cmsdk-apb-uart.c, qemu/hw/arm/mps2.c), which is how real
 * embedded engineers validate firmware against a specific board's memory
 * map without needing physical silicon on the bench.
 *
 * This is the M10 milestone's "real hardware target" (spec §9): the
 * PHASE compiler itself is completely unaware this header exists. Only
 * the runtime layer changes -- see phase_runtime_hw.c.
 */
#ifndef PHASE_HW_MPS2AN385_H
#define PHASE_HW_MPS2AN385_H

#include <stdint.h>

typedef struct {
    volatile uint32_t DATA;      /* 0x00: RX/TX data (low byte) */
    volatile uint32_t STATE;     /* 0x04: status bits, see below */
    volatile uint32_t CTRL;      /* 0x08: control bits, see below */
    volatile uint32_t INTSTATUS; /* 0x0C: interrupt status/clear */
    volatile uint32_t BAUDDIV;   /* 0x10: baud rate divisor */
} Cmsdk_Uart;

#define UART_STATE_TXFULL (1u << 0)
#define UART_STATE_RXFULL (1u << 1)

#define UART_CTRL_TX_EN (1u << 0)
#define UART_CTRL_RX_EN (1u << 1)

/* UART0 base address on the AN385 FPGA image -- confirmed against
 * QEMU's hw/arm/mps2.c board model (`uartbase[] = {0x40004000, ...}`). */
#define UART0 ((Cmsdk_Uart *)0x40004000u)

#endif /* PHASE_HW_MPS2AN385_H */
