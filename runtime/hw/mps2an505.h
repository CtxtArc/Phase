/* The MPS2 AN505 (Cortex-M33) board's CMSDK APB UART0, for the runtime's
 * own register polls (phase_runtime_hw.c). The program runs in the Secure
 * state, so this is UART0's Secure alias: 0x50200000 (its Non-secure
 * address is 0x40200000) -- see qemu/hw/arm/mps2-tz.c.
 */
#ifndef PHASE_HW_MPS2AN505_H
#define PHASE_HW_MPS2AN505_H

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

#define UART0 ((Cmsdk_Uart *)0x50200000u)

#endif /* PHASE_HW_MPS2AN505_H */
