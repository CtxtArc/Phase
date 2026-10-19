/* The registers the NUCLEO-F103RB runtime itself uses (phase_runtime_hw.c):
 * the clocks, pins and USART2 behind the board's ST-LINK virtual COM port,
 * and the Cortex-M debug status. Addresses and bits from RM0008 (the
 * STM32F10x reference manual); a PHASE program uses
 * examples/devices/nucleo_f103rb.phase instead.
 */
#ifndef PHASE_HW_NUCLEO_F103RB_H
#define PHASE_HW_NUCLEO_F103RB_H

#include <stdint.h>

#define F1_REG(addr) (*(volatile uint32_t *)(addr))

#define RCC_APB2ENR F1_REG(0x40021018u)
#define RCC_APB1ENR F1_REG(0x4002101Cu)
#define RCC_APB2ENR_IOPAEN (1u << 2)
#define RCC_APB1ENR_USART2EN (1u << 17)

/* The F1 sets a pin's mode in four bits of CRL (pins 0-7): MODE[1:0] then
 * CNF[1:0]. PA2 (TX): alternate-function push-pull, 2 MHz = 0xA. PA3 (RX):
 * floating input = 0x4, as at reset. */
#define GPIOA_CRL F1_REG(0x40010800u)

#define USART2_SR F1_REG(0x40004400u)
#define USART2_DR F1_REG(0x40004404u)
#define USART2_BRR F1_REG(0x40004408u)
#define USART2_CR1 F1_REG(0x4000440Cu)
#define USART_SR_TC (1u << 6)
#define USART_SR_TXE (1u << 7)
#define USART_CR1_TE (1u << 3)
#define USART_CR1_RE (1u << 2)
#define USART_CR1_UE (1u << 13)

/* 115200 baud from the 8 MHz HSI the chip starts on (every bus clock is
 * 8 MHz): USARTDIV = 4.34, mantissa 4, fraction 0.34 * 16 = 5 (0.6% off). */
#define USART_BRR_115200_HSI ((4u << 4) | 5u)

/* DBGMCU_CR (RM0008 section 31.16.3): keep the debug port alive in
 * Sleep/Stop/Standby, so a `wfi` never cuts the ST-LINK off. */
#define DBGMCU_CR F1_REG(0xE0042004u)
#define DBGMCU_CR_DBG_ALL_LOW_POWER (0x7u)

/* Debug Halting Control and Status: C_DEBUGEN is set while a debugger
 * (the ST-LINK, under probe-rs or OpenOCD) is attached. */
#define DHCSR F1_REG(0xE000EDF0u)
#define DHCSR_C_DEBUGEN (1u << 0)

#endif /* PHASE_HW_NUCLEO_F103RB_H */
